//! A page and the other person's desktop talking in their conversation's
//! record: the record's id is the two people's, derived on both sides, so the
//! page opens it like any other record of the workspace. The desktop (a
//! native device of the other person) admits the page to the record's channel
//! and opens what it wrote, live; the page opens what the desktop writes
//! back. The host carries ciphertext only.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::items_after_statements,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

// Placing the workspace has the host build its governance in an automerge worker.
tilo_automerge_isolate::test_worker!();

use std::collections::BTreeSet;

use channel_crypto::{ChannelPeer, VaultKey};
use collab_types::{
    ContentRetention, DocumentFrame, DocumentMutation, Ed25519PublicKey, MlsGovernanceFrame,
    OpKind, SignedOp, document_artifact_id, document_attestation_id, document_channel_id,
};
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay::{Inbound, RelayClient, RelayCoeditSession};
use tilo_session_host::PeerEvent;
use tilo_session_host::invite::guest_certificate;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, UserIdentity};

use support::{door, enrol, enter, redeem, start_workspace_host, wait_for};

/// A device certificate for `root`'s page, as the person's own root signs it.
async fn page_cert(root: &SigningKey, label: &str) -> (SigningKey, DeviceCert) {
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let cert = guest_certificate(
        root,
        key.verifying_key().to_bytes(),
        label,
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    (key, cert)
}

/// Publish `message` through the host the way a shell's session does.
fn publish(session: &RelayCoeditSession, message: &NeighbourMsg) {
    let (channel, retention) = neighbour_wire::retention(message, false).unwrap();
    let body = postcard::to_allocvec(message).unwrap();
    match retention {
        neighbour_wire::Retention::Checkpoint => session.publish_checkpoint(channel, body),
        neighbour_wire::Retention::Delta => session.publish_retained_delta(channel, body),
    }
    .unwrap();
}

/// What `peer` queued to publish, taken.
fn take(peer: &mut ChannelPeer, vault: &VaultKey) -> Vec<MlsGovernanceFrame> {
    let frames = peer.outbox().to_vec();
    peer.published(vault).unwrap();
    frames
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "async Relay::shutdown consumes the relay; the suggested later drop is a use-after-move"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_and_the_other_persons_desktop_talk_in_their_conversations_record() {
    // Alice runs the host her workspace lives on, and her desktop is another
    // device of hers; Bob takes part through the workspace's door.
    let (alice_root, _) = UserIdentity::generate("Alice", 1);
    let alice_root_key = alice_root.verifying_key().to_bytes();
    let bob_root = SigningKey::generate(&mut UnwrapErr(SysRng));
    let bob_root_key = bob_root.verifying_key().to_bytes();
    let host_device = enrol(&alice_root, "alice-home", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice_root, &host_device, state.path(), false).await;
    let workspace = host
        .relay
        .workspaces()
        .unwrap()
        .create("Pages talking")
        .await
        .unwrap();

    // Bob's page, let in through the workspace's door: a browser of bob's.
    let (bob_key, bob_cert) = page_cert(&bob_root, "Bob's page").await;
    let bob = enter(
        &host,
        door(
            &host,
            &alice_root,
            &host_device,
            &workspace.workspace,
            bob_root_key,
        )
        .await,
        &bob_cert,
        &bob_key,
        &host_device.cert,
        workspace.workspace_id,
    )
    .await;

    // Alice's desktop: a native device of hers, let in the same way.
    let alice = enrol(&alice_root, "alice-desktop", DeviceCapabilities::primary());
    redeem(
        &host,
        door(
            &host,
            &alice_root,
            &host_device,
            &workspace.workspace,
            alice_root_key,
        )
        .await,
        &alice.cert,
        &alice.key,
    )
    .await;

    // The conversation's record is named by the two people it is between, so
    // the page derives its id without asking anyone: it opens the record like
    // any other record of the workspace.
    let mut people = [hex::encode(bob_root_key), hex::encode(alice_root_key)];
    people.sort_unstable();
    let pair = collab_types::direct_pair_id(&workspace.workspace, &people[0], &people[1]).unwrap();

    // Alice's desktop holds the workspace's key scope, which the conversation's
    // record resolves to: no grant narrows it, so the record is read by the
    // workspace's readers, as the desktop's governance replica resolves it.
    // The page opens the record and writes its message, which waits for the
    // keys the desktop has to hand out.
    let route = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: workspace.workspace.clone(),
        record: collab_types::direct_record(&pair),
    };
    let channel = document_channel_id(&route.app_id, &route.cluster_id, &route.record);
    let scope = collab_types::workspace_scope(&workspace.workspace);
    let artifact = document_artifact_id(&route.app_id, &route.cluster_id, &route.record);
    let vault = VaultKey::new([7; 32]);
    let desktop_dir = tempfile::tempdir().unwrap();
    let mut desktop_channel = ChannelPeer::create_now(
        Some(std::sync::Arc::new(channel_crypto::Files::new(
            desktop_dir.path(),
        ))),
        scope,
        Box::new(alice.key.clone()),
        &vault,
    )
    .unwrap();
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let client = RelayClient::connect(
        endpoint,
        host.relay.node_addr(),
        alice_root_key,
        alice.cert.clone(),
        alice.key.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    let alice_device = alice.key.verifying_key().to_bytes();
    let desktop = RelayCoeditSession::join(client, workspace.workspace_id, alice_device).unwrap();
    publish(
        &desktop,
        &NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(alice.cert.clone()),
            revocations: Vec::new(),
        },
    );
    for (index, frame) in take(&mut desktop_channel, &vault).into_iter().enumerate() {
        publish(
            &desktop,
            &NeighbourMsg::MlsGovernance {
                workspace: workspace.workspace.clone(),
                frame: Box::new(frame),
                delivery: 1 + u64::try_from(index).unwrap(),
            },
        );
    }
    // The scope's readers are the workspace's members' devices: the two
    // people in the conversation, each on their own devices.
    let bob_device = Ed25519PublicKey::from_bytes(bob_cert.device_public_key);

    bob.open_record(workspace.workspace_id, route.clone())
        .unwrap();
    bob.open_route(route.clone()).unwrap();
    let message = DocumentMutation::Incremental(b"a message from Bob's page".to_vec());
    bob.publish(route.clone(), &message).unwrap();

    // The desktop admits the page to the record's channel when it asks, then
    // opens what the page sealed in it: the message arrives live.
    let mut admitted = false;
    let mut opened = None;
    let mut delivery = 2_u64;
    let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
    while opened.is_none() {
        let inbound = tokio::time::timeout_at(deadline, desktop.recv())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "the page's message to reach the desktop; the page's channels:\n{}",
                    bob.status().channels
                )
            })
            .expect("the desktop's session open");
        let Inbound::Delta { data, .. } = inbound else {
            continue;
        };
        match postcard::from_bytes::<NeighbourMsg>(&data) {
            Ok(NeighbourMsg::MlsAccessRequest { request, .. }) if request.channel == scope => {
                assert_eq!(request.device, bob_device, "the page asks, not the host");
                request.verify().expect("signed by the page's device key");
                desktop_channel.learn_request(*request, &vault).unwrap();
                let allowed =
                    BTreeSet::from([Ed25519PublicKey::from_bytes(alice_device), bob_device]);
                desktop_channel.reconcile_now(&allowed, &vault).unwrap();
                for frame in take(&mut desktop_channel, &vault) {
                    publish(
                        &desktop,
                        &NeighbourMsg::MlsGovernance {
                            workspace: workspace.workspace.clone(),
                            frame: Box::new(frame),
                            delivery,
                        },
                    );
                    delivery = delivery.checked_add(1).unwrap();
                }
                admitted = true;
            }
            Ok(NeighbourMsg::AppDocument {
                envelope,
                record_id,
                ..
            }) if record_id == route.record => {
                assert_eq!(envelope.author, bob_device, "what the page wrote");
                let metadata = envelope
                    .encryption
                    .expect("the page never writes the conversation's record in the clear");
                let clear = desktop_channel
                    .content_keys()
                    .unwrap()
                    .for_unit(channel)
                    .open_content(artifact, &metadata, &envelope.payload)
                    .unwrap();
                opened = Some(DocumentFrame::decode(&clear).unwrap());
            }
            _ => {}
        }
    }
    assert!(
        admitted,
        "the desktop admitted the page to the conversation's record"
    );
    assert_eq!(
        opened,
        Some(DocumentFrame::Whole(message)),
        "the desktop reads what the page wrote, live"
    );

    // The desktop writes back, sealed the same way; the page reads it live.
    let frame = DocumentMutation::Incremental(b"and one back from Alice's desktop".to_vec())
        .encode()
        .unwrap();
    let (metadata, sealed) = desktop_channel
        .content_keys()
        .unwrap()
        .for_unit(channel)
        .seal_content(artifact, ContentRetention::Delta, &frame)
        .unwrap();
    let mut op = SignedOp::new_unsigned_with_encryption(
        sealed,
        artifact,
        Ed25519PublicKey::from_bytes(alice_device),
        document_attestation_id(&route.app_id, &route.cluster_id, &route.record),
        OpKind::Change,
        std::time::SystemTime::now(),
        2,
        metadata,
    );
    collab_types::sign_op(&mut op, &alice.key).unwrap();
    publish(
        &desktop,
        &NeighbourMsg::AppDocument {
            peer: neighbour_wire::peer_handle(&alice_device),
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            record_id: route.record.clone(),
            envelope: Box::new(op),
        },
    );
    let reader = bob.clone();
    let reply = wait_for("the desktop's message on the conversation's record", || {
        let (reader, route) = (reader.clone(), route.clone());
        async move {
            reader.drain().into_iter().find_map(|event| match event {
                PeerEvent::Document(document)
                    if document.route == route && document.author == alice_device =>
                {
                    document.mutation()
                }
                _ => None,
            })
        }
    })
    .await;
    assert_eq!(
        reply,
        DocumentMutation::Incremental(b"and one back from Alice's desktop".to_vec()),
        "the page reads what the desktop wrote"
    );

    desktop.close().await;
    bob.close();
    drop(host.web);
    host.relay.shutdown().await;
}
