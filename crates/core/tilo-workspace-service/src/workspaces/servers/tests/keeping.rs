//! Every device of the person enters each workspace their list shows on it, told by the list's
//! change. An always-on server keeps every workspace of the list instead: one its entry turns
//! keeping off for it lets go of, then enters again when keeping is turned back on.
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
use crate::workspaces::fixtures::{
    TestHost, device_cert, hosting_device, list_workspace, test_host,
};
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
    let (desk_key, desk_cert) = hosting_device();
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
        Box::pin(keep_listed(workspaces)).await.unwrap(),
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

/// The desk, which founded a workspace and says where it serves it, and a laptop of the same
/// person that is no server of theirs and has their own workspace, which lists nothing yet.
struct DeskAndLaptop {
    desk: TestHost,
    made: String,
    hint: tilo_peer_state::join::TransportHint,
    laptop: TestHost,
    _state: [tempfile::TempDir; 2],
}

async fn desk_and_laptop() -> DeskAndLaptop {
    let desk_state = tempfile::tempdir().unwrap();
    let laptop_state = tempfile::tempdir().unwrap();
    let (desk_key, desk_cert) = hosting_device();
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
    // The desk's person's other device: the same root, a key of its own.
    let (key, cert) = device_cert(
        &SigningKey::from_bytes(&[1; 32]),
        "laptop",
        DeviceCapabilities::member(),
    );
    let laptop = test_host(laptop_state.path(), &key, &cert).await;
    let workspaces = laptop.workspaces();
    workspaces.set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key),
    }));
    let person = person_workspace_id(&cert.user_public_key);
    workspaces
        .create_workspace(person_workspace::NAME, Some(&person))
        .await
        .expect("the person's own workspace is founded");
    assert!(
        read(workspaces)
            .await
            .unwrap()
            .active(&cert.device_public_key)
            .is_none(),
        "the laptop is no server of the person's"
    );
    DeskAndLaptop {
        desk,
        made: made.workspace,
        hint,
        laptop,
        _state: [desk_state, laptop_state],
    }
}

/// A device of the person's that is no server of theirs enters each workspace their list shows on
/// it, told by the list's change: the daemon takes in what the person made on another device,
/// and the desktop shows what it holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_that_is_no_server_enters_what_the_list_shows_on_it() {
    let devices = desk_and_laptop().await;
    let workspaces = devices.laptop.workspaces();

    list_workspace(workspaces, &devices.made, "Desk", &devices.hint).await;
    keeps(
        workspaces,
        &devices.made,
        true,
        "the laptop enters the workspace its person's list gained",
    )
    .await;
    assert!(
        workspaces.is_member(&devices.made).await.unwrap(),
        "it entered as a device of the person, as a member copy of the desk's"
    );
    assert!(
        workspaces.not_here().is_empty(),
        "{:?}",
        workspaces.not_here()
    );
}

/// A workspace the list shows on a device while the device keeping it is away is not here yet,
/// and the device says so, with why, where it lists the person's workspaces. It tries again by
/// itself, and once the device keeping it answers again it holds it, with nothing asked of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_listed_workspace_whose_device_is_away_is_not_here_with_why_until_it_answers() {
    let devices = desk_and_laptop().await;
    let workspaces = devices.laptop.workspaces();
    devices.desk.away();

    list_workspace(workspaces, &devices.made, "Desk", &devices.hint).await;
    let not_here = {
        let mut said = workspaces.not_here_changed();
        let made = devices.made.clone();
        tilo_test_env::within(
            "the laptop says the workspace is not here yet",
            said.wait_for(|missing| missing.iter().any(|missing| missing.workspace == made)),
        )
        .await
        .expect("the laptop keeps saying what it misses")
        .clone()
    };
    let [missing] = not_here.as_slice() else {
        panic!("one workspace is not here: {not_here:?}");
    };
    assert_eq!(missing.name, "Desk", "{missing:?}");
    assert!(!missing.why.trim().is_empty(), "it says why: {missing:?}");
    assert!(workspaces.state(&devices.made).await.is_none());

    devices.desk.back();
    keeps(
        workspaces,
        &devices.made,
        true,
        "the laptop takes the workspace in once the desk answers again",
    )
    .await;
    tilo_test_env::within(
        "the laptop no longer says the workspace is not here",
        workspaces
            .not_here_changed()
            .wait_for(std::vec::Vec::is_empty),
    )
    .await
    .expect("the laptop keeps saying what it misses");
}
