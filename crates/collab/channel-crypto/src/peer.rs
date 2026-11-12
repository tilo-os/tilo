//! One participant device's side of one channel.
//!
//! A channel's MLS group holds exactly the devices its membership lets read
//! it: a projection the caller computes from the workspace's membership and
//! access policy (`allowed`), never a second record of who belongs. Every
//! participant device keeps a [`ChannelPeer`] and follows the same rules:
//!
//! - **Any participant commits.** [`ChannelPeer::reconcile`] writes the one
//!   commit that moves the group to `allowed`: it removes every member the
//!   projection no longer names, and adds every allowed device that asked
//!   ([`ChannelPeer::learn_request`]) with the Welcome and the bundle of
//!   every past content key. A member asking with a key package whose
//!   signature key is not the one it joined with lost its state, and is
//!   removed and added again in the same commit. Members take turns at it
//!   ([`ChannelPeer::turn`]) so that one commits the change and the others
//!   follow it: members that all commit the same change at once fork the
//!   group, and each one whose commit lost commits again to hand over its
//!   branch's keys, forking it again.
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
//!   commits for one epoch that followed different frames compete the same
//!   way, at the point where they diverged: of the two children of the last
//!   frame both branches share, the lower digest wins. Each device keeps the
//!   newest frames of its own branch (at most [`MAX_BRANCH`] of them, and
//!   [`MAX_BRANCH_BYTES`] of commits), so it finds that point however many
//!   commits either side went on by. A device whose branch lost there cannot
//!   follow the winner from where it stands, so it starts over and asks again
//!   (below). A device on the winning branch remembers the commits that lost
//!   against it, and the ones that went on from them, and publishes its
//!   branch again from the point of divergence whenever one reaches it: the
//!   device that went on hears the winner's commit at that point and starts
//!   over. A commit whose branch split off below the frames a device holds
//!   decides nothing there; the device publishes its own branch again for
//!   the other side to decide on.
//! - **Any participant creates the group, once.** A participant that holds
//!   no state for a channel waits and asks; one that sees no genesis
//!   ([`ChannelPeer::found`]) creates the group, and one that has seen a
//!   genesis never creates another. Two geneses made at once compete like
//!   two commits: every device keeps the lower digest, and a member of the
//!   other group starts over and joins the winner.
//! - **No branch takes content with it.** A device that leaves a group state
//!   behind (a genesis or a commit that lost, or a state it can no longer
//!   follow) keeps that state's content keys, and once it is in the group
//!   again its next commit hands every member the ones they lack (sealed for
//!   that commit's epoch, [`MlsGovernanceAction::Commit`]'s `history`), and
//!   every later joiner gets them with the rest of the history. Whatever was
//!   sealed under a branch that lost stays readable to every member.
//! - **Falling behind starts over.** Hosts keep only a channel's newest
//!   frame, so a member that finds a frame it can never reach (it missed
//!   one, or followed a branch that lost) waits again with a fresh member
//!   key and asks anew; any participant admits it, history included.
//!
//! A device that cannot name the channel's record (it lost the route that
//! says whose record the channel carries) cannot tell who may read it, so it
//! commits nothing; it still follows the commits of the participants that
//! can ([`ChannelPeer::follow`]).
//!
//! Removal takes effect at the membership op, which the caller's broker
//! enforces on its own; the commit that rotates the key comes from whichever
//! participant reconciles next. A store (carrier) device never keeps a peer.
//!
//! State is written before anything it produced leaves the device: a frame
//! reaches [`ChannelPeer::outbox`] only once the state holding it is
//! durable (in the host's store for a peer opened from one, or in the export a
//! browser writes before it sends). A Welcome or another member's commit
//! taken in produces nothing that leaves the device, so its state is written
//! later ([`ChannelPeer::save_deferred`]), off the path to the page's first
//! frame: losing it only makes the device ask again.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
use std::sync::Arc;

use collab_types::{
    ChannelId, Ed25519PublicKey, JoinMaterial, Joining, MlsAccessRequestV1, MlsGovernanceAction,
    MlsGovernanceFrame,
};
use openmls::prelude::BasicCredential;
use serde::{Deserialize, Serialize};
use tilo_seal::{Context, Label};
use zeroize::{Zeroize as _, Zeroizing};

use crate::formats::Step;
use crate::gate::ChannelGate;
use crate::persistence::MemberRecord;
use crate::signer::{DeviceSigner, complete_now};
use crate::{
    ChannelContentKeys, ChannelGroup, ContentKeyring, Error, KeyPackage, MemberKey, Result,
    VaultKey,
};

/// Whether the membership `allowed` backs `frame`: it is signed by a device
/// the membership names, and a commit changes the group only by devices the
/// membership names. A genesis carries no add or remove, so this is its whole
/// rule.
pub fn backs(frame: &MlsGovernanceFrame, allowed: &BTreeSet<Ed25519PublicKey>) -> bool {
    if !allowed.contains(&frame.signed_by) {
        return false;
    }
    let removed: BTreeSet<_> = frame.removed().iter().collect();
    let added: BTreeSet<_> = frame.added().iter().collect();
    !added.iter().any(|device| !allowed.contains(*device))
        && !removed
            .iter()
            .any(|device| allowed.contains(*device) && !added.contains(*device))
}

/// Requests one channel keeps, one per device.
const MAX_REQUESTS: usize = 4096;
/// Frames below the head a peer keeps of its own branch, to find where an
/// incoming commit's branch diverged from it. A branch that went on by more
/// commits than this before the other side's reached it can no longer be
/// placed: such a commit is ignored here, and the device that holds the
/// point of divergence decides.
pub const MAX_BRANCH: usize = 32;
/// The MLS bytes the kept branch frames may hold together; the oldest go
/// first beyond it.
pub const MAX_BRANCH_BYTES: usize = 8 * 1024 * 1024;
/// Commits a peer remembers as having lost against its branch.
const MAX_LOST: usize = 64;
/// Sealed peer state larger than this is refused when read.
const MAX_PEER_STATE: usize = 320 * 1024 * 1024;
const PEER_RECORD: &[u8] = b"channel-crypto/channel-peer";
const PEER_VERSION: u8 = 3;
/// How peer state an earlier build saved becomes the current format
/// ([`crate::formats`]).
const PEER_UPGRADES: &[Step] = &[Step {
    from: 2,
    upgrade: peer_2_to_3,
}];

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
    /// A commit admitted this waiting device with a member key it no longer
    /// holds. It still waits, and asks again with the key it holds now.
    AdmittedWithAnotherKey,
    /// A Welcome for this waiting device was refused for the reason named.
    ///
    /// [`crate::WelcomeRefusal::SignedByAnotherDevice`] is a member of the
    /// channel sending it into a group that is not the one the frame commits.
    /// Like [`Self::Desynchronised`], the device waits with a fresh member key
    /// and must send a new access request: the members that took the frame's
    /// commit hold this device admitted with the key it asked with, and admit
    /// it again only when it asks with another.
    ///
    /// [`crate::WelcomeRefusal::DeviceNotAllowed`] is judged against this
    /// device's view of the membership, which may be behind or ahead of the
    /// committer's: an honest Welcome names a device admitted a moment ago
    /// that this device has not heard of yet, or one it heard was removed
    /// that the committer has not. So the device keeps the key the Welcome
    /// is for, and the caller offers the same frame again once its view of
    /// the membership changed. It gives up with [`ChannelPeer::refuse_welcome`]
    /// when the channel moved past the frame or its view does not come to
    /// back the Welcome in time.
    WelcomeRefused(crate::WelcomeRefusal),
}

/// What [`ChannelPeer::reconcile`] changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub admitted: Vec<Ed25519PublicKey>,
    /// Devices removed, including members removed to be admitted again.
    pub removed: Vec<Ed25519PublicKey>,
    /// The commit handed every member content keys of a branch that lost.
    pub history: bool,
    /// Requests dropped because their key package was malformed, named
    /// another device, or was refused by MLS.
    pub refused: Vec<Ed25519PublicKey>,
}

impl Reconciled {
    /// Whether the group changed.
    pub fn changed(&self) -> bool {
        !self.admitted.is_empty() || !self.removed.is_empty() || self.history
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
    /// The frames `head` follows on this device's branch, oldest first and
    /// with their join material withheld, bounded by [`MAX_BRANCH`] and
    /// [`MAX_BRANCH_BYTES`].
    branch: Vec<MlsGovernanceFrame>,
    /// Commits that lost against this branch, and those that went on from
    /// them, each with the epoch this branch diverged from it at (the epoch
    /// of the winning child), newest last.
    lost: Vec<([u8; 32], u64)>,
    /// This device's place before `head`, sealed: the group before `head`'s
    /// commit, or the member key `head`'s Welcome admitted. A lower sibling
    /// of `head` then replaces it.
    parent: Option<Vec<u8>>,
    /// The digest of the genesis this channel follows.
    genesis: Option<[u8; 32]>,
    /// That genesis, published again when a losing one appears, so a device
    /// that made the loser while it heard nothing hears of the winner.
    genesis_frame: Option<MlsGovernanceFrame>,
    /// The newest epoch a checked frame named, for a device not in the
    /// group: what its broker gates content on.
    seen_epoch: Option<u64>,
    /// Each device's newest request.
    requests: BTreeMap<Ed25519PublicKey, MlsAccessRequestV1>,
    /// Frames to publish, written before they were queued.
    outbox: Vec<MlsGovernanceFrame>,
    /// Content keys the group this device is in may lack: those of group
    /// states it left behind, and those of legacy per-record groups it holds
    /// for units this group now carries ([`Self::adopt`]). Handed to the
    /// members with its next commit.
    orphaned: ContentKeyring,
    /// `None` for a peer kept in memory.
    states: Option<Arc<dyn PeerStates>>,
    /// The `issued_at` of the last request minted here, so one minted again
    /// in the same second is still newer.
    issued: u64,
    /// The format the state this peer was loaded from was written in, when
    /// an earlier build wrote it ([`crate::formats`]).
    upgraded_from: Option<u8>,
    /// A change not yet written to `states`: one whose loss only makes this
    /// device ask for the channel again ([`Self::save_deferred`]).
    unsaved: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerFile {
    version: u8,
    channel: ChannelId,
    record: Vec<u8>,
    parent: Option<Vec<u8>>,
    head: Option<MlsGovernanceFrame>,
    branch: Vec<MlsGovernanceFrame>,
    lost: Vec<([u8; 32], u64)>,
    genesis: Option<[u8; 32]>,
    seen_epoch: Option<u64>,
    requests: Vec<MlsAccessRequestV1>,
    outbox: Vec<MlsGovernanceFrame>,
    issued: u64,
    orphaned: Vec<(u64, [u8; 32])>,
    genesis_frame: Option<MlsGovernanceFrame>,
}

/// Peer state as format 2 kept it: format 3 added the device's recent frames
/// below its head (`branch`) and the commits that lost against it (`lost`).
#[derive(Deserialize)]
struct PeerFileV2 {
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
    orphaned: Vec<(u64, [u8; 32])>,
    genesis_frame: Option<MlsGovernanceFrame>,
}

/// Format 2 written as format 3. A device upgraded holds no frames below its
/// head yet and knows of no commit that lost against it: it keeps both from
/// the next frame it takes in, as it does after joining.
fn peer_2_to_3(state: &[u8]) -> Result<Vec<u8>> {
    let old: PeerFileV2 =
        postcard::from_bytes(state).map_err(|error| Error::Serialization(error.to_string()))?;
    debug_assert_eq!(old.version, 2);
    postcard::to_allocvec(&PeerFile {
        version: 3,
        channel: old.channel,
        record: old.record,
        parent: old.parent,
        head: old.head,
        branch: Vec::new(),
        lost: Vec::new(),
        genesis: old.genesis,
        seen_epoch: old.seen_epoch,
        requests: old.requests,
        outbox: old.outbox,
        issued: old.issued,
        orphaned: old.orphaned,
        genesis_frame: old.genesis_frame,
    })
    .map_err(|error| Error::Serialization(error.to_string()))
}

fn digest(frame: &MlsGovernanceFrame) -> Result<[u8; 32]> {
    frame
        .digest()
        .map_err(|error| Error::Serialization(error.to_string()))
}

fn governance(message: impl Into<String>) -> Error {
    Error::Governance(message.into())
}

/// Where a peer's sealed state is kept between passes of the device: the
/// files under a desktop's channels directory ([`Files`]), or a page's own
/// store. `None` on a peer keeps its state in memory only.
pub trait PeerStates: std::fmt::Debug + collab_types::signer::MaybeSendSync {
    /// The state kept for `channel`, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be read, or holds state too
    /// large to be a peer's.
    fn read(&self, channel: ChannelId) -> Result<Option<Vec<u8>>>;

    /// Replace the state kept for `channel`.
    ///
    /// # Errors
    ///
    /// Returns an error when the store refuses the write.
    fn write(&self, channel: ChannelId, sealed: &[u8]) -> Result<()>;

    /// Drop the state kept for `channel`; a channel with none is not an error.
    ///
    /// # Errors
    ///
    /// Returns an error when the store refuses the removal.
    fn forget(&self, channel: ChannelId) -> Result<()>;
}

/// A desktop's channels directory: one file per peer, named for the channel it
/// belongs to, as every earlier build wrote it.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct Files(PathBuf);

#[cfg(not(target_arch = "wasm32"))]
impl Files {
    /// The peers kept in `dir`.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self(dir.into())
    }

    fn path(&self, channel: ChannelId) -> PathBuf {
        self.0.join(format!("{}.peer", channel.0))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl PeerStates for Files {
    fn read(&self, channel: ChannelId) -> Result<Option<Vec<u8>>> {
        let too_large = Error::InvalidState("sealed peer state is too large".into());
        tilo_files::native::bounded::read_or(&self.path(channel), MAX_PEER_STATE as u64, too_large)
    }

    fn write(&self, channel: ChannelId, sealed: &[u8]) -> Result<()> {
        tilo_files::native::atomic::write_private(&self.path(channel), sealed).map_err(Error::Io)
    }

    fn forget(&self, channel: ChannelId) -> Result<()> {
        match std::fs::remove_file(self.path(channel)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }
}

impl ChannelPeer {
    /// Open the state `states` keeps for `channel`, or `None` when there is
    /// none.
    ///
    /// # Errors
    ///
    /// Returns an error for corrupt or undecryptable state, or state of
    /// another channel or device.
    pub fn open(
        states: Option<Arc<dyn PeerStates>>,
        channel: ChannelId,
        signer: Box<dyn DeviceSigner>,
        vault: &VaultKey,
    ) -> Result<Option<Self>> {
        let Some(bytes) = states
            .as_ref()
            .map(|states| states.read(channel))
            .transpose()?
            .flatten()
        else {
            return Ok(None);
        };
        let mut peer = Self::restore(channel, signer, &bytes, vault)?;
        peer.states = states;
        // State an earlier build saved is kept in the current format from
        // now on, so the upgrade runs once.
        if peer.upgraded_from.is_some() {
            peer.save(vault)?;
        }
        Ok(Some(peer))
    }

    /// A new peer for `channel` that waits for access, keeping its state in
    /// `states` (or in memory when there is none).
    ///
    /// # Errors
    ///
    /// Returns an error when the member key cannot be minted or written.
    pub fn waiting(
        states: Option<Arc<dyn PeerStates>>,
        channel: ChannelId,
        signer: Box<dyn DeviceSigner>,
        vault: &VaultKey,
    ) -> Result<Self> {
        let device = Ed25519PublicKey::from_bytes(signer.device_public_key());
        let mut peer = Self {
            channel,
            device,
            signer,
            record: MemberRecord::Waiting(Box::new(MemberKey::for_device(&device)?)),
            head: None,
            branch: Vec::new(),
            lost: Vec::new(),
            parent: None,
            genesis: None,
            genesis_frame: None,
            seen_epoch: None,
            requests: BTreeMap::new(),
            outbox: Vec::new(),
            orphaned: ContentKeyring::new(),
            states,
            issued: 0,
            upgraded_from: None,
            unsaved: false,
        };
        peer.save(vault)?;
        Ok(peer)
    }

    /// Create `channel`'s group with this device alone in it, and queue its
    /// genesis. Only for a channel no genesis is known of: a participant
    /// that holds no state for a channel first waits and catches up with
    /// what its workspace retains, then creates the group with [`Self::found`].
    ///
    /// # Errors
    ///
    /// Returns an error when the group cannot be created, signed or written.
    pub async fn create(
        states: Option<Arc<dyn PeerStates>>,
        channel: ChannelId,
        signer: Box<dyn DeviceSigner>,
        vault: &VaultKey,
    ) -> Result<Self> {
        let mut peer = Self::waiting(states, channel, signer, vault)?;
        peer.found(vault).await?;
        Ok(peer)
    }

    /// [`Self::create`] with a signer that answers at once.
    ///
    /// # Errors
    ///
    /// As [`Self::create`], or when the signer would have to be awaited.
    pub fn create_now(
        states: Option<Arc<dyn PeerStates>>,
        channel: ChannelId,
        signer: Box<dyn DeviceSigner>,
        vault: &VaultKey,
    ) -> Result<Self> {
        complete_now(Self::create(states, channel, signer, vault))
    }

    /// Create the channel's group with this waiting device alone in it, and
    /// queue its genesis, when no group of the channel is known here.
    /// Returns whether it did: a device that has seen a genesis or a commit,
    /// or holds a group, never creates another ([`Self::knows_group`]). The device's own access request goes
    /// unanswered from then on; the requests it holds are admitted by its
    /// next reconcile.
    ///
    /// # Errors
    ///
    /// Returns an error when the group cannot be created, signed or written;
    /// the device is then still waiting.
    pub async fn found(&mut self, vault: &VaultKey) -> Result<bool> {
        if self.knows_group() {
            return Ok(false);
        }
        let genesis = self
            .sign(MlsGovernanceFrame::unsigned(
                self.channel,
                [0; 32],
                0,
                MlsGovernanceAction::Genesis,
                self.device,
            ))
            .await?;
        // The group is founded with the member key this device asked with
        // while it waited: participants that hold its request then see it in
        // the group with the key it asked with. With any other key they take
        // the request for a device that lost its state and admit it again,
        // a commit that sets every participant committing at once.
        let member = match std::mem::replace(&mut self.record, MemberRecord::Removed) {
            MemberRecord::Waiting(member) => *member,
            other => {
                self.record = other;
                MemberKey::for_device(&self.device)?
            }
        };
        let founded_by = digest(&genesis)?;
        let group = match ChannelGroup::found(self.channel, member, founded_by) {
            Ok(group) => group,
            Err(error) => {
                self.record = MemberRecord::Waiting(Box::new(MemberKey::for_device(&self.device)?));
                return Err(error);
            }
        };
        self.record = MemberRecord::Joined(Box::new(group));
        self.parent = None;
        self.genesis = Some(founded_by);
        self.genesis_frame = Some(genesis.clone());
        self.restart_branch(genesis.clone());
        self.outbox.push(genesis);
        self.settle_orphaned();
        self.save(vault)?;
        Ok(true)
    }

    /// [`Self::found`] with a signer that answers at once.
    ///
    /// # Errors
    ///
    /// As [`Self::found`], or when the signer would have to be awaited.
    pub fn found_now(&mut self, vault: &VaultKey) -> Result<bool> {
        complete_now(self.found(vault))
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
            branch: self.branch.clone(),
            lost: self.lost.clone(),
            genesis: self.genesis,
            seen_epoch: self.seen_epoch,
            requests: self.requests.values().cloned().collect(),
            outbox: self.outbox.clone(),
            issued: self.issued,
            orphaned: self.orphaned.to_wire(),
            genesis_frame: self.genesis_frame.clone(),
        };
        let mut file = file;
        let encoded = postcard::to_allocvec(&file)
            .map(Zeroizing::new)
            .map_err(|error| Error::Serialization(error.to_string()));
        for (_, key) in &mut file.orphaned {
            key.zeroize();
        }
        let plaintext = encoded?;
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
        let (plaintext, upgraded_from) = crate::formats::upgrade(
            PEER_VERSION,
            PEER_UPGRADES,
            Zeroizing::new(plaintext.to_vec()),
        )?;
        let file: PeerFile = postcard::from_bytes(&plaintext)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        if file.channel != channel {
            return Err(Error::InvalidState(
                "peer state is of another channel".into(),
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
            return Err(Error::OtherDevice);
        }
        Ok(Self {
            channel,
            device,
            signer,
            record,
            head: file.head,
            branch: file.branch,
            lost: file.lost,
            parent: file.parent,
            genesis: file.genesis,
            genesis_frame: file.genesis_frame,
            seen_epoch: file.seen_epoch,
            requests: file
                .requests
                .into_iter()
                .map(|request| (request.device, request))
                .collect(),
            outbox: file.outbox,
            orphaned: ContentKeyring::from_wire(file.orphaned)?,
            states: None,
            issued: file.issued,
            upgraded_from,
            unsaved: false,
        })
    }

    /// The format of the state this peer was loaded from, when an earlier
    /// build saved it and loading upgraded it.
    #[must_use]
    pub const fn upgraded_from(&self) -> Option<u8> {
        self.upgraded_from
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
        ChannelGate {
            // A device that knows the channel has a group (it saw its
            // genesis or a commit of it, or is in it) must never take the
            // record for plaintext, whether or not it ever saw the genesis
            // itself: a member admitted later only ever sees a Welcome.
            encryption_required: self.knows_group(),
        }
    }

    /// Whether a genesis of this channel is known here.
    pub fn has_genesis(&self) -> bool {
        self.genesis.is_some()
    }

    /// Whether `frame` is a genesis other than the one known here: a group
    /// competing with this device's, which only a device that can name the
    /// channel's readers can judge ([`Self::receive`]).
    pub fn competes(&self, frame: &MlsGovernanceFrame) -> bool {
        matches!(frame.action, MlsGovernanceAction::Genesis)
            && self
                .genesis
                .is_some_and(|known| frame.digest().is_ok_and(|incoming| incoming != known))
    }

    /// Whether this device knows the channel has a group: it saw a genesis
    /// or a commit of it, or is in it. Such a device never creates one.
    pub fn knows_group(&self) -> bool {
        self.genesis.is_some()
            || self.seen_epoch.is_some()
            || !matches!(self.record, MemberRecord::Waiting(_))
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
    /// The emptied outbox is written with the next change, not on its own: a
    /// commit is then written once, not once more as it leaves. A device that
    /// restarts before then publishes those frames again, which every
    /// receiver and the host take as the duplicates they are.
    pub fn published(&mut self) {
        self.outbox.clear();
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

    /// Queue this device's branch again from `epoch` up to the head, when
    /// this device signed any of those frames: a device on a branch that
    /// diverged there then hears this branch's frame at that point and can
    /// tell which one won. Ending with the head keeps a host that keeps a
    /// channel's newest frame on it. Only a device that committed on the
    /// branch since then publishes it, so not every member repeats it.
    fn republish_from(&mut self, epoch: u64) {
        let signed = self
            .held()
            .any(|frame| frame.epoch >= epoch && frame.signed_by == self.device);
        if !signed {
            return;
        }
        for frame in &self.branch {
            if frame.epoch >= epoch && !self.outbox.contains(frame) {
                self.outbox.push(frame.clone());
            }
        }
        self.republish_head();
    }

    /// The frames of this device's branch it holds, oldest first, the head
    /// last.
    fn held(&self) -> impl Iterator<Item = &MlsGovernanceFrame> {
        self.branch.iter().chain(&self.head)
    }

    /// The frame this device's branch holds at `epoch`.
    fn held_at(&self, epoch: u64) -> Option<&MlsGovernanceFrame> {
        self.held().find(|frame| frame.epoch == epoch)
    }

    /// Make `frame` the head, on top of the one held now.
    fn advance(&mut self, frame: MlsGovernanceFrame) {
        let Some(previous) = self.head.replace(frame) else {
            return;
        };
        let previous = previous.withheld().ok().flatten().unwrap_or(previous);
        self.branch.push(previous);
        let size = |frame: &MlsGovernanceFrame| match &frame.action {
            MlsGovernanceAction::Commit {
                commit, history, ..
            } => commit
                .len()
                .saturating_add(history.as_ref().map_or(0, Vec::len)),
            MlsGovernanceAction::Genesis => 0,
        };
        // Keep the newest frames that fit both bounds.
        let mut bytes = 0_usize;
        let kept = self
            .branch
            .iter()
            .rev()
            .take(MAX_BRANCH)
            .take_while(|frame| {
                bytes = bytes.saturating_add(size(frame));
                bytes <= MAX_BRANCH_BYTES
            })
            .count();
        let drop = self.branch.len().saturating_sub(kept);
        self.branch.drain(..drop);
    }

    /// Start this device's branch over at `frame`: a genesis, or the commit
    /// whose Welcome admitted it.
    fn restart_branch(&mut self, frame: MlsGovernanceFrame) {
        self.forget_branch();
        self.head = Some(frame);
    }

    /// Hold no branch: this device left the group.
    fn forget_branch(&mut self) {
        self.head = None;
        self.branch.clear();
        self.lost.clear();
    }

    /// Remember that the commit `digest` lost against this branch, which
    /// diverged from it at `epoch`.
    fn lost_against(&mut self, digest: [u8; 32], epoch: u64) {
        if self.lost.iter().any(|(known, _)| *known == digest) {
            return;
        }
        if self.lost.len() >= MAX_LOST {
            self.lost.remove(0);
        }
        self.lost.push((digest, epoch));
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
    /// The request is kept in memory and written with the commit that admits
    /// its device, not on its own: writing the whole state for each request
    /// cost a crowd arriving together one write of the group, which grows with
    /// it, per person. A request lost with a restart before then is asked
    /// again, as the device asks until it is in.
    ///
    /// # Errors
    ///
    /// Returns an error for a request of another channel or a bad signature.
    pub fn learn_request(&mut self, request: MlsAccessRequestV1) -> Result<bool> {
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
        Ok(true)
    }

    /// The devices `allowed` names whose request this peer holds and that
    /// are not in the group with the key they asked with.
    pub fn asking(&self, allowed: &BTreeSet<Ed25519PublicKey>) -> Vec<Ed25519PublicKey> {
        let MemberRecord::Joined(group) = &self.record else {
            return Vec::new();
        };
        let joined = group.signature_keys();
        self.requests
            .iter()
            .filter(|(device, request)| {
                allowed.contains(*device) && !admitted_with(&joined, device, request)
            })
            .map(|(device, _)| *device)
            .collect()
    }

    /// Whether the group differs from `allowed`: a member it no longer
    /// names, an allowed device asking, or content keys of a state this
    /// device left behind that the members lack.
    pub fn differs(&self, allowed: &BTreeSet<Ed25519PublicKey>) -> bool {
        if !matches!(self.record, MemberRecord::Joined(_)) || !allowed.contains(&self.device) {
            return false;
        }
        self.members()
            .iter()
            .any(|member| *member != self.device && !allowed.contains(member))
            || !self.asking(allowed).is_empty()
            || !self.unshared().is_empty()
    }

    /// This device's turn to commit the change that moves the group to
    /// `allowed`: `0` when it commits at once, `n` when `n` other members
    /// come before it. The members `allowed` keeps take turns, the one that
    /// signed the group's head first (it was there a moment ago), then the
    /// rest by device key; every member at the same head agrees on the
    /// order. A device that holds content keys its group lacks hands them
    /// over itself, so it commits at once. `None` while this device commits
    /// nothing: it is not in the group, `allowed` does not name it, or the
    /// group is as `allowed` says.
    pub fn turn(&self, allowed: &BTreeSet<Ed25519PublicKey>) -> Option<usize> {
        if !self.differs(allowed) {
            return None;
        }
        if self.holds_unshared_keys() {
            return Some(0);
        }
        let mut committers: Vec<Ed25519PublicKey> = self
            .members()
            .into_iter()
            .filter(|member| allowed.contains(member))
            .collect();
        if let Some(signer) = self.head.as_ref().map(|head| head.signed_by)
            && let Some(at) = committers.iter().position(|member| *member == signer)
        {
            let signer = committers.remove(at);
            committers.insert(0, signer);
        }
        committers.iter().position(|member| *member == self.device)
    }

    /// Whether this device holds content keys of a state it left behind that
    /// its group lacks: its next reconcile hands them over.
    pub fn holds_unshared_keys(&self) -> bool {
        !self.unshared().is_empty()
    }

    /// The content keys of states this device left behind that its group
    /// lacks.
    fn unshared(&self) -> ContentKeyring {
        match &self.record {
            MemberRecord::Joined(group) => self.orphaned.missing_from(group.keyring()),
            MemberRecord::Waiting(_) | MemberRecord::Removed => ContentKeyring::new(),
        }
    }

    /// Keep the content keys of a group state this device leaves behind.
    fn orphan(&mut self, left: &MemberRecord) {
        if let MemberRecord::Joined(group) = left {
            // A full keyring keeps what it holds; the rest is lost as before.
            self.orphaned.merge(group.keyring()).ok();
        }
    }

    /// Every content key this device's group holds, or `None` while it is in
    /// none.
    pub fn keyring(&self) -> Option<&ContentKeyring> {
        match &self.record {
            MemberRecord::Joined(group) => Some(group.keyring()),
            MemberRecord::Waiting(_) | MemberRecord::Removed => None,
        }
    }

    /// Hand this group's members the content keys in `earlier`, those of a
    /// legacy group this device holds for a unit this group now carries. The
    /// keys travel with this group's next commit — [`Self::reconcile`]
    /// writes one for them alone where the group does not otherwise differ —
    /// and every later admission passes them on in the joiner's key history,
    /// so a device that joined after key scopes reads the unit's whole
    /// history with no holder of the old group online.
    ///
    /// Idempotent: keys the group already holds or already queued are not
    /// queued again, and calling it after the commit changes nothing. The
    /// commit is signed like every frame, so the handover is attributed to
    /// the device that made it.
    ///
    /// Whether the keys may be handed over is the caller's decision: they
    /// are the keys of a unit whose readers are exactly this group's.
    ///
    /// Returns whether any key was queued.
    pub fn adopt(&mut self, earlier: &ContentKeyring) -> bool {
        let MemberRecord::Joined(group) = &self.record else {
            return false;
        };
        let missing = earlier.missing_from(group.keyring());
        if missing.is_empty() || missing.missing_from(&self.orphaned).is_empty() {
            return false;
        }
        // A full keyring keeps what it holds; nothing is handed over then.
        self.orphaned.merge(&missing).is_ok()
    }

    /// Forget the orphaned keys the group now holds.
    fn settle_orphaned(&mut self) {
        if let MemberRecord::Joined(group) = &self.record {
            self.orphaned = self.orphaned.missing_from(group.keyring());
        }
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
        self.forget_answered_requests();
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
        let history = self.unshared();
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
                .stage_commit(&leaves, &packages, &history)
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
                if leaving.is_empty() && history.is_empty() {
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
        let handed_over = staged.history.is_some();
        let frame = MlsGovernanceFrame::unsigned(
            self.channel,
            previous,
            staged.epoch,
            MlsGovernanceAction::Commit {
                removed: removed.clone(),
                added: added.clone(),
                commit: staged.commit,
                joining: material,
                history: staged.history,
            },
            self.device,
        );
        let frame = match self.sign(frame).await {
            Ok(frame) => frame,
            Err(error) => return self.abandon(error),
        };
        self.merge_own(frame, parent, handed_over.then_some(&history), vault)?;
        outcome.admitted = added;
        outcome.removed = removed;
        outcome.history = handed_over;
        Ok(outcome)
    }

    /// Merge the commit this device staged and signed as `frame`, which
    /// handed the members `history`, and queue it.
    fn merge_own(
        &mut self,
        frame: MlsGovernanceFrame,
        parent: Vec<u8>,
        history: Option<&ContentKeyring>,
        vault: &VaultKey,
    ) -> Result<()> {
        let MemberRecord::Joined(group) = &mut self.record else {
            return Ok(());
        };
        group.merge_pending_transaction()?;
        // Their requests are answered: kept, they would be checked against
        // the whole group on every later pass.
        let added = frame.added();
        self.requests.retain(|device, _| !added.contains(device));
        if let Some(history) = history {
            // Handed over whether or not a full keyring keeps them here.
            group.keyring.merge(history).ok();
            self.orphaned = self.orphaned.missing_from(history);
        }
        self.settle_orphaned();
        self.parent = Some(parent);
        self.advance(frame.clone());
        self.outbox.extend(
            frame
                .into_deliveries()
                .map_err(|error| Error::Serialization(error.to_string()))?,
        );
        self.save(vault)
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

    /// Forget the requests of devices already in the group with the key they
    /// asked with: a device asks again until its Welcome arrives, and each
    /// answered request kept would be checked against the group on every
    /// pass.
    fn forget_answered_requests(&mut self) {
        let MemberRecord::Joined(group) = &self.record else {
            return;
        };
        if self.requests.is_empty() {
            return;
        }
        let joined = group.signature_keys();
        self.requests
            .retain(|device, request| !admitted_with(&joined, device, request));
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
        self.apply(frame, Some(allowed), vault)
    }

    /// Apply one frame while this device cannot tell who may read the
    /// channel (it cannot name the channel's record): a commit from a member
    /// of the group is followed as MLS checks it, and a Welcome admits this
    /// device. A genesis only counts while none is known, so no one can make
    /// this device leave its group with a genesis of their own.
    ///
    /// # Errors
    ///
    /// As [`Self::receive`].
    pub fn follow(&mut self, frame: MlsGovernanceFrame, vault: &VaultKey) -> Result<PeerEvent> {
        self.apply(frame, None, vault)
    }

    fn apply(
        &mut self,
        frame: MlsGovernanceFrame,
        allowed: Option<&BTreeSet<Ed25519PublicKey>>,
        vault: &VaultKey,
    ) -> Result<PeerEvent> {
        if frame.channel != self.channel {
            return Err(governance("frame names another channel"));
        }
        self.drop_unfinished();
        frame
            .check()
            .map_err(|error| governance(error.to_string()))?;
        if allowed.is_some_and(|allowed| !allowed.contains(&frame.signed_by)) {
            return Err(governance("frame is not signed by a participant"));
        }
        let incoming = digest(&frame)?;
        match &frame.action {
            MlsGovernanceAction::Genesis => {
                if allowed.is_none() && self.genesis.is_some_and(|known| known != incoming) {
                    return Ok(PeerEvent::Ignored);
                }
                self.receive_genesis(&frame, incoming, vault)
            }
            MlsGovernanceAction::Commit { .. } => {
                if let Some(allowed) = allowed
                    && !backs(&frame, allowed)
                {
                    return Err(governance(
                        "commit adds or removes a device the membership does not",
                    ));
                }
                match &self.record {
                    MemberRecord::Joined(_) => self.receive_commit(frame, incoming, allowed, vault),
                    MemberRecord::Waiting(_) => {
                        self.receive_welcome(frame, incoming, allowed, vault)
                    }
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
            Some(known) if known == incoming => {
                // A member admitted by Welcome knew its genesis by digest
                // only: keep the frame to publish again.
                if self.genesis_frame.is_none() {
                    self.genesis_frame = Some(frame.clone());
                    self.save(vault)?;
                }
                Ok(PeerEvent::Duplicate)
            }
            Some(known) if known < incoming => {
                if let Some(winner) = &self.genesis_frame
                    && !self.outbox.contains(winner)
                {
                    self.outbox.push(winner.clone());
                    self.save(vault)?;
                }
                Ok(PeerEvent::Ignored)
            }
            known => {
                self.genesis = Some(incoming);
                self.genesis_frame = Some(frame.clone());
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
        allowed: Option<&BTreeSet<Ed25519PublicKey>>,
        vault: &VaultKey,
    ) -> Result<PeerEvent> {
        let Some(head) = &self.head else {
            return self.desynchronise(vault);
        };
        let head_digest = digest(head)?;
        let head_epoch = head.epoch;
        for held in self.held() {
            if digest(held)? == incoming {
                return Ok(PeerEvent::Duplicate);
            }
        }
        if let Some(&(_, from)) = self
            .lost
            .iter()
            .find(|(lost, _)| *lost == frame.previous_frame_digest)
        {
            // It went on from a commit that lost against this branch, so it
            // lost too. Its author hears this branch's frame at the point of
            // divergence again and starts over.
            self.lost_against(incoming, from);
            self.republish_from(from);
            self.save(vault)?;
            return Ok(PeerEvent::Ignored);
        }
        let sibling = match self.held_at(frame.epoch) {
            Some(ours) if ours.previous_frame_digest == frame.previous_frame_digest => {
                Some(digest(ours)?)
            }
            _ => None,
        };
        if let Some(ours) = sibling {
            // The two branches diverged right below this frame: the lower
            // of the two children of their last shared frame wins.
            if incoming > ours {
                // Whoever committed on this branch since publishes it again,
                // so a host keeping the newest frame keeps the winner and
                // the device that went on from the loser hears it.
                self.lost_against(incoming, frame.epoch);
                self.republish_from(frame.epoch);
                self.save(vault)?;
                return Ok(PeerEvent::Ignored);
            }
            if frame.epoch == head_epoch {
                return self.replace_head(frame, incoming, allowed, vault);
            }
            // This device went on from a commit that lost. It cannot follow
            // the winner from where it stands, so it starts over and asks
            // again, handing over what it sealed on its branch once it is
            // back. Ignoring the winner as old would leave it on a branch no
            // other participant follows, sealing under keys none of them
            // hold.
            return self.desynchronise(vault);
        }
        if frame.epoch == head_epoch.saturating_add(1) && frame.previous_frame_digest == head_digest
        {
            return self.apply_commit(frame, allowed, vault);
        }
        if frame.epoch > head_epoch.saturating_add(1) {
            // The commits between are gone: a host keeps only the newest.
            return self.desynchronise(vault);
        }
        // The frame's branch split from this one below the frames held here
        // that it can be compared with: the frame it follows is not one this
        // device holds, though it holds that epoch's. Which branch won is
        // decided at the point of divergence, by a device that holds both
        // children there; publishing this branch again lets the other side
        // decide. A frame older than anything held here is of no concern.
        let oldest = self.held().next().map_or(head_epoch, |frame| frame.epoch);
        if frame.epoch > oldest {
            self.republish_from(oldest);
            self.save(vault)?;
        }
        Ok(PeerEvent::Ignored)
    }

    /// A commit for the head's epoch, following the frame the head follows,
    /// with a lower digest: it replaces the head.
    fn replace_head(
        &mut self,
        frame: MlsGovernanceFrame,
        incoming: [u8; 32],
        allowed: Option<&BTreeSet<Ed25519PublicKey>>,
        vault: &VaultKey,
    ) -> Result<PeerEvent> {
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
        let lost = self.head.take();
        let applied = if joins_from_welcome {
            self.receive_welcome(frame, incoming, allowed, vault)
        } else {
            self.apply_commit(frame, allowed, vault)
        };
        match applied {
            Ok(
                PeerEvent::Ignored
                | PeerEvent::Desynchronised
                | PeerEvent::AdmittedWithAnotherKey
                | PeerEvent::WelcomeRefused(_),
            )
            | Err(_) => {
                self.record = replaced;
                self.head = lost;
                self.desynchronise(vault)
            }
            Ok(event) => {
                // What this device sealed under the branch that lost is
                // handed to the members with its next commit.
                self.orphan(&replaced);
                self.settle_orphaned();
                self.save(vault)?;
                Ok(event)
            }
        }
    }

    /// Apply `frame`'s commit to the group held now, after checking its MLS
    /// proposals match what the frame declares.
    fn apply_commit(
        &mut self,
        frame: MlsGovernanceFrame,
        allowed: Option<&BTreeSet<Ed25519PublicKey>>,
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
            history,
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
        if removes_me && allowed.is_some_and(|allowed| allowed.contains(&me)) {
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
            self.forget_branch();
            self.parent = None;
            self.seen_epoch = Some(frame.epoch);
            self.save(vault)?;
            return Ok(PeerEvent::Removed);
        }
        if let Some(history) = history {
            // Keys of a branch that lost; one this group cannot open is only
            // the committer's loss.
            group.import_key_history(history).ok();
        }
        let epoch = group.epoch();
        // The commit answered the requests of the devices it added: the one
        // it admitted them with, and any older one.
        self.requests.retain(|device, _| !added.contains(device));
        self.parent = Some(parent);
        self.advance(frame);
        self.settle_orphaned();
        // Kept after the page shows ([`Self::save_deferred`]): a commit lost
        // with it is found again, or the device asks again.
        self.unsaved = true;
        Ok(PeerEvent::Advanced { epoch })
    }

    /// A commit for a waiting device: its admission when the frame carries
    /// a Welcome for this device's member key.
    ///
    /// The Welcome is refused unless the device that signed its `GroupInfo`
    /// is the one that signed the frame, and (when this device can tell who
    /// may read the channel) every leaf of its tree names a device `allowed`
    /// lets read it. Without that any member could send this device into a
    /// group of its own making, which would show only later, as a channel
    /// whose commits never apply. Both are a look at each leaf's credential,
    /// no signature work. A tree refused for a device `allowed` does not name
    /// leaves this device waiting with the same key, so the frame can be
    /// judged again under a later view ([`PeerEvent::WelcomeRefused`]).
    fn receive_welcome(
        &mut self,
        frame: MlsGovernanceFrame,
        incoming: [u8; 32],
        allowed: Option<&BTreeSet<Ed25519PublicKey>>,
        vault: &VaultKey,
    ) -> Result<PeerEvent> {
        let MemberRecord::Waiting(pending) = &self.record else {
            return Ok(PeerEvent::Ignored);
        };
        self.seen_epoch = Some(self.seen_epoch.unwrap_or(0).max(frame.epoch));
        let Some(material) = frame
            .join_material()
            .filter(|_| frame.added().contains(&self.device))
        else {
            return Ok(PeerEvent::Ignored);
        };
        if !ChannelGroup::welcome_names(&material.welcome, pending) {
            return Ok(PeerEvent::AdmittedWithAnotherKey);
        }
        let attempt = pending.duplicate()?;
        let parent = MemberRecord::seal_waiting(pending, vault)?;
        let signer = frame.signed_by;
        let judge = |parties: &crate::group::WelcomeParties| {
            if parties.sender != signer.as_bytes() {
                return Err(Error::WelcomeRefused(
                    crate::WelcomeRefusal::SignedByAnotherDevice,
                ));
            }
            if let Some(allowed) = allowed
                && !parties.members.iter().all(|member| {
                    <[u8; 32]>::try_from(member.as_slice())
                        .is_ok_and(|device| allowed.contains(&Ed25519PublicKey::from_bytes(device)))
                })
            {
                return Err(Error::WelcomeRefused(
                    crate::WelcomeRefusal::DeviceNotAllowed,
                ));
            }
            Ok(())
        };
        match ChannelGroup::join_with_history_checked(
            self.channel,
            &material.welcome,
            &material.key_history,
            attempt,
            judge,
        ) {
            Ok(group) => {
                let epoch = group.epoch();
                debug_assert_eq!(epoch, frame.epoch, "{incoming:?}");
                let founded_by = group.genesis();
                self.record = MemberRecord::Joined(Box::new(group));
                self.restart_branch(frame);
                self.parent = Some(parent);
                self.settle_orphaned();
                // The group names the genesis it grew from, so this device
                // judges a competing genesis as its founder does. A group
                // named at random (founded by an earlier build) names none.
                if let Some(founded_by) = founded_by {
                    match self.genesis {
                        Some(known) if known < founded_by => {
                            // Admitted to a group whose genesis lost: start
                            // over for the winner's, and tell the members
                            // that admitted it of the winner.
                            if let Some(winner) = &self.genesis_frame
                                && !self.outbox.contains(winner)
                            {
                                self.outbox.push(winner.clone());
                            }
                            return self.desynchronise(vault);
                        }
                        Some(known) if known == founded_by => {}
                        _ => {
                            self.genesis = Some(founded_by);
                            self.genesis_frame = None;
                        }
                    }
                }
                // Kept after the page shows ([`Self::save_deferred`]): a
                // join lost with it is asked for again.
                self.unsaved = true;
                Ok(PeerEvent::Joined { epoch })
            }
            Err(Error::WelcomeRefused(refusal)) => {
                if refusal == crate::WelcomeRefusal::SignedByAnotherDevice {
                    self.desynchronise(vault)?;
                }
                Ok(PeerEvent::WelcomeRefused(refusal))
            }
            // A Welcome for a key package this device no longer holds: stay
            // waiting; the next request is admitted anew.
            Err(_) => Ok(PeerEvent::Desynchronised),
        }
    }

    /// Give up on a Welcome this waiting device held for its view of the
    /// membership ([`PeerEvent::WelcomeRefused`]): wait with a fresh member
    /// key, which the members admit anew. Nothing for a device not waiting.
    ///
    /// # Errors
    ///
    /// A failed write of the state.
    pub fn refuse_welcome(&mut self, vault: &VaultKey) -> Result<PeerEvent> {
        if !matches!(self.record, MemberRecord::Waiting(_)) {
            return Ok(PeerEvent::Ignored);
        }
        self.desynchronise(vault)
    }

    /// Start over: wait with a fresh member key, keeping the gate.
    fn desynchronise(&mut self, vault: &VaultKey) -> Result<PeerEvent> {
        if let MemberRecord::Joined(group) = &self.record {
            self.seen_epoch = Some(self.seen_epoch.unwrap_or(0).max(group.epoch()));
        }
        let left = std::mem::replace(
            &mut self.record,
            MemberRecord::Waiting(Box::new(MemberKey::for_device(&self.device)?)),
        );
        self.orphan(&left);
        self.forget_branch();
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

    fn save(&mut self, vault: &VaultKey) -> Result<()> {
        let Some(states) = &self.states else {
            return Ok(());
        };
        let sealed = self.export_sealed(vault)?;
        states.write(self.channel, &sealed)?;
        self.unsaved = false;
        Ok(())
    }

    /// Whether this peer holds a change [`Self::save_deferred`] has yet to
    /// write.
    #[must_use]
    pub const fn has_unsaved(&self) -> bool {
        self.unsaved && self.states.is_some()
    }

    /// Write the change taking in a Welcome or a commit left unwritten.
    ///
    /// Sealing a large group's state takes as long as joining it, and nothing
    /// waits on it: the frame taken in produced nothing that leaves this
    /// device, and every change that does is written before it leaves (the
    /// module docs), this one with it. So the caller writes it once the
    /// person sees the channel's content, not before. A device that stops
    /// first keeps the state it had before the frame: a waiting device waits
    /// again and asks anew, and a member finds the frame it missed, or starts
    /// over and asks again. Either way it is admitted with the channel's
    /// history.
    ///
    /// # Errors
    ///
    /// Returns an error when the state cannot be sealed or written.
    pub fn save_deferred(&mut self, vault: &VaultKey) -> Result<()> {
        if self.has_unsaved() {
            self.save(vault)?;
        }
        Ok(())
    }
}

/// Whether `device` is in the group with the member key `request` asked
/// with, given every member's key ([`ChannelGroup::signature_keys`]).
fn admitted_with(
    joined: &std::collections::HashMap<Vec<u8>, Vec<u8>>,
    device: &Ed25519PublicKey,
    request: &MlsAccessRequestV1,
) -> bool {
    let Some(joined_with) = joined.get(device.as_bytes().as_slice()) else {
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

#[cfg(test)]
#[path = "peer_welcome_tests.rs"]
mod welcome_tests;

#[cfg(test)]
mod format_tests {
    use super::*;

    /// State in a format no upgrade reaches is told apart from state that is
    /// corrupt or another device's.
    #[test]
    fn state_in_a_format_no_upgrade_reaches_is_told_apart() {
        let vault = VaultKey::new([7; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let channel = ChannelId::new();
        let peer = ChannelPeer::waiting(None, channel, Box::new(key.clone()), &vault).unwrap();
        let exported = peer.export_sealed(&vault).unwrap();
        let context = || Context::new(Label::ChannelMlsState, PEER_RECORD);
        let mut plaintext = tilo_seal::envelope::open(&vault.0, context(), &exported).unwrap();
        plaintext[0] = 1;
        let unreachable = tilo_seal::envelope::seal(&vault.0, context(), &plaintext).unwrap();
        assert!(matches!(
            ChannelPeer::restore(channel, Box::new(key.clone()), &unreachable, &vault),
            Err(Error::OtherFormat)
        ));
        assert!(
            ChannelPeer::restore(channel, Box::new(key), &exported, &vault).is_ok(),
            "state of this build reads as before"
        );
    }
}
