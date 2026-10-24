//! Moving a room to another person: the owner offers it, the recipient's
//! host device accepts, the old host commits the handoff and serves the room
//! read-only, and the recipient's host takes it up from its own copy while
//! members follow.

use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::handoff::{HandoffAcceptance, HandoffError, OwnershipOffer};
use tilo_peer_state::join::TransportHint;
use tilo_peer_state::{StatePeer, TransportOrigin};
use tilo_relay_session::rooms::SetMemberRoleRequest;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use web_time::SystemTime;

use super::{HeldChannels, OwnershipError};
use crate::rooms::Rooms;
use crate::rooms::actions::tests::admit_device;
use crate::rooms::fixtures::TestHost;
use crate::rooms::tests::{governor, test_host};

/// A person's host device: a device that runs a room host, and the
/// person's root that certified it.
pub(crate) struct HostDevice {
    pub(crate) key: SigningKey,
    pub(crate) cert: DeviceCert,
}

impl HostDevice {
    pub(crate) fn new() -> Self {
        let root = SigningKey::from_bytes(&rand::random());
        let key = SigningKey::from_bytes(&rand::random());
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "recipient's host".into(),
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        Self { key, cert }
    }

    /// Where members reach this device's host, signed by it.
    fn hint(&self) -> TransportHint {
        tilo_peer_state::join::sign_transport_hint(
            "test",
            "{}",
            self.cert.clone(),
            self.key.verifying_key().to_bytes(),
            &self.key,
        )
        .unwrap()
    }

    /// This device's acceptance of `offer`, dated `accepted_at_ns`.
    fn accept_at(&self, offer: &OwnershipOffer, accepted_at_ns: u64) -> HandoffAcceptance {
        HandoffAcceptance::sign(
            offer,
            self.cert.clone(),
            self.hint(),
            accepted_at_ns,
            &self.key,
        )
        .unwrap()
    }

    pub(crate) fn accept(&self, offer: &OwnershipOffer) -> HandoffAcceptance {
        self.accept_at(offer, tilo_clock::unix_now_ns())
    }
}

/// A room on the owner's host, with the recipient's host device and one
/// other member in it.
struct Moving {
    owner_key: SigningKey,
    owner: DeviceCert,
    host: TestHost,
    room: String,
    recipient: HostDevice,
    member: DeviceCert,
}

impl Moving {
    async fn new(dir: &std::path::Path) -> Self {
        let (owner_key, owner) = governor();
        let host = test_host(dir, &owner_key, &owner).await;
        let room = host.rooms().create("Studio").await.unwrap().room;
        let recipient = HostDevice::new();
        admit_device(
            host.rooms(),
            &room,
            &owner,
            1,
            (&recipient.key, &recipient.cert),
        )
        .await;
        let (member_key, member) = crate::rooms::tests::guest_device();
        admit_device(host.rooms(), &room, &owner, 2, (&member_key, &member)).await;
        write_record(host.rooms(), &room, (&member_key, &member), "topic").await;
        Self {
            owner_key,
            owner,
            host,
            room,
            recipient,
            member,
        }
    }

    fn rooms(&self) -> &Rooms {
        self.host.rooms()
    }

    async fn offer(&self, request: u8) -> OwnershipOffer {
        self.rooms()
            .propose_ownership(
                &self.room,
                &self.owner,
                [request; 32],
                self.recipient.cert.user_public_key,
            )
            .await
            .unwrap()
    }

    /// The recipient's host device takes the room up on `offer`: the
    /// acceptance the old host committed.
    async fn hand_off(&self, offer: &OwnershipOffer) -> HandoffAcceptance {
        let acceptance = self.recipient.accept(offer);
        let handoff = self
            .rooms()
            .accept_ownership(
                &self.room,
                &self.recipient.cert,
                [0xAC; 32],
                acceptance.clone(),
            )
            .await
            .unwrap();
        assert_eq!(handoff.acceptance, acceptance);
        acceptance
    }
}

/// The chat app every test room holds a record of.
const APP: &str = "so.tilo.chat";

/// The member `cert`, whose key `key` is, writes `value` into the room's
/// chat record, and the room's governor takes the write in.
pub(super) async fn write_record(
    rooms: &Rooms,
    room: &str,
    (key, cert): (&SigningKey, &DeviceCert),
    value: &str,
) {
    use automerge::transaction::Transactable as _;
    use collab_types::CommitNamed as _;
    let (mut copy, _) = replica(rooms, room).await;
    let mut doc = copy
        .document(APP, "")
        .map(|bytes| automerge::AutoCommit::load(&bytes).unwrap())
        .unwrap_or_default();
    let heads = doc.get_heads();
    doc.put(automerge::ROOT, "topic", value).unwrap();
    doc.commit_edit("fixture", "topic");
    let message = copy
        .publish_incremental(APP, "", doc.save_after(&heads), key, SystemTime::now())
        .unwrap();
    rooms
        .absorb(
            room,
            cert.device_public_key,
            &postcard::to_stdvec(&message).unwrap(),
        )
        .await
        .unwrap();
}

/// A copy of `room` as `rooms` governs it now, as a member's device holds
/// it: restored under the governor its host's copy names.
pub(super) async fn replica(rooms: &Rooms, room: &str) -> (StatePeer, usize) {
    let state = rooms.room(room).await.unwrap();
    let governed = state.lock().await;
    let peer = StatePeer::restore(
        &governed.peer().durable().unwrap(),
        room,
        governed.record.authority.clone(),
        vec![governed.record.authority.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    (peer, governed.record.outgoing.len())
}

/// Hand `replica` what `rooms` published of `room` from publication `from`
/// on, as its host delivers it, following every handoff it carries. The
/// publications the replica has now.
pub(super) async fn deliver(
    rooms: &Rooms,
    room: &str,
    replica: &mut StatePeer,
    from: usize,
) -> usize {
    let state = rooms.room(room).await.unwrap();
    let governed = state.lock().await;
    let publisher = governed.record.authority.device_public_key;
    for publication in &governed.record.outgoing[from..] {
        let message: NeighbourMsg = postcard::from_bytes(&publication.bytes).unwrap();
        replica
            .receive_from(
                TransportOrigin::RetainedRelayDelta(publisher),
                message,
                SystemTime::now(),
            )
            .unwrap();
        replica.follow_handoffs().unwrap();
    }
    governed.record.outgoing.len()
}

async fn generation(rooms: &Rooms, room: &str) -> u64 {
    rooms.summary(room).await.unwrap().generation
}

fn owner_of(peer: &StatePeer, room: &str) -> Option<String> {
    tilo_desktop_model::projection::room_policy_of(peer.admitted_room().unwrap(), room).owner
}

/// Whether every change of `older`'s room is in `newer`'s: no history lost.
fn keeps_history(older: &StatePeer, newer: &StatePeer) -> bool {
    let mut newer = newer.admitted_room().unwrap().clone();
    older
        .admitted_room()
        .unwrap()
        .clone()
        .get_heads()
        .iter()
        .all(|head| newer.get_change_by_hash(head).is_some())
}

#[tokio::test]
async fn ownership_moves_to_the_recipients_device_and_members_follow_its_commits() {
    let old = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    let moving = Moving::new(old.path()).await;
    let room = moving.room.clone();
    let (mut recipients_copy, seen) = replica(moving.rooms(), &room).await;
    let (mut members_copy, _) = replica(moving.rooms(), &room).await;
    let before = replica(moving.rooms(), &room).await.0;

    let offer = moving.offer(1).await;
    moving.hand_off(&offer).await;
    deliver(moving.rooms(), &room, &mut recipients_copy, seen).await;
    deliver(moving.rooms(), &room, &mut members_copy, seen).await;
    for copy in [&recipients_copy, &members_copy] {
        assert_eq!(copy.authority(), &moving.recipient.cert);
        assert_eq!(
            owner_of(copy, &room),
            Some(hex::encode(moving.recipient.cert.user_public_key))
        );
    }

    // The old host serves the room as it stands and changes nothing more.
    assert!(
        moving
            .rooms()
            .rename(&room, "Elsewhere")
            .await
            .unwrap_err()
            .contains(&HandoffError::Moved.to_string())
    );

    let recipients = test_host(new.path(), &moving.recipient.key, &moving.recipient.cert).await;
    let adopted = recipients
        .rooms()
        .adopt(
            &room,
            &recipients_copy.durable().unwrap(),
            HeldChannels::default(),
        )
        .await
        .unwrap();
    assert_eq!(adopted.room, room, "the room keeps its id");
    assert_eq!(adopted.name, "Studio");
    assert!(adopted.generation > generation(moving.rooms(), &room).await);
    let seen = deliver(recipients.rooms(), &room, &mut members_copy, 0).await;
    assert_eq!(members_copy.authority(), &moving.recipient.cert);

    // Content and history are the room's own, whoever governs it.
    let (governing, _) = replica(recipients.rooms(), &room).await;
    assert!(keeps_history(&before, &governing));
    assert!(keeps_history(&before, &members_copy));
    assert!(
        !before.plaintext_routes().is_empty(),
        "the room holds records"
    );
    assert_eq!(before.plaintext_routes(), governing.plaintext_routes());
    for (app, record) in before.plaintext_routes() {
        assert_eq!(
            before.document(&app, &record),
            governing.document(&app, &record)
        );
    }

    // Members follow what the new host commits.
    recipients.rooms().rename(&room, "Studio B").await.unwrap();
    deliver(recipients.rooms(), &room, &mut members_copy, seen).await;
    assert_eq!(
        members_copy.governance().room_name().as_deref(),
        Some("Studio B")
    );
    // The previous owner stays, as an administrator the new owner can
    // demote.
    let previous = hex::encode(moving.owner.user_public_key);
    assert_eq!(
        tilo_desktop_model::projection::room_policy_of(
            members_copy.admitted_room().unwrap(),
            &room
        )
        .members
        .get(&previous),
        Some(&tilo_action_policy::Role::Owner)
    );
    recipients.shutdown().await;
    moving.host.shutdown().await;
}

#[tokio::test]
async fn a_crash_before_the_handoff_commit_leaves_the_old_host_governing() {
    let old = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    let moving = Moving::new(old.path()).await;
    let room = moving.room.clone();
    let offer = moving.offer(1).await;
    let (recipients_copy, _) = replica(moving.rooms(), &room).await;
    let Moving {
        owner_key,
        owner,
        host,
        recipient,
        ..
    } = moving;
    host.shutdown().await;

    // Stopped before it took the acceptance: the old host still governs,
    // the offer is still open, and the recipient's host has nothing to take
    // up.
    let host = test_host(old.path(), &owner_key, &owner).await;
    let recipients = test_host(new.path(), &recipient.key, &recipient.cert).await;
    assert!(matches!(
        recipients
            .rooms()
            .adopt(
                &room,
                &recipients_copy.durable().unwrap(),
                HeldChannels::default()
            )
            .await,
        Err(OwnershipError::NotHandedHere)
    ));
    host.rooms().rename(&room, "Still here").await.unwrap();
    host.rooms()
        .accept_ownership(&room, &recipient.cert, [0xAC; 32], recipient.accept(&offer))
        .await
        .unwrap();
    recipients.shutdown().await;
    host.shutdown().await;
}

#[tokio::test]
async fn a_crash_after_the_handoff_commit_leaves_only_the_new_governor() {
    let old = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    let moving = Moving::new(old.path()).await;
    let room = moving.room.clone();
    let (mut recipients_copy, seen) = replica(moving.rooms(), &room).await;
    let offer = moving.offer(1).await;
    moving.hand_off(&offer).await;
    deliver(moving.rooms(), &room, &mut recipients_copy, seen).await;
    let Moving {
        owner_key,
        owner,
        host,
        recipient,
        ..
    } = moving;
    host.shutdown().await;

    // The old host reads the handoff back from its log: it serves the room,
    // read-only, and governs it no more.
    let host = test_host(old.path(), &owner_key, &owner).await;
    assert!(host.rooms().rename(&room, "Mine again").await.is_err());
    assert!(matches!(
        host.rooms()
            .propose_ownership(&room, &owner, [9; 32], recipient.cert.user_public_key)
            .await,
        Err(OwnershipError::Handoff(HandoffError::Moved))
    ));
    let workspace_id = host.rooms().summary(&room).await.unwrap().workspace_id;
    assert!(moved_here(&host, workspace_id).await);

    // The new host takes it up, and still governs it after its own restart.
    let recipients = test_host(new.path(), &recipient.key, &recipient.cert).await;
    recipients
        .rooms()
        .adopt(
            &room,
            &recipients_copy.durable().unwrap(),
            HeldChannels::default(),
        )
        .await
        .unwrap();
    recipients.shutdown().await;
    let recipients = test_host(new.path(), &recipient.key, &recipient.cert).await;
    recipients.rooms().rename(&room, "Moved").await.unwrap();
    assert!(!moved_here(&recipients, workspace_id).await);
    recipients.shutdown().await;
    host.shutdown().await;
}

/// Whether `host` serves the workspace `workspace_id` as a room it handed on.
async fn moved_here(host: &TestHost, workspace_id: u64) -> bool {
    let workspace = host
        .rooms()
        .inner
        .host
        .workspaces
        .lock()
        .await
        .get(&workspace_id)
        .cloned()
        .unwrap();
    workspace.lock().await.moved
}

#[tokio::test]
async fn an_offline_member_catches_up_with_the_handoff_on_its_next_connect() {
    let old = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    let moving = Moving::new(old.path()).await;
    let room = moving.room.clone();
    let (mut away, seen) = replica(moving.rooms(), &room).await;
    let (mut recipients_copy, _) = replica(moving.rooms(), &room).await;
    let offer = moving.offer(1).await;
    moving.hand_off(&offer).await;
    deliver(moving.rooms(), &room, &mut recipients_copy, seen).await;
    let recipients = test_host(new.path(), &moving.recipient.key, &moving.recipient.cert).await;
    recipients
        .rooms()
        .adopt(
            &room,
            &recipients_copy.durable().unwrap(),
            HeldChannels::default(),
        )
        .await
        .unwrap();
    recipients.rooms().rename(&room, "Moved on").await.unwrap();

    // Away the whole time, it reaches the old host first, which still serves
    // what it committed: the handoff sends it on to the new host.
    assert_eq!(away.authority(), &moving.owner);
    deliver(moving.rooms(), &room, &mut away, seen).await;
    assert_eq!(away.authority(), &moving.recipient.cert);
    deliver(recipients.rooms(), &room, &mut away, 0).await;
    assert_eq!(away.governance().room_name().as_deref(), Some("Moved on"));
    recipients.shutdown().await;
    moving.host.shutdown().await;
}

#[tokio::test]
async fn offering_again_replaces_the_open_offer_and_its_acceptance_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let moving = Moving::new(dir.path()).await;
    let first = moving.offer(1).await;
    let second = moving.offer(2).await;
    assert_ne!(first.offer_id, second.offer_id);
    let state = moving.rooms().room(&moving.room).await.unwrap();
    assert_eq!(
        state.lock().await.peer().governance().ownership_offer(),
        Some(second.clone())
    );
    assert!(matches!(
        moving
            .rooms()
            .accept_ownership(
                &moving.room,
                &moving.recipient.cert,
                [0xAC; 32],
                moving.recipient.accept(&first),
            )
            .await,
        Err(OwnershipError::Handoff(HandoffError::WrongOffer))
    ));
    moving.hand_off(&second).await;
    moving.host.shutdown().await;
}

#[tokio::test]
async fn a_retried_offer_answers_the_offer_it_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let moving = Moving::new(dir.path()).await;
    let first = moving.offer(1).await;
    let at = generation(moving.rooms(), &moving.room).await;
    assert_eq!(moving.offer(1).await, first);
    assert_eq!(generation(moving.rooms(), &moving.room).await, at);
    moving.host.shutdown().await;
}

#[tokio::test]
async fn a_cancelled_offer_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let moving = Moving::new(dir.path()).await;
    let owner = hex::encode(moving.owner.user_public_key);
    let offer = moving.offer(1).await;
    moving
        .rooms()
        .cancel_ownership_offer(&moving.room, &moving.owner, [2; 32], offer.offer_id)
        .await
        .unwrap();
    let state = moving.rooms().room(&moving.room).await.unwrap();
    let (open, owner_now) = {
        let governed = state.lock().await;
        (
            governed.peer().governance().ownership_offer(),
            owner_of(governed.peer(), &moving.room),
        )
    };
    assert_eq!(open, None);
    assert_eq!(owner_now, Some(owner.clone()));
    // Taking it back again changes nothing either.
    let at = generation(moving.rooms(), &moving.room).await;
    assert_eq!(
        moving
            .rooms()
            .cancel_ownership_offer(&moving.room, &moving.owner, [3; 32], offer.offer_id)
            .await
            .unwrap(),
        at
    );
    assert!(matches!(
        moving
            .rooms()
            .accept_ownership(
                &moving.room,
                &moving.recipient.cert,
                [0xAC; 32],
                moving.recipient.accept(&offer),
            )
            .await,
        Err(OwnershipError::Handoff(HandoffError::WrongOffer))
    ));
    moving.rooms().rename(&moving.room, "Ours").await.unwrap();
    assert_eq!(
        owner_of(state.lock().await.peer(), &moving.room),
        Some(owner)
    );
    moving.host.shutdown().await;
}

#[tokio::test]
async fn only_the_owner_offers_the_room_or_takes_the_offer_back() {
    let dir = tempfile::tempdir().unwrap();
    let moving = Moving::new(dir.path()).await;
    // Even an administrator of the room is not its owner.
    moving
        .rooms()
        .set_member_role(
            &moving.room,
            &moving.owner,
            SetMemberRoleRequest {
                request_id: [7; 32],
                user_public_key: moving.member.user_public_key,
                role: tilo_peer_state::join::MemberRole::Moderator,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        moving
            .rooms()
            .propose_ownership(
                &moving.room,
                &moving.member,
                [1; 32],
                moving.recipient.cert.user_public_key,
            )
            .await,
        Err(OwnershipError::Handoff(HandoffError::NotOwner))
    ));
    let offer = moving.offer(2).await;
    assert!(matches!(
        moving
            .rooms()
            .cancel_ownership_offer(&moving.room, &moving.member, [3; 32], offer.offer_id)
            .await,
        Err(OwnershipError::Handoff(HandoffError::NotOwner))
    ));
    moving.host.shutdown().await;
}

#[tokio::test]
async fn the_room_is_offered_only_to_someone_else_who_is_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let moving = Moving::new(dir.path()).await;
    let stranger = HostDevice::new();
    assert!(matches!(
        moving
            .rooms()
            .propose_ownership(
                &moving.room,
                &moving.owner,
                [1; 32],
                stranger.cert.user_public_key,
            )
            .await,
        Err(OwnershipError::RecipientNotMember)
    ));
    assert!(matches!(
        moving
            .rooms()
            .propose_ownership(
                &moving.room,
                &moving.owner,
                [2; 32],
                moving.owner.user_public_key,
            )
            .await,
        Err(OwnershipError::Handoff(HandoffError::SameOwner))
    ));
    moving.host.shutdown().await;
}

#[tokio::test]
async fn only_the_device_an_acceptance_names_takes_the_room() {
    let dir = tempfile::tempdir().unwrap();
    let moving = Moving::new(dir.path()).await;
    let offer = moving.offer(1).await;
    assert!(matches!(
        moving
            .rooms()
            .accept_ownership(
                &moving.room,
                &moving.member,
                [0xAC; 32],
                moving.recipient.accept(&offer),
            )
            .await,
        Err(OwnershipError::NotTheAcceptingDevice)
    ));
    moving.host.shutdown().await;
}

#[tokio::test]
async fn an_acceptance_by_someone_the_room_was_not_offered_to_is_refused() {
    let old = tempfile::tempdir().unwrap();
    let (owner_key, owner) = governor();
    let host = test_host(old.path(), &owner_key, &owner).await;
    let room = host.rooms().create("Studio").await.unwrap().room;
    let recipient = HostDevice::new();
    let other = HostDevice::new();
    for (seed, device) in [(1, &recipient), (2, &other)] {
        admit_device(
            host.rooms(),
            &room,
            &owner,
            seed,
            (&device.key, &device.cert),
        )
        .await;
    }
    let offer = host
        .rooms()
        .propose_ownership(&room, &owner, [1; 32], recipient.cert.user_public_key)
        .await
        .unwrap();
    assert!(matches!(
        host.rooms()
            .accept_ownership(&room, &other.cert, [0xAC; 32], other.accept(&offer))
            .await,
        Err(OwnershipError::Handoff(HandoffError::SuccessorNotRecipient))
    ));
    host.shutdown().await;
}

#[tokio::test]
async fn an_acceptance_dated_after_the_old_host_takes_it_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let moving = Moving::new(dir.path()).await;
    let offer = moving.offer(1).await;
    let later = tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000);
    assert!(matches!(
        moving
            .rooms()
            .accept_ownership(
                &moving.room,
                &moving.recipient.cert,
                [0xAC; 32],
                moving.recipient.accept_at(&offer, later),
            )
            .await,
        Err(OwnershipError::Handoff(HandoffError::FutureAcceptance))
    ));
    moving.host.shutdown().await;
}

#[tokio::test]
async fn a_room_handed_on_takes_no_more_offers_or_acceptances() {
    let dir = tempfile::tempdir().unwrap();
    let moving = Moving::new(dir.path()).await;
    let offer = moving.offer(1).await;
    let accepted = moving.hand_off(&offer).await;
    // The same acceptance again, as a retry, answers with the handoff it
    // made; another acceptance of the offer is refused.
    let retried = moving
        .rooms()
        .accept_ownership(
            &moving.room,
            &moving.recipient.cert,
            [0xAD; 32],
            accepted.clone(),
        )
        .await
        .unwrap();
    assert_eq!(retried.acceptance, accepted);
    let again = moving
        .rooms()
        .accept_ownership(
            &moving.room,
            &moving.recipient.cert,
            [0xAE; 32],
            moving
                .recipient
                .accept_at(&offer, accepted.accepted_at_ns + 1),
        )
        .await;
    assert!(matches!(
        again,
        Err(OwnershipError::Handoff(HandoffError::Moved))
    ));
    assert!(matches!(
        moving
            .rooms()
            .propose_ownership(
                &moving.room,
                &moving.owner,
                [2; 32],
                moving.member.user_public_key,
            )
            .await,
        Err(OwnershipError::Handoff(HandoffError::Moved))
    ));
    moving.host.shutdown().await;
}

#[tokio::test]
async fn a_copy_that_does_not_hand_the_room_here_is_not_taken_up() {
    let old = tempfile::tempdir().unwrap();
    let new = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let moving = Moving::new(old.path()).await;
    let room = moving.room.clone();
    let (mut copy, seen) = replica(moving.rooms(), &room).await;
    let offer = moving.offer(1).await;
    moving.hand_off(&offer).await;
    deliver(moving.rooms(), &room, &mut copy, seen).await;

    // Handed to the recipient's host device, not to any other host.
    let other = HostDevice::new();
    let others = test_host(elsewhere.path(), &other.key, &other.cert).await;
    assert!(matches!(
        others
            .rooms()
            .adopt(&room, &copy.durable().unwrap(), HeldChannels::default())
            .await,
        Err(OwnershipError::NotHandedHere)
    ));
    others.shutdown().await;

    // A room file the recipient's host kept from before is set aside, not
    // lost.
    let recipients = test_host(new.path(), &moving.recipient.key, &moving.recipient.cert).await;
    let rooms_dir = new.path().join("rooms");
    std::fs::write(rooms_dir.join(format!("{room}.room")), b"an earlier copy").unwrap();
    recipients
        .rooms()
        .adopt(&room, &copy.durable().unwrap(), HeldChannels::default())
        .await
        .unwrap();
    let aside = std::fs::read_dir(&rooms_dir)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("{room}.set-aside-"))
        })
        .expect("the earlier room file is set aside");
    assert_eq!(
        std::fs::read(aside.path().join(format!("{room}.room"))).unwrap(),
        b"an earlier copy"
    );
    recipients.shutdown().await;
    moving.host.shutdown().await;
}

#[tokio::test]
async fn a_room_handed_on_leaves_its_old_hosts_directory() {
    let dir = tempfile::tempdir().unwrap();
    let moving = Moving::new(dir.path()).await;
    moving
        .rooms()
        .publish(
            &moving.room,
            &moving.owner,
            tilo_relay_protocol::directory::Listing {
                description: "Open studio".into(),
                default_role: tilo_relay_protocol::rooms::RoomRole::Commenter,
            },
        )
        .await
        .unwrap();
    let online = std::collections::BTreeSet::new();
    assert_eq!(moving.rooms().directory(&online).await.len(), 1);
    let offer = moving.offer(1).await;
    moving.hand_off(&offer).await;
    assert!(moving.rooms().directory(&online).await.is_empty());
    moving.host.shutdown().await;
}

#[tokio::test]
async fn an_invitation_to_a_room_handed_on_is_answered_with_where_it_went() {
    let dir = tempfile::tempdir().unwrap();
    let moving = Moving::new(dir.path()).await;
    let (_, guest) = crate::rooms::tests::guest_device();
    let invitation = moving
        .rooms()
        .issue(
            &moving.room,
            &moving.owner,
            tilo_relay_session::rooms::IssueInvitationRequest {
                request_id: [9; 32],
                role: tilo_peer_state::join::MemberRole::Editor,
                recipient_user_public_key: Some(guest.user_public_key),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(600_000_000_000)
                    .unwrap(),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let authority = invitation_authority(&invitation);
    let offer = moving.offer(1).await;
    let acceptance = moving.hand_off(&offer).await;

    let answer = moving
        .rooms()
        .join_service()
        .answer(
            guest.device_public_key,
            tilo_peer_state::join::JoinTransportRequest::Inspect {
                signed_invitation: invitation.signed_invitation,
            },
            tilo_clock::unix_now_ns(),
        )
        .await;
    let tilo_peer_state::join::JoinTransportResponse::Moved(moved) = answer else {
        panic!("the old host says where the room went: {answer:?}");
    };
    // Followed from the governor the invitation names, the handoffs lead
    // to the recipient's host and where it is reached.
    assert_eq!(
        tilo_peer_state::handoff::moved_to(&moving.room, &authority, &moved).unwrap(),
        (
            acceptance.successor.clone(),
            acceptance.successor_hint.clone()
        )
    );
    // Taken from anyone else's word, they lead nowhere.
    assert_eq!(
        tilo_peer_state::handoff::moved_to(&moving.room, &moving.member, &moved),
        Err(HandoffError::NotCurrentGovernor)
    );
    moving.host.shutdown().await;
}

/// The governor `invitation` names.
fn invitation_authority(invitation: &tilo_relay_session::rooms::IssuedInvitation) -> DeviceCert {
    tilo_peer_state::join::decode_and_verify_invitation(
        &invitation.signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap()
    .grant()
    .authority
    .clone()
}
