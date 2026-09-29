use super::*;
use crate::collaboration::{CollaborationService, IssueInvitationRequest};
use channel_crypto::VaultKey;
use collab_types::ContentRetention;
use ed25519_dalek::SigningKey;
use std::time::Instant;
use tilo_action_broker::channel_encryption::ChannelAccess;
use tilo_peer_state::join::{
    JoinChallengeRequest, JoinCompleteRequest, MemberRole, sign_join_proof,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

fn device(key: &SigningKey) -> Ed25519PublicKey {
    Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes())
}

/// Admit a device of a new person to `owner`'s room with `role`, through
/// the invitation service members join with.
fn admit(owner: &mut SessionOwner, seed: u8, role: MemberRole, foreign: bool) -> SigningKey {
    let root = SigningKey::from_bytes(&[seed; 32]);
    let key = SigningKey::from_bytes(&[seed + 1; 32]);
    let now = tilo_delivery_protocol::clock::now_ns();
    let mut capabilities = DeviceCapabilities::member();
    capabilities.foreign = foreign;
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("member {seed}"),
        capabilities,
        issued_at_ns: now - 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let service = CollaborationService::new(vec![]);
    let issued = service
        .issue(
            owner,
            owner.peer().authority().device_public_key,
            IssueInvitationRequest {
                request_id: [seed; 32],
                role,
                recipient_user_public_key: Some(cert.user_public_key),
                invitee_name: None,
                expires_at_ns: now + 120_000_000_000,
            },
            now,
        )
        .unwrap();
    let challenge = service
        .challenge(
            owner,
            cert.device_public_key,
            JoinChallengeRequest {
                signed_invitation: issued.signed_invitation,
                device_cert: cert.clone(),
                request_id: [seed + 1; 32],
            },
            now + 1,
        )
        .unwrap()
        .challenge;
    let proof = sign_join_proof(challenge, &key).unwrap();
    service
        .complete(
            owner,
            cert.device_public_key,
            JoinCompleteRequest { proof },
            now + 2,
        )
        .unwrap();
    key
}

/// One member's view of the room: it asks the governor for access.
struct Member {
    steward: Ed25519PublicKey,
}

impl ChannelDirectory for Member {
    fn governs(&self, _: RoomId) -> bool {
        false
    }
    fn steward_of(&self, _: RoomId) -> Option<Ed25519PublicKey> {
        Some(self.steward)
    }
    fn allowed_devices(&self, _: RoomId, _: u64) -> BTreeSet<Ed25519PublicKey> {
        BTreeSet::new()
    }
}

fn member_service(
    key: &SigningKey,
    root: &std::path::Path,
    steward: Ed25519PublicKey,
) -> ChannelEncryption {
    ChannelEncryption::open(
        ChannelEncryptionConfig {
            root: root.to_path_buf(),
            vault: VaultKey::new([7; 32]),
            device_key: key.clone(),
        },
        Box::new(Member { steward }),
        Arc::new(Mutex::new(BrokerChannels::default())),
    )
    .unwrap()
}

/// The room's relay: what one side publishes, every other side receives.
#[derive(Default)]
struct Relay {
    queues: Mutex<Vec<Vec<InboundChannelGovernance>>>,
}

struct Side<'a> {
    relay: &'a Relay,
    index: usize,
}

impl GovernanceTransport for Side<'_> {
    fn publish(&self, message: NeighbourMsg) -> bool {
        let Some(inbound) = InboundChannelGovernance::from_message(message) else {
            return false;
        };
        for (index, queue) in self.relay.queues.lock().unwrap().iter_mut().enumerate() {
            if index != self.index {
                queue.push(inbound.clone());
            }
        }
        true
    }
    fn take_inbound(&self) -> Vec<InboundChannelGovernance> {
        std::mem::take(&mut self.relay.queues.lock().unwrap()[self.index])
    }
}

#[test]
fn readers_are_the_rooms_certified_devices_whose_role_reads() {
    let directory = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&directory.path().join("authority")).unwrap();
    let editor = admit(&mut owner, 10, MemberRole::Editor, false);
    let viewer = admit(&mut owner, 20, MemberRole::Viewer, false);
    let foreign = admit(&mut owner, 30, MemberRole::Editor, true);
    let readers = readers_of(owner.peer(), SystemTime::now());
    let governor = Ed25519PublicKey::from_bytes(owner.peer().authority().device_public_key);
    assert_eq!(
        readers,
        BTreeSet::from([governor, device(&editor), device(&viewer), device(&foreign)])
    );
    assert!(
        !readers.contains(&Ed25519PublicKey::from_bytes(
            owner.host_certificate().device_public_key
        )),
        "the host's relay certificate never reads"
    );
}

#[test]
fn the_steward_admits_a_room_member_and_nobody_else() {
    let directory = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&directory.path().join("authority")).unwrap();
    let room = RoomId::parse(owner.peer().room()).unwrap();
    let governor = Ed25519PublicKey::from_bytes(owner.peer().authority().device_public_key);
    let editor = admit(&mut owner, 40, MemberRole::Editor, true);
    let stranger = SigningKey::from_bytes(&[50; 32]);
    let channel = collab_types::document_channel_id("so.tilo.chat", &room.to_string(), "");
    let artifact = collab_types::document_artifact_id("so.tilo.chat", &room.to_string(), "");

    let steward = Steward::open(&owner).unwrap();
    let mut member = member_service(&editor, &directory.path().join("editor"), governor);
    let mut outsider = member_service(&stranger, &directory.path().join("stranger"), governor);
    let relay = Relay {
        queues: Mutex::new(vec![Vec::new(); 3]),
    };
    let sides = [0, 1, 2].map(|index| Side {
        relay: &relay,
        index,
    });
    assert!(matches!(
        member.access(room, channel, Instant::now()),
        ChannelAccess::Waiting
    ));
    assert!(matches!(
        outsider.access(room, channel, Instant::now()),
        ChannelAccess::Waiting
    ));
    let wall = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut keys = None;
    for _ in 0..20 {
        member.pump(&sides[1], Instant::now(), wall);
        outsider.pump(&sides[2], Instant::now(), wall);
        steward.pump(&owner, &sides[0]);
        if let ChannelAccess::Ready(ready) = member.access(room, channel, Instant::now()) {
            keys = Some(ready);
            break;
        }
    }
    let keys = keys.expect("the governor's steward admits a member of its room");
    assert!(
        matches!(
            outsider.access(room, channel, Instant::now()),
            ChannelAccess::Waiting
        ),
        "a device outside the room is never admitted"
    );

    // What the member seals, the steward's keys open, also after reopening.
    let (metadata, sealed) = keys
        .seal_content(artifact, ContentRetention::Delta, b"a member's sealed edit")
        .unwrap();
    drop(steward);
    let steward = Steward::open(&owner).unwrap();
    let (ChannelAccess::Ready(held), _) = steward.keys().access(room, channel) else {
        panic!("the steward keeps its channel keys across a restart")
    };
    assert_eq!(
        &*held.open_content(artifact, &metadata, &sealed).unwrap(),
        b"a member's sealed edit"
    );
}

/// The member's keys for `channel`, once the steward admitted it over an
/// in-memory relay.
fn admitted_keys(
    owner: &SessionOwner,
    steward: &Steward,
    member: &mut ChannelEncryption,
    channel: collab_types::ChannelId,
) -> channel_crypto::ChannelContentKeys {
    let room = RoomId::parse(owner.peer().room()).unwrap();
    let relay = Relay {
        queues: Mutex::new(vec![Vec::new(); 2]),
    };
    let sides = [0, 1].map(|index| Side {
        relay: &relay,
        index,
    });
    let wall = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for _ in 0..20 {
        if let ChannelAccess::Ready(keys) = member.access(room, channel, Instant::now()) {
            return keys;
        }
        member.pump(&sides[1], Instant::now(), wall);
        steward.pump(owner, &sides[0]);
    }
    panic!("the steward admits a member of its room")
}

#[test]
fn the_host_keeps_sealed_writes_and_its_steward_compacts_them() {
    use ui::automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
    const APP: &str = "so.tilo.chat";
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("authority");
    let mut owner = SessionOwner::initialize(&root).unwrap();
    let room = owner.peer().room().to_owned();
    let governor = Ed25519PublicKey::from_bytes(owner.peer().authority().device_public_key);
    let editor = admit(&mut owner, 60, MemberRole::Editor, false);
    let channel = collab_types::document_channel_id(APP, &room, "");
    let artifact = collab_types::document_artifact_id(APP, &room, "");
    // Exact admission evidence, which sealed history requires, comes with
    // the delivery storage the device host selects.
    owner.activate_delivery().unwrap();
    let steward = Steward::open(&owner).unwrap();
    let mut member = member_service(&editor, &directory.path().join("editor"), governor);
    let keys = admitted_keys(&owner, &steward, &mut member, channel);

    // Each sealed write is kept by the host's keyless owner.
    let mut chat = AutoCommit::load(&owner.peer().document(APP, "").unwrap()).unwrap();
    for (seq, line) in (1..).zip(["one", "two", "three"]) {
        let heads = chat.get_heads();
        chat.put(ROOT, line, line).unwrap();
        chat.commit();
        let plaintext = collab_types::DocumentMutation::Incremental(chat.save_after(&heads))
            .encode()
            .unwrap();
        let (metadata, payload) = keys
            .seal_content(artifact, ContentRetention::Delta, &plaintext)
            .unwrap();
        let mut op = collab_types::SignedOp::new_unsigned_with_encryption(
            payload,
            artifact,
            device(&editor),
            collab_types::document_attestation_id(APP, &room, ""),
            collab_types::OpClass::Splice,
            SystemTime::now(),
            seq,
            metadata,
        );
        collab_types::sign_op(&mut op, &editor).unwrap();
        let received = owner
            .receive(
                tilo_peer_state::TransportOrigin::RelayDelta(editor.verifying_key().to_bytes()),
                NeighbourMsg::AppDocument {
                    peer: tilo_peer_state::peer_handle(&editor.verifying_key().to_bytes()),
                    app_id: APP.into(),
                    cluster_id: room.clone(),
                    record_id: String::new(),
                    envelope: Box::new(op),
                },
            )
            .unwrap();
        assert!(received.is_some(), "the host commits a sealed write");
    }

    // The steward compacts them; the compaction is published and survives
    // a restart, and it opens to every write for a key holder.
    steward.compact(&mut owner, APP, "").unwrap();
    drop(owner);
    let owner = SessionOwner::open(&root).unwrap();
    let compaction = owner
        .outgoing()
        .iter()
        .filter_map(|entry| postcard::from_bytes::<NeighbourMsg>(entry.bytes).ok())
        .find_map(|message| match message {
            NeighbourMsg::AppDocument { envelope, .. }
                if envelope.op_class == collab_types::OpClass::Compaction =>
            {
                Some(*envelope)
            }
            _ => None,
        })
        .expect("the host publishes its compaction");
    assert_eq!(compaction.author, governor);
    let room_id = RoomId::parse(&room).unwrap();
    let collab_types::DocumentMutation::Snapshot(bytes) = collab_types::DocumentMutation::decode(
        &steward
            .keys()
            .open_history_write(room_id, &compaction)
            .unwrap(),
    )
    .unwrap() else {
        panic!("a compaction seals a snapshot")
    };
    let merged = AutoCommit::load(&bytes).unwrap();
    for line in ["one", "two", "three"] {
        assert!(
            merged.get(ROOT, line).unwrap().is_some(),
            "{line} is compacted"
        );
    }
}
