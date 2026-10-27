//! The encrypted channels of the rooms a node governs.
//!
//! A relay whose certificate lets it own workspaces is the person's node:
//! the device that governs their rooms. Members never write a room's
//! records in the clear; each record's channel is an MLS group, and the
//! room's governor stewards it. So a node also stewards the channels of the
//! rooms it governs: it admits exactly the room's governed member devices
//! that ask for a channel, removes the ones governance removed, and sends
//! each of them the channel's governance frames. A channel is one record's:
//! a device a Read Deny on that record, its app or a pattern over it
//! withholds ([`Readers`]) is kept out of that channel alone. A channel id
//! is a hash of its record's route, so the steward keeps each route a device
//! proved with its request ([`Steward::learn_route`]); a channel whose route
//! it does not know is withheld from whoever a Deny over a pattern could
//! keep out of it. A relay with a plain host
//! certificate governs rooms without stewarding them; members accept a
//! steward only from a device certified to own workspaces.
//!
//! Channel state is sealed on disk under a key derived from the node's
//! device key, which already lives beside it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use channel_crypto::{ChannelSteward, VaultKey};
use collab_types::{
    ChannelId, ChannelRoute, Ed25519PublicKey, MlsAccessRequestV1, MlsGovernanceAckV1,
};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_lock::MutexRecover as _;

/// Channels one room keeps a steward for.
const MAX_CHANNELS: usize = 4096;

/// Routes one room keeps: one per channel it may steward, and as many again
/// for channels asked about that it never opened.
const MAX_ROUTES: usize = 2 * MAX_CHANNELS;

/// The devices governance lets read a room's channels: every reader of the
/// room, less the ones a Read Deny keeps out of a record's channel
/// (`tilo_peer_state::PeerGovernance::withheld_record_readers`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Readers {
    room: BTreeSet<Ed25519PublicKey>,
    /// Each channel whose record the steward can name, and who may not read
    /// that record.
    withheld: HashMap<ChannelId, BTreeSet<Ed25519PublicKey>>,
    /// Who is kept out of every channel whose record the steward cannot
    /// name: whoever a Deny over a pattern of records could keep out of it.
    unrouted: BTreeSet<Ed25519PublicKey>,
}

impl Readers {
    pub(super) fn new(
        room: BTreeSet<Ed25519PublicKey>,
        withheld: HashMap<ChannelId, BTreeSet<Ed25519PublicKey>>,
        unrouted: BTreeSet<Ed25519PublicKey>,
    ) -> Self {
        Self {
            room,
            withheld,
            unrouted,
        }
    }

    /// The devices that may read `channel`.
    pub(super) fn of(&self, channel: ChannelId) -> BTreeSet<Ed25519PublicKey> {
        let withheld = self.withheld.get(&channel).unwrap_or(&self.unrouted);
        self.room.difference(withheld).copied().collect()
    }

    /// Whether a Read Deny keeps `channel`'s record from any device; a
    /// channel whose record the steward cannot name counts as withheld when
    /// a Deny over a pattern of records could reach it.
    #[cfg(feature = "web")]
    pub(super) fn withholds(&self, channel: ChannelId) -> bool {
        !self
            .withheld
            .get(&channel)
            .unwrap_or(&self.unrouted)
            .is_empty()
    }

    /// Whether every device these readers let read a channel, `later` lets
    /// read it too: `later` only lets devices in.
    fn within(&self, later: &Self) -> bool {
        // A channel neither names is read by each room less its unrouted.
        let unnamed = |readers: &Self| -> BTreeSet<Ed25519PublicKey> {
            readers
                .room
                .difference(&readers.unrouted)
                .copied()
                .collect()
        };
        unnamed(self).is_subset(&unnamed(later))
            && self
                .withheld
                .keys()
                .chain(later.withheld.keys())
                .all(|channel| self.of(*channel).is_subset(&later.of(*channel)))
    }
}

/// The stewards of one governed room's channels.
pub(super) struct Stewardship {
    room: String,
    dir: PathBuf,
    key: SigningKey,
    vault: VaultKey,
    stewards: HashMap<ChannelId, ChannelSteward>,
    /// Each device's newest request per channel, kept until it is a member.
    requests: HashMap<ChannelId, BTreeMap<Ed25519PublicKey, MlsAccessRequestV1>>,
    /// The frames of each channel's outbox already published. The host
    /// retains them for whoever subscribes later and delivers them to
    /// whoever is live, so each is published once.
    published: HashMap<ChannelId, BTreeSet<[u8; 32]>>,
    delivery: u64,
}

/// A governed room's channel stewards, and the readers its governance lets
/// in as the room last handed them over.
pub(super) struct Steward {
    /// The newest governance generation handed over and its readers. Apart
    /// from the channels: governance that only lets devices in moves no
    /// channel, so the room hands its readers over without waiting for an
    /// admission in progress.
    readers: std::sync::Mutex<Option<(u64, Readers)>>,
    /// The route of each channel's record a device proved, kept on disk
    /// beside the channels. Apart from the channels too: readers are read
    /// with them before the channels are taken.
    routes: std::sync::Mutex<Routes>,
    /// Shared so a pass can take it to a blocking worker: its MLS work held
    /// a runtime worker for seconds when a crowd asked at once.
    pub(super) channels: Arc<tokio::sync::Mutex<Stewardship>>,
}

/// The routes a room's steward knows, and where it keeps them.
struct Routes {
    room: String,
    dir: PathBuf,
    known: HashMap<ChannelId, ChannelRoute>,
}

impl Steward {
    pub(super) fn new(channels: Stewardship) -> Self {
        let routes = Routes::load(&channels.room, &channels.dir);
        Self {
            readers: std::sync::Mutex::new(None),
            routes: std::sync::Mutex::new(routes),
            channels: Arc::new(tokio::sync::Mutex::new(channels)),
        }
    }

    fn known_routes(&self) -> std::sync::MutexGuard<'_, Routes> {
        self.routes.lock_recover()
    }

    /// Every route this steward knows, for deciding Read on the records
    /// their channels carry.
    pub(super) fn routes(&self) -> Vec<ChannelRoute> {
        self.known_routes().known.values().cloned().collect()
    }

    /// Keep `route` as `channel`'s, durably, when it proves to be the
    /// channel's route in this room. Returns whether it is now known; a
    /// route that does not prove itself, or one past the room's bound, is
    /// not, and the channel stays decided as one whose record is unknown.
    pub(super) fn learn_route(&self, channel: ChannelId, route: &ChannelRoute) -> bool {
        self.known_routes().learn(channel, route)
    }
}

impl Routes {
    fn path(dir: &Path, channel: ChannelId) -> PathBuf {
        dir.join(format!("{}.route", channel.0))
    }

    /// The routes kept under `dir` that still prove themselves in `room`.
    fn load(room: &str, dir: &Path) -> Self {
        let mut known = HashMap::new();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("route") {
                continue;
            }
            let Some(channel) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| uuid::Uuid::parse_str(stem).ok())
                .map(ChannelId)
            else {
                continue;
            };
            let route = std::fs::read(&path)
                .ok()
                .and_then(|bytes| postcard::from_bytes::<ChannelRoute>(&bytes).ok())
                .filter(|route| route.proves(room, channel));
            if let Some(route) = route
                && known.len() < MAX_ROUTES
            {
                known.insert(channel, route);
            }
        }
        Self {
            room: room.to_owned(),
            dir: dir.to_owned(),
            known,
        }
    }

    fn learn(&mut self, channel: ChannelId, route: &ChannelRoute) -> bool {
        if self.known.contains_key(&channel) {
            return true;
        }
        if !route.proves(&self.room, channel) || self.known.len() >= MAX_ROUTES {
            return false;
        }
        let saved = std::fs::create_dir_all(&self.dir)
            .and_then(|()| postcard::to_stdvec(route).map_err(std::io::Error::other))
            .and_then(|bytes| {
                tilo_files::native::atomic::write_with_mode(
                    &Self::path(&self.dir, channel),
                    &bytes,
                    0o600,
                )
            });
        if let Err(error) = saved {
            // Known for now; after a restart the device proves it again.
            tracing::warn!(room = %self.room, %error, "channel route could not be saved");
        }
        self.known.insert(channel, route.clone());
        true
    }
}

impl Steward {
    fn handed_over(&self) -> std::sync::MutexGuard<'_, Option<(u64, Readers)>> {
        self.readers.lock_recover()
    }

    /// Whether governance whose readers are `readers` must move the
    /// channels: it removed a reader handed over before, from the room or
    /// from one record's channel, or none were yet (the channels' published
    /// state is then unknown). A device let in is admitted to a channel when
    /// it asks.
    pub(super) fn moves_channels(&self, readers: &Readers) -> bool {
        self.handed_over()
            .as_ref()
            .is_none_or(|(_, before)| !before.within(readers))
    }

    pub(super) fn hand_over(&self, generation: u64, readers: &Readers) {
        let mut handed = self.handed_over();
        if handed
            .as_ref()
            .is_none_or(|(known, _)| *known <= generation)
        {
            *handed = Some((generation, readers.clone()));
        }
    }

    pub(super) fn readers(&self, generation: u64, taken: Readers) -> Readers {
        match &*self.handed_over() {
            Some((handed, readers)) if *handed > generation => readers.clone(),
            _ => taken,
        }
    }
}

impl Stewardship {
    /// The stewardship of `room`, kept under `dir`, when `key`'s certificate
    /// lets it own workspaces; channels already stewarded there are opened
    /// as members ask for them.
    pub(super) fn new(room: &str, dir: PathBuf, key: &SigningKey) -> Self {
        let vault = VaultKey::new(blake3::derive_key(
            "tilo-relay governed room channel vault v1",
            &key.to_bytes(),
        ));
        Self {
            room: room.to_owned(),
            dir,
            key: key.clone(),
            vault,
            stewards: HashMap::new(),
            requests: HashMap::new(),
            published: HashMap::new(),
            delivery: 0,
        }
    }

    /// Read keys for an existing channel, recovering its sealed state if cold.
    /// Missing channels are refused; this path never initializes a group.
    #[cfg(any(feature = "agents", feature = "web"))]
    pub(super) fn existing_keys(
        &mut self,
        channel: ChannelId,
    ) -> Result<channel_crypto::ChannelContentKeys, String> {
        if !self.stewards.contains_key(&channel) {
            if self.stewards.len() >= MAX_CHANNELS {
                return Err("the room stewards too many channels".into());
            }
            let steward =
                ChannelSteward::open_existing(&self.dir, channel, self.key.clone(), &self.vault)
                    .map_err(|error| error.to_string())?;
            self.stewards.insert(channel, steward);
        }
        self.stewards
            .get(&channel)
            .ok_or("the source channel is not open on this host")?
            .content_keys()
            .map_err(|error| error.to_string())
    }

    /// Open `channel` for this device's own endpoint, which is about to
    /// write its first record there: create its group when it has none yet,
    /// with `allowed` admitted as they ask, exactly as the first member's
    /// request would. Returns the governance messages to publish.
    #[cfg(feature = "agents")]
    pub(super) fn open_channel(
        &mut self,
        channel: ChannelId,
        allowed: &Readers,
    ) -> Result<Vec<NeighbourMsg>, String> {
        self.reconcile(channel, &allowed.of(channel))
    }

    fn device(&self) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(self.key.verifying_key().to_bytes())
    }

    /// The members of `channel`'s group, when this room stewards it.
    #[cfg(test)]
    pub(super) fn members(&self, channel: ChannelId) -> Option<BTreeSet<Ed25519PublicKey>> {
        self.stewards.get(&channel).map(ChannelSteward::members)
    }

    fn steward<'a>(
        stewards: &'a mut HashMap<ChannelId, ChannelSteward>,
        dir: &std::path::Path,
        key: &SigningKey,
        vault: &VaultKey,
        channel: ChannelId,
    ) -> Result<&'a mut ChannelSteward, String> {
        if !stewards.contains_key(&channel) {
            if stewards.len() >= MAX_CHANNELS {
                return Err("the room stewards too many channels".into());
            }
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
            let steward = ChannelSteward::open_or_create(dir, channel, key.clone(), vault)
                .map_err(|error| error.to_string())?;
            stewards.insert(channel, steward);
        }
        stewards
            .get_mut(&channel)
            .ok_or_else(|| "steward missing".into())
    }

    /// Take in `request`, which the authenticated device `author` sent, to
    /// be admitted by the next [`Self::admit_pending`] when governance lets
    /// it read the channel's record.
    pub(super) fn request(
        &mut self,
        author: [u8; 32],
        request: MlsAccessRequestV1,
        route: Option<&ChannelRoute>,
        allowed: &Readers,
    ) -> Result<(), String> {
        if request.device.as_bytes() != &author || request.verify().is_err() {
            return Err("access request is not the sender's own".into());
        }
        if route.is_some_and(|route| !route.proves(&self.room, request.channel)) {
            return Err("access request names a route that is not its channel's".into());
        }
        if !allowed.of(request.channel).contains(&request.device) {
            return Ok(());
        }
        // A device's latest request replaces any earlier one whatever its
        // clock says: a device that started over, or whose clock went back,
        // can no longer use the key package it asked with before. The
        // steward refuses a package it already admitted.
        self.requests
            .entry(request.channel)
            .or_default()
            .insert(request.device, request);
        Ok(())
    }

    /// Admit every device asking for a channel, one change per channel for
    /// all of them. Returns the governance messages to publish.
    pub(super) fn admit_pending(&mut self, allowed: &Readers) -> Result<Vec<NeighbourMsg>, String> {
        let channels: Vec<ChannelId> = self.requests.keys().copied().collect();
        let mut messages = Vec::new();
        for channel in channels {
            messages.extend(self.reconcile(channel, &allowed.of(channel))?);
        }
        Ok(messages)
    }

    /// Record members' acknowledgements of the frames they applied, each
    /// `(author, ack)` sent by the authenticated device `author`.
    pub(super) fn acknowledge(&mut self, acks: Vec<([u8; 32], MlsGovernanceAckV1)>) {
        let mut by_channel: HashMap<ChannelId, Vec<MlsGovernanceAckV1>> = HashMap::new();
        for (author, ack) in acks {
            if ack.device.as_bytes() == &author {
                by_channel.entry(ack.channel).or_default().push(ack);
            }
        }
        for (channel, acks) in by_channel {
            if let Some(steward) = self.stewards.get_mut(&channel)
                && let Err(error) = steward.acknowledge_all(&acks)
            {
                tracing::debug!(room = %self.room, %error, "channel acknowledgements refused");
            }
        }
    }

    /// Move every open channel to the devices `allowed` lets read it, after
    /// governance removed a reader. A channel whose readers did not change
    /// makes no commit, so a Read Deny on one record starts a new epoch in
    /// that record's channel alone.
    pub(super) fn reconcile_all(&mut self, allowed: &Readers) -> Result<Vec<NeighbourMsg>, String> {
        let channels: Vec<ChannelId> = self.stewards.keys().copied().collect();
        let mut messages = Vec::new();
        for channel in channels {
            messages.extend(self.reconcile(channel, &allowed.of(channel))?);
        }
        Ok(messages)
    }

    fn reconcile(
        &mut self,
        channel: ChannelId,
        allowed: &BTreeSet<Ed25519PublicKey>,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let own = self.device();
        let mut allowed = allowed.clone();
        allowed.insert(own);
        let pending = self.requests.remove(&channel).unwrap_or_default();
        let steward = Self::steward(
            &mut self.stewards,
            &self.dir,
            &self.key,
            &self.vault,
            channel,
        )?;
        steward
            .reconcile(&allowed, &pending, &self.vault)
            .map_err(|error| error.to_string())?;
        let members = steward.members();
        let frames = steward.outbox_frames();
        let journal: BTreeSet<[u8; 32]> = steward
            .journal()
            .frames()
            .iter()
            .filter_map(|frame| frame.digest().ok())
            .collect();
        // A request stays until its device is a member: one that could not
        // be admitted yet is retried on the next change.
        let waiting: BTreeMap<_, _> = pending
            .into_iter()
            .filter(|(device, _)| !members.contains(device))
            .collect();
        if !waiting.is_empty() {
            self.requests.insert(channel, waiting);
        }
        // The journal holds only genesis and the head, so what is
        // remembered as published stays as small.
        let mut published = self.published.remove(&channel).unwrap_or_default();
        published.retain(|digest| journal.contains(digest));
        let mut messages = Vec::new();
        for frame in frames {
            let Ok(digest) = frame.digest() else { continue };
            if !published.insert(digest) {
                continue;
            }
            // An admission goes out twice: whole to the devices it adds,
            // and without what they join from to everyone.
            for frame in frame.into_deliveries().map_err(|error| error.to_string())? {
                self.delivery = self.delivery.saturating_add(1);
                messages.push(NeighbourMsg::MlsGovernance {
                    room: self.room.clone(),
                    frame: Box::new(frame),
                    delivery: self.delivery,
                });
            }
        }
        self.published.insert(channel, published);
        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devices(devices: &[u8]) -> BTreeSet<Ed25519PublicKey> {
        devices
            .iter()
            .map(|device| Ed25519PublicKey::from_bytes([*device; 32]))
            .collect()
    }

    fn readers(devices: &[u8]) -> Readers {
        Readers::new(self::devices(devices), HashMap::new(), BTreeSet::new())
    }

    #[test]
    fn only_a_removed_reader_moves_the_channels() {
        let tmp = tempfile::tempdir().unwrap();
        let steward = Steward::new(Stewardship::new(
            "room",
            tmp.path().join("room.channels"),
            &SigningKey::from_bytes(&[9; 32]),
        ));
        // Nothing handed over yet: the channels' published state is unknown.
        assert!(steward.moves_channels(&readers(&[1])));
        steward.hand_over(1, &readers(&[1]));
        // A join only lets a device in.
        assert!(!steward.moves_channels(&readers(&[1, 2])));
        steward.hand_over(2, &readers(&[1, 2]));
        assert!(steward.moves_channels(&readers(&[2])));
        // An admission that read the room at generation 1 takes the readers
        // handed over since; one that read the newest keeps its own.
        assert_eq!(steward.readers(1, readers(&[1])), readers(&[1, 2]));
        assert_eq!(steward.readers(2, readers(&[1])), readers(&[1]));
        steward.hand_over(1, &readers(&[1]));
        assert_eq!(steward.readers(0, Readers::default()), readers(&[1, 2]));
    }

    /// A Read Deny on one record withholds its channel from the denied
    /// device and moves the channels; the room's other channels keep every
    /// reader, and lifting the Deny only lets the device in again.
    #[test]
    fn a_device_withheld_from_one_record_moves_the_channels() {
        let tmp = tempfile::tempdir().unwrap();
        let steward = Steward::new(Stewardship::new(
            "room",
            tmp.path().join("room.channels"),
            &SigningKey::from_bytes(&[9; 32]),
        ));
        let (payroll, notes) = (ChannelId::new(), ChannelId::new());
        let room = readers(&[1, 2]);
        steward.hand_over(1, &room);
        let denied = Readers::new(
            devices(&[1, 2]),
            HashMap::from([(payroll, devices(&[2]))]),
            BTreeSet::new(),
        );
        assert_eq!(denied.of(payroll), devices(&[1]));
        assert_eq!(denied.of(notes), devices(&[1, 2]));
        assert!(steward.moves_channels(&denied));
        steward.hand_over(2, &denied);
        assert!(!steward.moves_channels(&denied));
        assert!(!steward.moves_channels(&room));
        assert!(!steward.moves_channels(&Readers::new(
            devices(&[1, 2, 3]),
            HashMap::from([(payroll, devices(&[2]))]),
            BTreeSet::new(),
        )));
        assert!(steward.moves_channels(&Readers::new(
            devices(&[1, 2]),
            HashMap::from([(payroll, devices(&[2])), (notes, devices(&[2]))]),
            BTreeSet::new(),
        )));
        // So does a Deny over an app: every channel whose record is
        // unknown may be one of it.
        assert!(steward.moves_channels(&Readers::new(
            devices(&[1, 2]),
            HashMap::from([(payroll, devices(&[2]))]),
            devices(&[2]),
        )));
    }

    /// A withheld device's request for the denied record's channel is
    /// never admitted, and no group is created for it; its request for
    /// another record's channel is.
    #[test]
    fn a_withheld_device_is_not_admitted_to_the_denied_records_channel() {
        let tmp = tempfile::tempdir().unwrap();
        let mut stewardship = Stewardship::new(
            "room",
            tmp.path().join("room.channels"),
            &SigningKey::from_bytes(&[9; 32]),
        );
        let device = SigningKey::from_bytes(&[2; 32]);
        let device_key = Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
        let (payroll, notes) = (ChannelId::new(), ChannelId::new());
        let allowed = Readers::new(
            BTreeSet::from([device_key]),
            HashMap::from([(payroll, BTreeSet::from([device_key]))]),
            BTreeSet::new(),
        );
        std::fs::create_dir_all(tmp.path().join("member")).unwrap();
        for channel in [payroll, notes] {
            let request = channel_crypto::ChannelMember::open(
                &tmp.path().join("member"),
                channel,
                device_key,
                Ed25519PublicKey::from_bytes(
                    SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes(),
                ),
                &VaultKey::new([2; 32]),
            )
            .unwrap()
            .access_request(&device, 1, &VaultKey::new([2; 32]))
            .unwrap()
            .unwrap();
            stewardship
                .request(device.verifying_key().to_bytes(), request, None, &allowed)
                .unwrap();
        }
        stewardship.admit_pending(&allowed).unwrap();
        assert!(!stewardship.stewards.contains_key(&payroll));
        assert!(!stewardship.requests.contains_key(&payroll));
        assert!(
            stewardship
                .stewards
                .get(&notes)
                .unwrap()
                .members()
                .contains(&device_key)
        );
    }

    /// The access request `device` makes for `channel` at second `at`, as a
    /// member would.
    fn access_request(
        dir: &Path,
        device: &SigningKey,
        channel: ChannelId,
        at: u64,
    ) -> MlsAccessRequestV1 {
        std::fs::create_dir_all(dir).unwrap();
        let device_key = Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
        channel_crypto::ChannelMember::open(
            dir,
            channel,
            device_key,
            Ed25519PublicKey::from_bytes(
                SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes(),
            ),
            &VaultKey::new([2; 32]),
        )
        .unwrap()
        .access_request(device, at, &VaultKey::new([2; 32]))
        .unwrap()
        .unwrap()
    }

    /// A device that started over with its clock behind the requests its
    /// earlier instance made is not kept waiting behind them: its latest
    /// request is the one admitted.
    #[test]
    fn a_devices_latest_request_wins_whatever_its_clock() {
        let tmp = tempfile::tempdir().unwrap();
        let mut stewardship = Stewardship::new(
            "room",
            tmp.path().join("room.channels"),
            &SigningKey::from_bytes(&[9; 32]),
        );
        let device = SigningKey::from_bytes(&[2; 32]);
        let device_key = Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
        let channel = ChannelId::new();
        let allowed = Readers::new(
            BTreeSet::from([device_key]),
            HashMap::new(),
            BTreeSet::new(),
        );
        let ahead = access_request(&tmp.path().join("earlier"), &device, channel, 100);
        let fresh = access_request(&tmp.path().join("fresh"), &device, channel, 1);
        assert!(fresh.issued_at < ahead.issued_at);
        for request in [ahead, fresh.clone()] {
            stewardship
                .request(device.verifying_key().to_bytes(), request, None, &allowed)
                .unwrap();
        }
        assert_eq!(stewardship.requests[&channel][&device_key], fresh);
        stewardship.admit_pending(&allowed).unwrap();
        assert!(stewardship.members(channel).unwrap().contains(&device_key));
    }

    /// "Finance: Deny Read" for one device, as `readers` hands it over: every
    /// finance channel whose route the steward knows is withheld from it, as
    /// is a channel whose record it does not know; another app's routed
    /// channel is not. A route that is not its channel's refuses the request.
    #[test]
    fn a_read_deny_over_an_app_withholds_its_routed_and_its_unknown_channels() {
        let tmp = tempfile::tempdir().unwrap();
        let mut stewardship = Stewardship::new(
            "room",
            tmp.path().join("room.channels"),
            &SigningKey::from_bytes(&[9; 32]),
        );
        let (denied, other) = (
            SigningKey::from_bytes(&[2; 32]),
            SigningKey::from_bytes(&[3; 32]),
        );
        let key =
            |device: &SigningKey| Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
        let payroll = ChannelRoute::new("finance", "payroll");
        let draft = ChannelRoute::new("notes", "draft");
        let unknown = ChannelRoute::new("notes", "plan").channel("room");
        let (payroll_channel, draft_channel) = (payroll.channel("room"), draft.channel("room"));
        let allowed = Readers::new(
            BTreeSet::from([key(&denied), key(&other)]),
            HashMap::from([
                (payroll_channel, BTreeSet::from([key(&denied)])),
                (draft_channel, BTreeSet::new()),
            ]),
            BTreeSet::from([key(&denied)]),
        );
        assert_eq!(allowed.of(payroll_channel), BTreeSet::from([key(&other)]));
        assert_eq!(allowed.of(unknown), BTreeSet::from([key(&other)]));
        assert_eq!(
            allowed.of(draft_channel),
            BTreeSet::from([key(&denied), key(&other)])
        );

        let member = tmp.path().join("member");
        for device in [&denied, &other] {
            for (channel, route) in [
                (payroll_channel, Some(&payroll)),
                (draft_channel, Some(&draft)),
                (unknown, None),
            ] {
                let request = access_request(
                    &member.join(hex::encode(device.to_bytes())),
                    device,
                    channel,
                    1,
                );
                stewardship
                    .request(device.verifying_key().to_bytes(), request, route, &allowed)
                    .unwrap();
            }
        }
        let forged = access_request(&member.join("forged"), &other, unknown, 1);
        assert!(
            stewardship
                .request(
                    other.verifying_key().to_bytes(),
                    forged,
                    Some(&draft),
                    &allowed
                )
                .is_err()
        );
        stewardship.admit_pending(&allowed).unwrap();
        let members = |channel: ChannelId| stewardship.stewards[&channel].members();
        assert!(!members(payroll_channel).contains(&key(&denied)));
        assert!(members(payroll_channel).contains(&key(&other)));
        assert!(!members(unknown).contains(&key(&denied)));
        assert!(members(unknown).contains(&key(&other)));
        assert!(members(draft_channel).contains(&key(&denied)));
        assert!(members(draft_channel).contains(&key(&other)));
    }

    /// A route a device proved is kept with the room's channels: a steward
    /// opened again knows it from the start, a route that does not prove its
    /// channel is never kept, and a kept file that no longer proves its
    /// channel names nothing.
    #[test]
    fn learned_routes_survive_a_restart_and_only_true_ones_are_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("room.channels");
        let open = || {
            Steward::new(Stewardship::new(
                "room",
                dir.clone(),
                &SigningKey::from_bytes(&[9; 32]),
            ))
        };
        let payroll = ChannelRoute::new("finance", "payroll");
        let channel = payroll.channel("room");
        let steward = open();
        assert!(steward.routes().is_empty());
        assert!(!steward.learn_route(channel, &ChannelRoute::new("finance", "budget")));
        assert!(!steward.learn_route(
            ChannelRoute::new("notes", "x").channel("other"),
            &ChannelRoute::new("notes", "x")
        ));
        assert!(steward.learn_route(channel, &payroll));
        assert_eq!(steward.routes(), vec![payroll.clone()]);
        drop(steward);

        assert_eq!(open().routes(), vec![payroll]);
        let budget = ChannelRoute::new("finance", "budget");
        std::fs::write(
            Routes::path(&dir, channel),
            postcard::to_stdvec(&budget).unwrap(),
        )
        .unwrap();
        assert!(open().routes().is_empty());
    }

    /// What a member seals in a channel opens with the content keys the
    /// steward reads for a spectator, also from a stewardship that only
    /// finds the channel on disk; a channel never created stays missing.
    #[cfg(any(feature = "agents", feature = "web"))]
    #[test]
    fn a_members_content_opens_with_the_stewards_existing_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("room.channels");
        let steward_key = SigningKey::from_bytes(&[9; 32]);
        let mut stewardship = Stewardship::new("room", dir.clone(), &steward_key);
        let device = SigningKey::from_bytes(&[2; 32]);
        let device_key = Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
        let channel = ChannelId::new();
        let allowed = Readers::new(
            BTreeSet::from([device_key]),
            HashMap::new(),
            BTreeSet::new(),
        );
        std::fs::create_dir_all(tmp.path().join("member")).unwrap();
        let mut member = channel_crypto::ChannelMember::open(
            &tmp.path().join("member"),
            channel,
            device_key,
            Ed25519PublicKey::from_bytes(steward_key.verifying_key().to_bytes()),
            &VaultKey::new([2; 32]),
        )
        .unwrap();
        let request = member
            .access_request(&device, 1, &VaultKey::new([2; 32]))
            .unwrap()
            .unwrap();
        stewardship
            .request(device.verifying_key().to_bytes(), request, None, &allowed)
            .unwrap();
        for message in stewardship.admit_pending(&allowed).unwrap() {
            if let NeighbourMsg::MlsGovernance { frame, .. } = message {
                member.receive(*frame, &VaultKey::new([2; 32])).unwrap();
            }
        }
        let artifact = collab_types::ArtifactId::new();
        let (metadata, sealed) = member
            .content_keys()
            .expect("the admitted member holds the channel's keys")
            .seal_content(artifact, collab_types::ContentRetention::Delta, b"hello")
            .unwrap();
        let opened = |stewardship: &mut Stewardship| {
            stewardship
                .existing_keys(channel)
                .unwrap()
                .open_content(artifact, &metadata, &sealed)
                .unwrap()
                .to_vec()
        };
        assert_eq!(opened(&mut stewardship), b"hello");
        drop(stewardship);
        let mut cold = Stewardship::new("room", dir, &steward_key);
        assert_eq!(opened(&mut cold), b"hello");
        assert!(cold.existing_keys(ChannelId::new()).is_err());
    }
}
