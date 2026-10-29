//! One participant device's side of one channel.
//!
//! A channel's MLS group holds exactly the devices its membership lets read
//! it: a projection the caller computes from the room's membership and
//! access policy (`allowed`), never a second record of who belongs. Every
//! participant device keeps a [`ChannelPeer`] and follows the same rules:
//!
//! - **Any participant commits.** [`ChannelPeer::reconcile`] writes the one
//!   commit that moves the group to `allowed`: it removes every member the
//!   projection no longer names, and adds every allowed device that asked
//!   ([`ChannelPeer::learn_request`]) with the Welcome and the bundle of
//!   every past content key. A member asking with a key package whose
//!   signature key is not the one it joined with lost its state, and is
//!   removed and added again in the same commit.
//! - **Every commit is checked.** [`ChannelPeer::receive`] refuses a commit
//!   that adds a device `allowed` does not name, removes one it does (unless
//!   it adds it again), is not signed by an allowed member, or whose MLS
//!   proposals differ from what its frame declares.
//! - **One commit per epoch.** Of two frames following the same one, every
//!   device keeps the one with the lower digest. A device that applied the
//!   other goes back to the group it held before (kept beside the head for
//!   this) and applies the winner; if the group still differs from the
//!   projection, its next reconcile commits again on top. The device that
//!   committed the winner publishes it again when it sees the loser, so a
//!   host that keeps only a channel's newest frame keeps the winner. Two
//!   geneses compete the same way.
//! - **Falling behind starts over.** Hosts keep only a channel's newest
//!   frame, so a member that finds a frame it can never reach (it missed
//!   one, or followed a branch that lost) waits again with a fresh member
//!   key and asks anew; any participant admits it, history included.
//!
//! Removal takes effect at the membership op, which the caller's broker
//! enforces on its own; the commit that rotates the key comes from whichever
//! participant reconciles next. A store (carrier) device never keeps a peer.
//!
//! State is written before anything it produced leaves the device: a frame
//! reaches [`ChannelPeer::outbox`] only once the state holding it is
//! durable (on disk for a peer opened from a directory, or in the export a
//! browser writes before it sends).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use collab_types::{
    ChannelId, Ed25519PublicKey, JoinMaterial, Joining, MlsAccessRequestV1, MlsGovernanceAction,
    MlsGovernanceFrame,
};
use openmls::prelude::BasicCredential;
use serde::{Deserialize, Serialize};
use tilo_seal::{Context, Label};
use zeroize::Zeroizing;

use crate::gate::ChannelGate;
use crate::persistence::MemberRecord;
use crate::signer::{DeviceSigner, complete_now};
use crate::{ChannelContentKeys, ChannelGroup, Error, KeyPackage, MemberKey, Result, VaultKey};

/// Requests one channel keeps, one per device.
const MAX_REQUESTS: usize = 4096;
/// Sealed peer state larger than this is refused when read.
const MAX_PEER_STATE: usize = 320 * 1024 * 1024;
const PEER_RECORD: &[u8] = b"channel-crypto/channel-peer";
const PEER_VERSION: u8 = 1;

/// What the device can do with the channel's content right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Asked for access and not admitted yet: no content key is held.
    Waiting,
    /// Admitted; new content seals under this epoch.
    Ready { epoch: u64 },
    /// Removed. Access resumes only after a new request is admitted.
    Removed,
}

/// What applying one received frame changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerEvent {
    /// The frame was already applied.
    Duplicate,
    /// The frame changes nothing here: older than the head, on a branch that
    /// lost, or a commit this device is not part of.
    Ignored,
    /// The channel's genesis is now this one: content is sealed from now on.
    Genesis,
    /// This device joined the channel at `epoch`.
    Joined { epoch: u64 },
    /// The channel moved to `epoch`; content keys changed.
    Advanced { epoch: u64 },
    /// This device was removed.
    Removed,
    /// The frame cannot be reached from this device's state (it missed a
    /// commit, or its branch lost). The device is waiting again with a
    /// fresh member key and must send a new access request.
    Desynchronised,
}

/// What [`ChannelPeer::reconcile`] changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub admitted: Vec<Ed25519PublicKey>,
    /// Devices removed, including members removed to be admitted again.
    pub removed: Vec<Ed25519PublicKey>,
    /// Requests dropped because their key package was malformed, named
    /// another device, or was refused by MLS.
    pub refused: Vec<Ed25519PublicKey>,
}

impl Reconciled {
    /// Whether the group changed.
    pub fn changed(&self) -> bool {
        !self.admitted.is_empty() || !self.removed.is_empty()
    }
}

/// One participant device's state for one channel.
pub struct ChannelPeer {
    channel: ChannelId,
    device: Ed25519PublicKey,
    signer: Box<dyn DeviceSigner>,
    record: MemberRecord,
    /// The last frame applied here, as held (with its join material when
    /// this device had it). For a waiting device, `None`.
    head: Option<MlsGovernanceFrame>,
    /// This device's place before `head`, sealed: the group before `head`'s
    /// commit, or the member key `head`'s Welcome admitted. A lower sibling
    /// of `head` then replaces it.
    parent: Option<Vec<u8>>,
    /// The digest of the genesis this channel follows.
    genesis: Option<[u8; 32]>,
    /// The newest epoch a checked frame named, for a device not in the
    /// group: what its broker gates content on.
    seen_epoch: Option<u64>,
    /// Each device's newest request.
    requests: BTreeMap<Ed25519PublicKey, MlsAccessRequestV1>,
    /// Frames to publish, written before they were queued.
    outbox: Vec<MlsGovernanceFrame>,
    /// `None` for a peer kept in memory.
    path: Option<PathBuf>,
    /// The `issued_at` of the last request minted here, so one minted again
    /// in the same second is still newer.
    issued: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerFile {
    version: u8,
    channel: ChannelId,
    record: Vec<u8>,
    parent: Option<Vec<u8>>,
    head: Option<MlsGovernanceFrame>,
    genesis: Option<[u8; 32]>,
    seen_epoch: Option<u64>,
    requests: Vec<MlsAccessRequestV1>,
    outbox: Vec<MlsGovernanceFrame>,
    issued: u64,
}

fn digest(frame: &MlsGovernanceFrame) -> Result<[u8; 32]> {
    frame
        .digest()
        .map_err(|error| Error::Serialization(error.to_string()))
}

fn governance(message: impl Into<String>) -> Error {
    Error::Governance(message.into())
}

impl ChannelPeer {
    /// The file a peer for `channel` keeps under `dir`.
    pub fn path_in(dir: &Path, channel: ChannelId) -> PathBuf {
        dir.join(format!("{}.peer", channel.0))
    }

    /// Open the state kept for `channel` under `dir`, or `None` when there
    /// is none.
    ///
    /// # Errors
    ///
    /// Returns an error for corrupt or undecryptable state, or state of
    /// another channel or device.
    pub fn open(
        dir: &Path,
        channel: ChannelId,
        signer: Box<dyn DeviceSigner>,
        vault: &VaultKey,
    ) -> Result<Option<Self>> {
        let path = Self::path_in(dir, channel);
        let too_large = Error::InvalidState("sealed peer state is too large".into());
        let Some(bytes) =
            tilo_files::native::bounded::read_or(&path, MAX_PEER_STATE as u64, too_large)?
        else {
            return Ok(None);
        };
        let mut peer = Self::restore(channel, signer, &bytes, vault)?;
        peer.path = Some(path);
        Ok(Some(peer))
    }

    /// A new peer for `channel` that waits for access, kept under `dir`
    /// (or in memory when `dir` is `None`).
    ///
    /// # Errors
    ///
    /// Returns an error when the member key cannot be minted or written.
    pub fn waiting(
        dir: Option<&Path>,
        channel: ChannelId,
        signer: Box<dyn DeviceSigner>,
        vault: &VaultKey,
    ) -> Result<Self> {
        let device = Ed25519PublicKey::from_bytes(signer.device_public_key());
        let peer = Self {
            channel,
            device,
            signer,
            record: MemberRecord::Waiting(Box::new(MemberKey::for_device(&device)?)),
            head: None,
            parent: None,
            genesis: None,
            seen_epoch: None,
            requests: BTreeMap::new(),
            outbox: Vec::new(),
            path: dir.map(|dir| Self::path_in(dir, channel)),
            issued: 0,
        };
        peer.save(vault)?;
        Ok(peer)
    }

    /// Create `channel`'s group with this device alone in it, and queue its
    /// genesis. Only for a channel no genesis is known of: a participant
    /// that holds no state for a channel first catches up with what its room
    /// retains.
    ///
    /// # Errors
    ///
    /// Returns an error when the group cannot be created, signed or written.
    pub async fn create(
        dir: Option<&Path>,
        channel: ChannelId,
        signer: Box<dyn DeviceSigner>,
        vault: &VaultKey,
    ) -> Result<Self> {
        let device = Ed25519PublicKey::from_bytes(signer.device_public_key());
        let group = ChannelGroup::create(channel, MemberKey::for_device(&device)?)?;
        let mut peer = Self {
            channel,
            device,
            signer,
            record: MemberRecord::Joined(Box::new(group)),
            head: None,
            parent: None,
            genesis: None,
            seen_epoch: None,
            requests: BTreeMap::new(),
            outbox: Vec::new(),
            path: dir.map(|dir| Self::path_in(dir, channel)),
            issued: 0,
        };
        let genesis = peer
            .sign(MlsGovernanceFrame::unsigned(
                channel,
                [0; 32],
                0,
                MlsGovernanceAction::Genesis,
                device,
            ))
            .await?;
        peer.genesis = Some(digest(&genesis)?);
        peer.head = Some(genesis.clone());
        peer.outbox.push(genesis);
        peer.save(vault)?;
        Ok(peer)
    }

    /// [`Self::create`] with a signer that answers at once.
    ///
    /// # Errors
    ///
    /// As [`Self::create`], or when the signer would have to be awaited.
    pub fn create_now(
        dir: Option<&Path>,
        channel: ChannelId,
        signer: Box<dyn DeviceSigner>,
        vault: &VaultKey,
    ) -> Result<Self> {
        complete_now(Self::create(dir, channel, signer, vault))
    }

    /// This peer's state sealed under `vault`, for a device that keeps state
    /// in a store rather than files ([`Self::restore`] reads it back).
    ///
    /// # Errors
    ///
    /// Returns an error when the state cannot be encoded or sealed.
    pub fn export_sealed(&self, vault: &VaultKey) -> Result<Vec<u8>> {
        let file = PeerFile {
            version: PEER_VERSION,
            channel: self.channel,
            record: self.record.seal(vault)?,
            parent: self.parent.clone(),
            head: self.head.clone(),
            genesis: self.genesis,
            seen_epoch: self.seen_epoch,
            requests: self.requests.values().cloned().collect(),
            outbox: self.outbox.clone(),
            issued: self.issued,
        };
        let plaintext = Zeroizing::new(
            postcard::to_allocvec(&file)
                .map_err(|error| Error::Serialization(error.to_string()))?,
        );
        tilo_seal::envelope::seal(
            &vault.0,
            Context::new(Label::ChannelMlsState, PEER_RECORD),
            &plaintext,
        )
        .map_err(|_| Error::StateCrypto("state encryption failed"))
    }

    /// A peer kept in memory, restored from [`Self::export_sealed`].
    ///
    /// # Errors
    ///
    /// Returns an error for corrupt or undecryptable state, or state of
    /// another channel or device.
    pub fn restore(
        channel: ChannelId,
        signer: Box<dyn DeviceSigner>,
        bytes: &[u8],
        vault: &VaultKey,
    ) -> Result<Self> {
        if bytes.len() > MAX_PEER_STATE {
            return Err(Error::InvalidState("sealed peer state is too large".into()));
        }
        let plaintext = tilo_seal::envelope::open(
            &vault.0,
            Context::new(Label::ChannelMlsState, PEER_RECORD),
            bytes,
        )
        .map_err(|_| Error::StateCrypto("state authentication failed"))?;
        let file: PeerFile = postcard::from_bytes(&plaintext)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        if file.version != PEER_VERSION || file.channel != channel {
            return Err(Error::InvalidState(
                "peer state is of another version or channel".into(),
            ));
        }
        let device = Ed25519PublicKey::from_bytes(signer.device_public_key());
        let record = MemberRecord::open_sealed(&file.record, vault)?;
        let owner = match &record {
            MemberRecord::Waiting(key) => Some(key.name()),
            MemberRecord::Joined(group) => {
                if group.channel_id() != channel {
                    return Err(Error::InvalidState(
                        "peer state names another channel".into(),
                    ));
                }
                Some(group.member().name())
            }
            MemberRecord::Removed => None,
        };
        if owner.is_some_and(|owner| owner != device.as_bytes()) {
            return Err(Error::InvalidState(
                "peer state belongs to another device".into(),
            ));
        }
        Ok(Self {
            channel,
            device,
            signer,
            record,
            head: file.head,
            parent: file.parent,
            genesis: file.genesis,
            seen_epoch: file.seen_epoch,
            requests: file
                .requests
                .into_iter()
                .map(|request| (request.device, request))
                .collect(),
            outbox: file.outbox,
            path: None,
            issued: file.issued,
        })
    }

    pub fn channel_id(&self) -> ChannelId {
        self.channel
    }

    /// The device key this peer acts for.
    pub fn device(&self) -> Ed25519PublicKey {
        self.device
    }

    /// What the device can do with the channel's content.
    pub fn access(&self) -> Access {
        match &self.record {
            MemberRecord::Waiting(_) => Access::Waiting,
            MemberRecord::Joined(group) => Access::Ready {
                epoch: group.epoch(),
            },
            MemberRecord::Removed => Access::Removed,
        }
    }

    /// Content keys for the device's application endpoints, once admitted.
    pub fn content_keys(&self) -> Option<ChannelContentKeys> {
        match &self.record {
            MemberRecord::Joined(group) => group.content_keys().ok(),
            MemberRecord::Waiting(_) | MemberRecord::Removed => None,
        }
    }

    /// What a broker checks every document op of this channel against.
    pub fn gate(&self) -> ChannelGate {
        let head_epoch = match &self.record {
            MemberRecord::Joined(group) => Some(group.epoch()),
            MemberRecord::Waiting(_) | MemberRecord::Removed => self.seen_epoch,
        };
        ChannelGate {
            encryption_required: self.genesis.is_some(),
            head_epoch,
        }
    }

    /// Whether a genesis of this channel is known here.
    pub fn has_genesis(&self) -> bool {
        self.genesis.is_some()
    }

    /// Every device in the group, this one included; empty while not in it.
    pub fn members(&self) -> BTreeSet<Ed25519PublicKey> {
        match &self.record {
            MemberRecord::Joined(group) => group
                .member_identities()
                .into_iter()
                .filter_map(|(_, identity)| {
                    <[u8; 32]>::try_from(identity.as_slice())
                        .ok()
                        .map(Ed25519PublicKey::from_bytes)
                })
                .collect(),
            MemberRecord::Waiting(_) | MemberRecord::Removed => BTreeSet::new(),
        }
    }

    /// Frames this device wrote, or must publish again, that the transport
    /// has not taken yet. Call [`Self::published`] once it took them.
    pub fn outbox(&self) -> &[MlsGovernanceFrame] {
        &self.outbox
    }

    /// The transport took every frame of [`Self::outbox`].
    ///
    /// # Errors
    ///
    /// Returns an error when the emptied outbox cannot be written.
    pub fn published(&mut self, vault: &VaultKey) -> Result<()> {
        if self.outbox.is_empty() {
            return Ok(());
        }
        self.outbox.clear();
        self.save(vault)
    }

    /// Queue this device's head frame again: a transport that keeps a
    /// channel's newest frame lost it (a host restarted empty), or a device
    /// asks for it.
    pub fn republish_head(&mut self) {
        if let Some(head) = &self.head
            && !self.outbox.contains(head)
        {
            self.outbox
                .extend(head.clone().into_deliveries().unwrap_or_default());
        }
    }

    /// An access request carrying a fresh key package, still to be signed by
    /// the device key ([`MlsAccessRequestV1::unsigned`]), or `None` when the
    /// device is already admitted: for a device key held apart from this
    /// state (a browser's page custody). A removed device starts waiting
    /// again. With `fresh`, a waiting device asks with a new member key: the
    /// participants then see a device they may have admitted already (whose
    /// Welcome never arrived here) asking with another key, and admit it
    /// again.
    ///
    /// # Errors
    ///
    /// Returns an error when the key package cannot be minted or written.
    pub fn unsigned_access_request(
        &mut self,
        issued_at: u64,
        fresh: bool,
        vault: &VaultKey,
    ) -> Result<Option<MlsAccessRequestV1>> {
        if matches!(self.record, MemberRecord::Removed)
            || (fresh && matches!(self.record, MemberRecord::Waiting(_)))
        {
            self.record = MemberRecord::Waiting(Box::new(MemberKey::for_device(&self.device)?));
        }
        let MemberRecord::Waiting(member) = &self.record else {
            return Ok(None);
        };
        let package = crate::encode_key_package(&member.fresh_key_package()?)?;
        self.issued = issued_at.max(self.issued.saturating_add(1));
        // The package's HPKE secret now lives in the member key's storage;
        // it must be durable before the package leaves the device.
        self.save(vault)?;
        Ok(Some(MlsAccessRequestV1::unsigned(
            self.channel,
            self.device,
            package,
            self.issued,
        )))
    }

    /// [`Self::unsigned_access_request`], signed with this peer's signer.
    ///
    /// # Errors
    ///
    /// As [`Self::unsigned_access_request`], or when the signer fails.
    pub async fn access_request(
        &mut self,
        issued_at: u64,
        fresh: bool,
        vault: &VaultKey,
    ) -> Result<Option<MlsAccessRequestV1>> {
        let Some(mut request) = self.unsigned_access_request(issued_at, fresh, vault)? else {
            return Ok(None);
        };
        let message = request
            .signing_bytes()
            .map_err(|error| Error::Serialization(error.to_string()))?;
        request.signature = collab_types::Signature::from_bytes(
            self.signer
                .sign(message)
                .await
                .map_err(|error| Error::Signer(error.0))?,
        );
        Ok(Some(request))
    }

    /// [`Self::access_request`] with a signer that answers at once.
    ///
    /// # Errors
    ///
    /// As [`Self::access_request`], or when the signer would have to be
    /// awaited.
    pub fn access_request_now(
        &mut self,
        issued_at: u64,
        fresh: bool,
        vault: &VaultKey,
    ) -> Result<Option<MlsAccessRequestV1>> {
        complete_now(self.access_request(issued_at, fresh, vault))
    }

    /// Keep `request`, a device's newest ask for this channel, for the next
    /// [`Self::reconcile`]. The caller has checked the request came from the
    /// device it names. Returns whether it was new.
    ///
    /// # Errors
    ///
    /// Returns an error for a request of another channel, a bad signature,
    /// or a failed write.
    pub fn learn_request(&mut self, request: MlsAccessRequestV1, vault: &VaultKey) -> Result<bool> {
        if request.channel != self.channel {
            return Err(governance("access request names another channel"));
        }
        request
            .verify()
            .map_err(|error| governance(error.to_string()))?;
        if request.device == self.device || self.requests.get(&request.device) == Some(&request) {
            return Ok(false);
        }
        if !self.requests.contains_key(&request.device) && self.requests.len() >= MAX_REQUESTS {
            return Err(governance("the channel holds too many access requests"));
        }
        self.requests.insert(request.device, request);
        self.save(vault)?;
        Ok(true)
    }

    /// The devices `allowed` names whose request this peer holds and that
    /// are not in the group with the key they asked with.
    pub fn asking(&self, allowed: &BTreeSet<Ed25519PublicKey>) -> Vec<Ed25519PublicKey> {
        let MemberRecord::Joined(group) = &self.record else {
            return Vec::new();
        };
        self.requests
            .iter()
            .filter(|(device, request)| {
                allowed.contains(*device) && !admitted_with(group, device, request)
            })
            .map(|(device, _)| *device)
            .collect()
    }

    /// Whether the group differs from `allowed`: a member it no longer
    /// names, or an allowed device asking.
    pub fn differs(&self, allowed: &BTreeSet<Ed25519PublicKey>) -> bool {
        if !matches!(self.record, MemberRecord::Joined(_)) || !allowed.contains(&self.device) {
            return false;
        }
        self.members()
            .iter()
            .any(|member| *member != self.device && !allowed.contains(member))
            || !self.asking(allowed).is_empty()
    }

    /// Write the one commit that moves the group to `allowed` and queue it,
    /// when this device is in the group, `allowed` names it, and the group
    /// differs. Waits for nothing but the signer.
    ///
    /// # Errors
    ///
    /// Returns an error when the commit cannot be staged, signed or written;
    /// the group is then as it was.
    pub async fn reconcile(
        &mut self,
        allowed: &BTreeSet<Ed25519PublicKey>,
        vault: &VaultKey,
    ) -> Result<Reconciled> {
        self.drop_unfinished();
        let mut outcome = Reconciled::default();
        if !self.differs(allowed) {
            return Ok(outcome);
        }
        let members = self.members();
        let leaving: Vec<Ed25519PublicKey> = members
            .iter()
            .filter(|member| **member != self.device && !allowed.contains(*member))
            .copied()
            .collect();
        let mut joining = self.joining(allowed, &mut outcome);
        let MemberRecord::Joined(group) = &mut self.record else {
            return Ok(outcome);
        };
        let parent = MemberRecord::seal_joined(group, vault)?;
        let stage = |group: &mut ChannelGroup, joining: &[(Ed25519PublicKey, KeyPackage)]| {
            let mut removed = leaving.clone();
            removed.extend(
                joining
                    .iter()
                    .map(|(device, _)| *device)
                    .filter(|device| members.contains(device)),
            );
            let leaves: Vec<u32> = removed
                .iter()
                .filter_map(|device| group.leaf_index_of(device.as_bytes()))
                .collect();
            let packages: Vec<KeyPackage> =
                joining.iter().map(|(_, package)| package.clone()).collect();
            group
                .stage_commit(&leaves, &packages)
                .map(|staged| (staged, removed))
        };
        let (staged, removed) = match stage(group, &joining) {
            Ok(staged) => staged,
            // A key package MLS refuses would hold back every change: drop
            // those requests and commit the removals alone.
            Err(_) if !joining.is_empty() => {
                for (device, _) in std::mem::take(&mut joining) {
                    self.requests.remove(&device);
                    outcome.refused.push(device);
                }
                if leaving.is_empty() {
                    self.save(vault)?;
                    return Ok(outcome);
                }
                stage(group, &[])?
            }
            Err(error) => return Err(error),
        };
        let added: Vec<Ed25519PublicKey> = joining.iter().map(|(device, _)| *device).collect();
        let previous = match &self.head {
            Some(head) => digest(head)?,
            None => return self.abandon(Error::InvalidState("joined without a head".into())),
        };
        let material = staged.joining.map(|(welcome, key_history)| {
            Joining::Material(JoinMaterial {
                welcome,
                key_history,
            })
        });
        let frame = MlsGovernanceFrame::unsigned(
            self.channel,
            previous,
            staged.epoch,
            MlsGovernanceAction::Commit {
                removed: removed.clone(),
                added: added.clone(),
                commit: staged.commit,
                joining: material,
            },
            self.device,
        );
        let frame = match self.sign(frame).await {
            Ok(frame) => frame,
            Err(error) => return self.abandon(error),
        };
        let MemberRecord::Joined(group) = &mut self.record else {
            return Ok(outcome);
        };
        group.merge_pending_transaction()?;
        self.parent = Some(parent);
        self.head = Some(frame.clone());
        self.outbox.extend(
            frame
                .into_deliveries()
                .map_err(|error| Error::Serialization(error.to_string()))?,
        );
        self.save(vault)?;
        outcome.admitted = added;
        outcome.removed = removed;
        Ok(outcome)
    }

    /// The key package of every allowed device asking, dropping the requests
    /// whose package does not name their device.
    fn joining(
        &mut self,
        allowed: &BTreeSet<Ed25519PublicKey>,
        outcome: &mut Reconciled,
    ) -> Vec<(Ed25519PublicKey, KeyPackage)> {
        let mut joining = Vec::new();
        for device in self.asking(allowed) {
            let Some(request) = self.requests.get(&device) else {
                continue;
            };
            match device_key_package(request) {
                Ok(package) => joining.push((device, package)),
                Err(_) => outcome.refused.push(device),
            }
        }
        for device in &outcome.refused {
            self.requests.remove(device);
        }
        joining
    }

    /// [`Self::reconcile`] with a signer that answers at once.
    ///
    /// # Errors
    ///
    /// As [`Self::reconcile`], or when the signer would have to be awaited.
    pub fn reconcile_now(
        &mut self,
        allowed: &BTreeSet<Ed25519PublicKey>,
        vault: &VaultKey,
    ) -> Result<Reconciled> {
        complete_now(self.reconcile(allowed, vault))
    }

    /// Drop a commit a reconcile staged and never finished: its future was
    /// dropped while it awaited the signer.
    fn drop_unfinished(&mut self) {
        if let MemberRecord::Joined(group) = &mut self.record {
            group.discard_pending_transaction().ok();
        }
    }

    fn abandon<T>(&mut self, error: Error) -> Result<T> {
        if let MemberRecord::Joined(group) = &mut self.record {
            group.discard_pending_transaction().ok();
        }
        Err(error)
    }

    /// Apply one frame delivered by any transport, checked against
    /// `allowed`, the devices the membership lets read the channel now.
    ///
    /// # Errors
    ///
    /// Returns an error for a frame of another channel, a bad signature, a
    /// commit the membership does not back (see the module docs), or a
    /// failed write. A frame this device cannot reach is not an error: it
    /// yields [`PeerEvent::Desynchronised`].
    pub fn receive(
        &mut self,
        frame: MlsGovernanceFrame,
        allowed: &BTreeSet<Ed25519PublicKey>,
        vault: &VaultKey,
    ) -> Result<PeerEvent> {
        if frame.channel != self.channel {
            return Err(governance("frame names another channel"));
        }
        self.drop_unfinished();
        frame
            .check()
            .map_err(|error| governance(error.to_string()))?;
        if !allowed.contains(&frame.signed_by) {
            return Err(governance("frame is not signed by a participant"));
        }
        let incoming = digest(&frame)?;
        match &frame.action {
            MlsGovernanceAction::Genesis => self.receive_genesis(&frame, incoming, vault),
            MlsGovernanceAction::Commit { .. } => {
                let removed: BTreeSet<_> = frame.removed().iter().collect();
                let added: BTreeSet<_> = frame.added().iter().collect();
                if added.iter().any(|device| !allowed.contains(*device))
                    || removed
                        .iter()
                        .any(|device| allowed.contains(*device) && !added.contains(*device))
                {
                    return Err(governance(
                        "commit adds or removes a device the membership does not",
                    ));
                }
                match &self.record {
                    MemberRecord::Joined(_) => self.receive_commit(frame, incoming, allowed, vault),
                    MemberRecord::Waiting(_) => self.receive_welcome(frame, incoming, vault),
                    MemberRecord::Removed => Ok(PeerEvent::Ignored),
                }
            }
        }
    }

    fn receive_genesis(
        &mut self,
        frame: &MlsGovernanceFrame,
        incoming: [u8; 32],
        vault: &VaultKey,
    ) -> Result<PeerEvent> {
        match self.genesis {
            Some(known) if known == incoming => Ok(PeerEvent::Duplicate),
            Some(known) if known < incoming => Ok(PeerEvent::Ignored),
            known => {
                self.genesis = Some(incoming);
                self.seen_epoch = Some(self.seen_epoch.unwrap_or(0).max(frame.epoch));
                if known.is_some() && matches!(self.record, MemberRecord::Joined(_)) {
                    // This device's group grew from a genesis that lost.
                    return self.desynchronise(vault);
                }
                self.save(vault)?;
                Ok(PeerEvent::Genesis)
            }
        }
    }

    /// A commit for a device in the group, under the one-commit-per-epoch
    /// rule.
    fn receive_commit(
        &mut self,
        frame: MlsGovernanceFrame,
        incoming: [u8; 32],
        allowed: &BTreeSet<Ed25519PublicKey>,
        vault: &VaultKey,
    ) -> Result<PeerEvent> {
        let Some(head) = &self.head else {
            return self.desynchronise(vault);
        };
        let head_digest = digest(head)?;
        if incoming == head_digest {
            return Ok(PeerEvent::Duplicate);
        }
        let head_epoch = head.epoch;
        if frame.epoch == head_epoch && frame.previous_frame_digest == head.previous_frame_digest {
            if incoming > head_digest {
                // The sibling lost. Whoever wrote the head publishes it again,
                // so a host keeping the newest frame keeps the winner.
                if head.signed_by == self.device {
                    self.republish_head();
                    self.save(vault)?;
                }
                return Ok(PeerEvent::Ignored);
            }
            let Some(parent) = &self.parent else {
                return self.desynchronise(vault);
            };
            let parent = MemberRecord::open_sealed(parent, vault)?;
            let joins_from_welcome = matches!(parent, MemberRecord::Waiting(_));
            if joins_from_welcome && frame.join_material().is_none() {
                // The copy carrying the Welcome for this device follows.
                return Ok(PeerEvent::Ignored);
            }
            let replaced = std::mem::replace(&mut self.record, parent);
            let applied = if joins_from_welcome {
                self.receive_welcome(frame, incoming, vault)
            } else {
                self.apply_commit(frame, allowed, vault)
            };
            return match applied {
                Ok(PeerEvent::Ignored | PeerEvent::Desynchronised) | Err(_) => {
                    self.record = replaced;
                    self.desynchronise(vault)
                }
                Ok(event) => Ok(event),
            };
        }
        if frame.epoch <= head_epoch {
            return Ok(PeerEvent::Ignored);
        }
        if frame.epoch == head_epoch.saturating_add(1) {
            if frame.previous_frame_digest == head_digest {
                return self.apply_commit(frame, allowed, vault);
            }
            // It follows a sibling of the head: this branch lost when that
            // sibling's digest is lower, and the frame's when it is higher.
            if frame.previous_frame_digest < head_digest {
                return self.desynchronise(vault);
            }
            return Ok(PeerEvent::Ignored);
        }
        // The commits between are gone: a host keeps only the newest.
        self.desynchronise(vault)
    }

    /// Apply `frame`'s commit to the group held now, after checking its MLS
    /// proposals match what the frame declares.
    fn apply_commit(
        &mut self,
        frame: MlsGovernanceFrame,
        allowed: &BTreeSet<Ed25519PublicKey>,
        vault: &VaultKey,
    ) -> Result<PeerEvent> {
        let me = self.device;
        let MemberRecord::Joined(group) = &mut self.record else {
            return Ok(PeerEvent::Ignored);
        };
        let MlsGovernanceAction::Commit {
            removed,
            added,
            commit,
            ..
        } = &frame.action
        else {
            return Ok(PeerEvent::Ignored);
        };
        let members: BTreeSet<Vec<u8>> = group
            .member_identities()
            .into_iter()
            .map(|(_, identity)| identity)
            .collect();
        if !members.contains(frame.signed_by.as_bytes().as_slice())
            || removed
                .iter()
                .any(|device| !members.contains(device.as_bytes().as_slice()))
        {
            return Err(governance(
                "commit is not from a member, or removes a non-member",
            ));
        }
        if removed.contains(&me) && added.contains(&me) {
            // Admitted again with a key package this joined device no longer
            // holds: start over and ask with a fresh one.
            return self.desynchronise(vault);
        }
        let removes_me = removed.contains(&me);
        if removes_me && allowed.contains(&me) {
            return Err(governance(
                "commit removes this device, which the membership still names",
            ));
        }
        let parent = MemberRecord::seal_joined(group, vault)?;
        let declared = |devices: &[Ed25519PublicKey]| -> BTreeSet<Vec<u8>> {
            devices
                .iter()
                .map(|device| device.as_bytes().to_vec())
                .collect()
        };
        let (declared_added, declared_removed) = (declared(added), declared(removed));
        let signer = frame.signed_by;
        group.process_commit_checked(commit, |incoming| {
            let actual_added: BTreeSet<Vec<u8>> = incoming.added.iter().cloned().collect();
            let actual_removed: BTreeSet<Vec<u8>> = incoming.removed.iter().cloned().collect();
            if incoming.committer != signer.as_bytes()
                || actual_added != declared_added
                || actual_removed != declared_removed
                || actual_added.len() != incoming.added.len()
            {
                return Err(governance("commit does not do what its frame declares"));
            }
            Ok(())
        })?;
        if removes_me {
            self.record = MemberRecord::Removed;
            self.head = None;
            self.parent = None;
            self.seen_epoch = Some(frame.epoch);
            self.save(vault)?;
            return Ok(PeerEvent::Removed);
        }
        let epoch = group.epoch();
        // A request from a device the commit added with another key is older
        // than the one it was admitted with.
        let group: &ChannelGroup = group;
        self.requests.retain(|device, request| {
            !added.contains(device) || admitted_with(group, device, request)
        });
        self.parent = Some(parent);
        self.head = Some(frame);
        self.save(vault)?;
        Ok(PeerEvent::Advanced { epoch })
    }

    /// A commit for a waiting device: its admission when the frame carries
    /// a Welcome for this device's member key.
    fn receive_welcome(
        &mut self,
        frame: MlsGovernanceFrame,
        incoming: [u8; 32],
        vault: &VaultKey,
    ) -> Result<PeerEvent> {
        let MemberRecord::Waiting(pending) = &self.record else {
            return Ok(PeerEvent::Ignored);
        };
        self.seen_epoch = Some(self.seen_epoch.unwrap_or(0).max(frame.epoch));
        let welcome = frame
            .join_material()
            .filter(|_| frame.added().contains(&self.device))
            .filter(|material| ChannelGroup::welcome_names(&material.welcome, pending));
        let Some(material) = welcome else {
            return Ok(PeerEvent::Ignored);
        };
        let attempt = pending.duplicate()?;
        let parent = MemberRecord::seal_waiting(pending, vault)?;
        match ChannelGroup::process_welcome_with_history(
            self.channel,
            &material.welcome,
            &material.key_history,
            attempt,
        ) {
            Ok(group) => {
                let epoch = group.epoch();
                debug_assert_eq!(epoch, frame.epoch, "{incoming:?}");
                self.record = MemberRecord::Joined(Box::new(group));
                self.head = Some(frame);
                self.parent = Some(parent);
                self.save(vault)?;
                Ok(PeerEvent::Joined { epoch })
            }
            // A Welcome for a key package this device no longer holds: stay
            // waiting; the next request is admitted anew.
            Err(_) => Ok(PeerEvent::Desynchronised),
        }
    }

    /// Start over: wait with a fresh member key, keeping the gate.
    fn desynchronise(&mut self, vault: &VaultKey) -> Result<PeerEvent> {
        if let MemberRecord::Joined(group) = &self.record {
            self.seen_epoch = Some(self.seen_epoch.unwrap_or(0).max(group.epoch()));
        }
        self.record = MemberRecord::Waiting(Box::new(MemberKey::for_device(&self.device)?));
        self.head = None;
        self.parent = None;
        self.save(vault)?;
        Ok(PeerEvent::Desynchronised)
    }

    async fn sign(&self, mut frame: MlsGovernanceFrame) -> Result<MlsGovernanceFrame> {
        let message = frame
            .signing_bytes()
            .map_err(|error| Error::Serialization(error.to_string()))?;
        frame.signature = collab_types::Signature::from_bytes(
            self.signer
                .sign(message)
                .await
                .map_err(|error| Error::Signer(error.0))?,
        );
        frame
            .verify_signature()
            .map_err(|error| Error::Signer(error.to_string()))?;
        Ok(frame)
    }

    fn save(&self, vault: &VaultKey) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let sealed = self.export_sealed(vault)?;
        tilo_files::native::atomic::write_private(path, &sealed).map_err(Error::Io)
    }
}

/// Whether `device` is in `group` with the member key `request` asked with.
fn admitted_with(
    group: &ChannelGroup,
    device: &Ed25519PublicKey,
    request: &MlsAccessRequestV1,
) -> bool {
    let Some(joined_with) = group.signature_key_of(device.as_bytes()) else {
        return false;
    };
    device_key_package(request).is_ok_and(|package| {
        package.leaf_node().signature_key().as_slice() == joined_with.as_slice()
    })
}

/// Decode a request's key package and check its credential names the
/// requesting device.
fn device_key_package(request: &MlsAccessRequestV1) -> Result<KeyPackage> {
    let package = crate::decode_key_package(&request.key_package)?;
    let credential = BasicCredential::try_from(package.leaf_node().credential().clone())
        .map_err(Error::openmls)?;
    if credential.identity() != request.device.as_bytes() {
        return Err(governance("key package credential names another device"));
    }
    Ok(package)
}

impl std::fmt::Debug for ChannelPeer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelPeer")
            .field("channel", &self.channel)
            .field("access", &self.access())
            .field("requests", &self.requests.len())
            .field("outbox", &self.outbox.len())
            .finish_non_exhaustive()
    }
}
