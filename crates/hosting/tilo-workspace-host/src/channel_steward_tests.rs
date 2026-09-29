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
