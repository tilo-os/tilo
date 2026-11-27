//! One write of a page's is drafted, sealed and signed once, by its device's holder, and
//! published as it is: a session that restarts over what it kept publishes nothing a second time.
//!
//! The page's own replica holds the write as soon as the session's holder admitted it, and its
//! host holds the same signed op once. A second session over the same store drafts nothing again:
//! it finds the write admitted and published, and its ownership carries what its host has to
//! answer for.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the host lives as long as the test"
)]

tilo_automerge_isolate::test_worker!();

use std::sync::Arc;

use automerge::AutoCommit;
use tilo_session_client::{Cmd, MemoryPeerStore, Outbox as _};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use super::support::{
    HostSettings, WORKSPACE, durable, enrol, person_of, ready, route, say, start_host_serving,
    start_peer_with_store, wait_keyed,
};

/// The changes the host's saved copy of `bytes` holds.
fn changes(bytes: &[u8]) -> usize {
    AutoCommit::load(bytes)
        .expect("the host's copy loads")
        .get_changes(&[])
        .len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_writes_once_and_a_restart_publishes_nothing_again() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let page = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host_serving(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        HostSettings::default(),
    )
    .await;
    let workspaces = host.server.workspaces().unwrap();
    let store = Arc::new(MemoryPeerStore::default());

    let first = start_peer_with_store(
        host.iroh(),
        page.cert.clone(),
        host_device.cert.clone(),
        page.key.clone(),
        Arc::clone(&store),
    );
    first.join_placed(WORKSPACE, "Workspace").unwrap();
    first.send(Cmd::OpenRoute(route())).unwrap();
    ready(&[&first]).await;
    wait_keyed(&first, &route()).await;

    let mut doc = AutoCommit::new();
    first
        .write(route(), &say(&mut doc, person_of(&first), "one"))
        .unwrap();
    durable(&first).await;

    let held = workspaces
        .resource_document(WORKSPACE, &route().app_id, &route().document)
        .await
        .unwrap()
        .expect("the host holds the record");
    assert_eq!(
        changes(&held),
        1,
        "the host holds the page's one write, once"
    );
    let heads = AutoCommit::load(&held).unwrap().get_heads();

    // A new session over the same store: what the first admitted and published is not drafted
    // again, and nothing more reaches the host.
    first.close();
    let again = start_peer_with_store(host.iroh(), page.cert, host_device.cert, page.key, store);
    ready(&[&again]).await;
    durable(&again).await;

    let after = workspaces
        .resource_document(WORKSPACE, &route().app_id, &route().document)
        .await
        .unwrap()
        .expect("the host holds the record");
    assert_eq!(
        AutoCommit::load(&after).unwrap().get_heads(),
        heads,
        "a restart of the session publishes nothing again"
    );
    assert_eq!(changes(&after), 1, "the host still holds one write");
    again.close();
}
