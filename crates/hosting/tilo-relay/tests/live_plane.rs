//! Acknowledged commits and the live plane through one host: the author
//! learns the host order of a persisted commit while other subscribers get
//! it as a plain delta, and live frames reach only live members, stamped
//! with the authenticated sender and validated by the host.

use std::time::Duration;

use tilo_relay::live::{LiveFrame, LiveRecord, PresenceFrame};
use tilo_relay::{Envelope, RelayClient};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser, client_endpoint, spawn_relay};

const WS: u64 = 41;

async fn connect(
    user: &TestUser,
    device: &TestDevice,
    addr: iroh_base::EndpointAddr,
) -> RelayClient {
    let client = RelayClient::connect(
        client_endpoint().await,
        addr,
        user.user_pk,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    client
        .send(Envelope::Subscribe { workspace_id: WS })
        .unwrap();
    next(&client, |env| matches!(env, Envelope::Ready { .. })).await;
    client
}

async fn next(client: &RelayClient, mut want: impl FnMut(&Envelope) -> bool) -> Envelope {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let env = client.recv().await.expect("session open");
            if want(&env) {
                return env;
            }
        }
    })
    .await
    .expect("expected envelope")
}

async fn nothing_matching(client: &RelayClient, mut want: impl FnMut(&Envelope) -> bool) {
    let deadline = tokio::time::Instant::now()
        .checked_add(Duration::from_millis(300))
        .unwrap();
    while let Ok(Some(env)) = tokio::time::timeout_at(deadline, client.recv()).await {
        assert!(!want(&env), "unexpected envelope {env:?}");
    }
}

fn presence(seq: u64) -> Vec<u8> {
    LiveFrame::Presence(PresenceFrame {
        seq,
        sent_at_ms: 1,
        focus: Some(LiveRecord {
            app: "so.tilo.notes".into(),
            record: "r".into(),
        }),
        pointer: Some([3.0, 4.0]),
        selection: None,
        drag: None,
        participant: None,
    })
    .encode()
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario: commits, then the live plane, in order"
)]
async fn commits_are_acknowledged_and_live_frames_reach_live_members() {
    let user = TestUser::fresh("Ada");
    let host = user.enrol("home-server", DeviceCapabilities::host());
    let a = user.enrol("browser", DeviceCapabilities::member());
    let b = user.enrol("laptop", DeviceCapabilities::primary());
    let c = user.enrol("phone", DeviceCapabilities::member());
    let relay = spawn_relay(
        &user,
        host,
        &[WS],
        &[a.device_pubkey(), b.device_pubkey(), c.device_pubkey()],
    )
    .await;
    let addr = relay.relay.node_addr();
    let a_client = connect(&user, &a, addr.clone()).await;
    let b_client = connect(&user, &b, addr.clone()).await;
    let c_client = connect(&user, &c, addr).await;

    // Commit: the author gets the host order, the others a plain Delta.
    let mut seqs = Vec::new();
    for op in [[1u8; 16], [2u8; 16]] {
        a_client
            .send(Envelope::Commit {
                workspace_id: WS,
                op,
                author_device_public_key: a.device_pubkey(),
                authored_at_ns: 0,
                delta: vec![op[0]; 4],
            })
            .unwrap();
        let Envelope::Committed { op: acked, seq, .. } =
            next(&a_client, |env| matches!(env, Envelope::Committed { .. })).await
        else {
            unreachable!()
        };
        assert_eq!(acked, op);
        seqs.push(seq);
        let Envelope::Delta {
            author_device_public_key,
            delta,
            ..
        } = next(&b_client, |env| matches!(env, Envelope::Delta { .. })).await
        else {
            unreachable!()
        };
        assert_eq!(author_device_public_key, a.device_pubkey());
        assert_eq!(delta, vec![op[0]; 4]);
    }
    assert!(seqs[0] < seqs[1], "host order increases");

    // A commit forged as another author is refused, not fanned out.
    a_client
        .send(Envelope::Commit {
            workspace_id: WS,
            op: [3; 16],
            author_device_public_key: b.device_pubkey(),
            authored_at_ns: 0,
            delta: vec![9],
        })
        .unwrap();
    assert!(matches!(
        next(&a_client, |env| matches!(env, Envelope::CommitRefused { .. })).await,
        Envelope::CommitRefused { op, .. } if op == [3; 16]
    ));

    // Live: a and b join, c only subscribes.
    a_client
        .send(Envelope::LiveJoin { workspace_id: WS })
        .unwrap();
    b_client
        .send(Envelope::LiveJoin { workspace_id: WS })
        .unwrap();
    let joined = next(&a_client, |env| matches!(env, Envelope::LiveMember { .. })).await;
    assert_eq!(
        joined,
        Envelope::LiveMember {
            workspace_id: WS,
            device_public_key: b.device_pubkey(),
            active: true,
        }
    );
    // A forged sender and an invalid frame: the first arrives stamped with
    // the real sender, the second is dropped by the host.
    b_client
        .send(Envelope::Live {
            workspace_id: WS,
            from_device_public_key: c.device_pubkey(),
            frame: vec![0xff, 0x00],
        })
        .unwrap();
    b_client
        .send(Envelope::Live {
            workspace_id: WS,
            from_device_public_key: c.device_pubkey(),
            frame: presence(1),
        })
        .unwrap();
    let Envelope::Live {
        from_device_public_key,
        frame,
        ..
    } = next(&a_client, |env| matches!(env, Envelope::Live { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(from_device_public_key, b.device_pubkey());
    assert_eq!(frame, presence(1));
    nothing_matching(&c_client, |env| matches!(env, Envelope::Live { .. })).await;

    // Leaving is announced.
    b_client
        .send(Envelope::LiveLeave { workspace_id: WS })
        .unwrap();
    assert!(matches!(
        next(&a_client, |env| matches!(env, Envelope::LiveMember { .. })).await,
        Envelope::LiveMember { active: false, .. }
    ));
}
