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
use std::time::Duration;
use tilo_session_client::{Cmd, Outbox, TransportPhase};

use ed25519_dalek::Signer as _;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_server_client::frame::{read_frame, read_frame_or_eof, write_frame};
use tilo_server_protocol::auth::{Hello, HelloAck};
use tilo_server_protocol::entering::{
    ENTRY_BINDING_LABEL, EnterRefusal, Entered, Entering as EntryRequest, Entry, entry_digest,
};
use tilo_server_protocol::transport::{ALPN, Envelope};
use tilo_session_client::MemoryPeerStore;
use tilo_session_client::device::{DeviceRefusal, Entering, Held, entered, entry_request};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, UserIdentity};

use crate::support::{enrol, enter_page, in_workspace, start_peer, start_workspace_host};
use collab_types::WorkspaceId;

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

/// A device certificate `root`'s person makes for `device`, as a new visitor's page does.
async fn guest(root: &SigningKey, device: &SigningKey) -> DeviceCert {
    tilo_server_client::invite::guest_certificate(
        root,
        device.verifying_key().to_bytes(),
        "Stranger",
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap()
}

/// The keying material an entry on `connection` is bound to, as this end exports it.
fn binding(connection: &iroh::endpoint::Connection) -> [u8; 32] {
    let mut binding = [0u8; 32];
    connection
        .export_keying_material(&mut binding, ENTRY_BINDING_LABEL, &[])
        .unwrap();
    binding
}

/// A raw session stream to the host, as anyone on the internet opens one.
async fn dial(
    host: &crate::support::TestHost,
) -> (
    iroh::Endpoint,
    iroh::endpoint::Connection,
    iroh::endpoint::SendStream,
    iroh::endpoint::RecvStream,
) {
    let endpoint = host.iroh().bind(None).await.unwrap();
    let connection = endpoint.connect(host.iroh().addr(), ALPN).await.unwrap();
    let (send, recv) = connection.open_bi().await.unwrap();
    (endpoint, connection, send, recv)
}

/// What the host says on `recv` once it is done: nothing more, and the stream ends.
async fn says_nothing_more(recv: &mut iroh::endpoint::RecvStream) {
    let next = tokio::time::timeout(Duration::from_secs(20), read_frame_or_eof(recv, 1 << 20))
        .await
        .expect("the host ends the stream rather than keeping it open");
    assert!(
        matches!(next, Ok(None) | Err(_)),
        "the host said more to a stranger: {next:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_strangers_connection_carries_the_entry_and_nothing_else() {
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
    let private = workspaces.create("Private").await.unwrap();
    let root = new_key();
    let device = new_key();
    let cert = guest(&root, &device).await;
    let device_key = cert.device_public_key;
    let entry = |workspace: &str| EntryRequest {
        workspace: workspace.to_owned(),
        name: "Stranger".to_owned(),
        request_id: rand::random(),
        through: tilo_server_protocol::entering::Through::Entry,
    };
    // An entry's hello, its proof signed over `connection`'s keying material.
    let hello_on = |connection: &iroh::endpoint::Connection,
                    entering: EntryRequest,
                    access: Option<collab_types::MlsAccessRequestV1>| {
        let proof = device
            .sign(&entry_digest(&binding(connection), &device_key, &entering))
            .to_bytes();
        Hello::entering(
            cert.clone(),
            rand::random(),
            Entry {
                entering,
                access,
                proof,
            },
        )
    };
    let frame = |hello: &Hello| postcard::to_allocvec(hello).unwrap();
    let records = |workspace_id: WorkspaceId| {
        [Envelope::Subscribe { workspace_id }, Envelope::Ping].map(|asked| {
            tilo_delivery_protocol::transport_v2::encode(
                &tilo_delivery_protocol::transport_v2::EnvelopeV2::State(asked),
            )
            .unwrap()
        })
    };

    // A stranger's plain hello is turned away without a word.
    let (_endpoint, _connection, mut send, mut recv) = dial(&host).await;
    write_frame(&mut send, &frame(&Hello::new(cert.clone(), rand::random())))
        .await
        .unwrap();
    says_nothing_more(&mut recv).await;

    // A stranger asking to enter a workspace with no entry, with the workspace's records asked
    // right behind, is told so and nothing else: what followed its hello is never read.
    let (_endpoint, connection, mut send, mut recv) = dial(&host).await;
    write_frame(
        &mut send,
        &frame(&hello_on(&connection, entry(&private.workspace), None)),
    )
    .await
    .unwrap();
    for asked in records(private.workspace_id) {
        let _ = tokio::io::AsyncWriteExt::write_all(&mut send, &asked).await;
    }
    let _: HelloAck = postcard::from_bytes(&read_frame(&mut recv, 1 << 20).await.unwrap()).unwrap();
    let answer = tilo_delivery_protocol::transport_v2::decode(
        &tokio::time::timeout(Duration::from_secs(20), read_frame(&mut recv, 1 << 20))
            .await
            .expect("the host answers the entry")
            .unwrap(),
    )
    .unwrap();
    assert!(
        matches!(
            answer,
            tilo_delivery_protocol::transport_v2::EnvelopeV2::State(Envelope::Entered(
                Entered::Refused(EnterRefusal::Entry { .. })
            ))
        ),
        "{answer:?}"
    );
    says_nothing_more(&mut recv).await;

    // A stranger whose proof is not its key's signature, carrying a request for the workspace's
    // channel keys and its records asked behind, is turned away without a word: nothing is
    // served, the request goes nowhere, and it is admitted nowhere.
    let (_endpoint, _connection, mut send, mut recv) = dial(&host).await;
    let access = collab_types::MlsAccessRequestV1::signed(
        collab_types::WorkspaceId::parse(&lounge.workspace).unwrap(),
        vec![7; 512],
        tilo_clock::unix_now(),
        &device,
    )
    .unwrap();
    let forged = Hello::entering(
        cert.clone(),
        rand::random(),
        Entry {
            entering: entry(&lounge.workspace),
            access: Some(access),
            proof: [0; 64],
        },
    );
    write_frame(&mut send, &frame(&forged)).await.unwrap();
    for asked in records(lounge.workspace_id) {
        let _ = tokio::io::AsyncWriteExt::write_all(&mut send, &asked).await;
    }
    says_nothing_more(&mut recv).await;

    // A proof captured from one connection proves nothing on another: the stranger's own proof,
    // replayed on a second connection, is turned away there without a word, as an answer to one
    // host challenge was under the next.
    let (_endpoint, first, _send, _recv) = dial(&host).await;
    let captured = hello_on(&first, entry(&lounge.workspace), None);
    let (_endpoint, second, mut send, mut recv) = dial(&host).await;
    assert_ne!(binding(&first), binding(&second));
    write_frame(&mut send, &frame(&captured)).await.unwrap();
    for asked in records(lounge.workspace_id) {
        let _ = tokio::io::AsyncWriteExt::write_all(&mut send, &asked).await;
    }
    says_nothing_more(&mut recv).await;
    let members = workspaces.summary(&lounge.workspace).await.unwrap().members;
    assert!(
        members
            .iter()
            .all(|member| member.root() != Some(cert.user_public_key)),
        "a stranger whose entry never proved itself is a member"
    );

    // Its key, in a session of its own, is still a stranger's: the host admits it nowhere.
    let probe = start_peer(
        host.iroh(),
        cert.clone(),
        host_device.cert.clone(),
        device.clone(),
    );
    probe.send(Cmd::Join(lounge.workspace_id)).unwrap();
    // Wait for the server to turn the attempt away rather than for a while:
    // the session reports a lost connection once its attempt has ended.
    crate::support::wait_for("the server to turn the stranger's join away", || async {
        matches!(
            probe.status().phase,
            TransportPhase::Reconnecting
                | TransportPhase::Offline
                | TransportPhase::Failed
                | TransportPhase::Closed
        )
        .then_some(())
    })
    .await;
    let status = probe.status();
    assert_ne!(
        status.workspaces.get(&lounge.workspace_id),
        Some(&true),
        "a stranger whose entry never finished is in the workspace"
    );
    probe.close();

    // Its own entry, proved on its own connection, is admitted, and the answer names the host's
    // own signed address for the workspace: where a device that keeps the workspace's state itself
    // (a desktop) takes it in as a member's device, with nothing asked over HTTP.
    let (_endpoint, connection, mut send, mut recv) = dial(&host).await;
    write_frame(
        &mut send,
        &frame(&hello_on(&connection, entry(&lounge.workspace), None)),
    )
    .await
    .unwrap();
    let _: HelloAck = postcard::from_bytes(&read_frame(&mut recv, 1 << 20).await.unwrap()).unwrap();
    let answer = tilo_delivery_protocol::transport_v2::decode(
        &tokio::time::timeout(Duration::from_secs(20), read_frame(&mut recv, 1 << 20))
            .await
            .expect("the host answers the entry")
            .unwrap(),
    )
    .unwrap();
    let tilo_delivery_protocol::transport_v2::EnvelopeV2::State(Envelope::Entered(
        Entered::Admitted {
            workspace,
            host: Some(address),
            ..
        },
    )) = answer
    else {
        panic!("the entry admits the stranger's own entry, naming its host: {answer:?}");
    };
    assert_eq!(workspace, lounge.workspace);
    let hint: tilo_peer_state::join::TransportHint = postcard::from_bytes(&address).unwrap();
    let (address, _) =
        tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::authenticate_transport_hint(
            &hint,
        )
        .expect("the host signed its address");
    assert_eq!(address.workspace_id.to_string(), lounge.workspace);
    assert_eq!(address.workspace_id, lounge.workspace_id);
    assert_eq!(
        address.device_certificate.device_public_key, host_device.cert.device_public_key,
        "the address is the admitting host's own"
    );

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
