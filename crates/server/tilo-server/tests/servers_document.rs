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

use crate::common::{TestUser, serve_on};

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
    let governor = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let server = serve_on(&alice, &nas, WorkSetting::own_person()).await;
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
    let server = serve_on(&alice, &nas, WorkSetting::own_person()).await;
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
    let server = serve_on(&alice, &nas, WorkSetting::own_person()).await;
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

/// A person with no desktop enrols three servers one after another: the first
/// makes their admin workspace, each later one is taken in by one that holds
/// it, and all three then hold the one workspace. What is set at one of them
/// is applied at the others.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn servers_enrolled_one_after_another_share_one_admin_workspace() {
    let alice = TestUser::fresh("Alice");
    let devices =
        ["first", "second", "third"].map(|label| alice.enrol(label, DeviceCapabilities::member()));
    let mut hosts = Vec::new();
    for device in &devices {
        hosts.push(serve_on(&alice, device, WorkSetting::own_person()).await);
    }
    let policy = tempfile::tempdir().unwrap();
    let _following = hosts
        .iter()
        .enumerate()
        .map(|(at, hosted)| {
            applied::follow(
                hosted.host.workspaces(),
                policy.path().join(format!("{at}.request")),
            )
        })
        .collect::<Vec<_>>();
    let admin = admin_workspace_id(&alice.user_pk);
    let [first, second, third] = [&hosts[0], &hosts[1], &hosts[2]];

    servers::found(first.host.workspaces(), &first.host.node_addr())
        .await
        .unwrap();
    // The second asks the first, the third the second: any server holding the
    // workspace takes a later one in.
    for (asked, asking, device) in [(first, second, &devices[1]), (second, third, &devices[2])] {
        servers::admit(
            asked.host.workspaces(),
            &device.cert,
            &asking.host.node_addr(),
        )
        .await
        .unwrap();
        tilo_test_env::within("the later server to hold the admin workspace", async {
            while !servers::holds_admin(asking.host.workspaces()).await {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
    }
    tilo_test_env::within(
        "every server to name all three holders and read the one document",
        async {
            loop {
                let mut named = Vec::new();
                let mut named_servers = Vec::new();
                for hosted in &hosts {
                    named.push(
                        hosted
                            .host
                            .workspaces()
                            .hosts(&admin)
                            .await
                            .map_or(0, |hosts| hosts.len()),
                    );
                    named_servers.push(
                        servers::read(hosted.host.workspaces())
                            .await
                            .map_or(0, |servers| servers.servers.len()),
                    );
                }
                if named == [3, 3, 3] && named_servers == [3, 3, 3] {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        },
    )
    .await;
    let stranger = TestUser::fresh("Mallory").enrol("theirs", DeviceCapabilities::member());
    assert!(
        servers::admit(
            first.host.workspaces(),
            &stranger.cert,
            &third.host.node_addr(),
        )
        .await
        .is_err(),
        "another person's server is not taken in"
    );

    // Set at the third, applied at the first.
    servers::apply(
        third.host.workspaces(),
        Servers::SET_APPETITE.with((entry(&devices[0]), 500, true, 2)),
    )
    .await
    .unwrap();
    tilo_test_env::within("the first server to offer what the third set", async {
        while first.host.workspaces().offer() != Offer::of(500, true, 2) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(
        servers::read(second.host.workspaces())
            .await
            .unwrap()
            .servers
            .len(),
        3,
        "one document names every server"
    );
}
