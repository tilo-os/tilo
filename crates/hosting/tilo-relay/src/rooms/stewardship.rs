//! The encrypted channels of the rooms a node governs.
//!
//! A relay whose certificate lets it own workspaces is the person's node:
//! the device that governs their rooms. Members never write a room's
//! records in the clear; each record's channel is an MLS group, and the
//! room's governor stewards it. So a node also stewards the channels of the
//! rooms it governs: it admits exactly the room's governed member devices
//! that ask for a channel, removes the ones governance removed, and sends
//! each of them the channel's governance frames. A channel is one record's:
//! a device a Read Deny on that record withholds ([`Readers`]) is kept out
//! of that channel alone. A relay with a plain host
//! certificate governs rooms without stewarding them; members accept a
//! steward only from a device certified to own workspaces.
//!
//! Channel state is sealed on disk under a key derived from the node's
//! device key, which already lives beside it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use channel_crypto::{ChannelSteward, VaultKey};
use collab_types::{ChannelId, Ed25519PublicKey, MlsAccessRequestV1, MlsGovernanceAckV1};
use ed25519_dalek::SigningKey;
use tilo_peer_state::NeighbourMsg;

/// Channels one room keeps a steward for.
const MAX_CHANNELS: usize = 4096;

/// The devices governance lets read a room's channels: every reader of the
/// room, less the ones a Read Deny keeps out of one record's channel
/// (`tilo_peer_state::StatePeer::withheld_record_readers`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Readers {
    room: BTreeSet<Ed25519PublicKey>,
    withheld: HashMap<ChannelId, BTreeSet<Ed25519PublicKey>>,
}

impl Readers {
    pub(super) fn new(
        room: BTreeSet<Ed25519PublicKey>,
        withheld: HashMap<ChannelId, BTreeSet<Ed25519PublicKey>>,
    ) -> Self {
        Self { room, withheld }
    }

    /// The devices that may read `channel`.
    pub(super) fn of(&self, channel: ChannelId) -> BTreeSet<Ed25519PublicKey> {
        match self.withheld.get(&channel) {
            Some(withheld) => self.room.difference(withheld).copied().collect(),
            None => self.room.clone(),
        }
    }

    /// Whether every device these readers let read a channel, `later` lets
    /// read it too: `later` only lets devices in.
    fn within(&self, later: &Self) -> bool {
        // A channel `later` withholds nothing from is read by its whole
        // room, which then holds all of this room.
        self.room.is_subset(&later.room)
            && later
                .withheld
                .keys()
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
    pub(super) channels: tokio::sync::Mutex<Stewardship>,
}

impl Steward {
    pub(super) fn new(channels: Stewardship) -> Self {
        Self {
            readers: std::sync::Mutex::new(None),
            channels: tokio::sync::Mutex::new(channels),
        }
    }

    fn handed_over(&self) -> std::sync::MutexGuard<'_, Option<(u64, Readers)>> {
        self.readers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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

    /// Hand over `readers`, those of governance `generation`.
    pub(super) fn hand_over(&self, generation: u64, readers: &Readers) {
        let mut handed = self.handed_over();
        if handed
            .as_ref()
            .is_none_or(|(known, _)| *known <= generation)
        {
            *handed = Some((generation, readers.clone()));
        }
    }

    /// The readers of the newer of governance `generation`, whose readers
    /// are `taken`, and the generation handed over last. An admission reads
    /// the room before it waits for the channels; a governance change
    /// handed over meanwhile let in devices whose requests it now takes.
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
    #[cfg(feature = "agents")]
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
        allowed: &Readers,
    ) -> Result<(), String> {
        if request.device.as_bytes() != &author || request.verify().is_err() {
            return Err("access request is not the sender's own".into());
        }
        if !allowed.of(request.channel).contains(&request.device) {
            return Ok(());
        }
        let pending = self.requests.entry(request.channel).or_default();
        if pending
            .get(&request.device)
            .is_none_or(|known| known.issued_at <= request.issued_at)
        {
            pending.insert(request.device, request);
        }
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
        Readers::new(self::devices(devices), HashMap::new())
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
        // An older generation handed over late replaces nothing.
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
        let denied = Readers::new(devices(&[1, 2]), HashMap::from([(payroll, devices(&[2]))]));
        assert_eq!(denied.of(payroll), devices(&[1]));
        assert_eq!(denied.of(notes), devices(&[1, 2]));
        assert!(steward.moves_channels(&denied));
        steward.hand_over(2, &denied);
        assert!(!steward.moves_channels(&denied));
        // Lifting the Deny, or letting another device in, moves nothing.
        assert!(!steward.moves_channels(&room));
        assert!(!steward.moves_channels(&Readers::new(
            devices(&[1, 2, 3]),
            HashMap::from([(payroll, devices(&[2]))]),
        )));
        // Withholding a second record from the device moves them again.
        assert!(steward.moves_channels(&Readers::new(
            devices(&[1, 2]),
            HashMap::from([(payroll, devices(&[2])), (notes, devices(&[2]))]),
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
                .request(device.verifying_key().to_bytes(), request, &allowed)
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
}
