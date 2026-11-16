//! A page names its workspace through its host's workspaces entry, as its
//! shell does when someone renames the workspace: the answer and every list of
//! the workspace read the one name, which the workspace's governance keeps.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_relay_protocol::workspaces::{WorkspacesReply, WorkspacesRequest};
use tilo_session_client::{Link, Outbox};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{WORKSPACE, enrol, ready, start_host, start_peer};

/// Ask the host `request` of its workspace administration, and answer what it
/// said.
async fn ask(peer: &Link, request: WorkspacesRequest) -> WorkspacesReply {
    let answer = peer.ask_workspaces(request).unwrap();
    tilo_test_env::within("the host answers its workspaces entry", answer)
        .await
        .unwrap()
        .unwrap()
}

/// The workspace's name a page sends reaches the workspace's one name: the
/// host writes it into the workspace's governance, and the workspace comes
/// back, and is listed, called that.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_renames_the_workspace_for_every_list_of_it() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let page = start_peer(
        host.iroh(),
        browser.cert,
        host_device.cert.clone(),
        browser.key,
    );
    page.join_placed(WORKSPACE, "Workspace").unwrap();
    ready(&[&page]).await;

    let named = ask(
        &page,
        WorkspacesRequest::Rename {
            workspace: WORKSPACE.into(),
            name: "Tilo".into(),
        },
    )
    .await;
    let WorkspacesReply::Workspace(named) = named else {
        panic!("the host renames the workspace: {named:?}");
    };
    assert_eq!(named.name, "Tilo");

    let WorkspacesReply::Workspaces(listed) = ask(&page, WorkspacesRequest::List).await else {
        panic!("the host lists the person's workspaces");
    };
    assert!(
        listed
            .iter()
            .any(|held| held.workspace == WORKSPACE && held.name == "Tilo"),
        "{listed:?}"
    );
}
