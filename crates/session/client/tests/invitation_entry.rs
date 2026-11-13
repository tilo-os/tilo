//! Accepting an invitation, as every device does it: on its session's first message
//! (`device::invitation_entry`, which the page calls as `invitationEntry`, then the session's own
//! handshake), or, for a device already in a session with the host, on another stream of that
//! session's connection (`Device::redeem`). The device enters with the certificate it holds from
//! that very host for its own key, or with one the person's root certifies now, and never with
//! another device's or another host's.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the test is one entry, in order; the host lives until it ends"
)]

use std::sync::Arc;
use tilo_session_client::{Cmd, Outbox};

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_session_client::device::{
    Device, Entering, Held, HostRefusal, entered, entering_certificate, invitation_entry,
};
use tilo_session_client::{MemoryPeerStore, PeerConfig};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, UserIdentity};

use crate::support::{door, enrol, enter_page, in_workspace, start_workspace_host};

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

fn entering<'a>(
    held: Option<&'a Held>,
    root: Option<&'a SigningKey>,
    token: &'a [u8],
) -> Entering<'a> {
    Entering {
        held,
        root: root.map(|root| root as &dyn tilo_relay_client::DeviceSigner),
        label: "Firefox on Linux",
        name: "Bob",
        token: Some(token),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_accepts_an_invitation_on_its_session_under_its_own_certificate_from_that_host() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice, &host_device, state.path(), false).await;
    let workspaces = host.relay.workspaces().unwrap();
    let acme = workspaces.create("Acme").await.unwrap();
    let host_cert = host_device.cert.clone();
    let bob = new_key();
    let user = bob.verifying_key().to_bytes();
    let page = new_key();

    // The first time, the root certifies this very key, the session's first message carries the
    // invitation, and the workspace admits the device on it: the session is in.
    let (invitation, token) = door(&host, &alice, &host_device, &acme.workspace, user).await;
    let (cert, entry) = invitation_entry(
        &host_cert,
        &invitation,
        &page,
        entering(None, Some(&bob), &token),
    )
    .await
    .unwrap();
    assert_eq!(entry.workspace, acme.workspace, "the invitation names it");
    assert_eq!(cert.device_public_key, page.verifying_key().to_bytes());
    assert_eq!(cert.user_public_key, user);
    let peer = enter_page(
        host.iroh(),
        cert.clone(),
        host_cert.clone(),
        page.clone(),
        Arc::new(MemoryPeerStore::default()),
        entry,
    );
    peer.send(Cmd::Join(acme.workspace_id)).unwrap();
    let admission = entered(&peer).await.unwrap();
    assert_eq!(admission.workspace, acme.workspace);
    assert_eq!(admission.workspace_id, acme.workspace_id);
    assert_eq!(admission.name, "Acme");
    in_workspace(&peer, acme.workspace_id).await;
    peer.close();

    // Asked again, it enters under the certificate it holds, exactly.
    let held = Held {
        cert: cert.clone(),
        host_cert: Some(host_cert.clone()),
    };
    let (invitation, token) = door(&host, &alice, &host_device, &acme.workspace, user).await;
    let (again, _) = invitation_entry(
        &host_cert,
        &invitation,
        &page,
        entering(Some(&held), Some(&bob), &token),
    )
    .await
    .unwrap();
    assert_eq!(
        again, cert,
        "the certificate the workspace knows this device by"
    );

    // A certificate of another device's is never presented: the root certifies this key anew.
    let laptop = new_key();
    let theirs = Held {
        cert: DeviceCert {
            device_public_key: laptop.verifying_key().to_bytes(),
            ..cert.clone()
        },
        host_cert: Some(host_cert.clone()),
    };
    let (anew, _) = invitation_entry(
        &host_cert,
        &invitation,
        &page,
        entering(Some(&theirs), Some(&bob), &token),
    )
    .await
    .unwrap();
    assert_eq!(anew.device_public_key, page.verifying_key().to_bytes());
    assert_ne!(anew, theirs.cert);

    // Without a root, nothing it could enter with is no certificate: said so, typed, before the
    // host is asked to admit anything.
    let none = invitation_entry(
        &host_cert,
        &invitation,
        &page,
        entering(Some(&theirs), None, &token),
    )
    .await
    .unwrap_err();
    assert_eq!(none.code, HostRefusal::NO_CERTIFICATE, "{none:?}");

    // An invitation bound to someone else does not admit Bob's device: the workspace refuses on
    // the session, and the session stops.
    let carol = new_key().verifying_key().to_bytes();
    let (invitation, token) = door(&host, &alice, &host_device, &acme.workspace, carol).await;
    let tab = new_key();
    let (cert, entry) = invitation_entry(
        &host_cert,
        &invitation,
        &tab,
        entering(None, Some(&bob), &token),
    )
    .await
    .unwrap();
    let refused_peer = enter_page(
        host.iroh(),
        cert,
        host_cert.clone(),
        tab,
        Arc::new(MemoryPeerStore::default()),
        entry,
    );
    let refused = entered(&refused_peer).await.unwrap_err();
    assert_eq!(refused.code, "not-admitted", "{refused:?}");
    refused_peer.close();

    drop(host.web);
    host.relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_in_a_session_redeems_an_invitation_on_another_stream_of_its_connection() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice, &host_device, state.path(), false).await;
    let workspaces = host.relay.workspaces().unwrap();
    let acme = workspaces.create("Acme").await.unwrap();
    let beta = workspaces.create("Beta").await.unwrap();
    let host_cert = host_device.cert.clone();
    let bob = new_key();
    let user = bob.verifying_key().to_bytes();
    let key = new_key();

    // Bob's device is in Acme, through its session's own entry.
    let (invitation, token) = door(&host, &alice, &host_device, &acme.workspace, user).await;
    let (cert, entry) = invitation_entry(
        &host_cert,
        &invitation,
        &key,
        entering(None, Some(&bob), &token),
    )
    .await
    .unwrap();
    let device = Device::over(
        PeerConfig {
            host: host.iroh(),
            cert: cert.clone(),
            host_cert: host_cert.clone(),
            revocations: RevocationList::new(),
            entering: Some(entry),
        },
        Arc::new(key.clone()),
        "Firefox on Linux",
    );
    entered(device.session()).await.unwrap();
    assert_eq!(
        device.join(&acme.workspace).await.unwrap(),
        acme.workspace_id
    );

    // An invitation into Beta, without a token: its issuer's grant admits the device on another
    // stream of the same connection, and the session it holds goes on as it was.
    let issued = workspaces
        .issue(
            &beta.workspace,
            &host_device.cert,
            tilo_relay::workspaces::IssueInvitationRequest {
                request_id: [9; 32],
                role: tilo_peer_state::join::Role::Editor,
                recipient_user_public_key: Some(user),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
                inviter_name: Some("Alice".into()),
            },
        )
        .await
        .unwrap()
        .signed_invitation;
    let admission = device.redeem(&issued, "Bob").await.unwrap();
    assert_eq!(admission.workspace, beta.workspace);
    assert_eq!(
        device.join(&beta.workspace).await.unwrap(),
        beta.workspace_id
    );
    assert!(
        device.session().status().workspaces.get(&acme.workspace_id) == Some(&true),
        "the session is still in the workspace it was in"
    );

    // Spent, it admits nobody again.
    let again = device.redeem(&issued, "Bob").await.unwrap_err();
    assert!(
        matches!(again, tilo_relay_client::CertifyError::Denied(_)),
        "{again:?}"
    );
    device.close();

    drop(host.web);
    host.relay.shutdown().await;
}

#[tokio::test]
async fn a_held_certificate_serves_only_from_the_same_host_for_the_same_person_while_it_lasts() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host = enrol(&alice, "alice-home", DeviceCapabilities::host()).cert;
    let other_host = enrol(&alice, "alice-other", DeviceCapabilities::host()).cert;
    let bob = new_key();
    let page = new_key();
    let now = 1_000_000_000_000_u64;
    let cert = tilo_relay_client::invite::guest_certificate(
        &bob,
        page.verifying_key().to_bytes(),
        "Firefox on Linux",
        now,
    )
    .await
    .unwrap();
    let held = |cert: DeviceCert, host_cert: &DeviceCert| Held {
        cert,
        host_cert: Some(host_cert.clone()),
    };
    let choose = |held: Held, root: Option<&SigningKey>| {
        let host = host.clone();
        let page = page.clone();
        let root = root.cloned();
        async move {
            entering_certificate(
                Some(&held),
                &host,
                &page,
                root.as_ref()
                    .map(|root| root as &dyn tilo_relay_client::DeviceSigner),
                "Firefox on Linux",
                now,
            )
            .await
        }
    };

    assert_eq!(choose(held(cert.clone(), &host), None).await.unwrap(), cert);
    assert_eq!(
        choose(held(cert.clone(), &host), Some(&bob)).await.unwrap(),
        cert
    );
    // Another host's: a host set up again is never handed what was kept for the one before it.
    assert_eq!(
        choose(held(cert.clone(), &other_host), None)
            .await
            .unwrap_err()
            .code,
        HostRefusal::NO_CERTIFICATE
    );
    // Another person's, where this person's root is at hand.
    let carol = new_key();
    assert_ne!(
        choose(held(cert.clone(), &host), Some(&carol))
            .await
            .unwrap()
            .user_public_key,
        cert.user_public_key
    );
    // Ending within a day: certified anew.
    let ending = DeviceCert {
        expires_at_ns: Some(now + 60_000_000_000),
        ..cert.clone()
    };
    assert_ne!(
        choose(held(ending.clone(), &host), Some(&bob))
            .await
            .unwrap(),
        ending
    );
    let lasting = DeviceCert {
        expires_at_ns: Some(now + 7 * 86_400_000_000_000),
        ..cert.clone()
    };
    assert_eq!(
        choose(held(lasting.clone(), &host), Some(&bob))
            .await
            .unwrap(),
        lasting
    );
}
