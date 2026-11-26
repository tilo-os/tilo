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

use crate::support::new_key;
use std::sync::Arc;
use tilo_session_client::{Cmd, Outbox};

use ed25519_dalek::SigningKey;
use tilo_session_client::device::{
    Device, DeviceRefusal, Entering, Held, entered, entering_certificate, invitation_entry,
};
use tilo_session_client::{MemoryPeerStore, PeerConfig};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, UserIdentity};

use crate::support::{
    enrol, enter_page, entry_grant, in_workspace, start_page, start_workspace_host,
};

fn entering<'a>(
    held: Option<&'a Held>,
    root: Option<&'a SigningKey>,
    token: &'a [u8],
) -> Entering<'a> {
    Entering {
        held,
        root: root.map(|root| root as &dyn tilo_server_client::DeviceSigner),
        label: "Firefox on Linux",
        name: "Bob",
        token: Some(token),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_accepts_an_invitation_on_its_session_under_its_own_certificate_from_that_host() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice, &host_device, state.path(), false).await;
    let workspaces = host.server.workspaces().unwrap();
    let acme = workspaces.create("Acme").await.unwrap();
    let server_cert = host_device.cert.clone();
    let bob = new_key();
    let user = bob.verifying_key().to_bytes();
    let page = new_key();

    // The first time, the root certifies this very key, the session's first message carries the
    // invitation, and the workspace admits the device on it: the session is in.
    let (invitation, token) = entry_grant(&host, &alice, &host_device, &acme.workspace, user).await;
    let (cert, entry) = invitation_entry(
        &server_cert,
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
        server_cert.clone(),
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
        server_cert: Some(server_cert.clone()),
    };
    let (invitation, token) = entry_grant(&host, &alice, &host_device, &acme.workspace, user).await;
    let (again, _) = invitation_entry(
        &server_cert,
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
        server_cert: Some(server_cert.clone()),
    };
    let (anew, _) = invitation_entry(
        &server_cert,
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
        &server_cert,
        &invitation,
        &page,
        entering(Some(&theirs), None, &token),
    )
    .await
    .unwrap_err();
    assert_eq!(none.code, DeviceRefusal::NO_CERTIFICATE, "{none:?}");

    // An invitation bound to someone else does not admit Bob's device: the workspace refuses on
    // the session, and the session stops.
    let carol = new_key().verifying_key().to_bytes();
    let (invitation, token) =
        entry_grant(&host, &alice, &host_device, &acme.workspace, carol).await;
    let tab = new_key();
    let (cert, entry) = invitation_entry(
        &server_cert,
        &invitation,
        &tab,
        entering(None, Some(&bob), &token),
    )
    .await
    .unwrap();
    let refused_peer = enter_page(
        host.iroh(),
        cert,
        server_cert.clone(),
        tab,
        Arc::new(MemoryPeerStore::default()),
        entry,
    );
    let refused = entered(&refused_peer).await.unwrap_err();
    assert_eq!(refused.code, "not-admitted", "{refused:?}");
    refused_peer.close();

    drop(host.web);
    host.server.shutdown().await;
}

/// A page let in by an invitation holds its own person's root, and the server that keeps the
/// workspace is another person's. When that server turns the page's certificate away, the page
/// certifies the same device key again with its root (`recover({kind: 'reauthenticate'})` in
/// peer.mjs, through `certifyWithRoot`) and starts a session under the new certificate, with no
/// invitation: the workspace takes it back as the member it already is.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_an_invitation_let_in_certifies_itself_again_and_is_back_in_the_workspace() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let alices_device = enrol(&alice, "alice-home", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let alices = start_workspace_host(&alice, &alices_device, state.path(), false).await;
    let workspaces = alices.server.workspaces().unwrap();
    let acme = workspaces.create("Acme").await.unwrap();
    let server_cert = alices_device.cert.clone();
    let bob = new_key();
    let user = bob.verifying_key().to_bytes();
    let page = new_key();

    let (invitation, token) =
        entry_grant(&alices, &alice, &alices_device, &acme.workspace, user).await;
    let (cert, entry) = invitation_entry(
        &server_cert,
        &invitation,
        &page,
        entering(None, Some(&bob), &token),
    )
    .await
    .unwrap();
    let entered_peer = enter_page(
        alices.iroh(),
        cert.clone(),
        server_cert.clone(),
        page.clone(),
        Arc::new(MemoryPeerStore::default()),
        entry,
    );
    entered_peer.send(Cmd::Join(acme.workspace_id)).unwrap();
    entered(&entered_peer).await.unwrap();
    in_workspace(&entered_peer, acme.workspace_id).await;
    entered_peer.close();

    // The server is Alice's, the root Bob's: the root certifies Bob's own device all the same.
    let (again, answered) = tilo_server_client::certification::certify_with_root(
        &bob,
        &page,
        "Firefox on Linux",
        &server_cert,
    )
    .await
    .unwrap();
    assert_eq!(answered, server_cert);
    assert_eq!(again.user_public_key, user);
    assert_eq!(again.device_public_key, page.verifying_key().to_bytes());
    assert_ne!(again, cert, "a new certificate, not the one turned away");

    // Under it, a session with no invitation is in the workspace again.
    let back = start_page(
        alices.iroh(),
        again,
        server_cert,
        page,
        Arc::new(MemoryPeerStore::default()),
    );
    back.send(Cmd::Join(acme.workspace_id)).unwrap();
    in_workspace(&back, acme.workspace_id).await;
    back.close();

    drop(alices.web);
    alices.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_in_a_session_redeems_an_invitation_on_another_stream_of_its_connection() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice, &host_device, state.path(), false).await;
    let workspaces = host.server.workspaces().unwrap();
    let acme = workspaces.create("Acme").await.unwrap();
    let beta = workspaces.create("Beta").await.unwrap();
    let server_cert = host_device.cert.clone();
    let bob = new_key();
    let user = bob.verifying_key().to_bytes();
    let key = new_key();

    // Bob's device is in Acme, through its session's own entry.
    let (invitation, token) = entry_grant(&host, &alice, &host_device, &acme.workspace, user).await;
    let (cert, entry) = invitation_entry(
        &server_cert,
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
            server_cert: server_cert.clone(),
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
            tilo_server::workspaces::IssueInvitationRequest {
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
        matches!(again, tilo_server_client::CertifyError::Denied(_)),
        "{again:?}"
    );
    device.close();

    drop(host.web);
    host.server.shutdown().await;
}

/// An invitation's answer the connection lost is asked for again under the same request: the host
/// answers with the admission it made, never that the invitation is spent, so a device whose first
/// answer did not reach it is let in rather than turned away by its own redemption.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invitation_asked_again_under_the_same_request_admits_the_device_it_admitted() {
    use tilo_server_protocol::entering::{Entered, Entering as Entry, Through};
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice, &host_device, state.path(), false).await;
    let workspaces = host.server.workspaces().unwrap();
    let acme = workspaces.create("Acme").await.unwrap();
    let beta = workspaces.create("Beta").await.unwrap();
    let server_cert = host_device.cert.clone();
    let bob = new_key();
    let user = bob.verifying_key().to_bytes();
    let key = new_key();
    let (invitation, token) = entry_grant(&host, &alice, &host_device, &acme.workspace, user).await;
    let (cert, entry) = invitation_entry(
        &server_cert,
        &invitation,
        &key,
        entering(None, Some(&bob), &token),
    )
    .await
    .unwrap();
    let device = Device::over(
        PeerConfig {
            host: host.iroh(),
            cert,
            server_cert: server_cert.clone(),
            revocations: RevocationList::new(),
            entering: Some(entry),
        },
        Arc::new(key.clone()),
        "Firefox on Linux",
    );
    entered(device.session()).await.unwrap();
    let issued = workspaces
        .issue(
            &beta.workspace,
            &host_device.cert,
            tilo_server::workspaces::IssueInvitationRequest {
                request_id: [9; 32],
                role: tilo_peer_state::join::Role::Editor,
                recipient_user_public_key: None,
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
                inviter_name: Some("Alice".into()),
            },
        )
        .await
        .unwrap()
        .signed_invitation;
    let asked = Entry {
        workspace: beta.workspace.clone(),
        name: "Bob".into(),
        request_id: [7; 32],
        through: Through::Invitation {
            invitation: issued.clone(),
            token: None,
        },
        admission: Vec::new(),
    };
    for ask in ["first", "again"] {
        let (done, receive) = tilo_session_client::answer();
        device
            .session()
            .send(Cmd::Enter(asked.clone(), done))
            .unwrap();
        let answer = receive.await.unwrap().unwrap();
        assert!(
            matches!(&answer, Entered::Admitted { workspace, .. } if *workspace == beta.workspace),
            "{ask}: {answer:?}"
        );
    }
    // Under another request, the spent invitation admits nobody.
    let (done, receive) = tilo_session_client::answer();
    let other = Entry {
        request_id: [8; 32],
        ..asked
    };
    device.session().send(Cmd::Enter(other, done)).unwrap();
    let answer = receive.await.unwrap().unwrap();
    assert!(matches!(answer, Entered::Refused(_)), "{answer:?}");
    device.close();

    drop(host.web);
    host.server.shutdown().await;
}

#[tokio::test]
async fn a_held_certificate_serves_only_from_the_same_host_for_the_same_person_while_it_lasts() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host = enrol(&alice, "alice-home", DeviceCapabilities::member()).cert;
    let other_host = enrol(&alice, "alice-other", DeviceCapabilities::member()).cert;
    let bob = new_key();
    let page = new_key();
    let now = 1_000_000_000_000_u64;
    let cert = tilo_server_client::invite::guest_certificate(
        &bob,
        page.verifying_key().to_bytes(),
        "Firefox on Linux",
        now,
    )
    .await
    .unwrap();
    let held = |cert: DeviceCert, server_cert: &DeviceCert| Held {
        cert,
        server_cert: Some(server_cert.clone()),
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
                    .map(|root| root as &dyn tilo_server_client::DeviceSigner),
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
        DeviceRefusal::NO_CERTIFICATE
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
    // The day is the boundary itself: one ending exactly a day from now is certified anew, one
    // ending a nanosecond after that still serves.
    let day = 86_400_000_000_000_u64;
    let at_the_day = DeviceCert {
        expires_at_ns: Some(now + day),
        ..cert.clone()
    };
    let anew = choose(held(at_the_day.clone(), &host), Some(&bob))
        .await
        .unwrap();
    assert_ne!(anew, at_the_day);
    assert_eq!(anew.expires_at_ns, None, "certified anew, by the root");
    let past_the_day = DeviceCert {
        expires_at_ns: Some(now + day + 1),
        ..cert.clone()
    };
    assert_eq!(
        choose(held(past_the_day.clone(), &host), Some(&bob))
            .await
            .unwrap(),
        past_the_day
    );
    // Ending within a day with no root at hand: nothing held serves, and nothing certifies.
    assert_eq!(
        choose(held(ending, &host), None).await.unwrap_err().code,
        DeviceRefusal::NO_CERTIFICATE
    );
    // Kept without the host it was kept for: it serves no host, so only a root enters.
    let hostless = |cert: DeviceCert| Held {
        cert,
        server_cert: None,
    };
    assert_eq!(
        choose(hostless(cert.clone()), None).await.unwrap_err().code,
        DeviceRefusal::NO_CERTIFICATE
    );
    assert_eq!(
        choose(hostless(lasting.clone()), Some(&bob))
            .await
            .unwrap()
            .expires_at_ns,
        None,
        "a certificate kept for no host is not entered with: the root certifies anew"
    );
    // Nothing held at all: the root certifies this very key, and without one there is nothing.
    let first = entering_certificate(
        None,
        &host,
        &page,
        Some(&bob as &dyn tilo_server_client::DeviceSigner),
        "Firefox on Linux",
        now,
    )
    .await
    .unwrap();
    assert_eq!(first.device_public_key, page.verifying_key().to_bytes());
    assert_eq!(first.user_public_key, bob.verifying_key().to_bytes());
    assert_eq!(
        entering_certificate(None, &host, &page, None, "Firefox on Linux", now)
            .await
            .unwrap_err()
            .code,
        DeviceRefusal::NO_CERTIFICATE
    );
}

/// Bytes that are no invitation name no workspace: the entry is refused as not admitted, typed,
/// before a certificate is chosen or made, with a root at hand or without, and no host is asked.
#[tokio::test]
async fn an_invitation_that_names_no_workspace_is_refused_before_a_certificate_is_chosen() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host = enrol(&alice, "alice-home", DeviceCapabilities::member()).cert;
    let bob = new_key();
    let page = new_key();
    let rows: [(&str, &[u8]); 2] = [
        ("no bytes", b""),
        ("bytes that are no invitation", b"not an invitation"),
    ];
    for (row, invitation) in rows {
        for root in [Some(&bob), None] {
            let refused =
                invitation_entry(&host, invitation, &page, entering(None, root, b"token"))
                    .await
                    .unwrap_err();
            let case = format!("{row}, with a root: {}: {refused:?}", root.is_some());
            // Without a root and with nothing held, choosing a certificate first would have said
            // `no_certificate`: the invitation is read before that.
            assert_eq!(refused.code, DeviceRefusal::NOT_ADMITTED, "{case}");
            assert!(
                refused
                    .message
                    .starts_with("This invitation cannot be used: "),
                "{case}"
            );
            assert_eq!(refused.refusal, None, "{case}");
        }
    }
}

/// An invitation carries its server's own address, signed: a device that reaches the server only
/// through its packet relay (one that followed a link someone pasted into a message, from a
/// computer whose own site nobody else opens) dials the server from the invitation alone, and
/// enters the workspace.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invitation_opens_from_a_device_that_reaches_its_server_only_through_its_relay() {
    use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let server_device = enrol(&alice, "alice-home", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let running = crate::support::start_workspace_host_relaying(
        &alice,
        &server_device,
        state.path(),
        false,
        true,
    )
    .await;
    let acme = running
        .server
        .workspaces()
        .unwrap()
        .create("Acme")
        .await
        .unwrap();
    let bob = new_key();
    let (invitation, token) = entry_grant(
        &running,
        &alice,
        &server_device,
        &acme.workspace,
        bob.verifying_key().to_bytes(),
    )
    .await;

    // What `tilo login <link>` and the page read of it: the server it names, nothing else.
    let verified = tilo_workspace_join::decode_and_verify_invitation(
        &invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let grant = verified.grant();
    let server = grant
        .transport
        .iter()
        .filter_map(|hint| WorkspaceDeviceHintV1::from_transport_hint(hint).ok())
        .find(|server| server.workspace_id == grant.workspace_id && server.holds_keys)
        .expect("the invitation names the server that lets its guest in");
    let relay = server
        .packet_relay_url
        .parse::<iroh_base::RelayUrl>()
        .expect("the invitation names the server's packet relay");
    let through_relay =
        tilo_server_client::ServerAddress::published(&tilo_server_protocol::web::ServerAddress {
            endpoint_addr: server.endpoint_addr.clone(),
            relay_url: Some(relay.to_string()),
            web: None,
        })
        .over(tilo_server_client::Carrier::Relay);

    let page = new_key();
    let (cert, entry) = invitation_entry(
        &server.device_certificate,
        &invitation,
        &page,
        entering(None, Some(&bob), &token),
    )
    .await
    .unwrap();
    let peer = enter_page(
        through_relay,
        cert,
        server.device_certificate.clone(),
        page,
        Arc::new(MemoryPeerStore::default()),
        entry,
    );
    peer.send(Cmd::Join(acme.workspace_id)).unwrap();
    let admission = entered(&peer).await.unwrap();
    assert_eq!(admission.workspace, acme.workspace);
    in_workspace(&peer, acme.workspace_id).await;
    peer.close();
    drop(running.web);
    running.server.shutdown().await;
}

/// A person a server does not know yet (its operator never made them an owner, and no invitation
/// brought them) is told exactly that when they ask it for their workspaces, with their key and
/// what makes them known: not that some action is not theirs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_the_server_does_not_know_is_told_so_and_what_to_do() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let server_device = enrol(&alice, "alice-home", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let running = start_workspace_host(&alice, &server_device, state.path(), false).await;
    // It keeps workspaces for the people its site lets in, as tilo.sh's servers and a local stack
    // do: anyone's device connects, and what they may do is decided per request.
    let workspaces = running.server.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    assert!(
        workspaces
            .offer_own_workspaces(tilo_server::workspaces::OwnWorkspaceOffer::free(
                "tilo.test"
            ))
            .unwrap()
    );
    assert!(workspaces.admit_own_through(&lounge.workspace));
    let (bob, _) = UserIdentity::generate("Bob", 1);
    let laptop = enrol(&bob, "laptop", DeviceCapabilities::primary());
    let session = crate::support::start_peer(
        running.iroh(),
        laptop.cert.clone(),
        server_device.cert.clone(),
        laptop.key.clone(),
    );
    let device = Device::of(
        session.clone(),
        laptop.cert.clone(),
        server_device.cert.clone(),
        "laptop",
    );
    device
        .connected()
        .await
        .expect("a server that keeps workspaces for people lets anyone's device connect");
    let refused = device
        .person_workspaces(
            &tilo_server_protocol::workspaces::WorkspaceLabels::new(),
            None,
        )
        .await
        .expect_err("a person nobody here let in holds no list here");
    assert_eq!(
        refused.code,
        DeviceRefusal::NOT_ON_THIS_SERVER,
        "{refused:?}"
    );
    let key = hex::encode(bob.verifying_key().to_bytes());
    assert!(
        refused.message.contains(&key) && refused.message.contains("owner"),
        "the refusal names the person's key and what makes them known: {}",
        refused.message
    );
    session.close();
    drop(running.web);
    running.server.shutdown().await;
}
