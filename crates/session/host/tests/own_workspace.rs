//! A person's own workspace, as a device in the site's home workspace has it: the host makes it the
//! first time and answers the same one after, the very device that asked redeems its invitation
//! (so the key it holds is the one that opens it), and every refusal reads typed, as the host said
//! it. The page asks through this same entry (`enterOwnWorkspace`), as the CLI and the front door
//! do.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the test is one entry, in order; the host lives until it ends"
)]

mod support;

tilo_automerge_isolate::test_worker!();

use std::cell::Cell;

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use serde_json::{Map, json};
use tilo_relay::workspaces::OwnWorkspaceOffer;
use tilo_session_host::device::{Device, HostRefusal};
use tilo_session_host::invite::guest_certificate;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{door, enrol, enter as enter_as, start_workspace_host};

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_in_the_lounge_enters_its_persons_own_workspace_with_its_own_key() {
    // Alice's host keeps the Lounge; its door lets Bob's tab in.
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice, &host_device, state.path(), false).await;
    let workspaces = host.relay.workspaces().unwrap();
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
        door(
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
        .enter_own_workspace(lounge.workspace_id, None, None, "Bob", |_| false)
        .await
        .unwrap_err();
    assert_eq!(none.code, "unknown_action", "{none:?}");
    assert_eq!(none.refusal, None);

    assert!(
        workspaces
            .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
            .unwrap()
    );

    // The first time, the host makes it, and this device redeems the invitation: the workspace
    // admits this very key, and it is Bob's, not the Lounge. It is the one the tab named: a tab
    // names the person's own workspace in the link it hands an agent before it exists, so either
    // of them may be the one to make it.
    let named = "5c1f2e7a-8b3d-4e6f-9a0b-1c2d3e4f5a6b";
    let asked = Cell::new(0);
    let own = device
        .enter_own_workspace(lounge.workspace_id, None, Some(named), "Bob", |_| {
            asked.set(asked.get() + 1);
            false
        })
        .await
        .unwrap();
    assert_eq!(asked.get(), 1);
    assert_eq!(own.workspace, named);
    assert_ne!(
        own.name, "Lounge",
        "the person's own workspace is not titled after the site's"
    );
    assert_eq!(
        workspaces.own_workspace_of(&bob.verifying_key().to_bytes()),
        Some(own.workspace.clone())
    );
    assert_eq!(device.join(&own.workspace).await.unwrap(), own.workspace_id);

    // Asked again, it answers the same workspace; one this device is in already is not redeemed
    // again.
    let again = device
        .enter_own_workspace(lounge.workspace_id, None, None, "Bob", |id| {
            id == own.workspace
        })
        .await
        .unwrap();
    assert_eq!(again.workspace, own.workspace);
    assert_eq!(again.name, own.name);

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
        door(
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
        .enter_own_workspace(lounge.workspace_id, None, Some(named), "Bob", |_| false)
        .await
        .unwrap();
    assert_eq!(theirs.workspace, own.workspace);
    assert_eq!(laptop.join(&own.workspace).await.unwrap(), own.workspace_id);

    // A workspace Bob neither owns nor was invited into is refused, typed as the host refused it.
    let elsewhere = workspaces.create("Not Bob's").await.unwrap();
    let mut not_his = Map::new();
    not_his.insert("room".into(), json!(elsewhere.workspace));
    let refused = device
        .own_workspace(lounge.workspace_id, not_his, "Firefox on Linux")
        .await
        .unwrap_err();
    assert_eq!(refused.code, "refused", "{refused:?}");
    assert!(!refused.message.is_empty());
    assert_ne!(refused.code, HostRefusal::UNANSWERED);

    laptop_peer.close();
    tab.close();
    drop(host.web);
    host.relay.shutdown().await;
}
