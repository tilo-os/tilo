//! End-to-end encryption of collaboration channels on this device.
//!
//! Every document route is a channel with one MLS group. The device that
//! governs the route's room is the channel's steward: it creates the group
//! the first time any allowed device opens the route, and from then on keeps
//! the group equal to the devices the room's access policy allows. Every
//! other device holds a member view and asks the steward for access.
//!
//! This service runs both roles for every channel this device takes part
//! in. [`ChannelEncryption::access`] answers an application endpoint that
//! wants the content keys of a route (creating the steward group or starting
//! a request as needed), and [`ChannelEncryption::pump`] runs the governance
//! protocol over the room transport:
//!
//! - inbound frames advance member views, access requests queue at the
//!   steward, acknowledgements complete deliveries;
//! - stewards reconcile their groups with the access policy, so an invite,
//!   a removal, a role change that ends access, a new certified device or a
//!   revoked one becomes a commit;
//! - the steward's outbox is redelivered with exponential backoff until each
//!   device's cumulative acknowledgement covers it, so a network failure
//!   delays delivery but never loses it; waiting members re-send their
//!   request the same way.
//!
//! Every change registers the channel's plaintext gate and head epoch with
//! the broker, which refuses plaintext and stale-epoch content for the
//! channel on every ingress path. The broker itself never holds a key.
//!
//! Every change is also announced ([`ChannelKeyProvider::channel_changed`]):
//! an application endpoint holding a route's keys waits on that signal and
//! asks the identity service again only when it fires, rather than on a
//! timer. A key rotation, an invite, a removal, a role change that ends
//! reading and a device's enrolment or revocation each become a commit the
//! steward makes within [`RECONCILE_INTERVAL`] of the policy change (on its
//! announcement, or at the expiry, when the directory announces changes:
//! [`ChannelDirectory::policy_epoch`]); the
//! commit is announced on the steward the moment it is made and on a member
//! the moment its frame arrives, and the endpoint asks for keys at once.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use channel_crypto::{
    Access, ChannelContentKeys, ChannelMember, ChannelSteward, MemberEvent, VaultKey,
};
use collab_types::{
    ArtifactId, ChannelId, ContentRetention, Ed25519PublicKey, EncryptionMetadata,
    MlsAccessRequestV1, MlsGovernanceAckV1, MlsGovernanceFrameV1, SignedOp,
};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;

use crate::collab_core::BrokerChannels;
use crate::policy_schedule::{PolicySchedule, RECHECK_INTERVAL};
use tilo_desktop_model::RoomId;

/// How often stewards re-check the access policy when their directory
/// announces no changes, the longest an announced change waits, and how soon
/// a pass that left work due tries again ([`crate::policy_schedule`]).
const RECONCILE_INTERVAL: Duration = RECHECK_INTERVAL;
/// First and longest wait before redelivering unacknowledged frames.
const REDELIVERY_FIRST: Duration = Duration::from_secs(1);
const REDELIVERY_LONGEST: Duration = Duration::from_secs(60);
/// First and longest wait before a waiting member asks again.
const REQUEST_FIRST: Duration = Duration::from_secs(2);
const REQUEST_LONGEST: Duration = Duration::from_secs(120);

/// What an application endpoint may do with a route's content right now.
#[derive(Clone, Debug)]
pub enum ChannelAccess {
    /// Admitted: seal new content and open history with these keys.
    Ready(ChannelContentKeys),
    /// Asked the steward for access and not admitted yet. Nothing can be
    /// sealed or opened; the endpoint shows a waiting state.
    Waiting,
    /// This device was removed from the channel.
    Removed,
    /// Nobody can admit this device: the room's governor is unknown here.
    Unavailable,
}

/// A channel governance message received from the room transport. Every
/// variant is signed by a device key and verified by the channel service
/// before it changes any state.
#[derive(Debug, Clone)]
pub enum InboundChannelGovernance {
    /// A frame from a channel's steward.
    Frame {
        room: RoomId,
        frame: Box<collab_types::MlsGovernanceFrameV1>,
    },
    /// A device asking a channel's steward for membership.
    AccessRequest {
        room: RoomId,
        request: Box<collab_types::MlsAccessRequestV1>,
    },
    /// A device's cumulative acknowledgement of a channel's frames.
    Ack {
        room: RoomId,
        ack: Box<collab_types::MlsGovernanceAckV1>,
    },
}

impl InboundChannelGovernance {
    /// The governance `message` carries, if it is one for a valid room.
    #[must_use]
    pub fn from_message(message: NeighbourMsg) -> Option<Self> {
        match message {
            NeighbourMsg::MlsGovernance { room, frame, .. } => {
                RoomId::parse(&room).map(|room| Self::Frame { room, frame })
            }
            NeighbourMsg::MlsAccessRequest { room, request, .. } => {
                RoomId::parse(&room).map(|room| Self::AccessRequest { room, request })
            }
            NeighbourMsg::MlsGovernanceAck { room, ack, .. } => {
                RoomId::parse(&room).map(|room| Self::Ack { room, ack })
            }
            _ => None,
        }
    }
}

/// Where the channel protocol's messages travel: the room transport of the
/// host (the desktop session's network, the runtime's relay sessions).
pub trait GovernanceTransport {
    /// Hand one message to the transport. `false` when it was not accepted.
    fn publish(&self, message: NeighbourMsg) -> bool;
    /// Take every message received since the last call.
    fn take_inbound(&self) -> Vec<InboundChannelGovernance>;
}

/// The access policy inputs this service needs from the workspace model.
pub trait ChannelDirectory: Send {
    /// Whether this device governs `room`, and so stewards its channels.
    fn governs(&self, room: RoomId) -> bool;
    /// The device key of `room`'s governor, when this device does not govern
    /// it and knows who does.
    fn steward_of(&self, room: RoomId) -> Option<Ed25519PublicKey>;
    /// Every device that may read `room`'s channels at wall-clock `now`
    /// (seconds since the Unix epoch).
    fn allowed_devices(&self, room: RoomId, now: u64) -> BTreeSet<Ed25519PublicKey>;
    /// The [`tilo_identity_host::policy_change`] epoch (or a test's own) when
    /// every write to what [`Self::allowed_devices`] reads is announced and
    /// its expiries can be timed: stewards then re-check on announcements and
    /// at [`Self::next_policy_change`]. `None` (the default) keeps the
    /// re-check every [`RECHECK_INTERVAL`].
    fn policy_epoch(&self) -> Option<u64> {
        None
    }
    /// The first wall-clock second after `now` at which
    /// [`Self::allowed_devices`] for `room` can answer differently with
    /// nothing written: a grant or a certificate expiring, a revocation
    /// dated ahead. Read only while [`Self::policy_epoch`] is `Some`.
    fn next_policy_change(&self, room: RoomId, now: u64) -> Option<u64> {
        let _ = (room, now);
        None
    }
}

/// Durable location and keys of this device's channel state.
pub struct ChannelEncryptionConfig {
    /// Private directory holding sealed group and member state.
    pub root: PathBuf,
    /// Key sealing that state, held by the platform secret store.
    pub vault: VaultKey,
    /// This device's certified key: MLS credential, request and frame signer.
    pub device_key: SigningKey,
}

#[derive(Clone, Copy, Debug)]
struct Backoff {
    due: Instant,
    delay: Duration,
}

impl Backoff {
    fn now(now: Instant, first: Duration) -> Self {
        Self {
            due: now,
            delay: first,
        }
    }

    fn is_due(&self, now: Instant) -> bool {
        now >= self.due
    }

    fn fired(&mut self, now: Instant, longest: Duration) {
        self.due = now.checked_add(self.delay).unwrap_or(now);
        self.delay = self.delay.saturating_mul(2).min(longest);
    }
}

struct StewardEntry {
    room: RoomId,
    steward: ChannelSteward,
    requests: BTreeMap<Ed25519PublicKey, MlsAccessRequestV1>,
    redelivery: HashMap<Ed25519PublicKey, Backoff>,
    dirty: bool,
    /// Whether this session published the channel's genesis frame to the
    /// room. Devices that are not members yet learn from it who stewards
    /// the channel and that it is encrypted, so they ask for access and
    /// hold their edits instead of writing plaintext the channel refuses.
    announced: bool,
}

struct MemberEntry {
    room: RoomId,
    member: ChannelMember,
    request: Option<MlsAccessRequestV1>,
    retry: Backoff,
    ack_due: bool,
    /// An endpoint opened the route after this device was removed: ask the
    /// steward again, whose policy decides whether access resumes.
    rejoin: bool,
}

/// This device's side of every encrypted channel it takes part in.
pub struct ChannelEncryption {
    config: ChannelEncryptionConfig,
    directory: Box<dyn ChannelDirectory>,
    broker: Arc<Mutex<BrokerChannels>>,
    stewards: HashMap<ChannelId, StewardEntry>,
    members: HashMap<ChannelId, MemberEntry>,
    /// When stewards last re-checked the access policy, and what the next
    /// re-check waits for.
    reconcile: PolicySchedule,
    delivery: u64,
    generation: u64,
    /// The [`Self::generation`] each channel last changed at.
    changed: HashMap<ChannelId, u64>,
    /// [`Self::generation`], announced to whoever waits for a change.
    changes: tokio::sync::watch::Sender<u64>,
    /// An endpoint's [`Self::access`] queued governance the next pump sends
    /// (a genesis to announce, an access request to make). A host that parks
    /// its pump while idle wakes it on this; see [`ChannelKeyAccess::with_wake`].
    queued: bool,
}

impl ChannelEncryption {
    /// Open every channel state under `config.root` and register each
    /// channel's gate with the broker, so the plaintext gate holds from the
    /// first op after a restart.
    ///
    /// # Errors
    ///
    /// Returns an error when the state directory cannot be created or read.
    /// A single unreadable channel is skipped and logged instead.
    pub fn open(
        config: ChannelEncryptionConfig,
        directory: Box<dyn ChannelDirectory>,
        broker: Arc<Mutex<BrokerChannels>>,
    ) -> std::io::Result<Self> {
        create_private_dir(&config.root)?;
        let mut service = Self {
            config,
            directory,
            broker,
            stewards: HashMap::new(),
            members: HashMap::new(),
            reconcile: PolicySchedule::default(),
            delivery: 0,
            generation: 0,
            changed: HashMap::new(),
            changes: tokio::sync::watch::Sender::new(0),
            queued: false,
        };
        for room_dir in std::fs::read_dir(&service.config.root)? {
            let room_dir = room_dir?.path();
            let Some(room) = room_dir
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(RoomId::parse)
            else {
                continue;
            };
            for channel in channels_in(&room_dir.join("steward"), "group") {
                if let Err(error) = service.open_steward(room, channel) {
                    tracing::warn!(%error, %room, "channel steward state could not be opened");
                }
            }
            for channel in channels_in(&room_dir.join("member"), "member") {
                if let Err(error) = service.open_member(room, channel, Instant::now()) {
                    tracing::warn!(%error, %room, "channel member state could not be opened");
                }
            }
        }
        Ok(service)
    }

    /// This device's key.
    pub fn device(&self) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(self.config.device_key.verifying_key().to_bytes())
    }

    /// Increases whenever any channel's keys or access change.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The [`Self::generation`] at which `channel`'s keys or access last
    /// changed; 0 while they never did in this service.
    pub fn channel_change(&self, channel: ChannelId) -> u64 {
        self.changed.get(&channel).copied().unwrap_or(0)
    }

    /// What an endpoint of this device may do with `channel` in `room`.
    /// The first call for a route creates its group (when this device
    /// governs the room) or starts asking the steward for access. The caller
    /// has already decided the endpoint may read the route.
    pub fn access(&mut self, room: RoomId, channel: ChannelId, now: Instant) -> ChannelAccess {
        if self.directory.governs(room) {
            if !self.stewards.contains_key(&channel) {
                if let Err(error) = self.open_steward(room, channel) {
                    tracing::warn!(%error, %room, "channel group could not be created");
                    return ChannelAccess::Unavailable;
                }
                self.queued = true;
            }
            return self
                .stewards
                .get(&channel)
                .map_or(ChannelAccess::Unavailable, |entry| {
                    entry
                        .steward
                        .content_keys()
                        .map_or(ChannelAccess::Unavailable, ChannelAccess::Ready)
                });
        }
        if !self.members.contains_key(&channel) {
            match self.open_member(room, channel, now) {
                Ok(true) => self.queued = true,
                Ok(false) => return ChannelAccess::Unavailable,
                Err(error) => {
                    tracing::warn!(%error, %room, "channel member state could not be created");
                    return ChannelAccess::Unavailable;
                }
            }
        }
        let Some(entry) = self.members.get_mut(&channel) else {
            return ChannelAccess::Unavailable;
        };
        match entry.member.access() {
            Access::Ready { .. } => entry
                .member
                .content_keys()
                .map_or(ChannelAccess::Waiting, ChannelAccess::Ready),
            Access::Waiting => ChannelAccess::Waiting,
            Access::Removed => {
                if !entry.rejoin {
                    entry.rejoin = true;
                    entry.request = None;
                    entry.retry = Backoff::now(now, REQUEST_FIRST);
                    self.queued = true;
                }
                ChannelAccess::Removed
            }
        }
    }

    /// Observe a current key without opening a channel, requesting access or
    /// changing a removed member's rejoin state.
    fn existing_keys(&self, room: RoomId, channel: ChannelId) -> Option<ChannelContentKeys> {
        if self.directory.governs(room) {
            return self
                .stewards
                .get(&channel)
                .filter(|entry| entry.room == room)
                .and_then(|entry| entry.steward.content_keys().ok());
        }
        self.members
            .get(&channel)
            .filter(|entry| {
                entry.room == room && matches!(entry.member.access(), Access::Ready { .. })
            })
            .and_then(|entry| entry.member.content_keys())
    }

    /// Run the governance protocol once: apply what arrived, reconcile
    /// stewarded groups with the access policy, and (re)deliver what is
    /// due. `wall` is the Unix time the policy is evaluated at.
    pub fn pump(&mut self, transport: &dyn GovernanceTransport, now: Instant, wall: u64) {
        for inbound in transport.take_inbound() {
            self.receive(inbound, now, wall);
        }
        // Read before the policy is: a change announced while the stewards
        // reconcile has a later epoch and makes the next re-check due.
        let epoch = self.directory.policy_epoch();
        let reconcile_due = self
            .reconcile
            .due(now, epoch.unwrap_or(0))
            .is_some_and(|due| due <= now);
        let channels: Vec<ChannelId> = self.stewards.keys().copied().collect();
        let mut reconciled = false;
        for channel in channels {
            let due = reconcile_due || self.stewards.get(&channel).is_some_and(|entry| entry.dirty);
            if due {
                self.reconcile(channel, now, wall);
                reconciled = true;
            }
            self.redeliver(channel, transport, now);
        }
        if reconcile_due {
            self.reconcile
                .checked(now, epoch.unwrap_or(0), epoch.is_none());
        }
        // A new steward's room brings its own expiries.
        if reconciled && epoch.is_some() {
            self.register_next_policy_change(wall);
        }
        let channels: Vec<ChannelId> = self.members.keys().copied().collect();
        for channel in channels {
            self.drive_member(channel, transport, now, wall);
        }
    }

    /// When [`Self::pump`] next has work that no inbound message brings:
    /// `None` while there is none, so its caller can park until a message
    /// arrives or an endpoint's access wakes it ([`ChannelKeyAccess::with_wake`]).
    ///
    /// A steward re-checks its access policy when its directory announces a
    /// change ([`Self::waits_for_policy_change`]) and at the next expiry the
    /// last re-check read, which [`Self::pump`] registers with
    /// [`tilo_identity_host::policy_change::wake_at`]; neither is a deadline
    /// here. A directory that announces nothing keeps the re-check every
    /// [`RECONCILE_INTERVAL`]. The re-check also redelivers what members have
    /// not acknowledged (on their own backoff) and retries an unaccepted
    /// genesis announcement. A member is due only while it still has to
    /// acknowledge a frame or is asking for access, on its backoff. A member
    /// holding its keys has nothing to do until a frame arrives.
    #[must_use]
    pub fn next_pump_deadline(&self, now: Instant) -> Option<Instant> {
        let steward = if self.stewards.is_empty() {
            None
        } else if self.stewards.values().any(|entry| entry.dirty) {
            Some(now)
        } else {
            let policy = self
                .reconcile
                .due(now, self.directory.policy_epoch().unwrap_or(0));
            policy.into_iter().chain(self.next_redelivery(now)).min()
        };
        let member = self
            .members
            .values()
            .filter_map(|entry| {
                if entry.ack_due {
                    return Some(now);
                }
                let asking = match entry.member.access() {
                    Access::Waiting => true,
                    Access::Removed => entry.rejoin,
                    Access::Ready { .. } => false,
                };
                asking.then_some(entry.retry.due)
            })
            .min();
        steward.into_iter().chain(member).min()
    }

    /// When a steward next owes a delivery that no message brings, after a
    /// pass at `now`: a frame a member has not acknowledged, on that
    /// member's backoff, or a genesis announcement the transport did not
    /// accept, retried every [`RECONCILE_INTERVAL`] as before.
    fn next_redelivery(&self, now: Instant) -> Option<Instant> {
        let retry = now.checked_add(RECONCILE_INTERVAL).unwrap_or(now);
        self.stewards
            .values()
            .filter_map(|entry| {
                let genesis = (!entry.announced && !entry.steward.journal().frames().is_empty())
                    .then_some(retry);
                let frames = entry.redelivery.values().map(|backoff| backoff.due).min();
                genesis.into_iter().chain(frames).min()
            })
            .min()
    }

    /// Whether stewards wait for their directory to announce a policy
    /// change: there are some, the directory announces, and nothing changed
    /// since their last re-check. The host's pump then sleeps until an
    /// announcement wakes it ([`ChannelKeyAccess::arm_policy_watch`]).
    fn waits_for_policy_change(&self) -> bool {
        !self.stewards.is_empty()
            && self
                .directory
                .policy_epoch()
                .is_some_and(|epoch| self.reconcile.waits_for_change(epoch))
    }

    /// Register the first instant after `wall` at which the access policy of
    /// a stewarded room can change by time alone, so the stewards re-check
    /// then ([`tilo_identity_host::policy_change::wake_at`]).
    fn register_next_policy_change(&self, wall: u64) {
        let rooms: BTreeSet<RoomId> = self.stewards.values().map(|entry| entry.room).collect();
        let next = rooms
            .into_iter()
            .filter_map(|room| self.directory.next_policy_change(room, wall))
            .filter(|at| *at > wall)
            .min();
        if let Some(at) = next {
            tilo_identity_host::policy_change::wake_at(at.saturating_mul(1_000_000_000));
        }
    }

    fn receive(&mut self, inbound: InboundChannelGovernance, now: Instant, wall: u64) {
        match inbound {
            InboundChannelGovernance::Frame { room, frame } => {
                self.receive_frame(room, *frame, now);
            }
            InboundChannelGovernance::AccessRequest { room, request } => {
                self.receive_request(room, *request, wall);
            }
            InboundChannelGovernance::Ack { room, ack } => self.receive_ack(room, &ack, now),
        }
    }

    fn receive_frame(&mut self, room: RoomId, frame: MlsGovernanceFrameV1, now: Instant) {
        let channel = frame.channel;
        let Some(entry) = self.members.get_mut(&channel) else {
            return;
        };
        if entry.room != room {
            return;
        }
        let events = match entry.member.receive(frame, &self.config.vault) {
            Ok(events) => events,
            Err(error) => {
                tracing::debug!(%error, %room, "channel frame refused");
                return;
            }
        };
        entry.ack_due = true;
        let mut changed = false;
        for event in events {
            match event {
                MemberEvent::Joined { .. }
                | MemberEvent::Advanced { .. }
                | MemberEvent::Removed => {
                    changed = true;
                }
                MemberEvent::Desynchronised => {
                    // Ask again with a fresh key package; the steward
                    // re-admits a member whose newer request it sees.
                    entry.request = None;
                    entry.retry = Backoff::now(now, REQUEST_FIRST);
                    changed = true;
                }
                MemberEvent::Duplicate | MemberEvent::Buffered | MemberEvent::Recorded => {}
            }
        }
        let gate = entry.member.journal().gate();
        self.register_gate(channel, gate);
        if changed {
            self.bump(channel);
        }
    }

    fn receive_request(&mut self, room: RoomId, request: MlsAccessRequestV1, wall: u64) {
        if !self.directory.governs(room) || request.verify().is_err() {
            return;
        }
        let channel = request.channel;
        if !self.stewards.contains_key(&channel) {
            // Only a device the policy allows can make this device create a
            // group, so outsiders cannot make it mint unbounded state.
            if !self
                .directory
                .allowed_devices(room, wall)
                .contains(&request.device)
            {
                return;
            }
            if let Err(error) = self.open_steward(room, channel) {
                tracing::warn!(%error, %room, "channel group could not be created");
                return;
            }
        }
        let Some(entry) = self.stewards.get_mut(&channel) else {
            return;
        };
        if entry.room != room {
            return;
        }
        let newer = entry
            .requests
            .get(&request.device)
            .is_none_or(|known| request.issued_at > known.issued_at);
        if newer {
            entry.requests.insert(request.device, request);
            entry.dirty = true;
            let path = requests_path(&self.config.root, room, channel);
            if let Err(error) = save_requests(&path, &entry.requests) {
                tracing::warn!(%error, %room, "channel access requests could not be saved");
            }
        }
    }

    fn receive_ack(&mut self, room: RoomId, ack: &MlsGovernanceAckV1, now: Instant) {
        let Some(entry) = self.stewards.get_mut(&ack.channel) else {
            return;
        };
        if entry.room != room {
            return;
        }
        match entry.steward.acknowledge(ack) {
            Ok(completed) if completed > 0 => {
                // Progress: the next redelivery, if any is still owed,
                // starts from the shortest wait again.
                entry
                    .redelivery
                    .insert(ack.device, Backoff::now(now, REDELIVERY_FIRST));
            }
            Ok(_) => {}
            Err(error) => tracing::debug!(%error, %room, "channel acknowledgement refused"),
        }
    }

    fn reconcile(&mut self, channel: ChannelId, now: Instant, wall: u64) {
        let Some(room) = self.stewards.get(&channel).map(|entry| entry.room) else {
            return;
        };
        let mut allowed = self.directory.allowed_devices(room, wall);
        allowed.insert(self.device());
        let Some(entry) = self.stewards.get_mut(&channel) else {
            return;
        };
        entry.dirty = false;
        let outcome = match entry
            .steward
            .reconcile(&allowed, &entry.requests, &self.config.vault)
        {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::warn!(%error, %room, "channel membership change failed");
                return;
            }
        };
        if !outcome.refused.is_empty() {
            tracing::warn!(%room, refused = outcome.refused.len(), "malformed channel access requests refused");
        }
        if outcome.changed() {
            // Every device owed the new frames hears about them now rather
            // than at its backoff deadline.
            for outgoing in entry.steward.outbox() {
                entry
                    .redelivery
                    .insert(outgoing.recipient, Backoff::now(now, REDELIVERY_FIRST));
            }
            let gate = entry.steward.journal().gate();
            self.register_gate(channel, gate);
            self.bump(channel);
        }
    }

    fn redeliver(&mut self, channel: ChannelId, transport: &dyn GovernanceTransport, now: Instant) {
        let Some(entry) = self.stewards.get_mut(&channel) else {
            return;
        };
        if !entry.announced
            && let Some(genesis) = entry.steward.journal().frames().first().cloned()
        {
            self.delivery = self.delivery.wrapping_add(1);
            entry.announced = transport.publish(NeighbourMsg::MlsGovernance {
                room: entry.room.to_string(),
                frame: Box::new(genesis),
                delivery: self.delivery,
            });
            tracing::debug!(room = %entry.room, ?channel, accepted = entry.announced, "channel genesis announced to the room");
        }
        let outbox = entry.steward.outbox();
        let owed: BTreeSet<Ed25519PublicKey> =
            outbox.iter().map(|outgoing| outgoing.recipient).collect();
        entry.redelivery.retain(|device, _| owed.contains(device));
        let mut sent = BTreeSet::new();
        for outgoing in outbox {
            let backoff = entry
                .redelivery
                .entry(outgoing.recipient)
                .or_insert_with(|| Backoff::now(now, REDELIVERY_FIRST));
            if !backoff.is_due(now) {
                continue;
            }
            backoff.fired(now, REDELIVERY_LONGEST);
            for frame in outgoing.frames {
                let Ok(digest) = frame.digest() else {
                    continue;
                };
                if !sent.insert(digest) {
                    continue;
                }
                self.delivery = self.delivery.wrapping_add(1);
                let _ = transport.publish(NeighbourMsg::MlsGovernance {
                    room: entry.room.to_string(),
                    frame: Box::new(frame),
                    delivery: self.delivery,
                });
            }
        }
    }

    fn drive_member(
        &mut self,
        channel: ChannelId,
        transport: &dyn GovernanceTransport,
        now: Instant,
        wall: u64,
    ) {
        let Some(entry) = self.members.get_mut(&channel) else {
            return;
        };
        if entry.ack_due {
            entry.ack_due = false;
            match entry.member.acknowledgement(&self.config.device_key) {
                Ok(Some(ack)) => {
                    self.delivery = self.delivery.wrapping_add(1);
                    let _ = transport.publish(NeighbourMsg::MlsGovernanceAck {
                        room: entry.room.to_string(),
                        ack: Box::new(ack),
                        delivery: self.delivery,
                    });
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "channel acknowledgement could not be signed"),
            }
        }
        let asking = match entry.member.access() {
            Access::Waiting => true,
            Access::Removed => entry.rejoin,
            Access::Ready { .. } => false,
        };
        if !asking || !entry.retry.is_due(now) {
            return;
        }
        if entry.request.is_none() {
            match entry
                .member
                .access_request(&self.config.device_key, wall, &self.config.vault)
            {
                Ok(request) => {
                    entry.request = request;
                    entry.rejoin = false;
                }
                Err(error) => {
                    tracing::warn!(%error, "channel access request could not be prepared");
                    return;
                }
            }
        }
        let Some(request) = entry.request.clone() else {
            return;
        };
        entry.retry.fired(now, REQUEST_LONGEST);
        self.delivery = self.delivery.wrapping_add(1);
        let _ = transport.publish(NeighbourMsg::MlsAccessRequest {
            room: entry.room.to_string(),
            request: Box::new(request),
            delivery: self.delivery,
        });
    }

    fn open_steward(&mut self, room: RoomId, channel: ChannelId) -> channel_crypto::Result<()> {
        let dir = self.config.root.join(room.to_string()).join("steward");
        create_private_dir(&dir)?;
        let steward = ChannelSteward::open_or_create(
            &dir,
            channel,
            self.config.device_key.clone(),
            &self.config.vault,
        )?;
        let requests = load_requests(&requests_path(&self.config.root, room, channel));
        let gate = steward.journal().gate();
        self.stewards.insert(
            channel,
            StewardEntry {
                room,
                steward,
                requests,
                redelivery: HashMap::new(),
                dirty: true,
                announced: false,
            },
        );
        self.register_gate(channel, gate);
        self.bump(channel);
        Ok(())
    }

    /// Open (or start) the member view. `Ok(false)` when the room's steward
    /// is not known yet.
    fn open_member(
        &mut self,
        room: RoomId,
        channel: ChannelId,
        now: Instant,
    ) -> channel_crypto::Result<bool> {
        let Some(steward) = self.directory.steward_of(room) else {
            return Ok(false);
        };
        let dir = self.config.root.join(room.to_string()).join("member");
        create_private_dir(&dir)?;
        let member =
            ChannelMember::open(&dir, channel, self.device(), steward, &self.config.vault)?;
        let gate = member.journal().gate();
        self.members.insert(
            channel,
            MemberEntry {
                room,
                member,
                request: None,
                retry: Backoff::now(now, REQUEST_FIRST),
                ack_due: false,
                rejoin: false,
            },
        );
        self.register_gate(channel, gate);
        Ok(true)
    }

    fn register_gate(&self, channel: ChannelId, gate: channel_crypto::ChannelGate) {
        if !gate.encryption_required {
            return;
        }
        if let Ok(mut broker) = self.broker.lock() {
            broker.register_channel_gate(channel, gate);
        }
    }

    /// Record that `channel`'s keys or access changed, and announce it.
    fn bump(&mut self, channel: ChannelId) {
        self.generation = self.generation.wrapping_add(1);
        self.changed.insert(channel, self.generation);
        self.changes.send_replace(self.generation);
    }
}

/// How long a pumping thread parks after a pass at `now`, given the
/// service's [`ChannelEncryption::next_pump_deadline`]: until the deadline,
/// or [`RECONCILE_INTERVAL`] when the pass left work due that it could not
/// do, so a failing step is retried at the cadence it always had instead of
/// spinning.
fn wait_after_pass(deadline: Option<Instant>, now: Instant) -> Option<Duration> {
    deadline.map(|deadline| match deadline.checked_duration_since(now) {
        Some(wait) if !wait.is_zero() => wait,
        _ => RECONCILE_INTERVAL,
    })
}

/// Wakes a host's parked governance pump.
pub type PumpWake = Arc<dyn Fn() + Send + Sync>;

/// Cheap shared handle for the identity service and the broker.
#[derive(Clone)]
pub struct ChannelKeyAccess {
    inner: Arc<Mutex<ChannelEncryption>>,
    wake: Option<PumpWake>,
}

impl ChannelKeyAccess {
    pub fn new(service: ChannelEncryption) -> Self {
        Self {
            inner: Arc::new(Mutex::new(service)),
            wake: None,
        }
    }

    /// As [`Self::new`], for a host that parks its pump while
    /// [`Self::has_live_channels`] is false: `wake` runs whenever an
    /// endpoint's access queues governance the next pump must send, so a
    /// first channel is announced or requested without waiting for a timer.
    pub fn with_wake(service: ChannelEncryption, wake: PumpWake) -> Self {
        Self {
            inner: Arc::new(Mutex::new(service)),
            wake: Some(wake),
        }
    }

    /// See [`ChannelEncryption::access`]; also returns the service's
    /// generation, which changes whenever any channel's keys or access do.
    pub fn access(&self, room: RoomId, channel: ChannelId) -> (ChannelAccess, u64) {
        let Ok(mut service) = self.inner.lock() else {
            return (ChannelAccess::Unavailable, 0);
        };
        let access = service.access(room, channel, Instant::now());
        let generation = service.generation();
        let queued = std::mem::take(&mut service.queued);
        drop(service);
        if queued && let Some(wake) = &self.wake {
            wake();
        }
        (access, generation)
    }

    /// Seal an ephemeral frame only with a key this device already holds for
    /// this exact room and channel. Observation never requests channel access,
    /// creates a group or rejoins a removed member. The caller must already
    /// have validated the publisher's current room admission and record route.
    ///
    /// # Errors
    ///
    /// Refuses missing, waiting or removed channels, a mismatched room, an
    /// unavailable service, or a sealing failure.
    pub fn seal_existing_live(
        &self,
        room: RoomId,
        channel: ChannelId,
        artifact: ArtifactId,
        plaintext: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        let service = self
            .inner
            .lock()
            .map_err(|_| "channel service unavailable")?;
        let keys = service
            .existing_keys(room, channel)
            .ok_or("no current key for the admitted record")?;
        let sealed = keys
            .seal_content(artifact, ContentRetention::Delta, plaintext)
            .map_err(|error| error.to_string());
        drop(service);
        sealed
    }

    /// Open an ephemeral frame only with a key this device already holds for
    /// this exact room and channel. Observation never requests channel access,
    /// creates a group or rejoins a removed member.
    ///
    /// # Errors
    ///
    /// Refuses a mismatched metadata channel, missing/waiting/removed keys,
    /// an unavailable service, or an authentication failure.
    pub fn open_existing_live(
        &self,
        room: RoomId,
        channel: ChannelId,
        artifact: ArtifactId,
        metadata: &EncryptionMetadata,
        payload: &[u8],
    ) -> Result<Vec<u8>, String> {
        if metadata.channel != channel {
            return Err("live frame channel changed".into());
        }
        let service = self
            .inner
            .lock()
            .map_err(|_| "channel service unavailable")?;
        let keys = service
            .existing_keys(room, channel)
            .ok_or("no current key for the admitted record")?;
        drop(service);
        keys.open_current_content(artifact, metadata, payload)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    }

    /// Open `envelope`, a new sealed write to one of `room`'s channels, for
    /// the durable owner that holds the channel's keys. Only under the
    /// channel's current epoch, and only while this device reads the
    /// channel; anything else is refused. Call it only once the envelope's
    /// author, role, route and signature were verified.
    ///
    /// # Errors
    ///
    /// Returns a message when the write is not sealed, this device holds no
    /// current key for its channel, or the payload does not open.
    pub fn open_new_write(&self, room: RoomId, envelope: &SignedOp) -> Result<Vec<u8>, String> {
        let (keys, metadata, ciphertext) = self.sealed_parts(room, envelope)?;
        keys.open_current_content(envelope.artifact, &metadata, &ciphertext)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    }

    /// Open `envelope`, a write of a sealed record's history (a compaction
    /// included), under any epoch whose key this device holds: what a key
    /// holder compacting the history reads. Call it only for writes already
    /// admitted with every envelope check.
    ///
    /// # Errors
    ///
    /// As [`Self::open_new_write`], except that an older epoch opens.
    pub fn open_history_write(&self, room: RoomId, envelope: &SignedOp) -> Result<Vec<u8>, String> {
        let (keys, metadata, ciphertext) = self.sealed_parts(room, envelope)?;
        keys.open_content(envelope.artifact, &metadata, &ciphertext)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    }

    /// Seal `plaintext`, a record's merged snapshot, for `artifact` on
    /// `channel` under the channel's current epoch as a checkpoint: the
    /// ciphertext of a sealed compaction.
    ///
    /// # Errors
    ///
    /// Returns a message when this device holds no current key for the
    /// channel or sealing fails.
    pub fn seal_current_checkpoint(
        &self,
        room: RoomId,
        channel: ChannelId,
        artifact: ArtifactId,
        plaintext: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        let (ChannelAccess::Ready(keys), _) = self.access(room, channel) else {
            return Err("this device holds no key for the record's channel".into());
        };
        keys.seal_content(artifact, ContentRetention::Checkpoint, plaintext)
            .map_err(|error| error.to_string())
    }

    fn sealed_parts<'a>(
        &self,
        room: RoomId,
        envelope: &'a SignedOp,
    ) -> Result<(ChannelContentKeys, EncryptionMetadata, Cow<'a, [u8]>), String> {
        let metadata = envelope.encryption.ok_or("the write is not sealed")?;
        let ciphertext =
            collab_types::sealed_ciphertext(envelope).map_err(|error| error.to_string())?;
        let (ChannelAccess::Ready(keys), _) = self.access(room, metadata.channel) else {
            return Err("this device holds no key for the write's channel".into());
        };
        Ok((keys, metadata, ciphertext))
    }

    /// The payload a renewal of `envelope`, a sealed write of this device
    /// to one of `room`'s channels, carries under a fresh timestamp and
    /// sequence: the identical ciphertext while its epoch is the channel's
    /// current one, else its content re-sealed under the current epoch. The
    /// plaintext never leaves this call.
    ///
    /// # Errors
    ///
    /// Returns a message when the write is not sealed with a content key,
    /// this device holds no current key for its channel, or the payload
    /// does not open.
    pub fn renew_sealed(
        &self,
        room: RoomId,
        envelope: &SignedOp,
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        let metadata = envelope.encryption.ok_or("the write is not sealed")?;
        let retention = metadata
            .retention()
            .ok_or("the write is not sealed with a content key")?;
        let (ChannelAccess::Ready(keys), _) = self.access(room, metadata.channel) else {
            return Err("this device holds no key for the write's channel".into());
        };
        let plaintext = keys
            .open_content(envelope.artifact, &metadata, &envelope.payload)
            .map_err(|error| error.to_string())?;
        if metadata.epoch == keys.epoch() {
            return Ok((metadata, envelope.payload.clone()));
        }
        keys.seal_content(envelope.artifact, retention, &plaintext)
            .map_err(|error| error.to_string())
    }

    /// See [`ChannelEncryption::pump`].
    pub fn pump(&self, transport: &dyn GovernanceTransport) {
        let _ = self.pump_until_due(transport);
    }

    /// [`Self::pump`], then how long until the protocol next has work that
    /// no message brings ([`ChannelEncryption::next_pump_deadline`]); `None`
    /// while it has none, and the caller parks until a governance message
    /// arrives or the wake given to [`Self::with_wake`] fires.
    ///
    /// Work this pass found due and still left due (an access request that
    /// could not be prepared) is retried after [`RECONCILE_INTERVAL`], the
    /// cadence every pass had while the caller woke on a fixed timer.
    #[must_use]
    pub fn pump_until_due(&self, transport: &dyn GovernanceTransport) -> Option<Duration> {
        let mut service = self.inner.lock().ok()?;
        let now = Instant::now();
        service.pump(transport, now, tilo_clock::unix_now());
        let deadline = service.next_pump_deadline(now);
        drop(service);
        wait_after_pass(deadline, now)
    }

    /// Arm `watch` (whose wake pokes this host's pump) when stewards wait
    /// for a policy announcement, so a revocation, a role change or an
    /// expiry re-checks their groups with no timer running. Returns `false`
    /// when a change already arrived that the arming will not wake for: the
    /// caller pumps again now instead of parking.
    ///
    /// A device stewarding no channel never arms it, and so sleeps through
    /// every announcement.
    pub fn arm_policy_watch(&self, watch: &tilo_identity_host::policy_change::Watch) -> bool {
        let Ok(service) = self.inner.lock() else {
            return true;
        };
        if !service.waits_for_policy_change() {
            return true;
        }
        let _ = watch.arm();
        let unchanged = service.directory.policy_epoch() == Some(service.reconcile.epoch());
        drop(service);
        unchanged
    }

    /// Whether a timer still has to pump governance. An idle device with no
    /// channel parks the drainer until the next inbound frame.
    pub fn has_live_channels(&self) -> bool {
        self.inner
            .lock()
            .is_ok_and(|service| !service.stewards.is_empty() || !service.members.is_empty())
    }

    /// See [`ChannelEncryption::generation`].
    pub fn generation(&self) -> u64 {
        self.inner.lock().map_or(0, |service| service.generation())
    }

    /// See [`ChannelEncryption::channel_change`].
    pub fn channel_change(&self, channel: ChannelId) -> u64 {
        self.inner
            .lock()
            .map_or(0, |service| service.channel_change(channel))
    }

    /// Wakes whenever any channel's keys or access change; `None` when the
    /// service is unavailable after a poisoned lock.
    fn changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.inner
            .lock()
            .ok()
            .map(|service| service.changes.subscribe())
    }
}

/// Where the identity service finds this session's channel encryption
/// service. The broker that owns the service can start after the identity
/// service (onboarding defers it), so the slot is filled when it does.
#[derive(Clone, Default)]
pub struct ChannelKeyProvider {
    slot: Arc<Mutex<Option<ChannelKeyAccess>>>,
    /// Counts [`Self::set`] calls, for whoever waits on the service.
    installed: Arc<tokio::sync::watch::Sender<u64>>,
}

impl ChannelKeyProvider {
    /// The slot of this session: its compositor starts one collaboration
    /// broker and one identity service, which meet here.
    pub fn session() -> Self {
        static SESSION: std::sync::OnceLock<ChannelKeyProvider> = std::sync::OnceLock::new();
        SESSION.get_or_init(Self::default).clone()
    }

    /// Point the slot at a broker's service (or at none).
    pub fn set(&self, access: Option<ChannelKeyAccess>) {
        if let Ok(mut slot) = self.slot.lock() {
            *slot = access;
        }
        self.installed
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// The current service, if a broker with one is running.
    pub fn get(&self) -> Option<ChannelKeyAccess> {
        self.slot.lock().ok().and_then(|slot| slot.clone())
    }

    /// Wait until `channel`'s keys or this device's access to them may have
    /// changed since `after`, the value the previous wait returned (0 at
    /// first), and return the value to wait from next.
    ///
    /// Resolves at once when the channel changed since `after`; otherwise
    /// when it next changes, or when the service is installed, replaced or
    /// goes away. It reports only that this one channel changed, nothing
    /// about its keys or any other channel: the endpoint then asks the
    /// identity service, which checks the device's trust and the person's
    /// role on every answer. Costs no wakeup while nothing changes.
    pub async fn channel_changed(&self, channel: ChannelId, after: u64) -> u64 {
        // Subscribed before looking, so an installation between the two is
        // still seen.
        let mut installed = self.installed.subscribe();
        let current = |provider: &Self| {
            provider
                .get()
                .map_or(0, |access| access.channel_change(channel))
        };
        let Some((access, mut changes)) = self
            .get()
            .and_then(|access| access.changes().map(|changes| (access, changes)))
        else {
            // No service: nothing changes until one is installed. The
            // installation counter lives as long as `self`.
            let _ = installed.changed().await;
            return current(self);
        };
        loop {
            let change = access.channel_change(channel);
            if change != after {
                return change;
            }
            tokio::select! {
                outcome = changes.changed() => {
                    if outcome.is_err() {
                        // The service was dropped: whatever replaces it
                        // answers differently.
                        return change.wrapping_add(1);
                    }
                }
                _ = installed.changed() => return current(self),
            }
        }
    }
}

/// The identity service's route keys, from this host's channel service once
/// one runs.
impl tilo_identity_host::broker::ChannelKeySource for ChannelKeyProvider {
    fn access(
        &self,
        room: RoomId,
        channel: ChannelId,
        known_generation: u64,
    ) -> Option<Result<tilo_identity_host::broker::ChannelKeys, String>> {
        use runtime_identity::wire::ChannelAccess as Wire;
        let channels = self.get()?;
        let (access, generation) = channels.access(room, channel);
        Some(match access {
            ChannelAccess::Ready(keys) if generation != known_generation => keys
                .to_bytes()
                .map(|bytes| (Wire::Ready, generation, bytes.to_vec()))
                .map_err(|error| error.to_string()),
            ChannelAccess::Ready(_) => Ok((Wire::Ready, generation, Vec::new())),
            ChannelAccess::Waiting => Ok((Wire::Waiting, generation, Vec::new())),
            ChannelAccess::Removed => Ok((Wire::Removed, generation, Vec::new())),
            ChannelAccess::Unavailable => Ok((Wire::Unavailable, generation, Vec::new())),
        })
    }
}

fn requests_path(state_root: &Path, room: RoomId, channel: ChannelId) -> PathBuf {
    state_root
        .join(room.to_string())
        .join("steward")
        .join(format!("{}.requests", channel.0))
}

fn load_requests(path: &Path) -> BTreeMap<Ed25519PublicKey, MlsAccessRequestV1> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| postcard::from_bytes(&bytes).ok())
        .unwrap_or_default()
}

fn save_requests(
    path: &Path,
    requests: &BTreeMap<Ed25519PublicKey, MlsAccessRequestV1>,
) -> std::io::Result<()> {
    let bytes = postcard::to_allocvec(requests).map_err(std::io::Error::other)?;
    tilo_files::native::atomic::write_private(path, &bytes)
}

/// Channel ids of the `*.{extension}` files in `dir`.
fn channels_in(dir: &Path, extension: &str) -> Vec<ChannelId> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some(extension) {
                return None;
            }
            let stem = path.file_stem()?.to_str()?;
            uuid::Uuid::parse_str(stem).ok().map(ChannelId)
        })
        .collect()
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Fixtures other hosts' channel tests share: an in-memory room transport
/// and a fixed access policy.
#[cfg(any(test, feature = "test-support"))]
#[allow(
    clippy::wildcard_imports,
    clippy::missing_panics_doc,
    clippy::unwrap_used,
    reason = "test fixtures: a poisoned lock is the test failing"
)]
pub mod testing {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// An in-memory room: every endpoint publishes to all the others while
    /// the room is up, and loses what it publishes while it is down.
    #[derive(Default)]
    pub struct Room {
        pub queues: Mutex<HashMap<usize, Vec<InboundChannelGovernance>>>,
        pub down: AtomicBool,
    }

    pub struct Endpoint {
        pub room: Arc<Room>,
        pub id: usize,
        pub room_id: RoomId,
    }

    impl Endpoint {
        /// Endpoint `id` of `room`, joined to its broadcast.
        pub fn join(room: &Arc<Room>, room_id: RoomId, id: usize) -> Self {
            room.queues.lock().unwrap().entry(id).or_default();
            Self {
                room: Arc::clone(room),
                id,
                room_id,
            }
        }
    }

    impl GovernanceTransport for Endpoint {
        fn publish(&self, message: NeighbourMsg) -> bool {
            if self.room.down.load(Ordering::SeqCst) {
                return false;
            }
            let room = self.room_id;
            let inbound = match message {
                NeighbourMsg::MlsGovernance { frame, .. } => {
                    InboundChannelGovernance::Frame { room, frame }
                }
                NeighbourMsg::MlsAccessRequest { request, .. } => {
                    InboundChannelGovernance::AccessRequest { room, request }
                }
                NeighbourMsg::MlsGovernanceAck { ack, .. } => {
                    InboundChannelGovernance::Ack { room, ack }
                }
                _ => return false,
            };
            for (id, queue) in self.room.queues.lock().unwrap().iter_mut() {
                if *id != self.id {
                    queue.push(inbound.clone());
                }
            }
            true
        }

        fn take_inbound(&self) -> Vec<InboundChannelGovernance> {
            std::mem::take(self.room.queues.lock().unwrap().entry(self.id).or_default())
        }
    }

    #[derive(Clone, Default)]
    pub struct Policy {
        pub allowed: Arc<Mutex<BTreeSet<Ed25519PublicKey>>>,
        /// Devices allowed only until a wall-clock second: a grant that
        /// expires.
        pub until: Arc<Mutex<BTreeMap<Ed25519PublicKey, u64>>>,
        /// How the directory announces changes; `None` announces none, and
        /// stewards keep their one-second re-check.
        pub announces: Arc<Mutex<Option<Announcements>>>,
    }

    /// The epoch a test [`Directory`] reports as its
    /// [`ChannelDirectory::policy_epoch`].
    #[derive(Clone)]
    pub enum Announcements {
        /// [`tilo_identity_host::policy_change`]'s, as production reports.
        Process,
        /// The test's own, bumped by hand.
        Own(Arc<std::sync::atomic::AtomicU64>),
    }

    pub struct Directory {
        pub governs: bool,
        pub steward: Ed25519PublicKey,
        pub policy: Policy,
    }

    impl ChannelDirectory for Directory {
        fn governs(&self, _: RoomId) -> bool {
            self.governs
        }
        fn steward_of(&self, _: RoomId) -> Option<Ed25519PublicKey> {
            (!self.governs).then_some(self.steward)
        }
        fn allowed_devices(&self, _: RoomId, now: u64) -> BTreeSet<Ed25519PublicKey> {
            let mut allowed = self.policy.allowed.lock().unwrap().clone();
            allowed.extend(
                self.policy
                    .until
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, until)| now < **until)
                    .map(|(device, _)| *device),
            );
            allowed
        }
        fn policy_epoch(&self) -> Option<u64> {
            match &*self.policy.announces.lock().unwrap() {
                None => None,
                Some(Announcements::Process) => Some(tilo_identity_host::policy_change::epoch()),
                Some(Announcements::Own(epoch)) => Some(epoch.load(Ordering::SeqCst)),
            }
        }
        fn next_policy_change(&self, _: RoomId, now: u64) -> Option<u64> {
            self.policy
                .until
                .lock()
                .unwrap()
                .values()
                .copied()
                .filter(|until| *until > now)
                .min()
        }
    }

    pub fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    pub fn public(seed: u8) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(key(seed).verifying_key().to_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use std::sync::atomic::Ordering;

    struct Device {
        service: ChannelEncryption,
        broker: Arc<Mutex<BrokerChannels>>,
        endpoint: Endpoint,
    }

    /// Device `seed` of a room stewarded by device `steward`, with its
    /// state under `dir` and transport endpoint `id` of `room`.
    fn device(
        room: &Arc<Room>,
        room_id: RoomId,
        id: usize,
        (seed, steward): (u8, u8),
        policy: &Policy,
        dir: &Path,
    ) -> Device {
        room.queues.lock().unwrap().entry(id).or_default();
        let broker = Arc::new(Mutex::new(BrokerChannels::default()));
        let service = ChannelEncryption::open(
            ChannelEncryptionConfig {
                root: dir.join("channels"),
                vault: VaultKey::new([seed; 32]),
                device_key: key(seed),
            },
            Box::new(Directory {
                governs: seed == steward,
                steward: public(steward),
                policy: policy.clone(),
            }),
            Arc::clone(&broker),
        )
        .unwrap();
        Device {
            service,
            broker,
            endpoint: Endpoint {
                room: Arc::clone(room),
                id,
                room_id,
            },
        }
    }

    /// Run every device's protocol for `seconds` of simulated time.
    fn run(devices: &mut [&mut Device], now: &mut Instant, seconds: u64) {
        for _ in 0..seconds.saturating_mul(4) {
            *now = now.checked_add(Duration::from_millis(250)).unwrap();
            for device in devices.iter_mut() {
                device.service.pump(&device.endpoint, *now, 1_000);
            }
        }
    }

    fn ready_epoch(access: &ChannelAccess) -> Option<u64> {
        match access {
            ChannelAccess::Ready(keys) => Some(keys.epoch()),
            _ => None,
        }
    }

    fn refuses_plaintext(device: &Device, channel: ChannelId) -> bool {
        !device
            .broker
            .lock()
            .unwrap()
            .document_encryption_allowed(channel, None)
    }

    #[test]
    fn observing_live_keys_never_opens_a_channel_or_rejoins_after_removal() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let channel = collab_types::document_channel_id("so.tilo.chat", &room_id.to_string(), "");
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(public(2));
        let (owner_dir, member_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut owner = device(&room, room_id, 0, (1, 1), &policy, owner_dir.path());
        let mut member = device(&room, room_id, 1, (2, 1), &policy, member_dir.path());
        for service in [&owner.service, &member.service] {
            let generation = service.generation();
            assert!(service.existing_keys(room_id, channel).is_none());
            assert!(service.stewards.is_empty());
            assert!(service.members.is_empty());
            assert_eq!(service.generation(), generation);
        }

        // Only this explicit fixture admission creates or requests keys.
        let mut now = Instant::now();
        owner.service.access(room_id, channel, now);
        member.service.access(room_id, channel, now);
        run(&mut [&mut owner, &mut member], &mut now, 5);
        assert!(member.service.existing_keys(room_id, channel).is_some());
        assert!(
            member
                .service
                .existing_keys(RoomId::new(), channel)
                .is_none()
        );
        assert!(
            owner
                .service
                .existing_keys(RoomId::new(), channel)
                .is_none()
        );

        policy.allowed.lock().unwrap().clear();
        run(&mut [&mut owner, &mut member], &mut now, 3);
        let generation = member.service.generation();
        let rejoin = member.service.members.get(&channel).unwrap().rejoin;
        assert!(!rejoin);
        assert!(member.service.existing_keys(room_id, channel).is_none());
        assert_eq!(member.service.generation(), generation);
        assert!(!member.service.members.get(&channel).unwrap().rejoin);
        let access = ChannelKeyAccess::new(member.service);
        assert!(
            access
                .seal_existing_live(room_id, channel, ArtifactId::new(), b"semantic event")
                .is_err()
        );
        assert!(
            !access
                .inner
                .lock()
                .unwrap()
                .members
                .get(&channel)
                .unwrap()
                .rejoin
        );
    }

    #[test]
    fn opening_a_channel_wakes_a_parked_pump_and_reading_it_does_not() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let dir = tempfile::tempdir().unwrap();
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&wakes);
        let owner = device(&room, room_id, 0, (1, 1), &Policy::default(), dir.path());
        let access = ChannelKeyAccess::with_wake(
            owner.service,
            Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        );
        assert!(!access.has_live_channels());
        let channel = ChannelId::new();
        access.access(room_id, channel);
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "a new steward group has a genesis to announce"
        );
        assert!(access.has_live_channels());
        access.access(room_id, channel);
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "an open channel queues nothing new"
        );

        let member_dir = tempfile::tempdir().unwrap();
        let member = device(
            &room,
            room_id,
            1,
            (2, 1),
            &Policy::default(),
            member_dir.path(),
        );
        let member_wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&member_wakes);
        let member = ChannelKeyAccess::with_wake(
            member.service,
            Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        );
        assert!(matches!(
            member.access(room_id, channel).0,
            ChannelAccess::Waiting
        ));
        assert_eq!(
            member_wakes.load(Ordering::SeqCst),
            1,
            "a new member has an access request to make"
        );
        member.access(room_id, channel);
        assert_eq!(member_wakes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn live_sealing_requires_an_existing_key_for_the_exact_room() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let channel = ChannelId::new();
        let dir = tempfile::tempdir().unwrap();
        let owner = device(&room, room_id, 0, (1, 1), &Policy::default(), dir.path());
        let access = ChannelKeyAccess::new(owner.service);
        let artifact = ArtifactId::new();
        let missing_metadata =
            EncryptionMetadata::content_key(1, channel, collab_types::ContentRetention::Delta);
        assert!(
            access
                .seal_existing_live(room_id, channel, artifact, b"event")
                .is_err()
        );
        assert!(
            access
                .open_existing_live(
                    room_id,
                    channel,
                    artifact,
                    &missing_metadata,
                    b"not ciphertext",
                )
                .is_err()
        );
        assert!(access.inner.lock().unwrap().stewards.is_empty());
        assert!(access.inner.lock().unwrap().members.is_empty());
        let (ChannelAccess::Ready(keys), _) = access.access(room_id, channel) else {
            panic!("explicit fixture admission must provide keys");
        };
        let generation = access.generation();
        let (metadata, sealed) = access
            .seal_existing_live(room_id, channel, artifact, b"event")
            .unwrap();
        assert_eq!(
            access
                .open_existing_live(room_id, channel, artifact, &metadata, &sealed)
                .unwrap(),
            b"event"
        );
        let wrong_channel = ChannelId::new();
        assert!(
            access
                .open_existing_live(room_id, wrong_channel, artifact, &metadata, &sealed,)
                .is_err(),
            "a held key for one record cannot open another record's channel"
        );
        assert!(
            !access
                .inner
                .lock()
                .unwrap()
                .members
                .contains_key(&wrong_channel),
            "a live frame cannot request or rejoin a channel"
        );
        assert_eq!(
            keys.open_current_content(artifact, &metadata, &sealed)
                .unwrap()
                .as_slice(),
            b"event"
        );
        assert_eq!(access.generation(), generation);
        assert!(
            access
                .seal_existing_live(RoomId::new(), channel, artifact, b"event")
                .is_err()
        );
    }

    #[test]
    fn a_new_channel_is_encrypted_from_creation_and_follows_the_policy() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let channel = ChannelId::new();
        let policy = Policy::default();
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&room, room_id, 0, (1, 1), &policy, owner_dir.path());
        let mut laptop = device(&room, room_id, 1, (2, 1), &policy, laptop_dir.path());

        assert!(ready_epoch(&owner.service.access(room_id, channel, now)).is_some());
        assert!(
            refuses_plaintext(&owner, channel),
            "encrypted from creation"
        );
        assert!(matches!(
            laptop.service.access(room_id, channel, now),
            ChannelAccess::Waiting
        ));

        // Not allowed yet: the request waits at the steward.
        run(&mut [&mut owner, &mut laptop], &mut now, 5);
        assert!(matches!(
            laptop.service.access(room_id, channel, now),
            ChannelAccess::Waiting
        ));

        policy.allowed.lock().unwrap().insert(public(2));
        run(&mut [&mut owner, &mut laptop], &mut now, 5);
        let steward_epoch = ready_epoch(&owner.service.access(room_id, channel, now));
        assert_eq!(
            ready_epoch(&laptop.service.access(room_id, channel, now)),
            steward_epoch
        );
        assert!(refuses_plaintext(&laptop, channel));

        policy.allowed.lock().unwrap().clear();
        run(&mut [&mut owner, &mut laptop], &mut now, 3);
        assert!(matches!(
            laptop.service.access(room_id, channel, now),
            ChannelAccess::Removed
        ));
    }

    /// Run every device's protocol for `seconds` of simulated time the way
    /// the broker's drain thread does (TIL-706): a device runs a pass only
    /// when a message waits for it or the wait its last pass returned has
    /// run out, and otherwise stays parked. `parked` carries each device's
    /// wait across calls (`None`: parked until a message); a test that calls
    /// [`ChannelEncryption::access`] wakes the device as the installed pump
    /// wake would. Returns the passes run.
    fn run_parked(
        devices: &mut [&mut Device],
        parked: &mut [Option<Instant>],
        now: &mut Instant,
        seconds: u64,
    ) -> usize {
        let end = now.checked_add(Duration::from_secs(seconds)).unwrap();
        let mut passes = 0_usize;
        loop {
            let mut ran = false;
            for (device, until) in devices.iter_mut().zip(parked.iter_mut()) {
                let inbound = device
                    .endpoint
                    .room
                    .queues
                    .lock()
                    .unwrap()
                    .get(&device.endpoint.id)
                    .is_some_and(|queue| !queue.is_empty());
                if !inbound && until.is_none_or(|until| until > *now) {
                    continue;
                }
                device.service.pump(&device.endpoint, *now, 1_000);
                *until = wait_after_pass(device.service.next_pump_deadline(*now), *now)
                    .map(|wait| now.checked_add(wait).unwrap());
                passes = passes.saturating_add(1);
                ran = true;
                assert!(passes < 10_000, "a parked pump must not spin");
            }
            if ran {
                continue;
            }
            match parked.iter().flatten().min() {
                Some(next) if *next <= end => *now = *next,
                _ => {
                    *now = end;
                    return passes;
                }
            }
        }
    }

    /// TIL-706: a member holding its keys has nothing to pump until a frame
    /// arrives; a steward re-checks its policy once a reconcile interval.
    #[test]
    fn only_a_steward_or_an_unfinished_member_has_a_pump_deadline() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let channel = ChannelId::new();
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(public(2));
        let (owner_dir, member_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&room, room_id, 0, (1, 1), &policy, owner_dir.path());
        let mut member = device(&room, room_id, 1, (2, 1), &policy, member_dir.path());
        assert_eq!(owner.service.next_pump_deadline(now), None, "no channel");
        assert_eq!(member.service.next_pump_deadline(now), None, "no channel");

        owner.service.access(room_id, channel, now);
        member.service.access(room_id, channel, now);
        assert_eq!(
            owner.service.next_pump_deadline(now),
            Some(now),
            "a new group reconciles at once"
        );
        assert_eq!(
            member.service.next_pump_deadline(now),
            Some(now),
            "a new member asks at once"
        );

        run(&mut [&mut owner, &mut member], &mut now, 5);
        assert!(ready_epoch(&member.service.access(room_id, channel, now)).is_some());
        assert_eq!(
            member.service.next_pump_deadline(now),
            None,
            "a member holding its keys parks until a frame arrives"
        );
        let reconcile = owner.service.next_pump_deadline(now).unwrap();
        assert!(reconcile > now.checked_sub(RECONCILE_INTERVAL).unwrap());
        assert!(reconcile <= now.checked_add(RECONCILE_INTERVAL).unwrap());
        assert_eq!(
            wait_after_pass(Some(now), now),
            Some(RECONCILE_INTERVAL),
            "work a pass left due is retried at the old cadence, not spun on"
        );
        assert_eq!(wait_after_pass(None, now), None);
    }

    /// TIL-706: a member the steward has not admitted asks again on its
    /// backoff, and is due exactly then.
    #[test]
    fn a_waiting_member_is_due_on_its_request_backoff() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let channel = ChannelId::new();
        let dir = tempfile::tempdir().unwrap();
        let mut member = device(&room, room_id, 1, (2, 1), &Policy::default(), dir.path());
        let now = Instant::now();
        assert!(matches!(
            member.service.access(room_id, channel, now),
            ChannelAccess::Waiting
        ));
        member.service.pump(&member.endpoint, now, 1_000);
        assert_eq!(
            member.service.next_pump_deadline(now),
            Some(now.checked_add(REQUEST_FIRST).unwrap())
        );
        let later = now.checked_add(REQUEST_FIRST).unwrap();
        member.service.pump(&member.endpoint, later, 1_000);
        assert_eq!(
            member.service.next_pump_deadline(later),
            Some(later.checked_add(REQUEST_FIRST.saturating_mul(2)).unwrap())
        );
    }

    /// TIL-706: devices that park between deadlines reach the same outcome
    /// in the same simulated time as devices pumped on a fixed tick:
    /// admission, then removal once the policy drops the member.
    #[test]
    fn parked_pumps_follow_the_policy_like_a_fixed_tick() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let channel = ChannelId::new();
        let policy = Policy::default();
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&room, room_id, 0, (1, 1), &policy, owner_dir.path());
        let mut laptop = device(&room, room_id, 1, (2, 1), &policy, laptop_dir.path());

        assert!(ready_epoch(&owner.service.access(room_id, channel, now)).is_some());
        assert!(matches!(
            laptop.service.access(room_id, channel, now),
            ChannelAccess::Waiting
        ));
        // Both opened a channel, which wakes their pumps.
        let mut parked = [Some(now), Some(now)];
        run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 5);
        assert!(matches!(
            laptop.service.access(room_id, channel, now),
            ChannelAccess::Waiting
        ));

        policy.allowed.lock().unwrap().insert(public(2));
        run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 5);
        let steward_epoch = ready_epoch(&owner.service.access(room_id, channel, now));
        assert_eq!(
            ready_epoch(&laptop.service.access(room_id, channel, now)),
            steward_epoch
        );
        assert_eq!(parked[1], None, "the admitted member parks");

        // Ten idle seconds cost the member nothing and the steward one
        // policy re-check a reconcile interval.
        let idle = run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 10);
        assert!(idle <= 11, "{idle} passes in ten idle seconds");

        policy.allowed.lock().unwrap().clear();
        run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 3);
        assert!(matches!(
            laptop.service.access(room_id, channel, now),
            ChannelAccess::Removed
        ));
    }

    /// TIL-707: with a directory that announces its changes, an idle steward
    /// has no deadline at all, and a revocation is enforced as soon as it is
    /// announced, with no clock having run in between.
    #[test]
    fn an_announced_revocation_is_enforced_at_once_and_idle_stewards_park() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let channel = ChannelId::new();
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let policy = Policy::default();
        *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::clone(&epoch)));
        policy.allowed.lock().unwrap().insert(public(2));
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&room, room_id, 0, (1, 1), &policy, owner_dir.path());
        let mut laptop = device(&room, room_id, 1, (2, 1), &policy, laptop_dir.path());
        assert!(ready_epoch(&owner.service.access(room_id, channel, now)).is_some());
        assert!(matches!(
            laptop.service.access(room_id, channel, now),
            ChannelAccess::Waiting
        ));
        let mut parked = [Some(now), Some(now)];
        run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 5);
        assert!(ready_epoch(&laptop.service.access(room_id, channel, now)).is_some());
        assert_eq!(parked, [None, None], "both park once the member holds keys");

        // A minute with nothing announced runs no pass at all.
        let idle = run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 60);
        assert_eq!(idle, 0, "an idle steward polls nothing");
        assert!(owner.service.waits_for_policy_change());

        // The revocation, announced. The steward is due at once, and one
        // pass removes the member.
        policy.allowed.lock().unwrap().clear();
        epoch.fetch_add(1, Ordering::SeqCst);
        assert!(!owner.service.waits_for_policy_change());
        assert_eq!(owner.service.next_pump_deadline(now), Some(now));
        parked[0] = Some(now); // what the armed watch's wake does
        run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 1);
        assert!(matches!(
            laptop.service.access(room_id, channel, now),
            ChannelAccess::Removed
        ));
    }

    /// TIL-707: announcements in quick succession cost at most one re-check
    /// a reconcile interval, and none waits longer than that.
    #[test]
    fn announcements_in_quick_succession_coalesce() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let channel = ChannelId::new();
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let policy = Policy::default();
        *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::clone(&epoch)));
        let dir = tempfile::tempdir().unwrap();
        let mut now = Instant::now();
        let mut owner = device(&room, room_id, 0, (1, 1), &policy, dir.path());
        assert!(ready_epoch(&owner.service.access(room_id, channel, now)).is_some());
        owner.service.pump(&owner.endpoint, now, 1_000);
        let reconciled = now;
        now = now.checked_add(Duration::from_millis(100)).unwrap();
        epoch.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            owner.service.next_pump_deadline(now),
            Some(reconciled.checked_add(RECONCILE_INTERVAL).unwrap()),
            "a change right after a re-check waits out the interval"
        );
    }

    /// TIL-707: a grant that expires while nothing else happens is enforced
    /// when it expires: the steward registered the expiry, the process clock
    /// announces it, and the pump's watch wakes it. In between, the steward
    /// stays asleep.
    #[test]
    fn a_grant_expiring_while_idle_is_enforced_at_its_expiry() {
        /// Waking, reconciling and the member applying the removal, on a
        /// loaded runner.
        const SCHEDULING: Duration = Duration::from_millis(500);
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let channel = ChannelId::new();
        let policy = Policy::default();
        *policy.announces.lock().unwrap() = Some(Announcements::Process);
        let expiry = tilo_clock::unix_now().saturating_add(3);
        policy.until.lock().unwrap().insert(public(2), expiry);
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let owner = device(&room, room_id, 0, (1, 1), &policy, owner_dir.path());
        let laptop = device(&room, room_id, 1, (2, 1), &policy, laptop_dir.path());
        let (owner_endpoint, laptop_endpoint) = (owner.endpoint, laptop.endpoint);
        let owner = ChannelKeyAccess::new(owner.service);
        let laptop = ChannelKeyAccess::new(laptop.service);
        let (woke, wakes) = std::sync::mpsc::channel();
        let woke = Mutex::new(woke);
        let watch = tilo_identity_host::policy_change::Watch::new(move || {
            let _ = woke.lock().unwrap().send(());
        });
        assert!(ready_epoch(&owner.access(room_id, channel).0).is_some());
        let joined = Instant::now();
        while ready_epoch(&laptop.access(room_id, channel).0).is_none() {
            let _ = owner.pump_until_due(&owner_endpoint);
            let _ = laptop.pump_until_due(&laptop_endpoint);
            assert!(
                joined.elapsed() < tilo_test_env::hang_bound(),
                "never joined"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        // Idle until the grant runs out, pumping only when woken.
        let mut owner_passes = 0_u32;
        loop {
            let due = owner.pump_until_due(&owner_endpoint);
            owner_passes = owner_passes.saturating_add(1);
            let _ = laptop.pump_until_due(&laptop_endpoint);
            if matches!(laptop.access(room_id, channel).0, ChannelAccess::Removed) {
                break;
            }
            if owner.arm_policy_watch(&watch) {
                let wait = due.unwrap_or_else(tilo_test_env::hang_bound);
                let _ = wakes.recv_timeout(wait);
            }
            assert!(
                tilo_clock::unix_now() <= expiry.saturating_add(5),
                "the expired grant was never enforced"
            );
        }
        let removed_at = tilo_clock::unix_now_ns();
        let expired_at = expiry.saturating_mul(1_000_000_000);
        assert!(removed_at >= expired_at, "removed before the grant ended");
        let late = Duration::from_nanos(removed_at.saturating_sub(expired_at));
        assert!(
            late <= RECONCILE_INTERVAL.saturating_add(SCHEDULING),
            "enforced {late:?} after the grant ended"
        );
        // The pass that joined, the expiry, and an announcement another test
        // of this process may have made: never one a second.
        assert!(
            owner_passes <= 4,
            "the steward ran {owner_passes} passes in an idle wait of about 2 s"
        );
    }

    /// TIL-598: an app endpoint waiting on its route's channel hears of its
    /// admission and its removal the moment this device's service records
    /// them, and of nothing that happens to another channel.
    #[test]
    fn a_route_hears_of_its_own_channel_changes_and_only_those() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let (channel, other) = (ChannelId::new(), ChannelId::new());
        let policy = Policy::default();
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&room, room_id, 0, (1, 1), &policy, owner_dir.path());
        let Device {
            service, endpoint, ..
        } = device(&room, room_id, 1, (2, 1), &policy, laptop_dir.path());
        let keys = ChannelKeyProvider::default();
        keys.set(Some(ChannelKeyAccess::new(service)));
        let laptop = keys.get().unwrap();
        assert!(ready_epoch(&owner.service.access(room_id, channel, now)).is_some());
        assert!(matches!(
            laptop.access(room_id, channel).0,
            ChannelAccess::Waiting
        ));
        let mut pump = |rounds: usize| {
            for _ in 0..rounds {
                now = now.checked_add(Duration::from_millis(250)).unwrap();
                owner.service.pump(&owner.endpoint, now, 1_000);
                laptop.pump(&endpoint);
            }
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // A wait, already parked on the service when this returns.
        let wait = |after: u64| {
            let keys = keys.clone();
            let waiting = runtime.spawn(async move { keys.channel_changed(channel, after).await });
            runtime.block_on(async { tokio::time::sleep(Duration::from_millis(20)).await });
            assert!(!waiting.is_finished(), "nothing changed since {after}");
            waiting
        };
        let resolved = |waiting: tokio::task::JoinHandle<u64>| {
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_millis(200), waiting)
                    .await
                    .ok()
                    .map(Result::unwrap)
            })
        };

        let before = laptop.channel_change(channel);
        let waiting = wait(before);
        pump(8);
        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(20)).await });
        assert!(!waiting.is_finished(), "not admitted, nothing changed");

        policy.allowed.lock().unwrap().insert(public(2));
        pump(8);
        let admitted = resolved(waiting).expect("admission is announced");
        assert_ne!(admitted, before);
        assert!(ready_epoch(&laptop.access(room_id, channel).0).is_some());

        let waiting = wait(admitted);
        assert!(matches!(
            laptop.access(room_id, other).0,
            ChannelAccess::Waiting
        ));
        pump(8);
        assert!(ready_epoch(&laptop.access(room_id, other).0).is_some());
        assert_ne!(laptop.channel_change(other), 0, "the other channel changed");
        let waiting = match resolved(waiting) {
            None => wait(admitted),
            Some(change) => panic!("woken by another channel's change ({change})"),
        };

        policy.allowed.lock().unwrap().clear();
        pump(8);
        assert!(resolved(waiting).is_some(), "removal is announced");
        assert!(matches!(
            laptop.access(room_id, channel).0,
            ChannelAccess::Removed
        ));
    }

    /// A device that is not a member yet (a browser, which learns nothing
    /// about the room's governance but what the host carries) learns from
    /// the room that the channel exists, is encrypted and who stewards it.
    #[test]
    fn a_new_channel_announces_its_genesis_to_the_room() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let channel = ChannelId::new();
        let policy = Policy::default();
        let owner_dir = tempfile::tempdir().unwrap();
        let mut now = Instant::now();
        let mut owner = device(&room, room_id, 0, (5, 5), &policy, owner_dir.path());
        let observer = Endpoint {
            room: Arc::clone(&room),
            id: 1,
            room_id,
        };
        room.queues.lock().unwrap().entry(1).or_default();
        assert!(ready_epoch(&owner.service.access(room_id, channel, now)).is_some());
        run(&mut [&mut owner], &mut now, 3);
        let genesis: Vec<_> = observer
            .take_inbound()
            .into_iter()
            .filter_map(|inbound| match inbound {
                InboundChannelGovernance::Frame { frame, .. } => Some(frame),
                _ => None,
            })
            .collect();
        assert_eq!(genesis.len(), 1, "announced once per session");
        assert_eq!(genesis[0].channel, channel);
        assert_eq!(genesis[0].signed_by, public(5));
        assert!(matches!(
            genesis[0].action,
            collab_types::MlsGovernanceAction::Genesis
        ));
    }

    #[test]
    fn delivery_survives_a_network_outage() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let channel = ChannelId::new();
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(public(4));
        let (owner_dir, phone_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&room, room_id, 0, (3, 3), &policy, owner_dir.path());
        let mut phone = device(&room, room_id, 1, (4, 3), &policy, phone_dir.path());
        owner.service.access(room_id, channel, now);
        phone.service.access(room_id, channel, now);
        // The request gets through, then the network fails before the
        // Welcome can.
        run(&mut [&mut phone], &mut now, 1);
        room.down.store(true, Ordering::SeqCst);
        run(&mut [&mut owner, &mut phone], &mut now, 30);
        assert!(matches!(
            phone.service.access(room_id, channel, now),
            ChannelAccess::Waiting
        ));
        assert!(!owner.service.stewards[&channel].steward.outbox().is_empty());
        room.down.store(false, Ordering::SeqCst);
        run(&mut [&mut owner, &mut phone], &mut now, 130);
        assert!(ready_epoch(&phone.service.access(room_id, channel, now)).is_some());
        assert!(
            owner.service.stewards[&channel].steward.outbox().is_empty(),
            "the outbox drains once the phone acknowledges"
        );
    }

    #[test]
    fn channel_state_and_gates_survive_a_restart() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let channel = ChannelId::new();
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(public(6));
        let (owner_dir, tablet_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        {
            let mut owner = device(&room, room_id, 0, (5, 5), &policy, owner_dir.path());
            let mut tablet = device(&room, room_id, 1, (6, 5), &policy, tablet_dir.path());
            owner.service.access(room_id, channel, now);
            tablet.service.access(room_id, channel, now);
            run(&mut [&mut owner, &mut tablet], &mut now, 5);
            assert!(ready_epoch(&tablet.service.access(room_id, channel, now)).is_some());
        }
        for (id, seed, dir) in [(0, 5, owner_dir.path()), (1, 6, tablet_dir.path())] {
            let mut restarted = device(&room, room_id, id, (seed, 5), &policy, dir);
            assert!(
                refuses_plaintext(&restarted, channel),
                "the plaintext gate holds from the first op after a restart"
            );
            assert!(ready_epoch(&restarted.service.access(room_id, channel, now)).is_some());
        }
    }

    /// Key loss: a laptop is lost with all its channel state. Restoring the
    /// person's identity backup on a replacement machine brings back the
    /// root identity, not the lost device key; onboarding certifies a new
    /// device key. Once the room's governor knows that certificate, the
    /// policy allows the new device, the steward admits it, and its key
    /// history opens everything written before the loss. The lost device is
    /// removed, so it cannot read what is written afterwards.
    #[test]
    fn a_restored_identity_regains_the_whole_channel_on_its_new_device() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let channel = ChannelId::new();
        let artifact = collab_types::ArtifactId::new();
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(public(8));
        let (owner_dir, lost_dir, new_dir) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let mut now = Instant::now();
        let mut owner = device(&room, room_id, 0, (7, 7), &policy, owner_dir.path());
        let mut lost = device(&room, room_id, 1, (8, 7), &policy, lost_dir.path());
        owner.service.access(room_id, channel, now);
        lost.service.access(room_id, channel, now);
        run(&mut [&mut owner, &mut lost], &mut now, 5);
        let ChannelAccess::Ready(keys) = owner.service.access(room_id, channel, now) else {
            panic!("owner keys");
        };
        let before = keys
            .seal_content(
                artifact,
                collab_types::ContentRetention::Delta,
                b"before the loss",
            )
            .unwrap();
        let ChannelAccess::Ready(lost_keys) = lost.service.access(room_id, channel, now) else {
            panic!("the laptop was a member");
        };
        drop(lost);

        // The replacement machine's new device key replaces the lost one in
        // the person's certified device set.
        {
            let mut allowed = policy.allowed.lock().unwrap();
            allowed.remove(&public(8));
            allowed.insert(public(9));
        }
        let mut restored = device(&room, room_id, 2, (9, 7), &policy, new_dir.path());
        assert!(matches!(
            restored.service.access(room_id, channel, now),
            ChannelAccess::Waiting
        ));
        run(&mut [&mut owner, &mut restored], &mut now, 5);
        let ChannelAccess::Ready(restored_keys) = restored.service.access(room_id, channel, now)
        else {
            panic!("the restored identity's new device is admitted");
        };
        assert!(
            restored_keys
                .open_content(artifact, &before.0, &before.1)
                .is_ok(),
            "its key history opens everything written before the loss"
        );
        let ChannelAccess::Ready(keys) = owner.service.access(room_id, channel, now) else {
            panic!("owner keys");
        };
        let after = keys
            .seal_content(
                artifact,
                collab_types::ContentRetention::Delta,
                b"after the loss",
            )
            .unwrap();
        assert!(
            restored_keys
                .open_content(artifact, &after.0, &after.1)
                .is_ok()
        );
        assert!(
            lost_keys
                .open_content(artifact, &after.0, &after.1)
                .is_err(),
            "the lost device was removed and cannot read what follows"
        );
    }

    fn sealed_op(
        artifact: collab_types::ArtifactId,
        (metadata, payload): (EncryptionMetadata, Vec<u8>),
        op_class: collab_types::OpClass,
    ) -> SignedOp {
        SignedOp::new_unsigned_with_encryption(
            payload,
            artifact,
            public(1),
            collab_types::AttestationId::default(),
            op_class,
            std::time::SystemTime::UNIX_EPOCH,
            1,
            metadata,
        )
    }

    /// The key holder compacting a sealed record reads its whole history,
    /// older epochs included, and seals the merged record as a current
    /// checkpoint that opens as a new write; a new write still opens only
    /// under the current epoch.
    #[test]
    fn a_key_holder_opens_history_and_seals_a_current_checkpoint() {
        use collab_types::OpClass;
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let channel = ChannelId::new();
        let artifact = collab_types::ArtifactId::new();
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(public(12));
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&room, room_id, 0, (11, 11), &policy, owner_dir.path());
        let mut laptop = device(&room, room_id, 1, (12, 11), &policy, laptop_dir.path());
        let ChannelAccess::Ready(first) = owner.service.access(room_id, channel, now) else {
            panic!("owner keys");
        };
        let old = sealed_op(
            artifact,
            first
                .seal_content(artifact, ContentRetention::Delta, b"before the rotation")
                .unwrap(),
            OpClass::Splice,
        );
        laptop.service.access(room_id, channel, now);
        run(&mut [&mut owner, &mut laptop], &mut now, 5);
        let keys = ChannelKeyAccess::new(owner.service);
        let (ChannelAccess::Ready(current), _) = keys.access(room_id, channel) else {
            panic!("owner keys");
        };
        assert!(current.epoch() > first.epoch(), "adding the laptop rotated");

        assert!(keys.open_new_write(room_id, &old).is_err());
        assert_eq!(
            keys.open_history_write(room_id, &old).unwrap(),
            b"before the rotation"
        );
        let sealed = keys
            .seal_current_checkpoint(room_id, channel, artifact, b"merged record")
            .unwrap();
        assert_eq!(sealed.0.epoch, current.epoch());
        assert_eq!(sealed.0.retention(), Some(ContentRetention::Checkpoint));
        let payload = collab_types::SealedCompaction::new(std::slice::from_ref(&old), sealed.1)
            .unwrap()
            .encode()
            .unwrap();
        let compaction = sealed_op(artifact, (sealed.0, payload), OpClass::Compaction);
        assert_eq!(
            keys.open_new_write(room_id, &compaction).unwrap(),
            b"merged record"
        );
        assert_eq!(
            keys.open_history_write(room_id, &compaction).unwrap(),
            b"merged record"
        );
    }

    fn sealed_write(artifact: collab_types::ArtifactId, keys: &ChannelContentKeys) -> SignedOp {
        let (metadata, payload) = keys
            .seal_content(artifact, collab_types::ContentRetention::Delta, b"the edit")
            .unwrap();
        SignedOp::new_unsigned_with_encryption(
            payload,
            artifact,
            public(10),
            collab_types::document_attestation_id("test.app", "room", ""),
            collab_types::OpClass::Splice,
            std::time::SystemTime::now(),
            1,
            metadata,
        )
    }

    #[test]
    fn a_renewal_keeps_a_current_ciphertext_and_reseals_one_from_before_a_rotation() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let channel = ChannelId::new();
        let artifact = collab_types::ArtifactId::new();
        let policy = Policy::default();
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&room, room_id, 0, (10, 10), &policy, owner_dir.path());
        let mut laptop = device(&room, room_id, 1, (11, 10), &policy, laptop_dir.path());
        let ChannelAccess::Ready(before) = owner.service.access(room_id, channel, now) else {
            panic!("the steward holds the channel's keys");
        };
        let stale = sealed_write(artifact, &before);
        policy.allowed.lock().unwrap().insert(public(11));
        laptop.service.access(room_id, channel, now);
        run(&mut [&mut owner, &mut laptop], &mut now, 5);
        let access = ChannelKeyAccess::new(owner.service);
        let (ChannelAccess::Ready(current), _) = access.access(room_id, channel) else {
            panic!("the steward holds the channel's keys");
        };
        assert!(
            current.epoch() > before.epoch(),
            "adding a member rotated the epoch"
        );

        let fresh = sealed_write(artifact, &current);
        assert_eq!(
            access.renew_sealed(room_id, &fresh).unwrap(),
            (fresh.encryption.unwrap(), fresh.payload.clone()),
            "a current-epoch write renews with its identical ciphertext"
        );

        let (metadata, payload) = access.renew_sealed(room_id, &stale).unwrap();
        assert_eq!(metadata.epoch, current.epoch());
        assert_eq!(
            current
                .open_current_content(artifact, &metadata, &payload)
                .unwrap()
                .as_slice(),
            b"the edit",
            "the re-sealed content opens as a current write"
        );
        assert!(
            current
                .open_current_content(artifact, &stale.encryption.unwrap(), &stale.payload)
                .is_err(),
            "the stale ciphertext itself would be refused as a new write"
        );

        let mut tampered = stale;
        tampered.payload[0] ^= 1;
        assert!(access.renew_sealed(room_id, &tampered).is_err());
        let mut plaintext = fresh;
        plaintext.encryption = None;
        assert!(access.renew_sealed(room_id, &plaintext).is_err());
    }

    #[test]
    fn a_device_without_ready_access_renews_nothing() {
        let room = Arc::new(Room::default());
        let room_id = RoomId::new();
        let channel = ChannelId::new();
        let artifact = collab_types::ArtifactId::new();
        let policy = Policy::default();
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let now = Instant::now();
        let mut owner = device(&room, room_id, 0, (12, 12), &policy, owner_dir.path());
        let laptop = device(&room, room_id, 1, (13, 12), &policy, laptop_dir.path());
        let ChannelAccess::Ready(keys) = owner.service.access(room_id, channel, now) else {
            panic!("the steward holds the channel's keys");
        };
        let write = sealed_write(artifact, &keys);
        let waiting = ChannelKeyAccess::new(laptop.service);
        assert!(matches!(
            waiting.access(room_id, channel).0,
            ChannelAccess::Waiting
        ));
        assert!(
            waiting.renew_sealed(room_id, &write).is_err(),
            "a device still waiting for access renews nothing"
        );
    }
}
