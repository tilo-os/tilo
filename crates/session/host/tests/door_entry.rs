//! Entering a listed workspace through its door, as every device does it
//! (`device::enter_through_door`, which the page calls as `enterThroughDoor`): the door lets the
//! person in and the workspace admits this very device, under the certificate it holds from that
//! host or one its person's root makes now; a door that says no says why in a code of its own.

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

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_relay_client::login::Door;
use tilo_relay_protocol::web::API_PREFIX;
use tilo_session_host::device::{Entering, Held, enter_through_door};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{enrol, in_workspace, start_peer, start_workspace_host};

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
async fn a_device_enters_a_listed_workspace_through_its_door_and_a_closed_door_says_why() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice, &host_device, state.path(), false).await;
    let workspaces = host.relay.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .publish(
            &lounge.workspace,
            &host_device.cert,
            tilo_relay_protocol::directory::Listing {
                description: "Coffee and tea".into(),
                default_role: tilo_relay_protocol::workspaces::WorkspaceRole::Editor,
            },
        )
        .await
        .unwrap();
    let https = host.https();
    let door = Door {
        client: &*https,
        path: format!("{API_PREFIX}/directory/{}/join", lounge.workspace),
    };

    // Bob's tab, with his root, goes through the door: the workspace admits this very key.
    let bob = new_key();
    let page = new_key();
    let (admission, cert, host_cert) =
        enter_through_door(&*https, &door, &page, entering(None, &bob))
            .await
            .unwrap();
    assert_eq!(admission.workspace, lounge.workspace);
    assert_eq!(cert.device_public_key, page.verifying_key().to_bytes());
    assert_eq!(cert.user_public_key, bob.verifying_key().to_bytes());
    assert_eq!(host_cert, host_device.cert);
    let peer = start_peer(host.iroh(), cert.clone(), host_cert.clone(), page.clone());
    peer.join(lounge.workspace_id).unwrap();
    in_workspace(&peer, lounge.workspace_id).await;
    peer.close();

    // Through the door again, it enters under the certificate it holds.
    let held = Held {
        cert: cert.clone(),
        host_cert: Some(host_cert),
    };
    let (_, again, _) = enter_through_door(&*https, &door, &page, entering(Some(&held), &bob))
        .await
        .unwrap();
    assert_eq!(again, cert);

    // A door that takes people by invitation says so, in its code: someone without one is
    // told to be invited, not left to read a sentence.
    workspaces
        .list_invitation_only(&lounge.workspace, &host_device.cert, true)
        .await
        .unwrap();
    let carol = new_key();
    let closed = enter_through_door(&*https, &door, &new_key(), entering(None, &carol))
        .await
        .unwrap_err();
    assert_eq!(closed.code, "invitation-only", "{closed:?}");
    assert!(!closed.message.is_empty());

    drop(host.web);
    host.relay.shutdown().await;
}
