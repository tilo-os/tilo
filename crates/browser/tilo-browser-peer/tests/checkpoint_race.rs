//! A checkpoint taken by a device that had not yet received a concurrent
//! change must not erase that change for devices that join later: the host
//! keeps every retained change the checkpoint does not contain.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::future_not_send,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable as _};
use collab_types::DocumentMutation;
use tilo_automerge::ApplyIncoming as _;
use tilo_browser_peer::{Host, PeerEvent, PeerHandle};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{enrol, ready, route, start_host, start_peer, wait_for, workspace};

async fn durable(peer: &PeerHandle) {
    let receipt = peer.barrier().unwrap();
    wait_for("durable", || async { receipt.try_recv().ok() })
        .await
        .unwrap();
}

fn read(doc: &AutoCommit, key: &str) -> Option<String> {
    doc.get(ROOT, key)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_scalar().ok())
        .and_then(|scalar| scalar.into_string().ok())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_checkpoint_missing_a_concurrent_change_keeps_it_for_late_joiners() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let (a, b, c) = (
        enrol(&root, "browser", DeviceCapabilities::member()),
        enrol(&root, "laptop", DeviceCapabilities::member()),
        enrol(&root, "phone", DeviceCapabilities::member()),
    );
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let https = || Host::Https(host.https());
    let a = start_peer(https(), a.cert, host_device.cert.clone(), a.key);
    let b = start_peer(https(), b.cert, host_device.cert.clone(), b.key);
    a.join(workspace()).unwrap();
    b.join(workspace()).unwrap();
    ready(&[&a, &b]).await;

    let mut base = AutoCommit::new();
    base.put(ROOT, "title", "base").unwrap();
    base.commit();
    a.publish(route(), &DocumentMutation::Snapshot(base.save()))
        .unwrap();
    durable(&a).await;

    // B commits a change A has not merged yet ...
    let mut on_b = base.fork();
    on_b.put(ROOT, "from_b", "concurrent").unwrap();
    on_b.commit();
    b.publish(
        route(),
        &DocumentMutation::Incremental(on_b.save_after(&base.get_heads())),
    )
    .unwrap();
    durable(&b).await;
    // ... and A then checkpoints its own replica, which lacks it.
    let mut on_a = base.fork();
    on_a.put(ROOT, "from_a", "own").unwrap();
    on_a.commit();
    a.publish(route(), &DocumentMutation::Snapshot(on_a.save()))
        .unwrap();
    durable(&a).await;

    let joiner = start_peer(https(), c.cert, host_device.cert.clone(), c.key);
    joiner.join(workspace()).unwrap();
    let mut doc = AutoCommit::new();
    wait_for("the late joiner to converge", || {
        for event in joiner.drain() {
            if let PeerEvent::Document(document) = event {
                match document.mutation().unwrap() {
                    DocumentMutation::Snapshot(bytes) => {
                        doc.merge_incoming(&mut AutoCommit::load(&bytes).unwrap())
                            .unwrap();
                    }
                    DocumentMutation::Incremental(bytes) => {
                        doc.load_incoming(&bytes).unwrap();
                    }
                }
            }
        }
        let done = read(&doc, "from_b").is_some() && read(&doc, "from_a").is_some();
        async move { done.then_some(()) }
    })
    .await;
    assert_eq!(read(&doc, "title").as_deref(), Some("base"));
    assert_eq!(read(&doc, "from_a").as_deref(), Some("own"));
    assert_eq!(read(&doc, "from_b").as_deref(), Some("concurrent"));
}
