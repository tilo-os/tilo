//! A page joining a workspace of its person that its host does not govern yet
//! has the host place it there first.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the test holds its host for its length"
)]

mod support;

// This binary re-execs as the automerge worker (support is shared with crates that lack the dep).
tilo_automerge_isolate::test_worker!();

use tilo_browser_peer::Host;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{enrol, start_peer, start_workspace_host};

/// A page joining a workspace of its person that the host does not govern
/// yet has the host place it there first, under the workspace's name, and
/// is let in as its owner: the workspace has governance like every other.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_places_its_persons_workspace_before_joining_it() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let laptop = enrol(&alice, "alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice, &host_device, state.path(), false).await;
    let workspace = "0b7c6f0e-2d55-4f3a-9c1e-5a8d2b4e6f10";
    let workspace_id = tilo_relay_protocol::room_workspace_id(workspace).unwrap();
    let page = start_peer(
        Host::Https(host.https()),
        laptop.cert.clone(),
        host_device.cert.clone(),
        laptop.key.clone(),
    );
    page.join_placed(workspace, "Garden").unwrap();
    support::in_workspace(&page, workspace_id).await;
    let summary = host
        .relay
        .workspaces()
        .unwrap()
        .summary(workspace)
        .await
        .unwrap();
    assert_eq!(summary.name, "Garden");
    assert_eq!(summary.workspace_id, workspace_id);
    // Placed once: a page joining again keeps the workspace as it is.
    let again = start_peer(
        Host::Https(host.https()),
        laptop.cert.clone(),
        host_device.cert.clone(),
        laptop.key.clone(),
    );
    again.join_placed(workspace, "Another name").unwrap();
    support::in_workspace(&again, workspace_id).await;
    assert_eq!(
        host.relay
            .workspaces()
            .unwrap()
            .summary(workspace)
            .await
            .unwrap()
            .name,
        "Garden"
    );
}
