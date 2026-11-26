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

use automerge::{AutoCommit, ROOT, ReadDoc as _};
use collab_types::DocumentMutation;
use tilo_automerge::ApplyIncoming as _;
use tilo_session_client::Outbox;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{durable, enrol, person_of, ready, route, set_field, start_host, start_peer};

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
    let host_device = enrol(&root, "home-server", DeviceCapabilities::member());
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
    let https = || host.iroh();
    let a = start_peer(https(), a.cert, host_device.cert.clone(), a.key);
    let b = start_peer(https(), b.cert, host_device.cert.clone(), b.key);
    a.join_placed(crate::support::WORKSPACE, "Workspace")
        .unwrap();
    b.join_placed(crate::support::WORKSPACE, "Workspace")
        .unwrap();
    ready(&[&a, &b]).await;

    let mut base = AutoCommit::new();
    set_field(&mut base, person_of(&a), "note", "title", "base");
    a.write(route(), &DocumentMutation::Snapshot(base.save()))
        .unwrap();
    durable(&a).await;

    // B commits a change A has not merged yet ...
    let mut on_b = base.fork();
    set_field(&mut on_b, person_of(&b), "note", "from_b", "concurrent");
    b.write(
        route(),
        &DocumentMutation::Incremental(on_b.save_after(&base.get_heads())),
    )
    .unwrap();
    durable(&b).await;
    // ... and A then checkpoints its own replica, which lacks it.
    let mut on_a = base.fork();
    set_field(&mut on_a, person_of(&a), "note", "from_a", "own");
    a.write(route(), &DocumentMutation::Snapshot(on_a.save()))
        .unwrap();
    durable(&a).await;

    let joiner = start_peer(https(), c.cert, host_device.cert.clone(), c.key);
    joiner
        .join_placed(crate::support::WORKSPACE, "Workspace")
        .unwrap();
    let mut all = on_a.fork();
    all.merge_incoming(&mut on_b).unwrap();
    let doc = crate::support::holds(&joiner, &route(), &all).await;
    assert_eq!(read(&doc, "title").as_deref(), Some("base"));
    assert_eq!(read(&doc, "from_a").as_deref(), Some("own"));
    assert_eq!(read(&doc, "from_b").as_deref(), Some("concurrent"));
}
