//! A room this relay governs: another person is invited, joins, edits with
//! the room's owner, sees their presence, and is removed again; a device the
//! owner revokes leaves the room on every member, and so does one a
//! collaborator revokes; a relay whose own device is revoked stops governing.

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
use collab_types::CommitNamed as _;
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

/// As [`receive_until`], for a replica that is sent messages it refuses:
/// a refused message changes nothing and is skipped.
async fn receive_refusing_until(
    what: &str,
    session: &RelayCoeditSession,
    peer: &mut StatePeer,
    mut done: impl FnMut(&StatePeer) -> bool,
) {
    tilo_test_env::within(what, async {
        while !done(peer) {
            if let Inbound::Delta { author, data } =
                session.recv().await.expect("the session stays open")
            {
                let message: NeighbourMsg = postcard::from_bytes(&data).unwrap();
                let origin = if author == peer.authority().device_public_key {
                    TransportOrigin::RetainedRelayDelta(author)
                } else {
                    TransportOrigin::RelayDelta(author)
                };
                let _ = peer.receive_from(origin, message, SystemTime::now());
            }
        }
    })
    .await;
}

/// Invite `person` to `room` and redeem the invitation on `device`: the
/// replica its bootstrap gives that device.
async fn join(
    admin: &RoomAdminClient,
    address: &iroh::EndpointAddr,
    room: &str,
    person: &TestUser,
    device: &TestDevice,
    request: u8,
) -> StatePeer {
    let invitation = admin
        .invite(
            address.clone(),
            room,
            MemberRole::Editor,
            Some(person.user_pk),
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
    let bootstrap = WorkspaceJoinClient::new(device_endpoint(device).await)
        .redeem_hint(
            &verified.grant().transport[0],
            invitation.signed_invitation.clone(),
            device.cert.clone(),
            &device.sk,
            [request; 32],
        )
        .await
        .unwrap()
        .bootstrap;
    let mut peer = StatePeer::new(
        &bootstrap.room,
        bootstrap.authority.clone(),
        bootstrap.certified_roster.clone(),
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    peer.select_admission_v5().unwrap();
    for bytes in &bootstrap.governance_messages {
        peer.receive_from(
            TransportOrigin::RetainedRelayDelta(bootstrap.authority.device_public_key),
            postcard::from_bytes(bytes).unwrap(),
            SystemTime::now(),
        )
        .unwrap();
    }
    peer
}

fn revoke(person: &TestUser, device: [u8; 32]) -> tilo_user_identity::RevocationEntry {
    let mut revocation = tilo_user_identity::RevocationEntry {
        user_public_key: person.user_pk,
        revoked_device_public_key: device,
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason: tilo_user_identity::RevocationReason::Lost,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revocation, &person.user_sk).unwrap();
    revocation
}

fn hello(
    device: &TestDevice,
    revocations: Vec<tilo_user_identity::RevocationEntry>,
) -> NeighbourMsg {
    NeighbourMsg::IdentityHello {
        cert: Box::new(device.cert.clone()),
        revocations,
    }
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

fn member_device(peer: &StatePeer, device: [u8; 32]) -> bool {
    peer.governed_access(SystemTime::now())
        .is_ok_and(|access| access.member_devices.contains(&device))
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
    let client = RoomAdminClient::new(device_endpoint(&member).await, member.cert.clone());
    let invocation = uuid::Uuid::new_v4();
    let receipt = terminal_audit(&hex::encode(bob.user_pk), invocation);
    let command = || RoomCommand::RecordEffectReceipt {
        room: room.room.clone(),
        invocation,
        redacted: receipt.clone(),
    };
    assert!(
        client.send(relay.node_addr(), command()).await.is_err(),
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
    let first = client.send(relay.node_addr(), command()).await.unwrap();
    assert!(matches!(first, RoomReply::Governance { .. }));
    assert_eq!(
        client.send(relay.node_addr(), command()).await.unwrap(),
        first,
        "exact retry retains the original generation"
    );
    let changed =
        StatePeer::validate_workspace_effect_receipt_encoding(invocation, &receipt).unwrap();
    let mut changed = changed.clone();
    changed.completed_at = 2;
    assert!(
        client
            .send(
                relay.node_addr(),
                RoomCommand::RecordEffectReceipt {
                    room: room.room.clone(),
                    invocation,
                    redacted: serde_json::to_vec(&changed).unwrap(),
                }
            )
            .await
            .is_err(),
        "conflicting terminal retry must be refused"
    );
    let spoof = uuid::Uuid::new_v4();
    assert!(
        client
            .send(
                relay.node_addr(),
                RoomCommand::RecordEffectReceipt {
                    room: room.room.clone(),
                    invocation: spoof,
                    redacted: terminal_audit(&hex::encode(alice.user_pk), spoof),
                }
            )
            .await
            .is_err(),
        "a member must not impersonate the governor"
    );
    assert!(
        client
            .send(relay.node_addr(), RoomCommand::List)
            .await
            .is_err(),
        "recording personal audit confers no administration"
    );
    let mismatched = RoomAdminClient::new(device_endpoint(&host).await, member.cert.clone());
    assert!(
        mismatched.send(relay.node_addr(), command()).await.is_err(),
        "certificate must equal the authenticated Iroh endpoint"
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
        client
            .send(
                relay.node_addr(),
                RoomCommand::RecordEffectReceipt {
                    room: room.room.clone(),
                    invocation: malformed.invocation.0,
                    redacted: serde_json::to_vec(&malformed).unwrap(),
                }
            )
            .await
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
        client.send(relay.node_addr(), command()).await.is_err(),
        "removal must refuse even a formerly successful retry"
    );
    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_its_person_revokes_leaves_a_governed_room_on_every_member() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let tablet = alice.enrol("alice-tablet", DeviceCapabilities::joined());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let address = relay.node_addr();

    // Alice's laptop creates a room and enters it; her tablet enters too.
    let admin = RoomAdminClient::new(device_endpoint(&laptop).await, laptop.cert.clone());
    let room = admin.create(address.clone(), "Planning").await.unwrap();
    for device in [&laptop, &tablet] {
        let client = RoomAdminClient::new(device_endpoint(device).await, device.cert.clone());
        assert!(matches!(
            client
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
    }

    // Bob is invited and joins as an editor.
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
    let bootstrap = WorkspaceJoinClient::new(device_endpoint(&phone).await)
        .redeem_hint(
            &verified.grant().transport[0],
            invitation.signed_invitation.clone(),
            phone.cert.clone(),
            &phone.sk,
            [8; 32],
        )
        .await
        .unwrap()
        .bootstrap;
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
    let mut presence = Vec::new();

    // Both members see the tablet as a member of the room.
    receive_until(
        "Alice's replica admits her tablet and Bob",
        &alice_session,
        &mut alice_peer,
        &mut presence,
        |peer, _| {
            member_device(peer, tablet.device_pubkey())
                && role(peer, phone.device_pubkey()) == Some(collab_types::Role::Editor)
        },
    )
    .await;
    assert!(member_device(&bob_peer, tablet.device_pubkey()));
    assert_eq!(
        role(&bob_peer, tablet.device_pubkey()),
        Some(collab_types::Role::Owner)
    );
    crate::common::wait_until_subscribed(&relay, room.workspace_id, &phone.device_pubkey()).await;

    // A record both members hold, and the tablet's copy of the room as it
    // stood before it was revoked.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "from Alice").unwrap();
    doc.commit_edit("fixture", "topic");
    let message = alice_peer
        .publish_incremental(APP, "", doc.save_after(&[]), &laptop.sk, SystemTime::now())
        .unwrap();
    publish(&alice_session, &message);
    receive_until(
        "Alice's record in Bob's replica",
        &bob_session,
        &mut bob_peer,
        &mut presence,
        |peer, _| text(peer, "topic").as_deref() == Some("from Alice"),
    )
    .await;
    let mut tablet_copy = bob_peer.clone();

    // Alice revokes the tablet; the relay learns it from its revocation log.
    let mut revocation = tilo_user_identity::RevocationEntry {
        user_public_key: alice.user_pk,
        revoked_device_public_key: tablet.device_pubkey(),
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason: tilo_user_identity::RevocationReason::Lost,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revocation, &alice.user_sk).unwrap();
    tilo_relay::store::save_revocations(state.path(), std::slice::from_ref(&revocation)).unwrap();

    // Every member's replica drops the tablet from the room's membership.
    for (what, session, peer) in [
        (
            "Alice's replica drops the revoked tablet",
            &alice_session,
            &mut alice_peer,
        ),
        (
            "Bob's replica drops the revoked tablet",
            &bob_session,
            &mut bob_peer,
        ),
    ] {
        receive_until(what, session, peer, &mut presence, |peer, _| {
            !member_device(peer, tablet.device_pubkey())
        })
        .await;
        assert!(peer.revocations().contains(&revocation), "{what}");
        assert_eq!(role(peer, tablet.device_pubkey()), None, "{what}");
        assert!(
            member_device(peer, laptop.device_pubkey()),
            "Alice's laptop stays"
        );
        assert_eq!(
            role(peer, phone.device_pubkey()),
            Some(collab_types::Role::Editor),
            "Bob stays"
        );
    }

    // A write the tablet signs from its stale copy is refused.
    let forged = write(&mut tablet_copy, "topic", "from the tablet", &tablet.sk);
    assert!(
        bob_peer
            .receive_from(
                TransportOrigin::RelayDelta(tablet.device_pubkey()),
                forged,
                SystemTime::now(),
            )
            .is_err(),
        "a revoked device's write is refused"
    );
    assert_eq!(text(&bob_peer, "topic").as_deref(), Some("from Alice"));

    // The revoked tablet can neither enter again nor return in a new room.
    let refused = RoomAdminClient::new(device_endpoint(&tablet).await, tablet.cert.clone());
    assert!(
        refused
            .send(
                address.clone(),
                RoomCommand::Enter {
                    room: room.room.clone()
                }
            )
            .await
            .is_err()
    );
    let later = admin.create(address.clone(), "Later").await.unwrap();
    admin
        .send(
            address.clone(),
            RoomCommand::Enter {
                room: later.room.clone(),
            },
        )
        .await
        .unwrap();
    let mut later_peer = StatePeer::new(
        &later.room,
        host.cert.clone(),
        vec![host.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    let later_session = session(&relay, &laptop, later.workspace_id).await;
    receive_until(
        "a room created after the revocation carries it",
        &later_session,
        &mut later_peer,
        &mut presence,
        |peer, _| peer.revocations().contains(&revocation),
    )
    .await;
    later_session.close().await;

    // The room keeps the revocation across a restart of its relay.
    alice_session.close().await;
    bob_session.close().await;
    relay.shutdown().await;
    let relay = start(state.path(), &alice, &host).await;
    let restored = relay.rooms().unwrap().list().await;
    assert_eq!(restored.len(), 2);
    let mut fresh = StatePeer::new(
        &room.room,
        host.cert.clone(),
        vec![host.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    let laptop_session = session(&relay, &laptop, room.workspace_id).await;
    receive_until(
        "a fresh replica learns the revocation from the room's history",
        &laptop_session,
        &mut fresh,
        &mut presence,
        |peer, _| peer.revocations().contains(&revocation),
    )
    .await;
    assert!(!member_device(&fresh, tablet.device_pubkey()));
    assert!(member_device(&fresh, laptop.device_pubkey()));
    laptop_session.close().await;
    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_its_collaborator_revokes_leaves_the_room_on_every_member() {
    let alice = TestUser::fresh("Alice");
    let carol = TestUser::fresh("Carol");
    let dave = TestUser::fresh("Dave");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = carol.enrol("carol-phone", DeviceCapabilities::member());
    let tablet = carol.enrol("carol-tablet", DeviceCapabilities::member());
    let stranger = dave.enrol("dave-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let address = relay.node_addr();

    // Alice's relay governs the room; Carol joins it with two devices.
    let admin = RoomAdminClient::new(device_endpoint(&laptop).await, laptop.cert.clone());
    let room = admin.create(address.clone(), "Studio").await.unwrap();
    admin
        .send(
            address.clone(),
            RoomCommand::Enter {
                room: room.room.clone(),
            },
        )
        .await
        .unwrap();
    let mut carol_peer = join(&admin, &address, &room.room, &carol, &phone, 3).await;
    join(&admin, &address, &room.room, &carol, &tablet, 4).await;
    let mut alice_peer = StatePeer::new(
        &room.room,
        host.cert.clone(),
        vec![host.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    let alice_session = session(&relay, &laptop, room.workspace_id).await;
    let carol_session = session(&relay, &phone, room.workspace_id).await;
    receive_refusing_until(
        "Alice's replica admits both of Carol's devices",
        &alice_session,
        &mut alice_peer,
        |peer| {
            member_device(peer, phone.device_pubkey())
                && member_device(peer, tablet.device_pubkey())
        },
    )
    .await;
    receive_refusing_until(
        "Carol's replica admits her tablet",
        &carol_session,
        &mut carol_peer,
        |peer| member_device(peer, tablet.device_pubkey()),
    )
    .await;

    // What the governor must not publish: a forged revocation, one signed
    // by an identity that is not in the room, Carol's revocation of a key
    // that is not hers, here Alice's phone before it enters, and of a key
    // of hers the room does not know, carried by a member.
    let alice_phone = alice.enrol("alice-phone", DeviceCapabilities::joined());
    let mut forged = revoke(&carol, tablet.device_pubkey());
    forged.signature = [7; 64];
    let foreign = revoke(&dave, stranger.device_pubkey());
    let overreach = revoke(&carol, alice_phone.device_pubkey());
    let unknown = revoke(&carol, [42; 32]);
    publish(&carol_session, &hello(&phone, vec![forged]));
    publish(&carol_session, &hello(&phone, vec![foreign.clone()]));
    publish(
        &carol_session,
        &hello(&phone, vec![overreach.clone(), unknown.clone()]),
    );

    // Carol revokes her tablet; her phone says so as it says who it is.
    let revocation = revoke(&carol, tablet.device_pubkey());
    publish(&carol_session, &hello(&phone, vec![revocation.clone()]));

    // Every member's replica takes the revocation from the room's
    // governance and drops the tablet; Carol's phone and Alice stay.
    for (what, session, peer) in [
        ("Alice's replica", &alice_session, &mut alice_peer),
        ("Carol's replica", &carol_session, &mut carol_peer),
    ] {
        receive_refusing_until(what, session, peer, |peer| {
            peer.governance_revocations().contains(&revocation)
        })
        .await;
        assert!(!member_device(peer, tablet.device_pubkey()), "{what}");
        assert!(member_device(peer, phone.device_pubkey()), "{what}");
        assert!(member_device(peer, laptop.device_pubkey()), "{what}");
        assert_eq!(
            peer.governance_revocations(),
            vec![revocation.clone()],
            "{what}: neither the forged nor the foreign revocation is published"
        );
    }
    assert!(
        !alice_peer.governance_revocations().contains(&foreign),
        "a non-member identity's revocation is not published"
    );

    // Alice's phone, which Carol tried to revoke, still enters the room.
    let phone_admin = RoomAdminClient::new(
        device_endpoint(&alice_phone).await,
        alice_phone.cert.clone(),
    );
    assert!(matches!(
        phone_admin
            .send(
                address.clone(),
                RoomCommand::Enter {
                    room: room.room.clone(),
                },
            )
            .await
            .unwrap(),
        RoomReply::Entered { changed: true }
    ));

    // Carol's root revoking a key of Alice's counts for no one: a replica
    // that holds Carol's validly signed revocation of Alice's phone still
    // counts the phone a member and takes its writes.
    alice_peer
        .receive_from(
            TransportOrigin::RelayDelta(phone.device_pubkey()),
            hello(&phone, vec![overreach.clone()]),
            SystemTime::now(),
        )
        .unwrap();
    assert!(alice_peer.revocations().contains(&overreach));
    receive_refusing_until(
        "Alice's replica admits her phone",
        &alice_session,
        &mut alice_peer,
        |peer| member_device(peer, alice_phone.device_pubkey()),
    )
    .await;
    assert_eq!(
        role(&alice_peer, alice_phone.device_pubkey()),
        Some(collab_types::Role::Owner)
    );
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "from Alice's phone").unwrap();
    doc.commit_edit("fixture", "topic");
    let mut phone_copy = alice_peer.clone();
    let write = phone_copy
        .publish_incremental(
            APP,
            "",
            doc.save_after(&[]),
            &alice_phone.sk,
            SystemTime::now(),
        )
        .unwrap();
    let taken = alice_peer
        .receive_from(
            TransportOrigin::RelayDelta(alice_phone.device_pubkey()),
            write,
            SystemTime::now(),
        )
        .unwrap();
    assert_eq!(taken.rejected, 0, "Alice's phone's write is taken");

    // The relay no longer admits the tablet to the room.
    let refused = async {
        let client = RelayClient::connect_to_pinned_host(
            device_endpoint(&tablet).await,
            relay.node_addr(),
            tablet.cert.clone(),
            tablet.sk.clone(),
            RevocationList::new(),
        )
        .await
        .ok()?;
        client
            .send(Envelope::Subscribe {
                workspace_id: room.workspace_id,
            })
            .ok()?;
        tilo_test_env::within("the relay answers a revoked member", client.recv()).await
    }
    .await;
    assert!(
        !matches!(refused, Some(Envelope::Ready { .. })),
        "a revoked collaborator device is not admitted: {refused:?}"
    );
    alice_session.close().await;
    carol_session.close().await;
    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relay_whose_device_is_revoked_stops_governing() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let tablet = alice.enrol("alice-tablet", DeviceCapabilities::joined());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let address = relay.node_addr();
    let admin = RoomAdminClient::new(device_endpoint(&laptop).await, laptop.cert.clone());
    let room = admin.create(address.clone(), "Ledger").await.unwrap();
    admin
        .send(
            address.clone(),
            RoomCommand::Enter {
                room: room.room.clone(),
            },
        )
        .await
        .unwrap();
    let mut bob_peer = join(&admin, &address, &room.room, &bob, &phone, 5).await;
    assert_eq!(
        role(&bob_peer, phone.device_pubkey()),
        Some(collab_types::Role::Editor)
    );

    // What the relay's key would publish to remove Bob, signed before and
    // sent after the revocation.
    let mut governor = bob_peer.clone();
    governor
        .remove_member(bob.user_pk, &host.sk, SystemTime::now())
        .unwrap();
    let removal = governor
        .durable_mutation_messages(&bob_peer, &host.sk, SystemTime::now(), 1_000)
        .unwrap();

    // Alice revokes her relay's device; the relay learns it from its log.
    let revocation = revoke(&alice, host.device_pubkey());
    tilo_relay::store::save_revocations(state.path(), std::slice::from_ref(&revocation)).unwrap();

    // It no longer governs: it neither admits nor changes anything.
    let tablet_admin = RoomAdminClient::new(device_endpoint(&tablet).await, tablet.cert.clone());
    let enter = async |address: iroh::EndpointAddr| {
        tablet_admin
            .send(
                address,
                RoomCommand::Enter {
                    room: room.room.clone(),
                },
            )
            .await
    };
    tilo_test_env::within("the relay refuses to admit once revoked", async {
        while enter(address.clone()).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        admin
            .send(
                address.clone(),
                RoomCommand::Remove {
                    room: room.room.clone(),
                    request: RemoveMemberRequest {
                        request_id: [10; 32],
                        user_public_key: bob.user_pk,
                    },
                },
            )
            .await
            .is_err(),
        "a revoked governor changes no governance"
    );
    assert!(
        admin
            .invite(
                address.clone(),
                &room.room,
                MemberRole::Viewer,
                None,
                Duration::from_secs(600),
            )
            .await
            .is_err(),
        "a revoked governor issues no invitations"
    );
    assert!(
        admin.create(address.clone(), "After").await.is_err(),
        "a revoked governor creates no rooms"
    );

    // Bob learns the revocation from Alice's laptop, as members do, and
    // refuses what the revoked relay signs from then on.
    bob_peer
        .receive_from(
            TransportOrigin::RelayDelta(laptop.device_pubkey()),
            hello(&laptop, vec![revocation]),
            SystemTime::now(),
        )
        .unwrap();
    for message in removal {
        let _ = bob_peer.receive_from(
            TransportOrigin::RetainedRelayDelta(host.device_pubkey()),
            message,
            SystemTime::now(),
        );
    }
    assert_eq!(
        role(&bob_peer, phone.device_pubkey()),
        Some(collab_types::Role::Editor),
        "Bob's replica refuses the revoked governor's removal"
    );

    // Restarted, the relay still hosts the room without governing it.
    relay.shutdown().await;
    let relay = start(state.path(), &alice, &host).await;
    assert_eq!(relay.rooms().unwrap().list().await.len(), 1);
    assert!(enter(relay.node_addr()).await.is_err());
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
            MemberRole::Commenter,
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

#[path = "governed_rooms/topology_namespace.rs"]
mod topology_namespace;
