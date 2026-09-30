//! A room this relay governs: another person is invited, joins, edits with
//! the room's owner, sees their presence, and is removed again.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "one end-to-end scenario whose relays and sessions live until it ends"
)]

use std::time::{Duration, SystemTime};

use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use tilo_peer_state::join::{MemberRole, decode_and_verify_invitation};
use tilo_peer_state::{NeighbourMsg, StatePeer, TransportOrigin};
use tilo_relay::coedit::{Inbound, RelayCoeditSession};
use tilo_relay::rooms::{RemoveMemberRequest, RoomAdminClient, RoomCommand, RoomReply};
use tilo_relay::workspace_join::WorkspaceJoinClient;
use tilo_relay::{Envelope, Relay, RelayClient, RelayConfig};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser};

const APP: &str = "so.tilo.chat";

/// An endpoint whose key is the device's own, as a device dials with.
async fn device_endpoint(device: &TestDevice) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&device.sk.to_bytes()))
        .bind()
        .await
        .unwrap()
}

async fn start(state: &std::path::Path, alice: &TestUser, host: &TestDevice) -> Relay {
    Relay::start(
        RelayConfig {
            state_root: state.to_path_buf(),
            ..RelayConfig::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: alice.user_pk,
            device_signing_key: host.sk.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap()
}

async fn session(relay: &Relay, device: &TestDevice, workspace_id: u64) -> RelayCoeditSession {
    let client = RelayClient::connect_to_pinned_host(
        device_endpoint(device).await,
        relay.node_addr(),
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    RelayCoeditSession::join(client, workspace_id, device.device_pubkey()).unwrap()
}

/// Apply what `session` receives to `peer` until `done` holds, recording
/// every presence payload by its author.
async fn receive_until(
    what: &str,
    session: &RelayCoeditSession,
    peer: &mut StatePeer,
    presence: &mut Vec<([u8; 32], Vec<u8>)>,
    mut done: impl FnMut(&StatePeer, &[([u8; 32], Vec<u8>)]) -> bool,
) {
    tilo_test_env::within(what, async {
        while !done(peer, presence) {
            match session.recv().await.expect("the session stays open") {
                Inbound::Delta { author, data } => {
                    let message: NeighbourMsg = postcard::from_bytes(&data).unwrap();
                    let origin = if author == peer.authority().device_public_key {
                        TransportOrigin::RetainedRelayDelta(author)
                    } else {
                        TransportOrigin::RelayDelta(author)
                    };
                    peer.receive_from(origin, message, SystemTime::now())
                        .unwrap();
                }
                Inbound::Presence { author, payload } => presence.push((author, payload)),
                _ => {}
            }
        }
    })
    .await;
}

fn publish(session: &RelayCoeditSession, message: &NeighbourMsg) {
    let (channel, kind) = tilo_peer_state::transport::retention(message, false).unwrap();
    let body = postcard::to_stdvec(message).unwrap();
    if kind == tilo_peer_state::transport::Retention::Checkpoint {
        session.publish_checkpoint(channel, body)
    } else {
        session.publish_retained_delta(channel, body)
    }
    .unwrap();
}

/// Set `key` to `value` in `peer`'s copy of the record and sign the change.
fn write(
    peer: &mut StatePeer,
    key: &str,
    value: &str,
    key_pair: &ed25519_dalek::SigningKey,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&peer.document(APP, "").unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, key, value).unwrap();
    doc.commit();
    peer.publish_incremental(APP, "", doc.save_after(&heads), key_pair, SystemTime::now())
        .unwrap()
}

fn text(peer: &StatePeer, key: &str) -> Option<String> {
    let doc = AutoCommit::load(&peer.document(APP, "")?).ok()?;
    doc.get(ROOT, key)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

fn role(peer: &StatePeer, device: [u8; 32]) -> Option<collab_types::Role> {
    peer.permissions(device, SystemTime::now())
        .ok()
        .map(|(role, _, _)| role)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invited_person_joins_a_governed_room_edits_sees_presence_and_is_removed() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let address = relay.node_addr();

    // Alice's laptop creates a room on her relay and enters it as its owner.
    let admin = RoomAdminClient::new(device_endpoint(&laptop).await, laptop.cert.clone());
    let room = admin
        .create(address.clone(), "Design review")
        .await
        .unwrap();
    assert_eq!(room.name, "Design review");
    assert!(matches!(
        admin
            .send(
                address.clone(),
                RoomCommand::Enter {
                    room: room.room.clone()
                }
            )
            .await
            .unwrap(),
        RoomReply::Entered { changed: true }
    ));

    // Bob's device cannot administer Alice's rooms.
    let intruder = RoomAdminClient::new(device_endpoint(&phone).await, phone.cert.clone());
    assert!(
        intruder
            .send(address.clone(), RoomCommand::List)
            .await
            .is_err()
    );

    // Alice invites Bob's person; his device redeems through the signed
    // address the invitation carries and is admitted by the room's governor.
    let invitation = admin
        .invite(
            address.clone(),
            &room.room,
            MemberRole::Editor,
            Some(bob.user_pk),
            Duration::from_secs(600),
        )
        .await
        .unwrap();
    let verified = decode_and_verify_invitation(
        &invitation.signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    assert_eq!(
        verified.grant().authority,
        host.cert,
        "the relay governs the room"
    );
    let admission = WorkspaceJoinClient::new(device_endpoint(&phone).await)
        .redeem_hint(
            &verified.grant().transport[0],
            invitation.signed_invitation.clone(),
            phone.cert.clone(),
            &phone.sk,
            [7; 32],
        )
        .await
        .unwrap();
    let bootstrap = admission.bootstrap;
    assert_eq!(bootstrap.room, room.room);
    assert_eq!(bootstrap.workspace_id, room.workspace_id);
    assert_eq!(bootstrap.role, MemberRole::Editor);

    // Bob's own replica, from the bootstrap alone, makes him an editor.
    let mut bob_peer = StatePeer::new(
        &bootstrap.room,
        bootstrap.authority.clone(),
        bootstrap.certified_roster.clone(),
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    bob_peer.select_admission_v5().unwrap();
    for bytes in &bootstrap.governance_messages {
        bob_peer
            .receive_from(
                TransportOrigin::RetainedRelayDelta(bootstrap.authority.device_public_key),
                postcard::from_bytes(bytes).unwrap(),
                SystemTime::now(),
            )
            .unwrap();
    }
    assert_eq!(
        role(&bob_peer, phone.device_pubkey()),
        Some(collab_types::Role::Editor)
    );

    // Alice's replica learns governance from the relay's retained history.
    let mut alice_peer = StatePeer::new(
        &room.room,
        host.cert.clone(),
        vec![host.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    let alice_session = session(&relay, &laptop, room.workspace_id).await;
    let bob_session = session(&relay, &phone, room.workspace_id).await;
    let mut alice_presence = Vec::new();
    let mut bob_presence = Vec::new();
    receive_until(
        "Alice's replica admits both people",
        &alice_session,
        &mut alice_peer,
        &mut alice_presence,
        |peer, _| {
            role(peer, laptop.device_pubkey()) == Some(collab_types::Role::Owner)
                && role(peer, phone.device_pubkey()) == Some(collab_types::Role::Editor)
        },
    )
    .await;
    crate::common::wait_until_subscribed(&relay, room.workspace_id, &phone.device_pubkey()).await;

    // Alice writes; Bob's replica verifies and applies it.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "from Alice").unwrap();
    doc.commit();
    let message = alice_peer
        .publish_incremental(APP, "", doc.save_after(&[]), &laptop.sk, SystemTime::now())
        .unwrap();
    publish(&alice_session, &message);
    receive_until(
        "Alice's write in Bob's replica",
        &bob_session,
        &mut bob_peer,
        &mut bob_presence,
        |peer, _| text(peer, "topic").as_deref() == Some("from Alice"),
    )
    .await;

    // Bob writes as an editor; Alice's replica verifies and applies it.
    let mut doc = AutoCommit::load(&bob_peer.document(APP, "").unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "reply", "from Bob").unwrap();
    doc.commit();
    let message = bob_peer
        .publish_incremental(
            APP,
            "",
            doc.save_after(&heads),
            &phone.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&bob_session, &message);
    receive_until(
        "Bob's write in Alice's replica",
        &alice_session,
        &mut alice_peer,
        &mut alice_presence,
        |peer, _| text(peer, "reply").as_deref() == Some("from Bob"),
    )
    .await;

    // A conversation: each write depends on the other person's last one.
    for turn in 0..3 {
        let said = format!("Alice {turn}");
        publish(
            &alice_session,
            &write(&mut alice_peer, "turn", &said, &laptop.sk),
        );
        receive_until(
            "Alice's turn at Bob",
            &bob_session,
            &mut bob_peer,
            &mut bob_presence,
            |peer, _| text(peer, "turn").as_deref() == Some(said.as_str()),
        )
        .await;
        let said = format!("Bob {turn}");
        publish(
            &bob_session,
            &write(&mut bob_peer, "turn", &said, &phone.sk),
        );
        receive_until(
            "Bob's turn at Alice",
            &alice_session,
            &mut alice_peer,
            &mut alice_presence,
            |peer, _| text(peer, "turn").as_deref() == Some(said.as_str()),
        )
        .await;
    }

    // Each sees the other's presence.
    alice_session
        .publish_presence(b"alice is here".to_vec())
        .unwrap();
    bob_session
        .publish_presence(b"bob is here".to_vec())
        .unwrap();
    receive_until(
        "Alice's presence at Bob",
        &bob_session,
        &mut bob_peer,
        &mut bob_presence,
        |_, seen| {
            seen.iter()
                .any(|(author, _)| *author == laptop.device_pubkey())
        },
    )
    .await;
    receive_until(
        "Bob's presence at Alice",
        &alice_session,
        &mut alice_peer,
        &mut alice_presence,
        |_, seen| {
            seen.iter()
                .any(|(author, _)| *author == phone.device_pubkey())
        },
    )
    .await;

    // Alice removes Bob: the relay stops admitting his device.
    let RoomReply::Governance { generation } = admin
        .send(
            address.clone(),
            RoomCommand::Remove {
                room: room.room.clone(),
                request: RemoveMemberRequest {
                    request_id: [9; 32],
                    user_public_key: bob.user_pk,
                },
            },
        )
        .await
        .unwrap()
    else {
        panic!("a governance change")
    };
    assert!(generation > room.generation);
    let refused = async {
        let client = RelayClient::connect_to_pinned_host(
            device_endpoint(&phone).await,
            relay.node_addr(),
            phone.cert.clone(),
            phone.sk.clone(),
            RevocationList::new(),
        )
        .await
        .ok()?;
        client
            .send(Envelope::Subscribe {
                workspace_id: room.workspace_id,
            })
            .ok()?;
        tilo_test_env::within("the relay answers a removed member", client.recv()).await
    }
    .await;
    assert!(
        !matches!(refused, Some(Envelope::Ready { .. })),
        "a removed member is not admitted: {refused:?}"
    );

    // The room survives a restart of its relay, governance and all.
    alice_session.close().await;
    bob_session.close().await;
    relay.shutdown().await;
    let relay = start(state.path(), &alice, &host).await;
    let rooms = relay.rooms().unwrap().list().await;
    let [restored] = rooms.as_slice() else {
        panic!("one governed room: {rooms:?}")
    };
    assert_eq!(restored.room, room.room);
    assert_eq!(restored.generation, generation);
    assert!(
        restored
            .members
            .iter()
            .any(|member| member.user_public_key == bob.user_pk && member.removed),
        "{:?}",
        restored.members
    );
    relay.shutdown().await;
}
