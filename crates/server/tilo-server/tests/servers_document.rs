//! A server does what its entry in its person's admin workspace says: it holds
//! the workspaces the entry names and lets go of the ones it stops naming,
//! offers the work its entry allows, and follows the update policy its entry
//! names.

#![allow(
    clippy::significant_drop_tightening,
    reason = "hosts, routers and fixtures deliberately stay alive until each test ends"
)]

use tilo_resource::Id;
use tilo_server::servers::{self, Servers, admin_workspace_id, applied};
use tilo_server::workspaces::Offer;
use tilo_server_protocol::WorkSetting;
use tilo_user_identity::DeviceCapabilities;

use crate::common::{TestUser, host_on};

/// The document's entry id of the device `device`.
fn entry(device: &crate::common::TestDevice) -> Id {
    Id::from_raw(hex::encode(device.device_pubkey()))
}

/// Choosing a server as a holder in the admin workspace is the one action an
/// owner takes: the device that governs the workspace hands it to
/// the server and names it, and naming it no more lets it go.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_holds_and_releases_the_workspaces_its_entry_names() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let governor = host_on(&alice, &laptop, WorkSetting::none()).await;
    let server = host_on(&alice, &nas, WorkSetting::own_person()).await;
    let workspaces = governor.host.workspaces();
    let admin = admin_workspace_id(&alice.user_pk);
    workspaces
        .create_workspace("Servers", Some(&admin))
        .await
        .unwrap();
    // The server enrolled itself on its first start: its entry names the
    // address it is dialled at.
    servers::apply(
        workspaces,
        Servers::ENROLL_SERVER.with((
            hex::encode(nas.device_pubkey()),
            "nas".into(),
            serde_json::to_string(&server.host.node_addr()).unwrap(),
        )),
    )
    .await
    .unwrap();

    let workspace = workspaces.create("Notes").await.unwrap();
    let retained = || async {
        server
            .host
            .workspace_documents()
            .retained(workspace.workspace_id)
            .await
            .ok()
    };
    assert!(retained().await.is_none(), "it holds nothing yet");

    servers::apply(
        workspaces,
        Servers::HOLD_WORKSPACE.with((
            entry(&nas),
            workspace.workspace.clone(),
            servers::KEY_HOLDER.into(),
        )),
    )
    .await
    .unwrap();
    let applied = applied::reconcile(workspaces).await.unwrap();
    assert!(
        applied.held.contains(&workspace.workspace),
        "the owner's device handed it the workspace: {applied:?}"
    );
    tilo_test_env::within("the server to hold the workspace", async {
        while retained().await.is_none() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;

    assert_eq!(
        workspaces.hosts(&workspace.workspace).await.unwrap().len(),
        2,
        "the workspace names the governor and its holder"
    );
    assert_eq!(
        servers::read(workspaces).await.unwrap().servers.len(),
        1,
        "the document still names the server"
    );
    // Naming it no more in its entry lets the workspace go.
    servers::apply(
        workspaces,
        Servers::RELEASE_WORKSPACE.with((entry(&nas), workspace.workspace.clone())),
    )
    .await
    .unwrap();
    let after = servers::read(workspaces).await.unwrap();
    let nas_entry = after.servers.get(&entry(&nas)).cloned().unwrap();
    assert_eq!(
        nas_entry.holding(&workspace.workspace),
        None,
        "the release took: {nas_entry:?}"
    );
    let applied = applied::reconcile(workspaces).await.unwrap();
    assert!(
        applied.released.contains(&workspace.workspace),
        "the workspace left its hosts: {applied:?}"
    );
    tilo_test_env::within("the server to let go of the workspace", async {
        while retained().await.is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// A server offers exactly what its entry says: an entry naming no compute
/// takes no work, and one naming compute and a machine budget offers it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_offers_what_its_entry_says() {
    let alice = TestUser::fresh("Alice");
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let server = host_on(&alice, &nas, WorkSetting::own_person()).await;
    let policy = tempfile::tempdir().unwrap();
    let policy_request = policy.path().join("update-policy.request");
    let workspaces = server.host.workspaces();
    let admin = admin_workspace_id(&alice.user_pk);
    workspaces
        .create_workspace("Servers", Some(&admin))
        .await
        .unwrap();
    servers::apply(
        workspaces,
        Servers::ENROLL_SERVER.with((
            hex::encode(nas.device_pubkey()),
            "nas".into(),
            serde_json::to_string(&server.host.node_addr()).unwrap(),
        )),
    )
    .await
    .unwrap();
    // A freshly enrolled server offers nothing until its entry says so.
    applied::reconcile_into(workspaces, &policy_request)
        .await
        .unwrap();
    assert!(!workspaces.offer().compute, "it runs no work yet");

    servers::apply(
        workspaces,
        Servers::SET_APPETITE.with((entry(&nas), 500, true, 2)),
    )
    .await
    .unwrap();
    let applied = applied::reconcile_into(workspaces, &policy_request)
        .await
        .unwrap();
    assert!(applied.offered);
    assert_eq!(workspaces.offer(), Offer::of(500, true, 2));
}

/// A retired server lets go of everything it holds and offers nothing, and
/// keeps its entry so it is never taken in again as a new server.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retired_server_lets_go_of_everything() {
    let alice = TestUser::fresh("Alice");
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let server = host_on(&alice, &nas, WorkSetting::own_person()).await;
    let policy = tempfile::tempdir().unwrap();
    let policy_request = policy.path().join("update-policy.request");
    let workspaces = server.host.workspaces();
    let workspace = workspaces.create("Kept").await.unwrap();
    let admin = admin_workspace_id(&alice.user_pk);
    workspaces
        .create_workspace("Servers", Some(&admin))
        .await
        .unwrap();
    servers::apply(
        workspaces,
        Servers::ENROLL_SERVER.with((
            hex::encode(nas.device_pubkey()),
            "nas".into(),
            serde_json::to_string(&server.host.node_addr()).unwrap(),
        )),
    )
    .await
    .unwrap();
    servers::apply(
        workspaces,
        Servers::SET_APPETITE.with((entry(&nas), 500, true, 2)),
    )
    .await
    .unwrap();
    applied::reconcile_into(workspaces, &policy_request)
        .await
        .unwrap();
    assert!(workspaces.offer().compute);

    servers::apply(workspaces, Servers::RETIRE_SERVER.with(entry(&nas)))
        .await
        .unwrap();
    let applied = applied::reconcile_into(workspaces, &policy_request)
        .await
        .unwrap();
    assert!(applied.retired, "its own entry says it was retired");
    assert!(
        !workspaces.offer().compute,
        "a retired server takes no work"
    );
    assert!(
        applied.released.contains(&workspace.workspace),
        "it let go of what it held: {applied:?}"
    );
    let still_there = workspaces.list().await;
    assert!(
        !still_there
            .iter()
            .any(|summary| summary.workspace == workspace.workspace),
        "it holds nothing: {still_there:?}"
    );
}
