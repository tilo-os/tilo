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
use tilo_desktop_model::RoomId;

/// How often stewards re-check the access policy without a trigger, which
/// also bounds how late an expiring grant is enforced.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
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
    last_reconcile: Option<Instant>,
    delivery: u64,
    generation: u64,
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
            last_reconcile: None,
            delivery: 0,
            generation: 0,
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

    /// What an endpoint of this device may do with `channel` in `room`.
    /// The first call for a route creates its group (when this device
    /// governs the room) or starts asking the steward for access. The caller
    /// has already decided the endpoint may read the route.
    pub fn access(&mut self, room: RoomId, channel: ChannelId, now: Instant) -> ChannelAccess {
        if self.directory.governs(room) {
            if !self.stewards.contains_key(&channel)
                && let Err(error) = self.open_steward(room, channel)
            {
                tracing::warn!(%error, %room, "channel group could not be created");
                return ChannelAccess::Unavailable;
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
                Ok(true) => {}
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
                }
                ChannelAccess::Removed
            }
        }
    }

    /// Run the governance protocol once: apply what arrived, reconcile
    /// stewarded groups with the access policy, and (re)deliver what is
    /// due. `wall` is the Unix time the policy is evaluated at.
    pub fn pump(&mut self, transport: &dyn GovernanceTransport, now: Instant, wall: u64) {
        for inbound in transport.take_inbound() {
            self.receive(inbound, now, wall);
        }
        let reconcile_due = self
            .last_reconcile
            .is_none_or(|last| now.saturating_duration_since(last) >= RECONCILE_INTERVAL);
        if reconcile_due {
            self.last_reconcile = Some(now);
        }
        let channels: Vec<ChannelId> = self.stewards.keys().copied().collect();
        for channel in channels {
            let due = reconcile_due || self.stewards.get(&channel).is_some_and(|entry| entry.dirty);
            if due {
                self.reconcile(channel, now, wall);
            }
            self.redeliver(channel, transport, now);
        }
        let channels: Vec<ChannelId> = self.members.keys().copied().collect();
        for channel in channels {
            self.drive_member(channel, transport, now, wall);
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
            self.bump();
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
            self.bump();
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
        self.bump();
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

    fn bump(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
}

/// Cheap shared handle for the identity service and the broker.
#[derive(Clone)]
pub struct ChannelKeyAccess {
    inner: Arc<Mutex<ChannelEncryption>>,
}

impl ChannelKeyAccess {
    pub fn new(service: ChannelEncryption) -> Self {
        Self {
            inner: Arc::new(Mutex::new(service)),
        }
    }

    /// See [`ChannelEncryption::access`]; also returns the service's
    /// generation, which changes whenever any channel's keys or access do.
    pub fn access(&self, room: RoomId, channel: ChannelId) -> (ChannelAccess, u64) {
        self.inner
            .lock()
            .map_or((ChannelAccess::Unavailable, 0), |mut service| {
                let access = service.access(room, channel, Instant::now());
                (access, service.generation())
            })
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

    /// See [`ChannelEncryption::pump`].
    pub fn pump(&self, transport: &dyn GovernanceTransport) {
        if let Ok(mut service) = self.inner.lock() {
            service.pump(transport, Instant::now(), tilo_clock::unix_now());
        }
    }

    /// See [`ChannelEncryption::generation`].
    pub fn generation(&self) -> u64 {
        self.inner.lock().map_or(0, |service| service.generation())
    }
}

/// Where the identity service finds this session's channel encryption
/// service. The broker that owns the service can start after the identity
/// service (onboarding defers it), so the slot is filled when it does.
#[derive(Clone, Default)]
pub struct ChannelKeyProvider {
    slot: Arc<Mutex<Option<ChannelKeyAccess>>>,
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
    }

    /// The current service, if a broker with one is running.
    pub fn get(&self) -> Option<ChannelKeyAccess> {
        self.slot.lock().ok().and_then(|slot| slot.clone())
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
    atomic_file::write_private(path, &bytes)
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
        fn allowed_devices(&self, _: RoomId, _: u64) -> BTreeSet<Ed25519PublicKey> {
            self.policy.allowed.lock().unwrap().clone()
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
}
