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
    RECORD_EFFECT_RECEIPT, RemoveMemberRequest, RoomAdminClient, RoomListing, member_resource,
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
/// as its owner, through the relay's own host actions on a workspace she
/// holds there.
async fn create_and_enter(
    relay: &Relay,
    host: &TestDevice,
    device: &TestDevice,
    name: &str,
) -> RoomListing {
    use governance_actions::{HOME_WORKSPACE, host_action, subscribed};
    let home = subscribed(relay, device, HOME_WORKSPACE).await;
    let created: CreatedRoom = host_action(
        &home,
        host,
        HOME_WORKSPACE,
        CREATE_ROOM,
        serde_json::json!({ "name": name }),
    )
    .await;
    assert_eq!(created.room.name, name);
    let entered: EnteredRoom = host_action(
        &home,
        host,
        HOME_WORKSPACE,
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
    let address = relay.node_addr();

    // Alice's laptop creates a room on her relay and enters it as its owner.
    // (That Bob's device cannot is `governance_actions`'s to show.)
    let room = create_and_enter(&relay, &host, &laptop, "Design review").await;

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

    // Alice removes Bob: the relay stops admitting his device. Removal is
    // still asked at the administration door.
    let admin = RoomAdminClient::new(device_endpoint(&laptop).await, laptop.cert.clone());
    let generation = admin
        .remove(
            address.clone(),
            &room.room,
            RemoveMemberRequest {
                request_id: [9; 32],
                user_public_key: bob.user_pk,
            },
        )
        .await
        .unwrap();
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

fn terminal_audit(actor: &str, invocation: uuid::Uuid) -> Vec<u8> {
    let mut context =
        tilo_actions::ActionContext::for_resource("workspace", "workspace", "Private fixture");
    context.facets = vec![
        tilo_actions::ActionFacet::new("tilo.receipt", "effect"),
        tilo_actions::ActionFacet::new("tilo.invocation", invocation.to_string()),
    ];
    format!(
        "{{\"invocation\":{},\"actor\":{},\"context\":{},\"status\":{},\"completed_at\":1}}",
        serde_json::to_string(&invocation).unwrap(),
        serde_json::to_string(actor).unwrap(),
        serde_json::to_string(&context).unwrap(),
        serde_json::to_string(&tilo_actions::EffectReceiptStatus::Succeeded).unwrap(),
    )
    .into_bytes()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn member_terminal_audit_uses_exact_authenticated_current_admission() {
    let alice = TestUser::fresh("Fixture owner");
    let bob = TestUser::fresh("Fixture member");
    let host = alice.enrol("private-host", DeviceCapabilities::host());
    let member = bob.enrol("private-member", DeviceCapabilities::member());
    let scratch = tempfile::tempdir().unwrap();
    let relay = Relay::start_with_relay_mode(
        RelayConfig {
            state_root: scratch.path().into(),
            iroh_listen: "127.0.0.1:0".parse().unwrap(),
            ..RelayConfig::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: alice.user_pk,
            device_signing_key: host.sk.clone(),
            device_cert: host.cert.clone(),
            revocations: RevocationList::new(),
        },
        iroh::RelayMode::Disabled,
    )
    .await
    .unwrap();
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Private terminal audit").await.unwrap();
    let workspace = room.workspace_id;
    let invocation = uuid::Uuid::new_v4();
    let receipt = terminal_audit(&hex::encode(bob.user_pk), invocation);
    // The member's own `tilo.member` action, recording `redacted` as the
    // receipt of `invocation`.
    let record = |invocation: uuid::Uuid, redacted: &[u8]| {
        governance_actions::invocation(
            tilo_relay::rooms::MEMBER_ACTIONS_PROVIDER,
            member_resource(&room.room),
            RECORD_EFFECT_RECEIPT,
            serde_json::json!({
                "invocation": invocation.to_string(),
                "redacted": String::from_utf8(redacted.to_vec()).unwrap(),
            }),
        )
    };
    assert!(
        !governance_actions::admitted(&relay, &member, workspace).await,
        "unadmitted device must be refused"
    );
    let issued = rooms
        .issue(
            &room.room,
            &host.cert,
            tilo_relay::rooms::IssueInvitationRequest {
                request_id: [61; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(bob.user_pk),
                invitee_name: None,
                inviter_name: None,
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(60_000_000_000)
                    .unwrap(),
            },
        )
        .await
        .unwrap();
    let invitation = decode_and_verify_invitation(
        &issued.signed_invitation,
        Some(bob.user_pk),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    WorkspaceJoinClient::new(device_endpoint(&member).await)
        .redeem_hint(
            &invitation.grant().transport[0],
            issued.signed_invitation,
            member.cert.clone(),
            &member.sk,
            [62; 32],
        )
        .await
        .unwrap();
    // The caller is the device the relay session authenticated: there is no
    // certificate to name besides it.
    let client = governance_actions::subscribed(&relay, &member, workspace).await;
    let ask = |message| governance_actions::ask(&client, &host, workspace, message);
    let first = governance_actions::outcome(ask(record(invocation, &receipt)).await).unwrap();
    assert!(first["generation"].as_u64().is_some(), "{first}");
    assert_eq!(
        governance_actions::outcome(ask(record(invocation, &receipt)).await).unwrap(),
        first,
        "exact retry retains the original generation"
    );
    let changed = tilo_peer_state::PeerWorkspace::validate_workspace_effect_receipt_encoding(
        invocation, &receipt,
    )
    .unwrap();
    let mut changed = changed.clone();
    changed.completed_at = 2;
    assert!(
        governance_actions::outcome(
            ask(record(invocation, &serde_json::to_vec(&changed).unwrap())).await
        )
        .is_err(),
        "conflicting terminal retry must be refused"
    );
    let spoof = uuid::Uuid::new_v4();
    assert!(
        governance_actions::outcome(
            ask(record(
                spoof,
                &terminal_audit(&hex::encode(alice.user_pk), spoof)
            ))
            .await
        )
        .is_err(),
        "a member must not impersonate the governor"
    );
    assert_eq!(
        governance_actions::outcome(
            ask(governance_actions::host_operation(
                tilo_relay::rooms::LIST_ROOMS,
                serde_json::json!({})
            ))
            .await
        ),
        Err(tilo_actions::remote::ActionHostError::UnknownAction),
        "recording personal audit confers no administration"
    );
    let mut malformed = changed;
    malformed.invocation = tilo_actions::InvocationId::new();
    malformed
        .context
        .facets
        .push(tilo_actions::ActionFacet::new(
            "private.arguments",
            "must not publish",
        ));
    assert!(
        governance_actions::outcome(
            ask(record(
                malformed.invocation.0,
                &serde_json::to_vec(&malformed).unwrap()
            ))
            .await
        )
        .is_err(),
        "unredacted extra facets must be refused"
    );
    rooms
        .remove_member(
            &room.room,
            &host.cert,
            RemoveMemberRequest {
                request_id: [63; 32],
                user_public_key: bob.user_pk,
            },
        )
        .await
        .unwrap();
    assert!(
        !matches!(
            governance_actions::try_ask(&client, &host, workspace, record(invocation, &receipt))
                .await
                .map(governance_actions::outcome),
            Ok(Ok(_))
        ),
        "removal must refuse even a formerly successful retry"
    );
    drop(client);
    assert!(
        !governance_actions::admitted(&relay, &member, workspace).await,
        "a removed member is not admitted again"
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
    let hash = *blake3::hash(SURFACE.as_bytes()).as_bytes();
    let hex = hex::encode(hash);
    tilo_peer_state::decide_with_surfaces(
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
    let room = create_and_enter(&relay, &host, &laptop, "Design review").await;

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
        .publish_incremental(APP, "", doc.save_after(&[]), &laptop.sk, SystemTime::now())
        .unwrap();
    publish(&alice_session, &smuggled);
    // An edit of a field the app publishes reaches Bob.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "from Alice").unwrap();
    doc.commit_edit("fixture", "topic");
    let message = alice_peer
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
    let hash = *blake3::hash(SURFACE.as_bytes()).as_bytes();
    let hex = hex::encode(hash);
    tilo_peer_state::decide_with_surfaces(
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
    let room = create_and_enter(&relay, &host, &laptop, "Design review").await;

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
    let room = create_and_enter(&relay, &host, &laptop, "Demo").await;
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

#[path = "governed_rooms/topology_namespace.rs"]
mod topology_namespace;

#[path = "governed_rooms/governance_actions.rs"]
mod governance_actions;

#[path = "governed_rooms/spectators.rs"]
mod spectators;

#[path = "governed_rooms/app_authorship.rs"]
mod app_authorship;

#[path = "governed_rooms/app_records.rs"]
mod app_records;

#[path = "governed_rooms/sealed_records.rs"]
mod sealed_records;

#[path = "governed_rooms/shared_desktop.rs"]
mod shared_desktop;
