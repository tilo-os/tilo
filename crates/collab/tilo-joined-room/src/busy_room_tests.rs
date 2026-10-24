//! "Joining a busy public room never fails because of how many messages it
//! has": becoming a member takes the room's governance alone, and its app
//! messages reach the room afterwards through its sync, each authorized as
//! any write is.
use crate::testing::{APP, ROOM, certificate};
use crate::{NativeWorkspaceOwner, RoomId, VerifiedNativeEnrollmentIntent, open_file};
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc as _, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use std::time::SystemTime;
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_peer_state::{
    InputDisposition, SeedDocument, SeedState, StatePeer, TransportOrigin, join, seed_room,
};
use tilo_user_identity::{DeviceCert, RevocationList};

/// People who write in the room before the joining device arrives.
const AUTHORS: u8 = 40;

fn ns(now: SystemTime) -> u64 {
    crate::testing::ns(now).unwrap()
}

/// The governor's invitation for `device`, redeemed by it.
fn redemption(
    governor: &DeviceCert,
    governor_key: &SigningKey,
    hint: &join::TransportHint,
    (device, device_key): (&DeviceCert, &SigningKey),
    nonce: u8,
    now: SystemTime,
) -> (
    join::VerifiedJoin,
    join::JoinCompleteRequest,
    join::VerifiedRedemption,
) {
    let signed = join::issue_invitation(
        join::InviteGrant {
            workspace_id: tilo_relay_protocol::room_workspace_id(ROOM).unwrap(),
            room: ROOM.into(),
            authority: governor.clone(),
            issuer: governor.clone(),
            delegation: None,
            role: join::MemberRole::Editor,
            nonce: [nonce; 16],
            recipient_user_public_key: Some(device.user_public_key),
            invitee_name: None,
            expires_at_ns: ns(now).checked_add(60_000_000_000).unwrap(),
            transport: vec![hint.clone()],
        },
        governor_key,
    )
    .unwrap();
    let verified = join::decode_and_verify_invitation(
        &signed,
        Some(device.user_public_key),
        &RevocationList::new(),
        ns(now),
    )
    .unwrap();
    let challenge = join::JoinChallenge::new(
        &verified,
        device,
        [nonce; 32],
        [nonce.wrapping_add(1); 32],
        ns(now).checked_add(30_000_000_000).unwrap(),
        ns(now),
    )
    .unwrap();
    let proof = join::sign_join_proof(challenge.clone(), device_key).unwrap();
    let redeemed =
        join::verify_join_proof(&proof, &challenge, device, &RevocationList::new(), ns(now))
            .unwrap();
    (verified, join::JoinCompleteRequest { proof }, redeemed)
}

/// `author`'s chat message `text`, signed as its write `seq`, after
/// everything `chat` holds.
fn message(
    chat: &mut AutoCommit,
    author: &SigningKey,
    seq: u64,
    text: &str,
    now: SystemTime,
) -> NeighbourMsg {
    use collab_types::{
        DocumentMutation, Ed25519PublicKey, OpKind, SignedOp, document_artifact_id,
        document_attestation_id, sign_op,
    };
    chat.set_actor(automerge::ActorId::from(
        author.verifying_key().to_bytes().to_vec(),
    ));
    let before = chat.get_heads();
    let messages = chat.get(ROOT, "messages").unwrap().unwrap().1;
    chat.put(&messages, text, text).unwrap();
    chat.commit_edit("chat", "messages");
    let mutation = DocumentMutation::Incremental(chat.save_after(&before));
    let author_key = author.verifying_key().to_bytes();
    let mut envelope = SignedOp::new_unsigned(
        mutation.encode().unwrap(),
        document_artifact_id(APP, ROOM, ""),
        Ed25519PublicKey::from_bytes(author_key),
        document_attestation_id(APP, ROOM, ""),
        OpKind::Change,
        now,
        seq,
    );
    sign_op(&mut envelope, author).unwrap();
    NeighbourMsg::AppDocument {
        peer: tilo_peer_state::peer_handle(&author_key),
        app_id: APP.into(),
        cluster_id: ROOM.into(),
        record_id: String::new(),
        envelope: Box::new(envelope),
    }
}

struct BusyRoom {
    intent: VerifiedNativeEnrollmentIntent,
    complete_bootstrap: join::JoinBootstrap,
    /// What the room's sync delivers once it is installed, in the host's
    /// order: the governor's checkpoint of the chat, then every message.
    sync: Vec<([u8; 32], NeighbourMsg)>,
    texts: Vec<String>,
    now: SystemTime,
}

/// A public room whose chat holds `messages_each` messages of each of
/// [`AUTHORS`] people, and whose join bootstrap carries each author's
/// messages ahead of the governance that admits the author.
#[expect(
    clippy::too_many_lines,
    reason = "one linear room history: governance, members, messages and the join"
)]
fn busy_room(messages_each: u64) -> BusyRoom {
    let now = SystemTime::now();
    let root = SigningKey::from_bytes(&[121; 32]);
    let governor_key = SigningKey::from_bytes(&[122; 32]);
    let governor = certificate(&root, &governor_key, "Lounge host");
    let device_key = SigningKey::from_bytes(&[123; 32]);
    let device = certificate(&SigningKey::from_bytes(&[124; 32]), &device_key, "Joining");
    let workspace_id = tilo_relay_protocol::room_workspace_id(ROOM).unwrap();
    let hint = IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
        room: ROOM.into(),
        workspace_id,
        governing_authority: governor.clone(),
        endpoint_addr: iroh_base::EndpointAddr::new(
            iroh_base::EndpointId::from_bytes(&governor.device_public_key).unwrap(),
        ),
        host_user_public_key: governor.user_public_key,
        host_device_certificate: governor.clone(),
        packet_relay_url: "https://relay.invalid".into(),
    }
    .transport_hint(&governor_key)
    .unwrap();
    let mut peer = StatePeer::new(
        ROOM,
        governor.clone(),
        vec![governor.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut chat = AutoCommit::new();
    chat.put_object(ROOT, "messages", ObjType::Map).unwrap();
    chat.commit_edit("chat", "messages");
    peer.install_seed(
        SeedState {
            room_changes: seed_room(ROOM, &governor, std::slice::from_ref(&governor)).unwrap(),
            documents: vec![SeedDocument {
                app_id: APP.into(),
                record_id: String::new(),
                kind: "chat".into(),
                bytes: chat.save(),
            }],
        },
        &governor_key,
        now,
    )
    .unwrap();
    peer.select_admission_v5().unwrap();
    let opening = peer
        .durable_bootstrap_messages(&governor_key, now, 1)
        .unwrap();
    let mut sync = Vec::new();
    let mut bootstrap = Vec::new();
    for message in opening {
        if matches!(message, NeighbourMsg::AppDocument { .. }) {
            sync.push((governor.device_public_key, message.clone()));
        }
        bootstrap.push(message);
    }
    let mut chat = AutoCommit::load(&peer.document(APP, "").unwrap()).unwrap();
    let mut texts = Vec::new();
    for author in 0..AUTHORS {
        let key = SigningKey::from_bytes(&[author.wrapping_add(1); 32]);
        let cert = certificate(
            &SigningKey::from_bytes(&[author.wrapping_add(160); 32]),
            &key,
            "Author",
        );
        for seq in 1..=messages_each {
            let text = format!("author {author} message {seq}");
            let written = message(&mut chat, &key, seq, &text, now);
            sync.push((cert.device_public_key, written.clone()));
            bootstrap.push(written);
            texts.push(text);
        }
        let before = peer.clone();
        let (_, _, redeemed) =
            redemption(&governor, &governor_key, &hint, (&cert, &key), author, now);
        assert!(
            peer.governance_mut()
                .admit_invited_member(&redeemed, &governor_key, now)
                .unwrap()
        );
        let generation = u64::from(author) + 2;
        bootstrap.extend(
            peer.durable_mutation_messages(&before, &governor_key, now, generation)
                .unwrap(),
        );
    }
    let before = peer.clone();
    let (verified, complete, redeemed) = redemption(
        &governor,
        &governor_key,
        &hint,
        (&device, &device_key),
        250,
        now,
    );
    assert!(
        peer.governance_mut()
            .admit_invited_member(&redeemed, &governor_key, now)
            .unwrap()
    );
    bootstrap.extend(
        peer.durable_mutation_messages(&before, &governor_key, now, u64::from(AUTHORS) + 2)
            .unwrap(),
    );
    let complete_bootstrap = join::JoinBootstrap {
        version: 1,
        invite_id: verified.invite_id(),
        device_cert: device.clone(),
        role: join::MemberRole::Editor,
        room: ROOM.into(),
        workspace_id,
        authority: governor.clone(),
        // The authors' certificates reach the device only through the
        // governance that admits them, after their messages.
        certified_roster: vec![governor, device.clone()],
        revocations: Vec::new(),
        records: peer.rows(),
        governance_messages: bootstrap
            .iter()
            .map(|message| postcard::to_stdvec(message).unwrap())
            .collect(),
        generation: u64::from(AUTHORS) + 2,
        hosts: Vec::new(),
    };
    let intent =
        VerifiedNativeEnrollmentIntent::from_proof(verified, complete, device, hint, now).unwrap();
    BusyRoom {
        intent,
        complete_bootstrap,
        sync,
        texts,
        now,
    }
}

fn install(room: BusyRoom) -> (tempfile::TempDir, NativeWorkspaceOwner, BusyRoom) {
    let directory = tempfile::tempdir().unwrap();
    let owner = open_file(&directory.path().join("joined-rooms.json")).unwrap();
    let pending = owner
        .prepare_enrollment_into(&room.intent, None, room.now)
        .unwrap();
    let enrollment = crate::testing::finalize(
        room.intent.clone(),
        pending,
        room.complete_bootstrap.clone(),
    )
    .unwrap();
    owner
        .install(&enrollment, room.now)
        .expect("joining a busy public room never fails because of how many messages it has");
    (directory, owner, room)
}

#[test]
fn joining_a_busy_public_room_never_fails_because_of_how_many_messages_it_has() {
    let room = busy_room(50);
    assert!(
        room.complete_bootstrap.governance_messages.len() > 30 * tilo_peer_state::MAX_HELD_INPUTS,
        "the replay holds far more than the prerequisite queue ever does"
    );
    assert!(
        room.complete_bootstrap.governance_messages.len() > 2000,
        "the bootstrap carries every message"
    );
    let (_directory, owner, room) = install(room);
    let id = RoomId::parse(ROOM).unwrap();
    owner
        .with_current(id, |view| {
            assert_eq!(
                view.peer().pending_count(),
                0,
                "installing held no app message"
            );
            assert!(
                view.peer().document(APP, "").is_none(),
                "app data is not part of becoming a member"
            );
            for (author, _) in &room.sync {
                view.peer()
                    .permissions(*author, room.now)
                    .expect("every author is authorized by the installed governance");
            }
            Ok(())
        })
        .unwrap();
}

/// How many relay inputs of one session the shell admits in one commit.
const BATCH: usize = 256;

#[test]
fn a_busy_rooms_messages_are_authorized_and_visible_once_its_app_syncs() {
    // Still many times what the prerequisite queue holds.
    let (_directory, owner, room) = install(busy_room(20));
    assert!(room.sync.len() > 12 * tilo_peer_state::MAX_HELD_INPUTS);
    let id = RoomId::parse(ROOM).unwrap();
    let inputs = room.sync.len();
    let started = std::time::Instant::now();
    let generation = owner
        .with_current(id, |view| Ok(view.generation()))
        .unwrap();
    let mut sync = room.sync.into_iter().peekable();
    while sync.peek().is_some() {
        let batch = sync
            .by_ref()
            .take(BATCH)
            .map(|(author, message)| (TransportOrigin::RetainedRelayDelta(author), message))
            .collect();
        let reports = owner
            .receive_batch(id, batch, room.now)
            .expect("the batch commits");
        for report in reports {
            let report =
                report.expect("each message is authorized against the installed governance");
            assert!(
                matches!(report.primary(), InputDisposition::Accepted(_)),
                "admitted, not held: {:?}",
                report.primary()
            );
            assert!(report.summary().dropped.is_empty());
        }
    }
    eprintln!("{inputs} relay inputs admitted in {:?}", started.elapsed());
    assert_eq!(
        owner
            .with_current(id, |view| Ok(view.generation()))
            .unwrap(),
        generation + u64::try_from(inputs.div_ceil(BATCH)).unwrap(),
        "a burst persists once per batch, not once per message"
    );
    owner
        .with_current(id, |view| {
            assert_eq!(view.peer().pending_count(), 0);
            let chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
            let messages = chat.get(ROOT, "messages").unwrap().unwrap().1;
            for text in &room.texts {
                assert_eq!(
                    chat.get(&messages, text.as_str())
                        .unwrap()
                        .and_then(|(value, _)| value.into_string().ok())
                        .as_deref(),
                    Some(text.as_str()),
                    "every message is visible"
                );
            }
            Ok(())
        })
        .unwrap();
}
