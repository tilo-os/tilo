//! Durable host: a pinned always-on host lets two devices that are
//! *never simultaneously online* still converge — and that guarantee
//! survives the host itself restarting.
//!
//! This is the durability half of the sync model. The host binds
//! workspace 9 with `pin = true` (a designated durable host). Device
//! A publishes an edit and goes offline **before B has ever
//! connected**, so the only path by which B can ever learn A's edit
//! is the host's on-disk store-and-forward queue. We then drop the
//! host process entirely and start a fresh one on the same state
//! directory — modelling the always-on box rebooting — before B
//! connects for the first time. B still catches up and converges,
//! proving the queued CRDT delta is durable across a host restart,
//! not merely held in RAM.
//!
//! Asserted on the real `Relay`/`WorkspaceRuntime` path with a
//! genuine Automerge document, so "converge" means identical heads,
//! not byte-equality of an opaque blob.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::Duration;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
use qos_relay::{Envelope, RelayClient};
use qos_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestUser, client_endpoint, spawn_relay_on};

fn read_str(doc: &AutoCommit, key: &str) -> Option<String> {
    match doc.get(ROOT, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Str(v) => Some(v.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn pending_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |rd| {
        rd.flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".delta"))
            .count()
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_simultaneously_online_devices_converge_across_host_restart() {
    const WS: u64 = 9;

    let user = TestUser::fresh("Alice");
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("phone", DeviceCapabilities::member());
    let members = [a.device_pubkey(), b.device_pubkey()];

    // The host's persistent state directory — kept alive across the
    // restart so the pending queue + iroh identity survive.
    let state = tempfile::tempdir().unwrap();
    let state_root = state.path().to_path_buf();

    // ---- Host boots; A publishes an edit and leaves. B has never
    //      connected. ----
    let host1_dev = user.enrol("home-server", DeviceCapabilities::host());
    let host1 = spawn_relay_on(&state_root, &user, host1_dev, &[WS], &members).await;
    let addr1 = host1.node_addr();

    let ep_a = client_endpoint().await;
    let a_client = RelayClient::connect(
        ep_a,
        addr1,
        user.user_pk,
        a.cert.clone(),
        a.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("A connect");
    a_client
        .send(Envelope::Subscribe { workspace_id: WS })
        .unwrap();
    // Drain Ready(WS, 0).
    let ready = tokio::time::timeout(Duration::from_secs(2), a_client.recv())
        .await
        .expect("A Ready timeout")
        .expect("A Ready closed");
    assert!(matches!(
        ready,
        Envelope::Ready {
            workspace_id: WS,
            pending_deltas: 0
        }
    ));

    let mut doc_a = AutoCommit::new();
    doc_a.put(ROOT, "note", "durable").unwrap();
    let delta = doc_a.save_incremental();
    a_client
        .send(Envelope::Delta {
            workspace_id: WS,
            author_device_public_key: a.device_pubkey(),
            authored_at_ns: 1,
            delta: delta.clone(),
            signature: [0u8; 64],
        })
        .unwrap();

    // B never connected, so the edit must be queued to disk for it.
    let b_hex = hex::encode(b.device_pubkey());
    let pending_dir = state_root
        .join("workspaces")
        .join(WS.to_string())
        .join("pending")
        .join(&b_hex);
    let mut queued = 0;
    for _ in 0..40 {
        queued = pending_count(&pending_dir);
        if queued >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(queued, 1, "A's edit must be persisted for the offline B");

    // A disconnects. Now neither device is online.
    drop(a_client);

    // ---- The host itself goes down and comes back up on the same
    //      state directory. ----
    drop(host1);
    // Give the OS a moment to release the previous iroh socket bind.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The queued delta is still on disk after the host process died.
    assert_eq!(
        pending_count(&pending_dir),
        1,
        "queued delta must survive the host process dying"
    );

    let host2_dev = user.enrol("home-server-2", DeviceCapabilities::host());
    let host2 = spawn_relay_on(&state_root, &user, host2_dev, &[WS], &members).await;
    let addr2 = host2.node_addr();

    // ---- B connects for the very first time, to the restarted host. ----
    let ep_b = client_endpoint().await;
    let b_client = RelayClient::connect(
        ep_b,
        addr2,
        user.user_pk,
        b.cert.clone(),
        b.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("B connect after restart");
    b_client
        .send(Envelope::Subscribe { workspace_id: WS })
        .unwrap();

    // B receives both the queued delta and a Ready(pending=1). The
    // host writes Ready inline while the drained delta travels the
    // outbound channel, so the two can arrive in either order — read
    // until we have both. B then converges with A even though they
    // were never online together and the host restarted in between.
    let mut doc_b = AutoCommit::new();
    let mut got_delta = false;
    let mut got_ready = false;
    for _ in 0..6 {
        if got_delta && got_ready {
            break;
        }
        let env = tokio::time::timeout(Duration::from_secs(3), b_client.recv())
            .await
            .expect("B catch-up timeout")
            .expect("B channel closed");
        match env {
            Envelope::Delta {
                workspace_id,
                delta,
                ..
            } => {
                assert_eq!(workspace_id, WS);
                doc_b
                    .load_incremental(&delta)
                    .expect("B applies queued delta");
                got_delta = true;
            }
            Envelope::Ready {
                workspace_id,
                pending_deltas,
            } => {
                assert_eq!(workspace_id, WS);
                assert_eq!(pending_deltas, 1, "host reports the one queued delta");
                got_ready = true;
            }
            other => panic!("unexpected envelope {other:?}"),
        }
    }
    assert!(got_delta, "B must receive the queued delta after restart");
    assert!(got_ready, "B must receive Ready");

    assert_eq!(read_str(&doc_b, "note").as_deref(), Some("durable"));
    assert_eq!(
        doc_a.get_heads(),
        doc_b.get_heads(),
        "B converges with A across a host restart, never having been online together"
    );
    assert_eq!(
        pending_count(&pending_dir),
        0,
        "queue drains after delivery"
    );

    drop(host2);
}
