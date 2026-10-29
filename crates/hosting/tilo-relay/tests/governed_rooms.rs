//! A room this relay governs: another person is invited, joins, edits with
//! the room's owner, sees their presence, and is removed again.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "one end-to-end scenario whose relays and sessions live until it ends"
)]

use std::time::SystemTime;

use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use collab_types::CommitNamed as _;
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::join::{MemberRole, decode_and_verify_invitation};
use tilo_peer_state::{StatePeer, TransportOrigin};
use tilo_relay::coedit::{Inbound, RelayCoeditSession};
use tilo_relay::rooms::{
    CREATE_ROOM, CreatedRoom, ENTER_ROOM, EnteredRoom, IssueInvitationRequest, IssuedInvitation,
    RemoveMemberRequest, RoomListing,
};
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

/// Alice's `device` creates a room named `name` on her relay and enters it
/// as its owner, through the relay's own host actions asked of it directly.
async fn create_and_enter(relay: &Relay, device: &TestDevice, name: &str) -> RoomListing {
    use governance_actions::{connected, host_action};
    let home = connected(relay, device).await;
    let created: CreatedRoom =
        host_action(&home, CREATE_ROOM, serde_json::json!({ "name": name })).await;
    assert_eq!(created.room.name, name);
    let entered: EnteredRoom = host_action(
        &home,
        ENTER_ROOM,
        serde_json::json!({ "room": created.room.room }),
    )
    .await;
    assert!(
        entered.changed,
        "the device is admitted as the room's owner's"
    );
    created.room
}

/// An invitation to `room` carrying `role`, for `person` only, that the
/// room's owner `issuer` issues.
async fn invite(
    relay: &Relay,
    issuer: &TestDevice,
    room: &str,
    role: MemberRole,
    person: [u8; 32],
) -> IssuedInvitation {
    relay
        .rooms()
        .unwrap()
        .issue(
            room,
            &issuer.cert,
            IssueInvitationRequest {
                request_id: *blake3::hash(&person).as_bytes(),
                role,
                recipient_user_public_key: Some(person),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(600_000_000_000)
                    .unwrap(),
                inviter_name: None,
            },
        )
        .await
        .unwrap()
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
    let (channel, kind) = neighbour_wire::retention(message, false).unwrap();
    let body = postcard::to_stdvec(message).unwrap();
    if kind == neighbour_wire::Retention::Checkpoint {
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
    doc.commit_edit("fixture", key);
    peer.publication_mut()
        .publish_incremental(APP, "", doc.save_after(&heads), key_pair, SystemTime::now())
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
        .map(|member| member.role)
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

    // Alice's laptop creates a room on her relay and enters it as its owner.
    // (That Bob's device cannot is `governance_actions`'s to show.)
    let room = create_and_enter(&relay, &laptop, "Design review").await;

    // Alice invites Bob's person; his device redeems through the signed
    // address the invitation carries and is admitted by the room's governor.
    let invitation = invite(&relay, &laptop, &room.room, MemberRole::Editor, bob.user_pk).await;
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
    doc.commit_edit("fixture", "topic");
    let message = alice_peer
        .publication_mut()
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
    doc.commit_edit("fixture", "reply");
    let message = bob_peer
        .publication_mut()
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
    let generation = relay
        .rooms()
        .unwrap()
        .remove_member(
            &room.room,
            &laptop.cert,
            RemoveMemberRequest {
                request_id: [9; 32],
                user_public_key: bob.user_pk,
            },
        )
        .await
        .unwrap()
        .generation;
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

/// The relay keeps and forwards only changes that name an action their app
/// publishes, decided against the app's surface the op names; a change
/// claiming anything else never reaches the room.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_relay_forwards_only_changes_their_app_surface_publishes() {
    const SURFACE: &str = r#"
[[resources]]
kind = "fixture"
label = "Fixture"
version = 1

[[resources.fields]]
owner = "fixture"
path = "topic"
stored = "value"
title = "Topic"
type = "String"
"#;
    // The surfaces it installs are the process's: no other test runs here.
    if !crate::common::in_own_process(
        "governed_rooms::the_relay_forwards_only_changes_their_app_surface_publishes",
    ) {
        return;
    }
    let hash = *blake3::hash(SURFACE.as_bytes()).as_bytes();
    let hex = hex::encode(hash);
    crate::common::install_process_surfaces(
        collab_types::Surfaces::from_source(std::sync::Arc::new(
            collab_types::surface::ContentSurfaces::new(move |_, wanted| {
                (wanted == hex).then(|| SURFACE.as_bytes().to_vec())
            }),
        )),
        move |_| Some(hash),
    );
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;

    // Alice's laptop creates a room on her relay and enters it as its owner.
    // (That Bob's device cannot is `governance_actions`'s to show.)
    let room = create_and_enter(&relay, &laptop, "Design review").await;

    // Alice invites Bob's person; his device redeems through the signed
    // address the invitation carries and is admitted by the room's governor.
    let invitation = invite(&relay, &laptop, &room.room, MemberRole::Editor, bob.user_pk).await;
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

    // A change claiming a verb the app does not publish: refused by the
    // relay, so it never reaches Bob (whose replica would refuse it loudly).
    // It goes first on the one connection, so Bob would see it first.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "smuggled", "from Alice").unwrap();
    let context = tilo_actions::ActionContext {
        what: Some(tilo_actions::Did::Verb(tilo_actions::VerbCall::new(
            tilo_actions::ActionKey::new(APP, "fixture", "explode"),
            [],
        ))),
        ..tilo_actions::ActionContext::for_resource("fixture", "fixture", "Fixture")
    };
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(context.change_message().unwrap()),
    );
    // Signed by a device that does not check its own changes.
    let mut unchecked = alice_peer.clone();
    unchecked.set_surfaces(collab_types::Surfaces::default());
    let smuggled = unchecked
        .publication_mut()
        .publish_incremental(APP, "", doc.save_after(&[]), &laptop.sk, SystemTime::now())
        .unwrap();
    publish(&alice_session, &smuggled);
    // An edit of a field the app publishes reaches Bob.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "from Alice").unwrap();
    doc.commit_edit("fixture", "topic");
    let message = alice_peer
        .publication_mut()
        .publish_incremental(APP, "", doc.save_after(&[]), &laptop.sk, SystemTime::now())
        .unwrap();
    publish(&alice_session, &message);
    receive_until(
        "Alice's published edit in Bob's replica",
        &bob_session,
        &mut bob_peer,
        &mut bob_presence,
        |peer, _| text(peer, "topic").is_some() || text(peer, "smuggled").is_some(),
    )
    .await;
    assert_eq!(
        text(&bob_peer, "smuggled"),
        None,
        "the relay forwarded a refused change"
    );
    relay.shutdown().await;
}
/// The relay decides each change against its author's grants, as the room's
/// governor projects them: a commenter's comment reaches the room, their
/// edit of the record does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_relay_refuses_an_action_the_author_is_not_granted() {
    const SURFACE: &str = r#"
[[resources]]
kind = "fixture"
label = "Fixture"
version = 1

[[resources.fields]]
owner = "fixture"
path = "topic"
stored = "value"
title = "Topic"
type = "String"

[[resources.verbs]]
id = "comment"
kind = "undoable"
label = "Comment"
field = "comments"
params = []
"#;
    // The surfaces it installs are the process's: no other test runs here.
    if !crate::common::in_own_process(
        "governed_rooms::the_relay_refuses_an_action_the_author_is_not_granted",
    ) {
        return;
    }
    let hash = *blake3::hash(SURFACE.as_bytes()).as_bytes();
    let hex = hex::encode(hash);
    crate::common::install_process_surfaces(
        collab_types::Surfaces::from_source(std::sync::Arc::new(
            collab_types::surface::ContentSurfaces::new(move |_, wanted| {
                (wanted == hex).then(|| SURFACE.as_bytes().to_vec())
            }),
        )),
        move |_| Some(hash),
    );
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;

    // Alice's laptop creates a room on her relay and enters it as its owner.
    // (That Bob's device cannot is `governance_actions`'s to show.)
    let room = create_and_enter(&relay, &laptop, "Design review").await;

    // Alice invites Bob's person; his device redeems through the signed
    // address the invitation carries and is admitted by the room's governor.
    let invitation = invite(
        &relay,
        &laptop,
        &room.room,
        MemberRole::Commenter,
        bob.user_pk,
    )
    .await;
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
    assert_eq!(bootstrap.role, MemberRole::Commenter);

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
        Some(collab_types::Role::Commenter)
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
    let mut bob_presence: Vec<([u8; 32], Vec<u8>)> = Vec::new();
    receive_until(
        "Alice's replica admits both people",
        &alice_session,
        &mut alice_peer,
        &mut alice_presence,
        |peer, _| {
            role(peer, laptop.device_pubkey()) == Some(collab_types::Role::Owner)
                && role(peer, phone.device_pubkey()) == Some(collab_types::Role::Commenter)
        },
    )
    .await;
    crate::common::wait_until_subscribed(&relay, room.workspace_id, &phone.device_pubkey()).await;

    // Alice's chat: the record exists before Bob writes to it.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "from Alice").unwrap();
    doc.commit_edit("fixture", "topic");
    let created = alice_peer
        .publication_mut()
        .publish_incremental(APP, "", doc.save_after(&[]), &laptop.sk, SystemTime::now())
        .unwrap();
    publish(&alice_session, &created);
    receive_until(
        "Alice's chat in Bob's replica",
        &bob_session,
        &mut bob_peer,
        &mut bob_presence,
        |peer, _| text(peer, "topic").is_some(),
    )
    .await;
    // Signed as Bob's device signs, without his own replica checking it.
    let sign = |doc: &mut AutoCommit, seq: u64| {
        let mutation = collab_types::DocumentMutation::Incremental(doc.save_after(&[]));
        let author = collab_types::Ed25519PublicKey::from_bytes(phone.device_pubkey());
        let mut op = collab_types::SignedOp::new_unsigned(
            mutation.encode().unwrap(),
            collab_types::document_artifact_id(APP, &room.room, ""),
            author,
            collab_types::document_attestation_id(APP, &room.room, ""),
            collab_types::OpKind::Change,
            SystemTime::now(),
            seq,
        )
        .with_surface(Some(hash));
        collab_types::sign_op(&mut op, &phone.sk).unwrap();
        NeighbourMsg::AppDocument {
            peer: tilo_peer_state::peer_handle(author.as_bytes()),
            app_id: APP.into(),
            cluster_id: room.room.clone(),
            record_id: String::new(),
            envelope: Box::new(op),
        }
    };
    // An edit of the topic: published by the app, not granted to a commenter.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "from Bob").unwrap();
    doc.commit_edit("fixture", "topic");
    publish(&bob_session, &sign(&mut doc, 1_000));
    // A comment: theirs to make.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "comment", "looks good").unwrap();
    let context = tilo_actions::ActionContext {
        what: Some(tilo_actions::Did::Verb(tilo_actions::VerbCall::new(
            tilo_actions::ActionKey::new(APP, "fixture", "comment"),
            [],
        ))),
        ..tilo_actions::ActionContext::for_resource("fixture", "fixture", "Fixture")
    };
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(context.change_message().unwrap()),
    );
    publish(&bob_session, &sign(&mut doc, 1_001));
    receive_until(
        "Bob's comment in Alice's replica",
        &alice_session,
        &mut alice_peer,
        &mut alice_presence,
        |peer, _| text(peer, "comment").is_some(),
    )
    .await;
    assert_eq!(
        text(&alice_peer, "topic").as_deref(),
        Some("from Alice"),
        "the relay forwarded an ungranted edit"
    );
    relay.shutdown().await;
}

/// A public room prunes the guests who left in one governance change, and
/// what it kept about how they joined goes with them: their invitations
/// admit nobody again, and the guest who stayed keeps their seat.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pruning_removes_idle_guests_in_one_change_and_their_invitations_admit_nobody() {
    let alice = TestUser::fresh("Alice");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let room = create_and_enter(&relay, &laptop, "Demo").await;
    let mut guests = Vec::new();
    for (seed, name) in [(17, "Ada"), (18, "Bea"), (19, "Cy")] {
        let guest = TestUser::fresh(name);
        let phone = guest.enrol("phone", DeviceCapabilities::member());
        let invitation = invite(
            &relay,
            &laptop,
            &room.room,
            MemberRole::Editor,
            guest.user_pk,
        )
        .await;
        let verified = decode_and_verify_invitation(
            &invitation.signed_invitation,
            None,
            &RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .unwrap();
        WorkspaceJoinClient::new(device_endpoint(&phone).await)
            .redeem_hint(
                &verified.grant().transport[0],
                invitation.signed_invitation.clone(),
                phone.cert.clone(),
                &phone.sk,
                [seed; 32],
            )
            .await
            .unwrap();
        guests.push((guest, phone, verified, invitation));
    }
    let rooms = relay.rooms().unwrap();
    let before = rooms.summary(&room.room).await.unwrap().generation;
    let idle = std::collections::BTreeSet::from([guests[0].0.user_pk, guests[1].0.user_pk]);
    let pruned = rooms
        .prune_members(&room.room, &host.cert, [9; 32], &idle)
        .await
        .unwrap();
    assert_eq!(pruned.generation, before + 1, "one change for every guest");

    let members = rooms.summary(&room.room).await.unwrap().members;
    let seated = |person: [u8; 32]| {
        members
            .iter()
            .any(|member| member.user_public_key == person && !member.removed)
    };
    assert!(!seated(guests[0].0.user_pk) && !seated(guests[1].0.user_pk));
    assert!(
        seated(guests[2].0.user_pk),
        "the guest who stayed keeps their seat"
    );

    let (_, phone, verified, invitation) = &guests[0];
    assert!(
        WorkspaceJoinClient::new(device_endpoint(phone).await)
            .redeem_hint(
                &verified.grant().transport[0],
                invitation.signed_invitation.clone(),
                phone.cert.clone(),
                &phone.sk,
                [8; 32],
            )
            .await
            .is_err(),
        "a pruned guest's invitation admits nobody again"
    );
    relay.shutdown().await;
}

#[path = "governed_rooms/members.rs"]
mod members;

#[path = "governed_rooms/governance_actions.rs"]
mod governance_actions;

#[path = "governed_rooms/spectators.rs"]
mod spectators;

#[path = "governed_rooms/app_records.rs"]
mod app_records;

#[path = "governed_rooms/sealed_records.rs"]
mod sealed_records;

#[path = "governed_rooms/shared_desktop.rs"]
mod shared_desktop;

#[path = "governed_rooms/revocations.rs"]
mod revocations;

#[path = "governed_rooms/hard_stops.rs"]
mod hard_stops;

#[path = "governed_rooms/app_push.rs"]
mod app_push;

#[cfg(feature = "agents")]
#[path = "governed_rooms/member_files.rs"]
mod member_files;
