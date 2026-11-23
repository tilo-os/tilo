//! An always-on server keeps every workspace of its person's list: one the
//! list gains it enters as a device of theirs, told by the list's change, and
//! one its entry turns keeping off for it lets go of, then enters again when
//! keeping is turned back on.
#![allow(
    clippy::significant_drop_tightening,
    reason = "each test keeps the device serving the workspace to its end"
)]

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tilo_resource::Id;
use tilo_user_identity::DeviceCapabilities;

use super::super::keeping::keep_listed;
use super::super::{Servers, apply, device_id, read};
use crate::workspaces::fixtures::{TestHost, device_cert, governor, list_workspace, test_host};
use crate::workspaces::{MemberDevice, Workspaces, person_workspace};
use tilo_server_protocol::servers::person_workspace_id;

/// The person's server, with their own workspace founded on it and its entry among their
/// servers: the device whose list is followed.
async fn server(state: &std::path::Path) -> (TestHost, [u8; 32]) {
    let (key, cert) = device_cert(
        &SigningKey::from_bytes(&[1; 32]),
        "attic",
        DeviceCapabilities::member(),
    );
    let host = test_host(state, &key, &cert).await;
    host.workspaces().set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key),
    }));
    let person = person_workspace_id(&cert.user_public_key);
    host.workspaces()
        .create_workspace(person_workspace::NAME, Some(&person))
        .await
        .expect("the person's own workspace is founded");
    apply(
        host.workspaces(),
        Servers::ENROLL_SERVER.with((
            hex::encode(cert.device_public_key),
            "Attic".into(),
            serde_json::to_string(&host.node_addr()).unwrap(),
        )),
    )
    .await
    .expect("the server is entered among the person's servers");
    (host, cert.device_public_key)
}

/// Wait until `kept` says what `server` keeps of `workspace`, which only the change it
/// follows brings about: no pass runs on a timer.
async fn keeps(server: &Workspaces, workspace: &str, kept: bool, what: &str) {
    let server = server.clone();
    let workspace = workspace.to_owned();
    tilo_test_env::within(what, async move {
        while server.state(&workspace).await.is_some() != kept {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// A workspace the person's list gains is kept on their server with nobody asking it: the
/// list's change reaches the server, which enters it as a member copy of the device serving it.
/// Turning keeping off in the server's entry lets the copy go, and turning it on again keeps it
/// again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_keeps_each_listed_workspace_until_its_entry_says_not() {
    let desk_state = tempfile::tempdir().unwrap();
    let server_state = tempfile::tempdir().unwrap();
    let (desk_key, desk_cert) = governor();
    let desk = test_host(desk_state.path(), &desk_key, &desk_cert).await;
    let made = desk
        .workspaces()
        .create("Desk")
        .await
        .expect("the desk founds a workspace");
    let hint = desk
        .workspaces()
        .hosts(&made.workspace)
        .await
        .unwrap()
        .into_iter()
        .find(|hint| hint.host.device_public_key == desk_cert.device_public_key)
        .expect("the desk signs where it is reached");
    let (server, device) = server(server_state.path()).await;
    let workspaces = server.workspaces();
    assert!(
        workspaces.state(&made.workspace).await.is_none(),
        "the server keeps nothing the list does not name"
    );

    list_workspace(workspaces, &made.workspace, "Desk", &hint).await;
    keeps(
        workspaces,
        &made.workspace,
        true,
        "the server keeps the workspace its person's list gained",
    )
    .await;
    assert!(
        workspaces.is_member(&made.workspace).await.unwrap(),
        "it entered as a device of the person, as a member copy of the desk's"
    );

    apply(
        workspaces,
        Servers::SET_KEEPING.with((device_id(&device), made.workspace.clone(), false)),
    )
    .await
    .expect("the server's entry turns keeping off");
    keeps(
        workspaces,
        &made.workspace,
        false,
        "the server lets go of the workspace its entry no longer keeps",
    )
    .await;
    let entry = read(workspaces).await.unwrap();
    assert!(!entry.active(&device).unwrap().keeps(&made.workspace));
    assert_eq!(
        keep_listed(workspaces).await.unwrap(),
        super::super::keeping::Kept::default(),
        "a workspace kept off is not entered again"
    );

    apply(
        workspaces,
        Servers::SET_KEEPING.with((
            Id::from_raw(hex::encode(device)),
            made.workspace.clone(),
            true,
        )),
    )
    .await
    .expect("the server's entry turns keeping on again");
    keeps(
        workspaces,
        &made.workspace,
        true,
        "the server keeps the workspace again once its entry says so",
    )
    .await;
    drop(desk);
}

/// A device with no entry among the person's servers keeps what it keeps: their list says
/// nothing of what it takes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_that_is_no_server_of_the_person_keeps_nothing_by_the_list() {
    let state = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(state.path(), &key, &cert).await;
    let person = person_workspace_id(&cert.user_public_key);
    host.workspaces()
        .create_workspace(person_workspace::NAME, Some(&person))
        .await
        .unwrap();
    assert_eq!(
        keep_listed(host.workspaces()).await.unwrap(),
        super::super::keeping::Kept::default()
    );
}
