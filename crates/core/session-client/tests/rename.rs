//! A page names its workspace through its host's workspaces entry, as its
//! shell does when someone renames the workspace: the answer and every list of
//! the workspace read the one name, which the workspace's governance keeps.

use tilo_server_protocol::workspaces::{WorkspacesReply, WorkspacesRequest};
use tilo_session_client::{Link, Outbox};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{WORKSPACE, enrol, ready, start_host, start_peer, wait_for};

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

/// A page's shell reads the workspace's one name from its governance, the only place it is kept:
/// a rename made on one device reaches every device showing the workspace, so each bar, overview
/// and search calls the workspace what it is called.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pages_shell_reads_the_name_the_governance_keeps() {
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
    // Another device of the person, showing the same workspace.
    let phone = enrol(&root, "phone", DeviceCapabilities::member());
    let phone = start_peer(host.iroh(), phone.cert, host_device.cert.clone(), phone.key);
    phone.join_placed(WORKSPACE, "Workspace").unwrap();
    ready(&[&page, &phone]).await;
    let workspace = tilo_desktop_model::WorkspaceId::parse(WORKSPACE).unwrap();
    tilo_test_env::within(
        "the page reads the name its workspace was made with",
        async {
            wait_for("the page reading the workspace's name", || {
                let named = page.engine().workspace_name(workspace);
                async move { named.filter(|name| name == "Workspace") }
            })
            .await;
        },
    )
    .await;
    ask(
        &page,
        WorkspacesRequest::Rename {
            workspace: WORKSPACE.into(),
            name: "Tilo".into(),
        },
    )
    .await;
    // The rename is the workspace's: its governance records it, and the page's shell reads it
    // there, whatever layout the page landed with.
    tilo_test_env::within("the page reads the workspace's renamed name", async {
        let named = wait_for("the page reading the renamed workspace", || {
            let named = page.engine().workspace_name(workspace);
            async move { named.filter(|name| name == "Tilo") }
        })
        .await;
        assert_eq!(named, "Tilo");
    })
    .await;
    // Every device showing the workspace reads the one name, not one it kept.
    tilo_test_env::within("the other device reads the renamed name", async {
        wait_for("the other device reading the renamed workspace", || {
            let named = phone.engine().workspace_name(workspace);
            async move { named.filter(|name| name == "Tilo") }
        })
        .await;
    })
    .await;
}
