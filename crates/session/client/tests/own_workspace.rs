//! A person's own workspace, as a device in the site's home workspace has it: the host makes it the
//! first time and answers the same one after, the very device that asked redeems its invitation
//! (so the key it holds is the one that opens it), and every refusal reads typed, as the host said
//! it. The page asks through this same entry (`enterOwnWorkspace`), as the CLI and the API server
//! do.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the test is one entry, in order; the host lives until it ends"
)]

use std::cell::Cell;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use serde_json::{Map, json};
use tilo_server::workspaces::OwnWorkspaceOffer;
use tilo_server_client::invite::guest_certificate;
use tilo_session_client::device::{Device, DeviceRefusal};
use tilo_session_client::{Outbox as _, PeerConfig};
use tilo_user_identity::{AgentScope, DeviceCapabilities, UserIdentity};

use crate::support::{
    enrol, enter as enter_as, enter_page, entry_grant, in_workspace, start_workspace_host,
};

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_in_the_lounge_enters_its_persons_own_workspace_with_its_own_key() {
    // Alice's host keeps the Lounge; its entry lets Bob's tab in.
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice, &host_device, state.path(), false).await;
    let workspaces = host.server.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();

    let bob = new_key();
    let tab_key = new_key();
    let tab_cert = guest_certificate(
        &bob,
        tab_key.verifying_key().to_bytes(),
        "Firefox on Linux",
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    let tab = enter_as(
        &host,
        entry_grant(
            &host,
            &alice,
            &host_device,
            &lounge.workspace,
            bob.verifying_key().to_bytes(),
        )
        .await,
        &tab_cert,
        &tab_key,
        &host_device.cert,
        lounge.workspace_id,
    )
    .await;
    // The tab's own session, as the page starts it, is the device that enters.
    let device = Device::of(
        tab.clone(),
        tab_cert.clone(),
        host_device.cert.clone(),
        "Firefox on Linux",
    );

    // A host that keeps no workspaces of people's own says so in a code the page reads, not in
    // words it would have to match.
    let none = device
        .enter_own_workspace("en-US", None, "Bob", |_| false, None)
        .await
        .unwrap_err();
    assert_eq!(none.code, "unknown_action", "{none:?}");
    assert_eq!(none.refusal, None);

    assert!(
        workspaces
            .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
            .unwrap()
    );
    // For the people the Lounge's entry let in, as Bob's tab was.
    assert!(workspaces.admit_own_through(&lounge.workspace));

    // An id the host never made is refused, not taken as the workspace to make: the host chooses
    // the id of a person's own workspace.
    let unknown = device
        .own_workspace(
            Map::from_iter([(
                "workspace".into(),
                json!("5c1f2e7a-8b3d-4e6f-9a0b-1c2d3e4f5a6b"),
            )]),
            tilo_actions::InvocationId::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(unknown.code, "refused", "{unknown:?}");
    assert_eq!(
        workspaces.own_workspace_of(&bob.verifying_key().to_bytes()),
        None
    );

    // The first time, the host makes it, and this device redeems the invitation: the workspace
    // admits this very key, and it is Bob's, not the Lounge.
    let asked = Cell::new(0);
    let own = device
        .enter_own_workspace(
            "en-US",
            None,
            "Bob",
            |_| {
                asked.set(asked.get() + 1);
                false
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(asked.get(), 1);
    assert_ne!(own.workspace, lounge.workspace);
    assert_ne!(
        own.name, "Lounge",
        "the person's own workspace is not titled after the site's"
    );
    assert_eq!(
        workspaces.own_workspace_of(&bob.verifying_key().to_bytes()),
        Some(own.workspace.clone())
    );
    assert_eq!(device.join(&own.workspace).await.unwrap(), own.workspace_id);

    // The host lists the person's workspaces, the one it just made for them included: the same
    // list the CLI's peer asks for, so a page whose own storage remembers nothing still offers
    // them. A workspace the answering host keeps itself names no other host.
    let listed = device
        .personal_list(
            &tilo_server_protocol::workspaces::WorkspaceLabels::new(),
            None,
        )
        .await
        .unwrap();
    let mine = listed
        .workspaces
        .iter()
        .find(|listed| listed.workspace == own.workspace)
        .expect("the host lists the workspace it made for the person");
    assert_eq!(mine.name, own.name, "named as the host names it");
    assert_eq!(mine.role.as_deref(), Some("owner"));
    // The host names itself as where it keeps it, and gives no address: a page
    // reaches it where it asked.
    let address = mine.host.as_ref().expect("the host names itself");
    assert!(!address.name.is_empty());
    assert_eq!(address.hint, None);
    assert!(
        listed
            .workspaces
            .iter()
            .any(|listed| listed.workspace == lounge.workspace),
        "the Lounge is one of the person's too: {listed:?}"
    );
    // A device asks the host as the person it is a device of, whatever it calls itself: the API
    // server names its device after the key it serves (`tilo api: Mover`), which is no person.
    let api = Device::of(
        tab.clone(),
        tab_cert.clone(),
        host_device.cert.clone(),
        "tilo api: Mover",
    );
    let listed_by_api = api
        .personal_list(
            &tilo_server_protocol::workspaces::WorkspaceLabels::new(),
            None,
        )
        .await
        .expect("a device named with a colon lists its person's workspaces");
    assert!(
        listed_by_api
            .workspaces
            .iter()
            .any(|listed| listed.workspace == own.workspace)
    );

    // A page opening that workspace through its own entry, as the workspace switcher does for one
    // this browser never entered: a device of Bob's the host has not seen enters as his, with no
    // invitation.
    let switcher_key = new_key();
    let (switcher_cert, switcher_entry) = tilo_session_client::device::entry_request(
        &host_device.cert,
        &own.workspace,
        &switcher_key,
        tilo_session_client::device::Entering {
            held: None,
            root: Some(&bob),
            label: "Switcher",
            name: "Bob",
            token: None,
        },
    )
    .await
    .unwrap();
    let switcher = enter_page(
        host.iroh(),
        switcher_cert,
        host_device.cert.clone(),
        switcher_key,
        Arc::new(tilo_session_client::MemoryPeerStore::default()),
        switcher_entry,
    );
    switcher
        .send(tilo_session_client::Cmd::Join(own.workspace_id))
        .unwrap();
    assert_eq!(
        tilo_session_client::device::entered(&switcher)
            .await
            .unwrap()
            .workspace,
        own.workspace
    );
    in_workspace(&switcher, own.workspace_id).await;
    switcher.close();

    // Asked again, it answers the same workspace; one this device is in already is not redeemed
    // again.
    let again = device
        .enter_own_workspace("en-US", None, "Bob", |id| id == own.workspace, None)
        .await
        .unwrap();
    assert_eq!(again.workspace, own.workspace);
    assert_eq!(again.name, own.name);

    // A make asked again under the invocation its first ask used — a page's dialog
    // asked once more because the first answer never came — answers the workspace
    // the first made, never a second one beside it.
    let once = tilo_actions::InvocationId::new();
    let asked_twice = Map::from_iter([("name".into(), json!("Twice"))]);
    let first = device
        .own_workspace(asked_twice.clone(), once)
        .await
        .unwrap();
    let second = device.own_workspace(asked_twice, once).await.unwrap();
    assert_eq!(first.0.workspace, second.0.workspace);
    assert_eq!(first.0.name, "Twice");
    let named_twice = device
        .personal_list(
            &tilo_server_protocol::workspaces::WorkspaceLabels::new(),
            None,
        )
        .await
        .unwrap()
        .workspaces
        .into_iter()
        .filter(|listed| listed.name == "Twice")
        .count();
    assert_eq!(named_twice, 1, "the second ask made no second workspace");

    // Another device of Bob's that holds the Lounge enters the same workspace of his under its
    // own key, never under the tab's.
    let laptop_key = new_key();
    let laptop_cert = guest_certificate(
        &bob,
        laptop_key.verifying_key().to_bytes(),
        "Laptop",
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    let laptop_peer = enter_as(
        &host,
        entry_grant(
            &host,
            &alice,
            &host_device,
            &lounge.workspace,
            bob.verifying_key().to_bytes(),
        )
        .await,
        &laptop_cert,
        &laptop_key,
        &host_device.cert,
        lounge.workspace_id,
    )
    .await;
    let laptop = Device::of(
        laptop_peer.clone(),
        laptop_cert,
        host_device.cert.clone(),
        "Laptop",
    );
    let theirs = laptop
        .enter_own_workspace("en-US", None, "Bob", |_| false, None)
        .await
        .unwrap();
    assert_eq!(theirs.workspace, own.workspace);
    assert_eq!(laptop.join(&own.workspace).await.unwrap(), own.workspace_id);

    // A workspace Bob neither owns nor was invited into is refused, typed as the host refused it.
    let elsewhere = workspaces.create("Not Bob's").await.unwrap();
    let mut not_his = Map::new();
    not_his.insert("workspace".into(), json!(elsewhere.workspace));
    let refused = device
        .own_workspace(not_his, tilo_actions::InvocationId::new())
        .await
        .unwrap_err();
    assert_eq!(refused.code, "refused", "{refused:?}");
    assert!(!refused.message.is_empty());
    assert_ne!(refused.code, DeviceRefusal::UNANSWERED);

    // A key Bob made for that one workspace enters it straight from its session with the host, in
    // no other workspace first: the host invites it in on the session and it redeems there.
    let script_key = new_key();
    let script_cert = tilo_server_client::certification::issue_agent_grant(
        &bob,
        script_key.verifying_key().to_bytes(),
        "A script",
        DeviceCapabilities::agent_grant(AgentScope::parse(&own.workspace).unwrap()),
        Some(tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000)),
    )
    .await
    .unwrap();
    let script = Device::over(
        PeerConfig {
            host: host.iroh(),
            cert: script_cert,
            server_cert: host_device.cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
            entering: None,
        },
        Arc::new(script_key),
        "A script",
    );
    script.connected().await.unwrap();
    assert_eq!(
        script
            .enter_workspace(&own.workspace, "A script")
            .await
            .unwrap(),
        own.workspace_id
    );
    // It makes no workspace, and enters no other.
    let made = script
        .own_workspace(
            Map::from_iter([("name".into(), json!("More"))]),
            tilo_actions::InvocationId::new(),
        )
        .await
        .unwrap_err();
    // Its grant names one workspace: making another is the person's to allow,
    // so they are asked on the devices that hold their identity.
    assert_eq!(made.code, "awaiting_approval", "{made:?}");
    assert!(
        script
            .enter_workspace(&elsewhere.workspace, "A script")
            .await
            .is_err()
    );
    script.close();

    laptop_peer.close();
    tab.close();
    drop(host.web);
    host.server.shutdown().await;
}
