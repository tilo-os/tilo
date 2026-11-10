//! Accepting an invitation, as every device does it (`device::accept_invitation`, which the page
//! calls as `acceptInvitation`): the device enters with the certificate it holds from that very
//! host for its own key, or with one the person's root certifies now, and never with another
//! device's or another host's.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the test holds its host for the length of the test, and shuts it down at the end"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard; the test is one entry, in order"
)]

mod support;

tilo_automerge_isolate::test_worker!();

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_session_host::device::{
    Entering, Held, HostRefusal, accept_invitation, entering_certificate,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, UserIdentity};

use support::{door, enrol, in_workspace, start_peer, start_workspace_host};

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
        root: root.map(|root| root as &dyn tilo_session_host::DeviceSigner),
        label: "Firefox on Linux",
        name: "Bob",
        token: Some(token),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_accepts_an_invitation_under_its_own_certificate_from_that_host() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice, &host_device, state.path(), false).await;
    let acme = host
        .relay
        .workspaces()
        .unwrap()
        .create("Acme")
        .await
        .unwrap();
    let https = host.https();
    let bob = new_key();
    let user = bob.verifying_key().to_bytes();
    let page = new_key();

    // The first time, the root certifies this very key, and the workspace admits it.
    let (invitation, token) = door(&host, &alice, &host_device, &acme.workspace, user).await;
    let (admission, cert, host_cert) = accept_invitation(
        &*https,
        &invitation,
        &page,
        entering(None, Some(&bob), &token),
    )
    .await
    .unwrap();
    assert_eq!(admission.workspace, acme.workspace);
    assert_eq!(admission.workspace_id, acme.workspace_id);
    assert_eq!(admission.name, "Acme");
    assert_eq!(cert.device_public_key, page.verifying_key().to_bytes());
    assert_eq!(cert.user_public_key, user);
    assert_eq!(host_cert, host_device.cert);
    // Its session starts under it and is in the workspace.
    let peer = start_peer(host.iroh(), cert.clone(), host_cert.clone(), page.clone());
    peer.join(acme.workspace_id).unwrap();
    in_workspace(&peer, acme.workspace_id).await;
    peer.close();

    // Asked again, it enters under the certificate it holds, exactly.
    let held = Held {
        cert: cert.clone(),
        host_cert: Some(host_cert.clone()),
    };
    let (invitation, token) = door(&host, &alice, &host_device, &acme.workspace, user).await;
    let (_, again, _) = accept_invitation(
        &*https,
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
    let (invitation, token) = door(&host, &alice, &host_device, &acme.workspace, user).await;
    let (_, anew, _) = accept_invitation(
        &*https,
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
    let (invitation, token) = door(&host, &alice, &host_device, &acme.workspace, user).await;
    let none = accept_invitation(
        &*https,
        &invitation,
        &page,
        entering(Some(&theirs), None, &token),
    )
    .await
    .unwrap_err();
    assert_eq!(none.code, HostRefusal::NO_CERTIFICATE, "{none:?}");

    // An invitation bound to someone else does not admit Bob's device: the workspace's refusal.
    let carol = new_key().verifying_key().to_bytes();
    let (invitation, token) = door(&host, &alice, &host_device, &acme.workspace, carol).await;
    let refused = accept_invitation(
        &*https,
        &invitation,
        &page,
        entering(None, Some(&bob), &token),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.code, HostRefusal::NOT_ADMITTED, "{refused:?}");

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
    let cert = tilo_session_host::invite::guest_certificate(
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
                    .map(|root| root as &dyn tilo_session_host::DeviceSigner),
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
