//! A page learns from the workspace's governance whether the workspace shares its
//! members' layout: in a workspace whose layout is personal it keeps its strips
//! and windows to itself and sends the workspace no desktop, which the workspace
//! would refuse; in a shared workspace it shares them as before.

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

use tilo_browser_peer::{Host, PeerEvent, PeerHandle};
use tilo_desktop_model::projection::WorkspaceLayout;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{enrol, ready, start_peer, start_workspace_host, wait_for};

/// Wait until `page` says `workspace`'s layout is `personal`, by its status and
/// by the event that tells its desktop.
async fn heard(page: &PeerHandle, workspace: &str, personal: bool) {
    let mut told = false;
    wait_for("the page to hear the workspace's layout scope", || {
        told |= page.drain().into_iter().any(|event| {
            matches!(
                event,
                PeerEvent::RoomLayout { workspace: r, personal: p } if r == workspace && p == personal
            )
        });
        let known = page.status().personal_rooms.contains(workspace) == personal;
        async move { (told && known).then_some(()) }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_keeps_to_its_workspaces_layout_scope() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let laptop = enrol(&alice, "alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice, &host_device, state.path(), false).await;
    let workspace = host.relay.rooms().unwrap().create("Team").await.unwrap();
    host.relay
        .rooms()
        .unwrap()
        .enter(&workspace.room, &laptop.cert)
        .await
        .unwrap();
    let page = start_peer(
        Host::Https(host.https()),
        laptop.cert.clone(),
        host_device.cert.clone(),
        laptop.key.clone(),
    );
    page.join(workspace.workspace_id).unwrap();
    ready(&[&page]).await;
    wait_for("the workspace to let the page in", || async {
        (page.status().rooms.get(&workspace.workspace_id) == Some(&true)).then_some(())
    })
    .await;
    assert!(
        !page.status().personal_rooms.contains(&workspace.room),
        "a workspace shares its members' layout until its governance says otherwise"
    );

    host.relay
        .rooms()
        .unwrap()
        .set_layout(&workspace.room, WorkspaceLayout::Personal)
        .await
        .unwrap();
    heard(&page, &workspace.room, true).await;

    host.relay
        .rooms()
        .unwrap()
        .set_layout(&workspace.room, WorkspaceLayout::Shared)
        .await
        .unwrap();
    heard(&page, &workspace.room, false).await;
}

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
        .rooms()
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
            .rooms()
            .unwrap()
            .summary(workspace)
            .await
            .unwrap()
            .name,
        "Garden"
    );
}
