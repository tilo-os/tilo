//! Acknowledged commits and the live plane's membership through one host:
//! the author learns the host order of a persisted commit while other
//! subscribers get it as a plain delta, and a device that joins the plane
//! learns who is on it and is itself told to the others.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]
use std::time::Duration;

use tilo_relay::Envelope;
use tilo_user_identity::DeviceCapabilities;

use crate::common::{TestUser, connect, next, spawn_relay};

fn ws() -> tilo_relay_protocol::WorkspaceId {
    tilo_relay_protocol::WorkspaceId::from_u128(41)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario: commits, then the live plane, in order"
)]
async fn commits_are_acknowledged_and_live_joins_are_announced() {
    let user = TestUser::fresh("Ada");
    let host = user.enrol("home-server", DeviceCapabilities::host());
    let a = user.enrol("browser", DeviceCapabilities::member());
    let b = user.enrol("laptop", DeviceCapabilities::primary());
    let c = user.enrol("phone", DeviceCapabilities::member());
    let relay = spawn_relay(
        &user,
        host,
        &[ws()],
        &[a.device_pubkey(), b.device_pubkey(), c.device_pubkey()],
    )
    .await;
    let addr = relay.relay.node_addr();
    let a_client = connect(&user, &a, addr.clone(), ws()).await;
    let b_client = connect(&user, &b, addr.clone(), ws()).await;
    let c_client = connect(&user, &c, addr, ws()).await;

    // Commit: the author gets the host order, the others a plain Delta.
    let mut seqs = Vec::new();
    for op in [[1u8; 16], [2u8; 16]] {
        a_client
            .send(Envelope::Commit {
                workspace_id: ws(),
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
            workspace_id: ws(),
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

    // Live: a and b join, c only subscribes. The joiner learns who is
    // already there, and they learn it joined.
    a_client
        .send(Envelope::RetiredLiveJoin { workspace_id: ws() })
        .unwrap();
    b_client
        .send(Envelope::RetiredLiveJoin { workspace_id: ws() })
        .unwrap();
    let joined = next(&a_client, |env| {
        matches!(env, Envelope::RetiredLiveMember { .. })
    })
    .await;
    assert_eq!(
        joined,
        Envelope::RetiredLiveMember {
            workspace_id: ws(),
            device_public_key: b.device_pubkey(),
            active: true,
        }
    );

    // Leaving is announced: b's session ends, its host drops it from the
    // plane and tells a, which is still on it.
    drop(b_client);
    assert!(matches!(
        next(&a_client, |env| matches!(
            env,
            Envelope::RetiredLiveMember { .. }
        ))
        .await,
        Envelope::RetiredLiveMember {
            device_public_key,
            active: false,
            ..
        } if device_public_key == b.device_pubkey()
    ));
    // c never joined, so it hears nothing of either.
    let quiet = tokio::time::timeout(Duration::from_millis(300), async {
        loop {
            if let Some(env) = c_client.recv().await {
                assert!(
                    !matches!(env, Envelope::RetiredLiveMember { .. }),
                    "a subscriber that never joined the plane hears no membership"
                );
            }
        }
    })
    .await;
    assert!(quiet.is_err(), "nothing was announced to c");
}
