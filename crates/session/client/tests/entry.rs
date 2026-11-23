//! Entering a listed workspace through its entry, as every device does it: on the session itself
//! (`device::entry_request`, which the page calls as `entryRequest`, then the session's own handshake):
//! the entry lets the person in and the workspace admits this very device, under the certificate
//! it holds from that host or one its person's root makes now; an entry that says no says why in a
//! code of its own. Until the workspace admitted it, a stranger's connection carries the entry and
//! nothing else.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the test is one entry, in order; the host lives until it ends"
)]

use std::sync::Arc;
use tilo_session_client::{Cmd, Outbox, TransportPhase};

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_session_client::MemoryPeerStore;
use tilo_session_client::device::{DeviceRefusal, Entering, Held, entered, entry_request};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{enrol, enter_page, in_workspace, start_peer, start_workspace_host};

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

fn entering<'a>(held: Option<&'a Held>, root: &'a SigningKey) -> Entering<'a> {
    Entering {
        held,
        root: Some(root),
        label: "Firefox on Linux",
        name: "Bob",
        token: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_enters_a_listed_workspace_on_its_session_and_a_closed_entry_says_why() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice, &host_device, state.path(), false).await;
    let workspaces = host.server.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .publish(
            &lounge.workspace,
            &host_device.cert,
            tilo_server_protocol::directory::Listing {
                description: "Coffee and tea".into(),
                default_role: tilo_server_protocol::workspaces::Role::Editor,
            },
        )
        .await
        .unwrap();
    let server_cert = host_device.cert.clone();

    // Bob's tab, with his root, enters through the entry on its session: the workspace admits this
    // very key, and the session subscribes at once.
    let bob = new_key();
    let page = new_key();
    let (cert, entry) = entry_request(&server_cert, &lounge.workspace, &page, entering(None, &bob))
        .await
        .unwrap();
    assert_eq!(cert.device_public_key, page.verifying_key().to_bytes());
    assert_eq!(cert.user_public_key, bob.verifying_key().to_bytes());
    let peer = enter_page(
        host.iroh(),
        cert.clone(),
        server_cert.clone(),
        page.clone(),
        Arc::new(MemoryPeerStore::default()),
        entry.clone(),
    );
    peer.send(Cmd::Join(lounge.workspace_id)).unwrap();
    let admission = entered(&peer).await.unwrap();
    assert_eq!(admission.workspace, lounge.workspace);
    assert_eq!(admission.workspace_id, lounge.workspace_id);
    in_workspace(&peer, lounge.workspace_id).await;
    peer.close();

    // The same entry asked again (its answer lost with its connection) is answered again.
    let again = enter_page(
        host.iroh(),
        cert.clone(),
        server_cert.clone(),
        page.clone(),
        Arc::new(MemoryPeerStore::default()),
        entry,
    );
    assert_eq!(entered(&again).await.unwrap().workspace, lounge.workspace);
    again.close();

    // A member comes back with no entry at all, under the certificate it holds.
    let held = Held {
        cert: cert.clone(),
        server_cert: Some(server_cert.clone()),
    };
    let (kept, _) = entry_request(
        &server_cert,
        &lounge.workspace,
        &page,
        entering(Some(&held), &bob),
    )
    .await
    .unwrap();
    assert_eq!(kept, cert);
    let member = start_peer(host.iroh(), cert, server_cert.clone(), page);
    member.send(Cmd::Join(lounge.workspace_id)).unwrap();
    in_workspace(&member, lounge.workspace_id).await;
    member.close();

    // An entry that takes people by invitation says so, in its code: someone without one is told to
    // be invited, not left to read a sentence, and the session stops.
    workspaces
        .list_invitation_only(&lounge.workspace, &host_device.cert, true)
        .await
        .unwrap();
    let carol = new_key();
    let tab = new_key();
    let (cert, entry) = entry_request(
        &server_cert,
        &lounge.workspace,
        &tab,
        entering(None, &carol),
    )
    .await
    .unwrap();
    let closed = enter_page(
        host.iroh(),
        cert,
        server_cert,
        tab,
        Arc::new(MemoryPeerStore::default()),
        entry,
    );
    let refused = entered(&closed).await.unwrap_err();
    assert_eq!(refused.code, "invitation-only", "{refused:?}");
    assert!(!refused.message.is_empty());
    assert_eq!(closed.status().phase, TransportPhase::Failed);
    closed.close();

    drop(host.web);
    host.server.shutdown().await;
}

/// The entry of a device that has no root at hand: what it holds is all it can enter under.
fn entering_without_a_root(held: Option<&Held>) -> Entering<'_> {
    Entering {
        held,
        root: None,
        label: "Firefox on Linux",
        name: "Bob",
        token: None,
    }
}

/// A device with no root at hand comes to the entry under what it holds from that very host for
/// its own key, and with anything else it is told it holds no certificate, typed, before the entry
/// or any host is asked.
#[tokio::test]
async fn a_device_without_a_root_comes_to_the_entry_only_under_what_it_holds_from_that_host() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let server_cert = enrol(&alice, "alice-home", DeviceCapabilities::member()).cert;
    let other_host = enrol(&alice, "alice-other", DeviceCapabilities::member()).cert;
    let (bob, page, tab) = (new_key(), new_key(), new_key());
    let (cert, _) = entry_request(&server_cert, "lounge", &page, entering(None, &bob))
        .await
        .unwrap();
    let own = Held {
        cert: cert.clone(),
        server_cert: Some(server_cert.clone()),
    };
    let from_another_host = Held {
        cert: cert.clone(),
        server_cert: Some(other_host),
    };

    let (kept, entry) = entry_request(
        &server_cert,
        "lounge",
        &page,
        entering_without_a_root(Some(&own)),
    )
    .await
    .unwrap();
    assert_eq!(kept, cert);
    assert_eq!(entry.workspace, "lounge");
    assert!(
        matches!(
            entry.through,
            tilo_server_protocol::entering::Through::Entry
        ),
        "{entry:?}"
    );

    let refusals = [
        ("nothing held", None, &page),
        ("held from another host", Some(&from_another_host), &page),
        ("held for another device's key", Some(&own), &tab),
    ];
    for (row, held, device) in refusals {
        let refused = entry_request(
            &server_cert,
            "lounge",
            device,
            entering_without_a_root(held),
        )
        .await
        .unwrap_err();
        assert_eq!(
            refused.code,
            DeviceRefusal::NO_CERTIFICATE,
            "{row}: {refused:?}"
        );
    }
}
