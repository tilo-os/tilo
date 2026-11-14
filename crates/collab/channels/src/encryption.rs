//! End-to-end encryption of workspaces on this device: one MLS group per workspace, whose members are
//! exactly the devices that hold the workspace's keys ([`ChannelDirectory::allowed_devices`]), and
//! every participant commits the group to that set. Every record of the workspace seals under the
//! group's content keys; read permission inside a workspace is not a key boundary.
//!
//! [`ChannelEncryption::pump`] runs the protocol over the workspace transport: inbound frames
//! advance the group and access requests wait for the next commit, frames the transport did not take
//! are published again, and two geneses made at once resolve to one without losing what was sealed
//! under the other ([`channel_crypto::peer`]).
//!
//! Every change registers the channel's gate ([`ChannelGates`]: plaintext and another channel's
//! ciphertext are refused on every ingress path) and is announced
//! ([`ChannelKeyProvider::channel_changed`]), so an endpoint holding a route's keys asks the identity
//! service again only when they change.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_clock::time::Instant;

use channel_crypto::{
    Access, ChannelContentKeys, ChannelPeer, PeerEvent, VaultKey, WelcomeRefusal,
};
use collab_types::signer::{DeviceSigner, SignFuture};
use collab_types::{
    Ed25519PublicKey, EncryptionMetadata, MlsAccessRequestV1, MlsGovernanceFrame, SignedOp,
};
use neighbour_wire::NeighbourMsg;

use crate::gates::ChannelGates;
use crate::schedule::{PolicySchedule, RECHECK_INTERVAL};
use tilo_desktop_model::WorkspaceId;

/// How often groups re-check the access policy when their directory
/// announces no changes, the longest an announced change waits, and how soon
/// a pass that left work due tries again ([`crate::schedule`]).
const RECONCILE_INTERVAL: Duration = RECHECK_INTERVAL;
/// First and longest wait before a waiting device asks again.
const REQUEST_FIRST: Duration = Duration::from_secs(2);
const REQUEST_LONGEST: Duration = Duration::from_secs(120);
/// How long a waiting device asks with one member key before it asks with
/// a fresh one: an admission whose Welcome never reached it is then answered
/// again.
const REQUEST_FRESH: Duration = Duration::from_secs(60);
/// How long each member before this one in turn ([`ChannelPeer::turn`]) has
/// to commit a change of a channel's group before this one does: long enough
/// for one member's commit to reach the others, so they follow it rather than
/// commit the same change beside it and fork the group.
const TURN: Duration = Duration::from_secs(2);
/// The least time between two commits this device writes for one channel.
/// Every member follows each commit, and in a group of hundreds a commit costs
/// each of them work that grows with the group: people arriving within this
/// of each other are admitted in one commit rather than one each. A change
/// after a quiet spell is committed at once.
const COMMIT_SPACING: Duration = Duration::from_millis(500);
/// How long the state a frame taken in left changed waits to be written
/// ([`ChannelPeer::save_deferred`]). Sealing the state of a channel of hundreds
/// takes tens of milliseconds, and the Welcome that admits a page arrives as it
/// enters: written at once, it would hold back the page's first frame, which is
/// drawn well within this. A device that stops before only asks again.
const DEFERRED_SAVE: Duration = Duration::from_secs(2);
/// Channels remembered as having a group this device takes no part in.
const MAX_GROUPS_SEEN: usize = 1 << 16;
/// How long a waiting device holds a Welcome whose tree names a device its
/// view of the membership does not, judging it again as that view changes,
/// before it gives up and asks with a fresh member key. Such a tree is an
/// honest one whenever this device's view is behind the committer's (a device
/// admitted a moment ago) or ahead of it (one removed a moment ago), and
/// asking again costs the whole group a commit: a lag of a moment is waited
/// out, and only a view that does not come round in this long, or a Welcome
/// the channel moved past, is given up on.
const WELCOME_WAIT: Duration = Duration::from_secs(10);
/// Frames one channel keeps for the policy that has not named their signer
/// yet. Bounded, newest kept: a frame this device's policy still does not
/// back after that many later ones is not going to be applied.
const MAX_HELD_FRAMES: usize = 8;

/// The widest a device's wait to create a channel's group spreads past
/// [`REQUEST_FIRST`], in milliseconds: devices that hold no state for the same
/// channel wait within these of each other, so one creates the group and the
/// others hear of it in time.
const FOUNDING_SPREAD_MS: u64 = 2_000;

/// The longest a device that holds no state for a channel waits for its genesis
/// (from the workspace's retained frames, or a participant answering its
/// request) before it creates the group itself: [`REQUEST_FIRST`] plus the
/// widest spread between devices. Whoever waits for a fresh channel's keys
/// waits longer than this, or it gives up on a group about to be created.
#[must_use]
pub const fn founding_wait() -> Duration {
    REQUEST_FIRST.saturating_add(Duration::from_millis(FOUNDING_SPREAD_MS))
}

/// The longest a device that is waiting for a channel's keys goes between asks
/// ([`REQUEST_LONGEST`]): each ask it leaves unanswered doubles the wait. A
/// reader of a record sealed in that channel reads nothing until the keys are
/// here, so whoever waits for it waits past this: an ask on a busy machine is
/// answered late, and a reader that gave up before the next one would call a
/// record that is arriving one that never came.
#[must_use]
pub const fn asking_again_wait() -> Duration {
    REQUEST_LONGEST.saturating_add(REQUEST_FIRST)
}

/// How long a device that took a channel up for another one that asked
/// ([`PeerEntry::wanted`]) waits to create its group, once no genesis is on
/// its way: the asker creates it at once if it can, this one only for an
/// asker that cannot (a reader). Each device waits its own while, set by its
/// key and the channel, so one creates the group and the others hear of it.
fn found_after(device: &Ed25519PublicKey, workspace: WorkspaceId) -> Duration {
    let spread = device
        .as_bytes()
        .iter()
        .zip(workspace.as_uuid().as_bytes())
        .fold(0_u64, |acc, (device, workspace)| {
            acc.wrapping_mul(31)
                .wrapping_add(u64::from(device ^ workspace))
        })
        % FOUNDING_SPREAD_MS;
    REQUEST_FIRST.saturating_add(Duration::from_millis(spread))
}

impl PeerEntry {
    /// Keep a Welcome `event` refused for a device the view `allowed` did not
    /// name (`frame`, kept while this device waited), judged again as that
    /// view changes; or give up on the one held once a later commit (at
    /// `epoch`) arrived, as a held Welcome is followed by nothing but its own
    /// commit: the members moved past it, and this device asks again to be
    /// admitted where they are. Whether this device now asks again.
    fn hold_welcome(
        &mut self,
        event: &PeerEvent,
        frame: Option<MlsGovernanceFrame>,
        allowed: Option<BTreeSet<Ed25519PublicKey>>,
        epoch: u64,
        store: &ChannelEncryptionConfig,
        now: Instant,
    ) -> bool {
        if let (PeerEvent::WelcomeRefused(WelcomeRefusal::DeviceNotAllowed), Some(frame)) =
            (event, frame)
        {
            let until = self
                .welcome
                .as_ref()
                .filter(|held| held.frame == frame)
                .map_or_else(
                    || now.checked_add(WELCOME_WAIT).unwrap_or(now),
                    |held| held.until,
                );
            self.welcome = Some(HeldWelcome {
                frame,
                judged: allowed,
                until,
            });
            return false;
        }
        if !self
            .welcome
            .as_ref()
            .is_some_and(|held| held.frame.epoch < epoch || self.peer.access() != Access::Waiting)
        {
            return false;
        }
        self.welcome = None;
        if self.peer.access() != Access::Waiting {
            return false;
        }
        give_up_welcome(self, store, now);
        true
    }
}

/// Give up on the Welcome `entry` held for its view of the membership: wait
/// with a fresh member key and ask with it at once, as a desynchronised device
/// does.
fn give_up_welcome(entry: &mut PeerEntry, store: &ChannelEncryptionConfig, now: Instant) {
    entry.welcome = None;
    if let Err(error) = entry.peer.refuse_welcome(&store.vault) {
        tracing::warn!(%error, workspace = %entry.workspace, "channel state could not be written");
    }
    entry.request = None;
    entry.retry = Backoff::now(now, REQUEST_FIRST);
    entry.fresh_at = now;
}

/// What an application endpoint may do with a route's content right now.
#[derive(Clone, Debug)]
pub enum ChannelAccess {
    /// Admitted: seal new content and open history with these keys.
    Ready(ChannelContentKeys),
    /// Asked the participants for access and not admitted yet. Nothing can
    /// be sealed or opened; the endpoint shows a waiting state.
    Waiting,
    Removed,
    /// Nobody can admit this device: the workspace's governance is unknown here.
    Unavailable,
    /// This device has no secret store open, so it keeps no channel state
    /// of its own and cannot join a channel. Asking again changes nothing
    /// while the session runs.
    NoSecretStore,
}

/// A channel governance message received from the workspace transport. Every
/// frame and request is signed by a device key and verified by the channel
/// service before it changes any state.
#[derive(Debug, Clone)]
pub enum InboundChannelGovernance {
    Frame {
        workspace: WorkspaceId,
        frame: Box<collab_types::MlsGovernanceFrame>,
    },
    /// A device asking the workspace's participants for membership.
    AccessRequest {
        workspace: WorkspaceId,
        request: Box<collab_types::MlsAccessRequestV1>,
    },
}

impl InboundChannelGovernance {
    /// The governance `message` carries, if it is one for a valid workspace.
    #[must_use]
    pub fn from_message(message: NeighbourMsg) -> Option<Self> {
        match message {
            NeighbourMsg::MlsGovernance {
                workspace, frame, ..
            } => WorkspaceId::parse(&workspace).map(|workspace| Self::Frame { workspace, frame }),
            NeighbourMsg::MlsAccessRequest {
                workspace, request, ..
            } => WorkspaceId::parse(&workspace)
                .map(|workspace| Self::AccessRequest { workspace, request }),
            _ => None,
        }
    }
}

/// Where the channel protocol's messages travel: the workspace transport of the
/// host (the desktop session's network, the runtime's relay sessions).
pub trait GovernanceTransport {
    /// Hand one message to the transport. `false` when it was not accepted.
    fn publish(&self, message: NeighbourMsg) -> bool;
    /// Take every message received since the last call.
    fn take_inbound(&self) -> Vec<InboundChannelGovernance>;
}

/// The access policy inputs this service needs from the workspace model.
pub trait ChannelDirectory: collab_types::signer::MaybeSendSync {
    /// Every device that holds `workspace`'s keys at wall-clock `now` (seconds since the Unix
    /// epoch): the devices of its members that read and run work there. `None` while this device
    /// cannot tell: the workspace's governance is unknown here, and it then admits and removes no
    /// one.
    ///
    /// A personal cluster ([`collab_types::personal_cluster`]) is read by the devices of the
    /// identity it derives from (certified, not revoked).
    fn allowed_devices(
        &self,
        workspace: WorkspaceId,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>>;
    /// The [`tilo_identity_host::policy_change`] epoch (or a test's own) when
    /// every write to what [`Self::allowed_devices`] reads is announced and
    /// its expiries can be timed: groups then re-check on announcements and
    /// at [`Self::next_policy_change`]. `None` (the default) keeps the
    /// re-check every [`RECHECK_INTERVAL`].
    fn policy_epoch(&self) -> Option<u64> {
        None
    }
    /// The first wall-clock second after `now` at which
    /// [`Self::allowed_devices`] for `workspace` can answer differently with
    /// nothing written: a grant or a certificate expiring, a revocation
    /// dated ahead. Read only while [`Self::policy_epoch`] is `Some`.
    fn next_policy_change(&self, workspace: WorkspaceId, now: u64) -> Option<u64> {
        let _ = (workspace, now);
        None
    }
    /// Announce a policy change when the wall clock reaches `at` (seconds
    /// since the Unix epoch), an instant [`Self::next_policy_change`] named.
    fn announce_at(&self, at: u64) {
        tilo_identity_host::policy_change::wake_at(at.saturating_mul(1_000_000_000));
    }
    /// Whether this device created `workspace` and so founds its group. Every
    /// other device waits for the genesis and joins by Welcome.
    fn founds(&self, workspace: WorkspaceId) -> bool;
}

/// The platform secret store's namespace holding a device's channel vault
/// master key: the key the sealed channel state of every workspace is
/// derived from. A host keeps it the way it keeps its other secrets — the
/// system keychain on a desktop, a non-extractable `WebCrypto` key in a
/// page — so the state's own store never holds the key that opens it.
pub const VAULT_SECRET_APP: &str = "so.tilo.collaboration.mls";

/// Where a host keeps this device's channel state: the peers of each
/// workspace's group. A desktop keeps
/// it in its channels directory ([`ChannelsDir`]); a page keeps it in its store.
pub trait ChannelState: std::fmt::Debug + collab_types::signer::MaybeSendSync {
    /// Make the store ready for the state a channel writes: a directory that
    /// pays for it is created owner-only here, so a write cannot land in one a
    /// person's umask left open. A store that always exists answers nothing.
    ///
    /// # Errors
    /// The store cannot be created.
    fn prepare(&self) -> Result<(), String> {
        Ok(())
    }
    /// Where the peer of each workspace's group is kept.
    fn peers(&self) -> Arc<dyn channel_crypto::PeerStates>;
    /// Every workspace this store holds a peer for, so a service registers
    /// their gates before the first op after a restart. A store that cannot
    /// list them answers none: its groups open as an endpoint asks about them.
    fn held(&self) -> Vec<WorkspaceId> {
        Vec::new()
    }
    /// Whether this store already keeps channel state: a vault whose master
    /// key is gone opens none of it, so it is never replaced by a new one. A
    /// store whose state lives elsewhere answers `false`.
    ///
    /// # Errors
    /// The store cannot be read.
    fn holds_channel_state(&self) -> Result<bool, String> {
        Ok(false)
    }
    /// Forget everything kept of `workspace`'s group: it was deleted.
    ///
    /// # Errors
    /// What it kept could not be removed.
    fn forget(&self, workspace: WorkspaceId) -> Result<(), String> {
        self.peers()
            .forget(workspace)
            .map_err(|error| error.to_string())
    }
}

/// A desktop's key directory: one `<workspace>.peer` file per workspace.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct ChannelsDir {
    state: PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl ChannelsDir {
    /// The state kept under `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { state: root.into() }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ChannelState for ChannelsDir {
    fn prepare(&self) -> Result<(), String> {
        tilo_files::native::atomic::create_private_directory(&self.state)
            .map_err(|error| error.to_string())
    }

    fn peers(&self) -> Arc<dyn channel_crypto::PeerStates> {
        Arc::new(channel_crypto::Files::new(self.state.clone()))
    }

    fn held(&self) -> Vec<WorkspaceId> {
        let entries = match std::fs::read_dir(&self.state) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                tracing::warn!(dir = %self.state.display(), %error, "channel state cannot be listed");
                return Vec::new();
            }
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("peer") {
                    return None;
                }
                let workspace = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(WorkspaceId::parse);
                if workspace.is_none() {
                    tracing::warn!(path = %path.display(), "key state file names no workspace");
                }
                workspace
            })
            .collect()
    }

    fn holds_channel_state(&self) -> Result<bool, String> {
        match std::fs::read_dir(&self.state) {
            Ok(mut entries) => Ok(entries.next().is_some()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!("{}: {error}", self.state.display())),
        }
    }
}

/// Durable location and keys of this device's own channel state: what a
/// participant of a channel needs, and only workspaces this device's own workspace
/// host does not take part in need it.
pub struct ChannelEncryptionConfig {
    /// Where this host keeps each workspace's channel state.
    pub state: Arc<dyn ChannelState>,
    /// Key sealing that state, held by the platform secret store.
    pub vault: VaultKey,
    /// This device's certified key, kept in its custody: the request and frame signer.
    pub signer: Arc<dyn DeviceSigner>,
    /// Whether this device settles its groups or only follows them.
    pub role: Role,
}

/// What a device does in the groups it takes part in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Founds groups and commits them to the access policy.
    Participant,
    /// Asks for access and follows the commits participants write, but never founds a group or
    /// commits: a page or an agent, whose device key signs asynchronously and which may not hold
    /// the grants a commit is decided on.
    Reader,
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

/// When this device may create a channel's group itself.
struct Founding {
    /// When a device that took the channel up for another one creates the
    /// group, if no genesis has arrived.
    at: Instant,
    /// Whether the policy, when last read, let this device read the channel
    /// and so create its group.
    allowed: bool,
}

/// One channel this device takes part in.
struct PeerEntry {
    workspace: WorkspaceId,
    peer: ChannelPeer,
    /// The request this device asks with while it waits, re-sent on `retry`.
    request: Option<MlsAccessRequestV1>,
    /// A request whose signature the device's custody has not answered yet.
    signing: Option<(MlsAccessRequestV1, SignFuture<'static>)>,
    retry: Backoff,
    /// When the next request mints a fresh member key: an admission whose
    /// Welcome never arrived here is then answered again.
    fresh_at: Instant,
    /// The group may differ from the policy: reconcile on the next pass.
    dirty: bool,
    /// The group differs from the policy and other members come before this
    /// one in turn to commit the change: it commits at this instant if none
    /// of them has by then.
    turn_at: Option<Instant>,
    /// When this device last committed a change of the channel's group: the
    /// next waits for [`COMMIT_SPACING`] after it.
    committed_at: Option<Instant>,
    /// An endpoint opened the route after this device was removed: ask
    /// again, and the policy decides whether access resumes.
    rejoin: bool,
    founding: Founding,
    /// An endpoint of this device opened the route. A channel taken up only
    /// because another device asked is left once a genesis arrives.
    wanted: bool,
    /// Frames this device's policy does not back yet, oldest first. The
    /// workspace's governance reaches a device as the workspace does, so a
    /// frame from a device admitted a moment ago is judged again once the
    /// policy here names it.
    held: Vec<MlsGovernanceFrame>,
    /// A Welcome refused for a device this device's view of the membership
    /// did not name ([`WELCOME_WAIT`]).
    welcome: Option<HeldWelcome>,
    /// Since when the peer holds a change it has not written
    /// ([`DEFERRED_SAVE`]).
    unsaved_since: Option<Instant>,
}

/// A Welcome a waiting device holds for its view of the membership.
struct HeldWelcome {
    frame: MlsGovernanceFrame,
    /// The devices the view named when it last refused the Welcome: it is
    /// judged again once the view names others.
    judged: Option<BTreeSet<Ed25519PublicKey>>,
    /// When the device gives up on it.
    until: Instant,
}

/// A channel an endpoint asked about that this device could not open: no
/// one can name its readers here, or opening its state failed. Its answer
/// changes with nothing arriving for it, so the service tells the route itself
/// ([`ChannelEncryption::bump`]) rather than leaving the endpoint to ask on
/// a timer.
struct Unopened {
    workspace: WorkspaceId,
    /// `None` while no one can name the channel's readers here: re-checked
    /// whenever the policy inputs announce a change. `Some` after a failed
    /// open: retried on its own backoff.
    retry: Option<Backoff>,
}

impl Unopened {
    fn failed(workspace: WorkspaceId, now: Instant) -> Self {
        let mut retry = Backoff::now(now, REQUEST_FIRST);
        retry.fired(now, REQUEST_LONGEST);
        Self {
            workspace,
            retry: Some(retry),
        }
    }

    fn unnamed(workspace: WorkspaceId) -> Self {
        Self {
            workspace,
            retry: None,
        }
    }
}

/// This device's side of every encrypted channel it takes part in.
///
/// Taking part in a workspace's channel needs this device's own channel state,
/// kept under a key from the platform secret store; a service opened
/// without it ([`Self::without_store`]) answers every workspace
/// [`ChannelAccess::NoSecretStore`].
pub struct ChannelEncryption {
    /// This device's own channel state; `None` when no secret store opened.
    store: Option<ChannelEncryptionConfig>,
    directory: Box<dyn ChannelDirectory>,
    gates: Arc<dyn ChannelGates>,
    peers: HashMap<WorkspaceId, PeerEntry>,
    /// When the groups last re-checked the access policy, and what the next
    /// re-check waits for.
    reconcile: PolicySchedule,
    delivery: u64,
    generation: u64,
    /// The [`Self::generation`] each channel last changed at.
    changed: HashMap<WorkspaceId, u64>,
    /// [`Self::generation`], announced to whoever waits for a change.
    changes: tokio::sync::watch::Sender<u64>,
    /// An endpoint's [`Self::access`] queued governance the next pump sends
    /// (a genesis to announce, an access request to make). A host that parks
    /// its pump while idle wakes it on this; see [`ChannelKeyAccess::with_wake`].
    queued: bool,
    /// Channels asked about that could not be opened; see [`Unopened`].
    unopened: HashMap<WorkspaceId, Unopened>,
    /// The gate this service last registered with the broker for each
    /// channel. Every access re-asserts its channel's gate, and a channel's
    /// keys are read for every live frame sealed or opened; taking the
    /// broker's lock for each made every such read wait for as long as a
    /// broker thread held it to append and sync durable history, which froze
    /// the desktop while its frame loop was the reader. Only a changed gate
    /// takes the lock.
    registered: HashMap<WorkspaceId, channel_crypto::ChannelGate>,
    /// Channels whose frames arrived while this device took no part in
    /// them: they have a group, so this device never creates one.
    groups_seen: std::collections::HashSet<WorkspaceId>,
    /// The policy epoch the last pass read, while the directory announces
    /// its changes: a request that went unanswered under the policy before
    /// it is asked again at once when it moves, rather than waiting out a
    /// backoff that has doubled past what the caller waits
    /// ([`Self::ask_waiting_now`]).
    policy_epoch: Option<u64>,
}

impl ChannelEncryption {
    /// Forget `workspace`, deleted: its group and keys,
    /// here and in the store.
    ///
    /// # Errors
    /// What the store kept could not be removed.
    pub fn forget_workspace(&mut self, workspace: WorkspaceId) -> Result<(), String> {
        self.peers.remove(&workspace);
        self.changed.remove(&workspace);
        self.unopened.remove(&workspace);
        self.registered.remove(&workspace);
        self.groups_seen.remove(&workspace);
        self.store
            .as_ref()
            .map_or(Ok(()), |store| store.state.forget(workspace))
    }

    /// Open every channel state the host keeps and register each channel's
    /// gate with the broker, so the plaintext gate holds from the first op after
    /// a restart.
    pub fn open(
        config: ChannelEncryptionConfig,
        directory: Box<dyn ChannelDirectory>,
        gates: Arc<dyn ChannelGates>,
    ) -> std::io::Result<Self> {
        config.state.prepare().map_err(std::io::Error::other)?;
        let held = config.state.held();
        let mut service = Self::with_store(Some(config), directory, gates);
        let now = tilo_clock::now();
        for workspace in held {
            if let Err(error) = service.open_peer(workspace, now) {
                tracing::warn!(%error, %workspace, "channel state could not be opened");
            }
        }
        Ok(service)
    }

    /// The service of a device whose secret store did not open: it answers
    /// every workspace [`ChannelAccess::NoSecretStore`].
    pub fn without_store(
        directory: Box<dyn ChannelDirectory>,
        gates: Arc<dyn ChannelGates>,
    ) -> Self {
        Self::with_store(None, directory, gates)
    }

    fn with_store(
        store: Option<ChannelEncryptionConfig>,
        directory: Box<dyn ChannelDirectory>,
        gates: Arc<dyn ChannelGates>,
    ) -> Self {
        Self {
            store,
            directory,
            gates,
            peers: HashMap::new(),
            reconcile: PolicySchedule::default(),
            delivery: 0,
            generation: 0,
            changed: HashMap::new(),
            changes: tokio::sync::watch::Sender::new(0),
            queued: false,
            unopened: HashMap::new(),
            registered: HashMap::new(),
            groups_seen: std::collections::HashSet::new(),
            policy_epoch: None,
        }
    }

    /// Increases whenever any channel's keys or access change.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The [`Self::generation`] at which `workspace`'s keys or access last
    /// changed; 0 while they never did in this service.
    pub fn workspace_change(&self, workspace: WorkspaceId) -> u64 {
        self.changed.get(&workspace).copied().unwrap_or(0)
    }

    /// What an endpoint of this device may do with `workspace`'s group. The
    /// first call starts taking part in it: this device asks the participants
    /// for access, and creates the group when no genesis arrives. The caller
    /// has already decided the endpoint may read the workspace.
    pub fn access(&mut self, workspace: WorkspaceId, now: Instant) -> ChannelAccess {
        if self.store.is_none() {
            self.note_unopened(workspace, Unopened::unnamed(workspace));
            return ChannelAccess::NoSecretStore;
        }
        if !self.peers.contains_key(&workspace) {
            match self.start_peer(workspace, now) {
                Ok(true) => self.queued = true,
                Ok(false) => {
                    self.note_unopened(workspace, Unopened::unnamed(workspace));
                    return ChannelAccess::Unavailable;
                }
                Err(error) => {
                    tracing::warn!(%error, %workspace, "channel state could not be created");
                    self.note_unopened(workspace, Unopened::failed(workspace, now));
                    return ChannelAccess::Unavailable;
                }
            }
        }
        self.unopened.remove(&workspace);
        let Some(entry) = self.peers.get_mut(&workspace) else {
            return ChannelAccess::Unavailable;
        };
        let was_wanted = entry.wanted;
        entry.wanted = true;
        let access = match entry.peer.access() {
            Access::Ready { .. } => entry
                .peer
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
        };
        // The read that first wants a group wakes the pass that asks for
        // it: a peer another member's access request opened is nobody's
        // wanted group until then, and the pump would park with the request
        // never sent.
        if !was_wanted {
            self.queued = true;
        }
        access
    }

    /// Remember that `channel` could not be opened, so its route hears when
    /// that may have changed. A failed open keeps the backoff it already
    /// has; a first entry wakes a parked pump, which then arms its policy
    /// watch or retry deadline for it.
    fn note_unopened(&mut self, workspace: WorkspaceId, entry: Unopened) {
        match self.unopened.get_mut(&workspace) {
            Some(known) if known.workspace == entry.workspace => {
                if entry.retry.is_none() {
                    known.retry = None;
                } else if known.retry.is_none() {
                    known.retry = entry.retry;
                }
            }
            _ => {
                self.unopened.insert(workspace, entry);
                self.queued = true;
            }
        }
    }

    /// Tell the routes of unopened channels whose answer may now differ:
    /// on a policy re-check (`recheck`), those this device may now take
    /// part in; and those whose failed open is due again.
    fn wake_unopened(&mut self, now: Instant, recheck: bool) {
        let mut changed = Vec::new();
        // Without a store nothing opens, whatever the workspace's governance says.
        let stored = self.store.is_some();
        let device = self.device();
        for (workspace, entry) in &mut self.unopened {
            match &mut entry.retry {
                None => {
                    let opens = stored && takes_part(&*self.directory, device, entry.workspace);
                    if recheck && opens {
                        changed.push((*workspace, true));
                    }
                }
                Some(retry) => {
                    if retry.is_due(now) {
                        retry.fired(now, REQUEST_LONGEST);
                        changed.push((*workspace, false));
                    }
                }
            }
        }
        for (workspace, settled) in changed {
            // A workspace whose governance is now known is told once; its route
            // asks and opens the group. A failed open stays until one
            // succeeds.
            if settled {
                self.unopened.remove(&workspace);
            }
            self.bump(workspace);
        }
    }

    /// Whether an unopened channel waits for a re-check of the policy that
    /// may name it. Without a secret store nothing opens, so a device that
    /// holds none has nothing to re-check and parks.
    fn unopened_waits_for_policy(&self) -> bool {
        self.store.is_some() && self.unopened.values().any(|entry| entry.retry.is_none())
    }

    /// Observe a current key without opening a channel, requesting access or
    /// changing a removed member's rejoin state.
    fn existing_keys(&self, workspace: WorkspaceId) -> Option<ChannelContentKeys> {
        self.peers
            .get(&workspace)
            .and_then(|entry| entry.peer.content_keys())
    }

    /// `workspace`'s keys as this device holds them, opening, joining or
    /// creating nothing: `None` when this device takes part in no group of
    /// `workspace`, `Some(Err(_))` when it does and holds none of its keys
    /// yet.
    pub(crate) fn record_keys(
        &self,
        workspace: WorkspaceId,
    ) -> Option<Result<ChannelContentKeys, String>> {
        if let Some(keys) = self.existing_keys(workspace) {
            return Some(Ok(keys));
        }
        if self.store.is_none()
            || self
                .directory
                .allowed_devices(workspace, tilo_clock::unix_now())
                .is_none()
        {
            return None;
        }
        Some(Err("this device holds no channel keys yet".into()))
    }

    fn device(&self) -> Option<Ed25519PublicKey> {
        self.store
            .as_ref()
            .map(|store| Ed25519PublicKey::from_bytes(store.signer.device_public_key()))
    }

    /// Whether this device only follows its groups ([`Role::Reader`]).
    fn reads_only(&self) -> bool {
        self.store
            .as_ref()
            .is_some_and(|store| store.role == Role::Reader)
    }

    /// Whether a request waits on its signature from the device's custody: [`Self::signatures`]
    /// then has something to await.
    #[must_use]
    pub fn signing(&self) -> bool {
        self.peers.values().any(|entry| entry.signing.is_some())
    }

    /// Await every signature the device's custody owes, keeping each signed request for the next
    /// [`Self::pump`] to send.
    pub async fn signatures(&mut self) {
        for (workspace, request, signing) in self.take_signing() {
            let signed = signing.await;
            self.signed(workspace, request, signed);
        }
    }

    /// The access requests whose signatures the device's custody owes, taken out of the service
    /// so a shared one is not held while they are awaited; each answer goes back with
    /// [`Self::signed`].
    pub fn take_signing(
        &mut self,
    ) -> Vec<(
        WorkspaceId,
        MlsAccessRequestV1,
        collab_types::signer::SignFuture<'static>,
    )> {
        self.peers
            .iter_mut()
            .filter_map(|(workspace, entry)| {
                let (request, signing) = entry.signing.take()?;
                Some((*workspace, request, signing))
            })
            .collect()
    }

    /// Keep `request`, signed by the device's custody, for the next [`Self::pump`] to send.
    pub fn signed(
        &mut self,
        workspace: WorkspaceId,
        mut request: MlsAccessRequestV1,
        signed: Result<[u8; 64], collab_types::signer::SignError>,
    ) {
        match signed {
            Ok(signature) => {
                if let Some(entry) = self.peers.get_mut(&workspace) {
                    request.signature = collab_types::Signature::from_bytes(signature);
                    entry.request = Some(request);
                    self.queued = true;
                }
            }
            Err(error) => tracing::warn!(%error, "channel access request was not signed"),
        }
    }

    /// Run the governance protocol once: apply what arrived, move groups to
    /// the access policy, ask for access, and publish what is due. `wall` is
    /// the Unix time the policy is evaluated at.
    pub fn pump(&mut self, transport: &dyn GovernanceTransport, now: Instant, wall: u64) {
        let inbound = transport.take_inbound();
        self.pump_inbound(inbound, transport, now, wall);
    }

    /// [`Self::pump`] with `inbound`, the governance already taken from
    /// `transport`.
    fn pump_inbound(
        &mut self,
        inbound: Vec<InboundChannelGovernance>,
        transport: &dyn GovernanceTransport,
        now: Instant,
        wall: u64,
    ) {
        for inbound in inbound {
            self.receive(inbound, now, wall);
        }
        self.retry_held(now, wall);
        self.judge_held_welcomes(now, wall);
        // Read before the policy is: a change announced while the groups
        // reconcile has a later epoch and makes the next re-check due.
        let epoch = self.directory.policy_epoch();
        // The policy moved since the last pass: a request that went unanswered
        // under the one before it may be answered under this one.
        let policy_moved = epoch.is_some() && epoch != self.policy_epoch;
        self.policy_epoch = epoch;
        let reconcile_due = self
            .reconcile
            .due(now, epoch.unwrap_or(0))
            .is_some_and(|due| due <= now);
        let workspaces: Vec<WorkspaceId> = self.peers.keys().copied().collect();
        let mut reconciled = false;
        if policy_moved {
            self.ask_waiting_now(now);
        }
        for workspace in workspaces {
            let due = reconcile_due
                || self
                    .peers
                    .get(&workspace)
                    .is_some_and(|entry| entry.dirty || entry.turn_at.is_some_and(|at| at <= now));
            self.found(workspace, now, wall);
            if due {
                self.reconcile(workspace, now, wall);
                reconciled = true;
            }
            self.publish(workspace, transport);
            self.ask(workspace, transport, now, wall);
            self.save_deferred(workspace, now);
        }
        // Unopened channels are re-checked with the policy, and a failed
        // open when its backoff is due.
        self.wake_unopened(now, reconcile_due);
        if reconcile_due {
            self.reconcile
                .checked(now, epoch.unwrap_or(0), epoch.is_none());
        }
        // A newly opened workspace brings its own expiries.
        if reconciled && epoch.is_some() {
            self.register_next_policy_change(wall);
        }
    }

    /// Write every channel's state a frame taken in left unwritten, now
    /// rather than once it has waited [`DEFERRED_SAVE`]: the state carries the
    /// keys a write seals under, and it is written before such a write leaves
    /// this device ([`ChannelPeer::save_deferred`]).
    pub fn save_pending(&mut self) {
        let Some(store) = &self.store else {
            return;
        };
        for entry in self.peers.values_mut() {
            if !entry.peer.has_unsaved() {
                continue;
            }
            entry.unsaved_since = None;
            if let Err(error) = entry.peer.save_deferred(&store.vault) {
                tracing::warn!(%error, workspace = %entry.workspace, "channel state could not be written");
            }
        }
    }

    /// Write what taking in a frame left unwritten in `channel`'s state once
    /// it has waited [`DEFERRED_SAVE`].
    fn save_deferred(&mut self, workspace: WorkspaceId, now: Instant) {
        let (Some(entry), Some(store)) = (self.peers.get_mut(&workspace), &self.store) else {
            return;
        };
        if !entry.peer.has_unsaved() {
            entry.unsaved_since = None;
            return;
        }
        let since = *entry.unsaved_since.get_or_insert(now);
        if now < since.checked_add(DEFERRED_SAVE).unwrap_or(since) {
            return;
        }
        entry.unsaved_since = None;
        if let Err(error) = entry.peer.save_deferred(&store.vault) {
            tracing::warn!(%error, workspace = %entry.workspace, "channel state could not be written");
        }
    }

    /// When [`Self::pump`] next has work that no inbound message brings:
    /// `None` while there is none, so its caller can park until a message
    /// arrives or an endpoint's access wakes it ([`ChannelKeyAccess::with_wake`]).
    ///
    /// A group re-checks its access policy when the directory announces a
    /// change (`Self::waits_for_policy_change`) and at the next expiry the
    /// last re-check read, which [`Self::pump`] registers with
    /// [`ChannelDirectory::announce_at`]; neither is a deadline here. While
    /// this device has a channel to settle, a directory that announces
    /// nothing re-checks every `RECONCILE_INTERVAL`, and frames the
    /// transport did not take are retried at the same cadence; that re-check
    /// is what lets a channel this device only waits on be founded, or
    /// opened, once the policy names it. A waiting device is due on its
    /// request backoff.
    #[must_use]
    pub fn next_pump_deadline(&self, now: Instant) -> Option<Instant> {
        let settling = !self.peers.is_empty() || self.unopened_waits_for_policy();
        let turn = self.peers.values().filter_map(|entry| entry.turn_at).min();
        let policy = if self.peers.values().any(|entry| entry.dirty) {
            Some(now)
        } else if settling {
            let unpublished = self
                .peers
                .values()
                .any(|entry| !entry.peer.outbox().is_empty())
                .then(|| now.checked_add(RECONCILE_INTERVAL).unwrap_or(now));
            self.reconcile
                .due(now, self.directory.policy_epoch().unwrap_or(0))
                .into_iter()
                .chain(unpublished)
                .min()
        } else {
            None
        };
        let asking = self
            .peers
            .values()
            .filter_map(|entry| {
                let asking = match entry.peer.access() {
                    Access::Waiting => entry.wanted,
                    Access::Removed => entry.rejoin,
                    Access::Ready { .. } => false,
                };
                asking.then_some(entry.retry.due)
            })
            .min();
        let founding = self
            .peers
            .iter()
            .filter(|(workspace, entry)| {
                !entry.wanted
                    && entry.founding.allowed
                    && entry.founding.at > now
                    && !entry.peer.knows_group()
                    && !self.groups_seen.contains(workspace)
            })
            .map(|(_, entry)| entry.founding.at)
            .min();
        let unopened = self
            .unopened
            .values()
            .filter_map(|entry| entry.retry.as_ref().map(|retry| retry.due))
            .min();
        let welcome = self
            .peers
            .values()
            .filter_map(|entry| entry.welcome.as_ref().map(|held| held.until))
            .min();
        let unsaved = self
            .peers
            .values()
            .filter_map(|entry| entry.unsaved_since)
            .filter_map(|since| since.checked_add(DEFERRED_SAVE))
            .min();
        policy
            .into_iter()
            .chain(unsaved)
            .chain(welcome)
            .chain(turn)
            .chain(asking)
            .chain(founding)
            .chain(unopened)
            .min()
    }

    /// Whether the groups wait for their directory to announce a policy
    /// change: some exist, the directory announces, and nothing changed
    /// since their last re-check. The host's pump then sleeps until an
    /// announcement wakes it ([`ChannelKeyAccess::arm_policy_watch`]).
    fn waits_for_policy_change(&self) -> bool {
        (!self.peers.is_empty() || self.unopened_waits_for_policy())
            && self
                .directory
                .policy_epoch()
                .is_some_and(|epoch| self.reconcile.waits_for_change(epoch))
    }

    /// Register the first instant after `wall` at which the access policy of
    /// a workspace this device takes part in can change by time alone, so the
    /// groups re-check then ([`ChannelDirectory::announce_at`]).
    fn register_next_policy_change(&self, wall: u64) {
        let workspaces: BTreeSet<WorkspaceId> =
            self.peers.values().map(|entry| entry.workspace).collect();
        let next = workspaces
            .into_iter()
            .filter_map(|workspace| self.directory.next_policy_change(workspace, wall))
            .filter(|at| *at > wall)
            .min();
        if let Some(at) = next {
            self.directory.announce_at(at);
        }
    }

    fn receive(&mut self, inbound: InboundChannelGovernance, now: Instant, wall: u64) {
        match inbound {
            InboundChannelGovernance::Frame { workspace, frame } => {
                self.receive_frame(workspace, *frame, now, wall);
            }
            InboundChannelGovernance::AccessRequest { workspace, request } => {
                self.receive_request(workspace, *request, now, wall);
            }
        }
    }

    /// Judge the frames each channel held for the policy ([`Self::receive_frame`])
    /// again: a frame whose signer the policy names now is applied as if it had
    /// just arrived, and one it still does not back is held again.
    fn retry_held(&mut self, now: Instant, wall: u64) {
        let held: Vec<(WorkspaceId, MlsGovernanceFrame)> = self
            .peers
            .iter_mut()
            .flat_map(|(_, entry)| {
                let workspace = entry.workspace;
                std::mem::take(&mut entry.held)
                    .into_iter()
                    .map(move |frame| (workspace, frame))
            })
            .collect();
        for (workspace, frame) in held {
            self.receive_frame(workspace, frame, now, wall);
        }
    }

    /// Judge again each Welcome held for this device's view of the
    /// membership once that view changed, and give up on one held for
    /// [`WELCOME_WAIT`].
    fn judge_held_welcomes(&mut self, now: Instant, wall: u64) {
        let held: Vec<WorkspaceId> = self
            .peers
            .iter()
            .filter(|(_, entry)| entry.welcome.is_some())
            .map(|(workspace, _)| *workspace)
            .collect();
        for workspace in held {
            let allowed = self.directory.allowed_devices(workspace, wall);
            let (Some(entry), Some(store)) = (self.peers.get_mut(&workspace), &self.store) else {
                continue;
            };
            let Some(held) = &entry.welcome else {
                continue;
            };
            if entry.peer.access() != Access::Waiting {
                entry.welcome = None;
            } else if now >= held.until {
                give_up_welcome(entry, store, now);
                self.bump(workspace);
            } else if held.judged != allowed {
                // Held again by `receive_frame` if the view still does not
                // back it, keeping when it is given up on.
                let frame = held.frame.clone();
                self.receive_frame(workspace, frame, now, wall);
            }
        }
    }

    fn receive_frame(
        &mut self,
        workspace: WorkspaceId,
        frame: MlsGovernanceFrame,
        now: Instant,
        wall: u64,
    ) {
        // A frame naming another workspace than the one it arrived in is refused.
        if frame.workspace != workspace {
            return;
        }
        if !self.peers.contains_key(&workspace) {
            // A device that was away keeps the state of the channels it takes
            // part in, and its host replays the governance it retained when
            // the device comes back, before its apps read anything: take that
            // in rather than forget it, so the group is current before the
            // first read of its record. A channel this device holds no state
            // of is one it takes no part in: it never creates a second group
            // of a channel whose group it has heard, and if it comes to take
            // part it asks to join.
            let opened = self.store.is_some()
                && match self.open_peer(workspace, now) {
                    Ok(opened) => opened,
                    Err(error) => {
                        tracing::warn!(%error, %workspace, "channel state could not be opened");
                        false
                    }
                };
            if !opened {
                if self.groups_seen.len() < MAX_GROUPS_SEEN {
                    self.groups_seen.insert(workspace);
                }
                return;
            }
            self.receive_frame(workspace, frame, now, wall);
            return;
        }
        let allowed = self.directory.allowed_devices(workspace, wall);
        let (Some(entry), Some(store)) = (self.peers.get_mut(&workspace), &self.store) else {
            return;
        };
        let unjudged = match &allowed {
            // The workspace's governance reached this device before the
            // policy here named the frame's signer.
            Some(allowed) => !channel_crypto::backs(&frame, allowed),
            // A group competing with this device's, while it cannot name who
            // reads the channel: whether its founder may is not known here.
            None => entry.peer.competes(&frame),
        };
        if unjudged {
            // Keep the frame for the policy that moves instead of refusing it
            // for good: a device admitted a moment ago writes, and a winning
            // genesis moves this device to the group the others are in.
            if !entry.held.contains(&frame) {
                if entry.held.len() == MAX_HELD_FRAMES {
                    entry.held.remove(0);
                }
                entry.held.push(frame);
            }
            return;
        }
        let (epoch, signer) = (frame.epoch, frame.signed_by);
        // A waiting device keeps the frame in case it carries a Welcome its
        // view of the membership does not back yet.
        let kept = (entry.peer.access() == Access::Waiting).then(|| frame.clone());
        let received = match &allowed {
            Some(allowed) => entry.peer.receive(frame, allowed, &store.vault),
            None => entry.peer.follow(frame, &store.vault),
        };
        let event = match received {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!(%error, %workspace, "channel frame refused");
                return;
            }
        };
        tracing::debug!(%workspace, epoch, ?signer, ?event, access = ?entry.peer.access(), "key frame taken in");
        if !entry.wanted && entry.peer.access() == Access::Waiting && entry.peer.knows_group() {
            // Taken up only to create the group if no one had: someone has,
            // and its participants admit whoever asked.
            self.peers.remove(&workspace);
            self.groups_seen.insert(workspace);
            if let Err(error) = store.state.peers().forget(workspace) {
                tracing::warn!(%error, %workspace, "unwanted channel state could not be removed");
            }
            return;
        }
        let changed = match event {
            PeerEvent::Joined { .. } | PeerEvent::Advanced { .. } => {
                // The group may still differ from the policy.
                entry.dirty = true;
                true
            }
            PeerEvent::Removed => true,
            PeerEvent::Desynchronised
            | PeerEvent::WelcomeRefused(WelcomeRefusal::SignedByAnotherDevice) => {
                if let PeerEvent::WelcomeRefused(refusal) = &event {
                    tracing::warn!(%refusal, %workspace, ?signer, "key Welcome refused");
                }
                // Ask again with a fresh member key; any participant admits
                // a member asking with another key than it joined with (the
                // members that took a refused Welcome's commit hold this
                // device admitted with the key it asked with).
                entry.request = None;
                entry.retry = Backoff::now(now, REQUEST_FIRST);
                entry.fresh_at = now;
                true
            }
            PeerEvent::AdmittedWithAnotherKey => {
                // The participants admit it again with the key it asks with
                // now: ask at once, not on the backoff.
                entry.retry = Backoff::now(now, REQUEST_FIRST);
                false
            }
            // A Welcome refused for this device's view is held below.
            PeerEvent::Genesis
            | PeerEvent::Duplicate
            | PeerEvent::Ignored
            | PeerEvent::WelcomeRefused(WelcomeRefusal::DeviceNotAllowed) => false,
        };
        let changed = entry.hold_welcome(&event, kept, allowed, epoch, store, now) || changed;
        let gate = entry.peer.gate();
        self.register_gate(workspace, gate);
        if changed {
            self.bump(workspace);
        }
    }

    fn receive_request(
        &mut self,
        workspace: WorkspaceId,
        request: MlsAccessRequestV1,
        now: Instant,
        wall: u64,
    ) {
        // A reader admits no one.
        if self.reads_only() || request.workspace != workspace {
            return;
        }
        if !self.peers.contains_key(&workspace)
            && request.verify().is_ok()
            && self
                .directory
                .allowed_devices(workspace, wall)
                .is_some_and(|allowed| {
                    allowed.contains(&request.device)
                        && self
                            .device()
                            .is_some_and(|device| allowed.contains(&device))
                })
        {
            // A device the policy allows asks for a channel this device may
            // read and holds nothing of: it creates the group if no genesis
            // arrives. Outsiders cannot make it mint state.
            match self.start_peer(workspace, now) {
                Ok(true) => {
                    if let Some(entry) = self.peers.get_mut(&workspace) {
                        entry.wanted = false;
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, %workspace, "channel state could not be created");
                    return;
                }
            }
        }
        let Some(entry) = self
            .peers
            .get_mut(&workspace)
            .filter(|_| self.store.is_some())
        else {
            return;
        };
        let device = request.device;
        let learned = entry.peer.learn_request(request);
        tracing::debug!(%workspace, ?device, ?learned, "key access request taken");
        match learned {
            Ok(true) => entry.dirty = true,
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, %workspace, "channel access request refused"),
        }
    }

    fn reconcile(&mut self, workspace: WorkspaceId, now: Instant, wall: u64) {
        if !self.peers.contains_key(&workspace) {
            return;
        }
        let allowed = self.directory.allowed_devices(workspace, wall);
        let (Some(entry), Some(store)) = (self.peers.get_mut(&workspace), &self.store) else {
            return;
        };
        entry.dirty = false;
        // While this device cannot tell who holds the workspace's keys it
        // commits nothing and removes no one; nor does a reader.
        let Some(allowed) = allowed.filter(|_| store.role == Role::Participant) else {
            entry.turn_at = None;
            return;
        };
        // Members before this one in turn commit the change first; it
        // follows theirs, and commits only once their turns went by.
        let Some(turn) = entry.peer.turn(&allowed) else {
            entry.turn_at = None;
            return;
        };
        let waits = TURN.saturating_mul(u32::try_from(turn).unwrap_or(u32::MAX));
        let spaced = entry
            .committed_at
            .and_then(|at| at.checked_add(COMMIT_SPACING));
        let at = *entry.turn_at.get_or_insert_with(|| {
            now.checked_add(waits)
                .unwrap_or(now)
                .max(spaced.unwrap_or(now))
        });
        if now < at {
            return;
        }
        entry.turn_at = None;
        let outcome = match entry.peer.reconcile_now(&allowed, &store.vault) {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::warn!(%error, %workspace, "channel membership change failed");
                return;
            }
        };
        if !outcome.refused.is_empty() {
            tracing::warn!(%workspace, refused = outcome.refused.len(), "malformed channel access requests refused");
        }
        if outcome.changed() {
            tracing::debug!(%workspace, admitted = ?outcome.admitted, removed = ?outcome.removed, "key group changed");
            entry.committed_at = Some(now);
            let gate = entry.peer.gate();
            self.register_gate(workspace, gate);
            self.bump(workspace);
        }
    }

    /// Hand the transport every frame this device wrote or must publish
    /// again; kept for the next pass while the transport refuses one.
    fn publish(&mut self, workspace: WorkspaceId, transport: &dyn GovernanceTransport) {
        let Some(entry) = self
            .peers
            .get_mut(&workspace)
            .filter(|_| self.store.is_some())
        else {
            return;
        };
        if entry.peer.outbox().is_empty() {
            return;
        }
        let mut accepted = true;
        for frame in entry.peer.outbox() {
            tracing::debug!(%workspace, epoch = frame.epoch, "key frame published");
            self.delivery = self.delivery.wrapping_add(1);
            accepted &= transport.publish(NeighbourMsg::MlsGovernance {
                workspace: entry.workspace.to_string(),
                frame: Box::new(frame.clone()),
                delivery: self.delivery,
            });
        }
        if accepted {
            entry.peer.published();
        }
    }

    /// Ask again at once for the channels this device still waits on: the
    /// policy moved, so a request that went unanswered under the one before
    /// it may be answered now, and the request is kept, signed, ready to go
    /// again.
    fn ask_waiting_now(&mut self, now: Instant) {
        for entry in self.peers.values_mut() {
            if entry.wanted && entry.peer.access() == Access::Waiting {
                entry.retry = Backoff::now(now, REQUEST_FIRST);
            }
        }
    }

    /// Ask the participants for access on the request backoff, while waiting.
    fn ask(
        &mut self,
        workspace: WorkspaceId,
        transport: &dyn GovernanceTransport,
        now: Instant,
        wall: u64,
    ) {
        let (Some(entry), Some(store)) = (self.peers.get_mut(&workspace), &self.store) else {
            return;
        };
        let asking = match entry.peer.access() {
            Access::Waiting => entry.wanted,
            Access::Removed => entry.rejoin,
            Access::Ready { .. } => false,
        };
        if !asking || !entry.retry.is_due(now) || entry.signing.is_some() {
            return;
        }
        let fresh = now >= entry.fresh_at;
        if entry.request.is_none() || fresh {
            let request = match entry
                .peer
                .unsigned_access_request(wall, fresh, &store.vault)
            {
                Ok(request) => request,
                Err(error) => {
                    tracing::warn!(%error, "channel access request could not be prepared");
                    return;
                }
            };
            entry.request = None;
            entry.rejoin = false;
            if fresh {
                entry.fresh_at = now.checked_add(REQUEST_FRESH).unwrap_or(now);
            }
            let Some(mut request) = request else {
                return;
            };
            let Ok(message) = request.signing_bytes() else {
                return;
            };
            let signer = Arc::clone(&store.signer);
            let mut signing: SignFuture<'static> =
                Box::pin(async move { signer.sign(message).await });
            // A key that answers at once (a native one) signs in this pass; custody that answers
            // later (a page's) leaves the request for [`Self::signatures`].
            match signing
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            {
                std::task::Poll::Ready(Ok(signature)) => {
                    request.signature = collab_types::Signature::from_bytes(signature);
                    entry.request = Some(request);
                }
                std::task::Poll::Ready(Err(error)) => {
                    tracing::warn!(%error, "channel access request was not signed");
                    return;
                }
                std::task::Poll::Pending => {
                    entry.signing = Some((request, signing));
                    return;
                }
            }
        }
        let Some(request) = entry.request.clone() else {
            return;
        };
        entry.retry.fired(now, REQUEST_LONGEST);
        tracing::debug!(%workspace, device = ?request.device, issued = request.issued_at, "key access asked");
        self.delivery = self.delivery.wrapping_add(1);
        let message = NeighbourMsg::MlsAccessRequest {
            workspace: entry.workspace.to_string(),
            request: Box::new(request),
            delivery: self.delivery,
        };
        let _ = transport.publish(message);
    }

    /// Start taking part in `channel`: wait for access, and for the group to
    /// be created ([`Self::found`]). `Ok(false)` while the directory cannot
    /// name the channel's readers here: a workspace whose governance is unknown
    /// and which is no personal cluster of this device's.
    fn start_peer(&mut self, workspace: WorkspaceId, now: Instant) -> channel_crypto::Result<bool> {
        if !takes_part(&*self.directory, self.device(), workspace) {
            return Ok(false);
        }
        if self.open_peer(workspace, now)? {
            return Ok(true);
        }
        let store = self.store.as_ref().ok_or_else(no_store)?;
        let signer = Box::new(Arc::clone(&store.signer));
        let peer =
            ChannelPeer::waiting(Some(store.state.peers()), workspace, signer, &store.vault)?;
        self.insert_peer(workspace, peer, now);
        Ok(true)
    }

    /// Create `channel`'s group when this device created the workspace
    /// ([`ChannelDirectory::founds`]) and holds its keys. A device that has
    /// seen a genesis never creates one.
    fn found(&mut self, workspace: WorkspaceId, now: Instant, wall: u64) {
        let Some(entry) = self.peers.get(&workspace) else {
            return;
        };
        if entry.peer.knows_group() || self.groups_seen.contains(&workspace) || self.reads_only() {
            return;
        }
        let due = self.directory.founds(workspace) && (entry.wanted || now >= entry.founding.at);
        let reads = self
            .directory
            .allowed_devices(workspace, wall)
            .zip(self.device())
            .is_some_and(|(allowed, device)| allowed.contains(&device));
        let (Some(entry), Some(store)) = (self.peers.get_mut(&workspace), &self.store) else {
            return;
        };
        // Looked at again on every pass, a policy re-check's included.
        entry.founding.allowed = reads;
        if !reads || !due {
            return;
        }
        match entry.peer.found_now(&store.vault) {
            Ok(true) => {
                // The requests it holds are admitted at once.
                entry.dirty = true;
                entry.wanted = true;
                let gate = entry.peer.gate();
                self.register_gate(workspace, gate);
                self.bump(workspace);
            }
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, %workspace, "channel group could not be created"),
        }
    }

    /// Open the state kept for `channel`. `Ok(false)` when there is none.
    fn open_peer(&mut self, workspace: WorkspaceId, now: Instant) -> channel_crypto::Result<bool> {
        let store = self.store.as_ref().ok_or_else(no_store)?;
        let opened = match ChannelPeer::open(
            Some(store.state.peers()),
            workspace,
            Box::new(Arc::clone(&store.signer)),
            &store.vault,
        ) {
            // A machine copied from another re-keyed its device: the state
            // names the device it was copied from, whose member it is, and
            // can never act for this one. It is dropped, never used, so this
            // device takes no leaf of its source's; it then joins as itself,
            // as any further device of the member does.
            Err(channel_crypto::Error::OtherDevice) => {
                tracing::info!(
                    %workspace,
                    "channel state named the device this machine was copied from; joining as this device"
                );
                store.state.peers().forget(workspace)?;
                return Ok(false);
            }
            // State in a format this build does not write: authentic and this
            // device's own, but unreadable. Dropped, so the device can ask to
            // join again; what only this device held is gone, which is why it
            // is an error.
            Err(channel_crypto::Error::OtherFormat) => {
                tracing::error!(
                    %workspace,
                    "channel state is in a format no upgrade reaches: dropping it and joining again; \
                     keys only this device held are lost"
                );
                store.state.peers().forget(workspace)?;
                return Ok(false);
            }
            opened => opened?,
        };
        let Some(peer) = opened else {
            return Ok(false);
        };
        self.insert_peer(workspace, peer, now);
        Ok(true)
    }

    fn insert_peer(&mut self, workspace: WorkspaceId, peer: ChannelPeer, now: Instant) {
        let gate = peer.gate();
        let found_at = now
            .checked_add(found_after(&peer.device(), workspace))
            .unwrap_or(now);
        self.peers.insert(
            workspace,
            PeerEntry {
                workspace,
                peer,
                request: None,
                signing: None,
                retry: Backoff::now(now, REQUEST_FIRST),
                fresh_at: now.checked_add(REQUEST_FRESH).unwrap_or(now),
                dirty: true,
                turn_at: None,
                committed_at: None,
                rejoin: false,
                founding: Founding {
                    at: found_at,
                    allowed: !self.reads_only(),
                },
                wanted: true,
                held: Vec::new(),
                welcome: None,
                unsaved_since: None,
            },
        );
        self.register_gate(workspace, gate);
    }

    fn register_gate(&mut self, workspace: WorkspaceId, gate: channel_crypto::ChannelGate) {
        if !gate.encryption_required || self.registered.get(&workspace) == Some(&gate) {
            return;
        }
        self.gates.register_channel_gate(workspace, gate);
        self.registered.insert(workspace, gate);
    }

    /// Record that `workspace`'s keys or access changed, and announce it.
    fn bump(&mut self, workspace: WorkspaceId) {
        self.generation = self.generation.wrapping_add(1);
        self.changed.insert(workspace, self.generation);
        self.changes.send_replace(self.generation);
    }
}

/// Why a channel of this device's own could not be opened: no secret store
/// keeps its state. Callers check for a store first.
fn no_store() -> std::io::Error {
    std::io::Error::other("no secret store keeps this device's channel state")
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

/// Whether this device may take the channel up: the directory names it among
/// the channel's readers. A channel whose readers the directory names without
/// this device can be neither founded nor joined by it, and a channel whose
/// readers no one can name here (a workspace this device holds no view of) opens
/// nothing.
fn takes_part(
    directory: &dyn ChannelDirectory,
    device: Option<Ed25519PublicKey>,
    workspace: WorkspaceId,
) -> bool {
    directory
        .allowed_devices(workspace, tilo_clock::unix_now())
        .zip(device)
        .is_some_and(|(allowed, device)| allowed.contains(&device))
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

    /// Run `operation` on the service; `None` when the lock is poisoned.
    pub(crate) fn run<T>(&self, operation: impl FnOnce(&mut ChannelEncryption) -> T) -> Option<T> {
        let mut service = self.inner.lock().ok()?;
        Some(operation(&mut service))
    }

    /// See [`ChannelEncryption::ask_on_entry`]; the custody's signature is awaited without
    /// holding the service.
    pub async fn ask_on_entry(
        &self,
        workspace: WorkspaceId,
        now: Instant,
        wall: u64,
    ) -> Option<MlsAccessRequestV1> {
        match self.run(|service| service.entry_request(workspace, now, wall))?? {
            entry::EntryRequest::Signed(request) => Some(request),
            entry::EntryRequest::Unsigned(workspace, mut request, signing) => {
                let signature = signing
                    .await
                    .inspect_err(
                        |error| tracing::warn!(%error, "channel access request was not signed"),
                    )
                    .ok()?;
                request.signature = collab_types::Signature::from_bytes(signature);
                self.run(|service| service.entry_signed(workspace, request, now))
            }
        }
    }

    /// Forget `workspace`, deleted ([`ChannelEncryption::forget_workspace`]).
    ///
    /// # Errors
    /// What the store kept could not be removed, or the service is gone.
    pub fn forget_workspace(&self, workspace: WorkspaceId) -> Result<(), String> {
        self.run(|service| service.forget_workspace(workspace))
            .unwrap_or_else(|| Err("the channel service is gone".into()))
    }

    /// As [`Self::new`], for a host that parks its pump until it is poked:
    /// `wake` runs whenever an endpoint's access queues governance the next
    /// pump must send, so a first channel is announced or requested without
    /// waiting for a timer.
    pub fn with_wake(service: ChannelEncryption, wake: PumpWake) -> Self {
        Self {
            inner: Arc::new(Mutex::new(service)),
            wake: Some(wake),
        }
    }

    /// `workspace`'s keys as this device holds them
    /// ([`ChannelEncryption::record_keys`]), never opening, joining or
    /// creating anything. `None` when this device takes part in no group
    /// of the workspace; [`Err`] when it does and holds none yet.
    pub fn record_keys(
        &self,
        workspace: WorkspaceId,
    ) -> Option<Result<ChannelContentKeys, String>> {
        self.run(|service| service.record_keys(workspace))
            .unwrap_or_else(|| Some(Err("the channel service is unavailable".into())))
    }

    /// See [`ChannelEncryption::access`]; also returns the service's
    /// generation, which changes whenever any group's keys or access do.
    pub fn access(&self, workspace: WorkspaceId) -> (ChannelAccess, u64) {
        self.access_at(workspace, tilo_clock::now())
    }

    /// [`Self::access`] at the caller's own `now`.
    pub fn access_at(&self, workspace: WorkspaceId, now: Instant) -> (ChannelAccess, u64) {
        let Some((access, generation, queued)) = self.run(|service| {
            let access = service.access(workspace, now);
            (
                access,
                service.generation(),
                std::mem::take(&mut service.queued),
            )
        }) else {
            return (ChannelAccess::Unavailable, 0);
        };
        if queued && let Some(wake) = &self.wake {
            wake();
        }
        (access, generation)
    }

    /// [`ChannelEncryption::signatures`] on the shared service, never holding it while the
    /// device's custody signs.
    pub async fn signatures(&self) {
        let Some(owed) = self.run(ChannelEncryption::take_signing) else {
            return;
        };
        let mut answered = Vec::with_capacity(owed.len());
        for (workspace, request, signing) in owed {
            answered.push((workspace, request, signing.await));
        }
        let queued = self.run(|service| {
            for (workspace, request, signed) in answered {
                service.signed(workspace, request, signed);
            }
            std::mem::take(&mut service.queued)
        });
        if queued == Some(true)
            && let Some(wake) = &self.wake
        {
            wake();
        }
    }

    /// Whether the device's custody owes a signature ([`ChannelEncryption::signing`]).
    pub fn signing(&self) -> bool {
        self.run(|service| service.signing()).unwrap_or(false)
    }

    /// See [`ChannelEncryption::save_pending`]: the state a frame taken in
    /// left unwritten, written before a write sealed under its keys leaves.
    pub fn save_pending(&self) {
        self.run(ChannelEncryption::save_pending);
    }

    /// Open `envelope`, a new sealed write to one of `workspace`'s channels, for
    /// the durable owner that holds the channel's keys. Only under the
    /// channel's current epoch, and only while this device reads the
    /// channel; anything else is refused. Call it only once the envelope's
    /// author, role, route and signature were verified.
    pub fn open_new_write(
        &self,
        workspace: WorkspaceId,
        envelope: &SignedOp,
    ) -> Result<Vec<u8>, String> {
        let (keys, metadata, ciphertext) = self.sealed_parts(workspace, envelope)?;
        keys.open_current_content(envelope.artifact, &metadata, &ciphertext)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    }

    fn sealed_parts<'a>(
        &self,
        workspace: WorkspaceId,
        envelope: &'a SignedOp,
    ) -> Result<(ChannelContentKeys, EncryptionMetadata, Cow<'a, [u8]>), String> {
        let metadata = envelope.encryption.ok_or("the write is not sealed")?;
        let ciphertext =
            collab_types::sealed_ciphertext(envelope).map_err(|error| error.to_string())?;
        let (ChannelAccess::Ready(keys), _) = self.access(workspace) else {
            return Err("this device holds no key for the write's channel".into());
        };
        Ok((keys, metadata, ciphertext))
    }

    /// The payload a renewal of `envelope`, a sealed write of this device
    /// to one of `workspace`'s channels, carries under a fresh timestamp and
    /// sequence: the identical ciphertext while its epoch is the channel's
    /// current one, else its content re-sealed under the current epoch. The
    /// plaintext never leaves this call.
    pub fn renew_sealed(
        &self,
        workspace: WorkspaceId,
        envelope: &SignedOp,
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        let metadata = envelope.encryption.ok_or("the write is not sealed")?;
        let retention = metadata
            .retention()
            .ok_or("the write is not sealed with a content key")?;
        let (ChannelAccess::Ready(keys), _) = self.access(workspace) else {
            return Err("this device holds no key for the write's channel".into());
        };
        let plaintext = keys
            .open_content(envelope.artifact, &metadata, &envelope.payload)
            .map_err(|error| error.to_string())?;
        if metadata.epoch == keys.epoch() {
            return Ok((metadata, envelope.payload.clone()));
        }
        // The state carrying the epoch's key is written before the write
        // sealed under it goes out ([`ChannelEncryption::save_pending`]).
        self.save_pending();
        keys.seal_content(envelope.artifact, retention, &plaintext)
            .map_err(|error| error.to_string())
    }

    /// See [`ChannelEncryption::pump`].
    pub fn pump(&self, transport: &dyn GovernanceTransport) {
        let _ = self.pump_until_due(transport);
    }

    /// [`Self::pump`] at the caller's own `now`, for a host that keeps a
    /// clock of its own.
    pub fn pump_at(&self, transport: &dyn GovernanceTransport, now: Instant) {
        self.run(|service| service.pump(transport, now, tilo_clock::unix_now()));
    }

    /// [`Self::pump`], then how long until the protocol next has work that
    /// no message brings ([`ChannelEncryption::next_pump_deadline`]); `None`
    /// while it has none, and the caller parks until a governance message
    /// arrives or the wake given to [`Self::with_wake`] fires.
    ///
    /// Work this pass found due and still left due (an access request that
    /// could not be prepared) is retried after `RECONCILE_INTERVAL`, the
    /// cadence every pass had while the caller woke on a fixed timer.
    #[must_use]
    pub fn pump_until_due(&self, transport: &dyn GovernanceTransport) -> Option<Duration> {
        let inbound = transport.take_inbound();
        let now = tilo_clock::now();
        let deadline = self.run(|service| {
            service.pump_inbound(inbound, transport, now, tilo_clock::unix_now());
            service.next_pump_deadline(now)
        })?;
        wait_after_pass(deadline, now)
    }

    /// Arm `watch` (whose wake pokes this host's pump) when groups wait for
    /// a policy announcement, so a revocation, a role change or an expiry
    /// re-checks them with no timer running. Returns `false` when a change
    /// already arrived that the arming will not wake for: the caller pumps
    /// again now instead of parking.
    ///
    /// A device taking part in no channel never arms it, and so sleeps
    /// through every announcement.
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

    /// See [`ChannelEncryption::generation`].
    pub fn generation(&self) -> u64 {
        self.inner.lock().map_or(0, |service| service.generation())
    }

    /// See [`ChannelEncryption::workspace_change`].
    pub fn workspace_change(&self, workspace: WorkspaceId) -> u64 {
        self.inner
            .lock()
            .map_or(0, |service| service.workspace_change(workspace))
    }

    /// Wakes whenever any channel's keys or access change; `None` when the
    /// service is unavailable after a poisoned lock.
    pub fn changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
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
        #[cfg(not(target_family = "wasm"))]
        {
            static SESSION: std::sync::OnceLock<ChannelKeyProvider> = std::sync::OnceLock::new();
            SESSION.get_or_init(Self::default).clone()
        }
        #[cfg(target_family = "wasm")]
        {
            // A page's one thread: the slot is the page's own, not a shared static.
            thread_local! {
                static SESSION: ChannelKeyProvider = ChannelKeyProvider::default();
            }
            SESSION.with(Clone::clone)
        }
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

    /// [`ChannelKeyAccess::generation`] of the service installed now, 0
    /// while none is: what a first [`Self::changed`] waits from.
    pub fn generation(&self) -> u64 {
        self.get().map_or(0, |access| access.generation())
    }

    /// Wait until any channel's keys or this device's access to them may have
    /// changed since `after`, the value the previous wait returned (or
    /// [`Self::generation`] read before looking at the keys), and return the
    /// value to wait from next. As [`Self::workspace_changed`], for every
    /// workspace at once.
    pub async fn changed(&self, after: u64) -> u64 {
        self.changed_where(after, ChannelKeyAccess::generation)
            .await
    }

    /// Wait until `workspace`'s keys or this device's access to them may have
    /// changed since `after`, the value the previous wait returned (0 at
    /// first), and return the value to wait from next.
    ///
    /// Resolves at once when the workspace changed since `after`; otherwise
    /// when it next changes, or when the service is installed, replaced or
    /// goes away. It reports only that this one channel changed, nothing
    /// about its keys or any other channel: the endpoint then asks the
    /// identity service, which checks the device's trust and the person's
    /// role on every answer. Costs no wakeup while nothing changes.
    pub async fn workspace_changed(&self, workspace: WorkspaceId, after: u64) -> u64 {
        self.changed_where(after, |access| access.workspace_change(workspace))
            .await
    }

    /// Wait until `current` of the installed service differs from `after`,
    /// or the service is installed, replaced or goes away.
    async fn changed_where(
        &self,
        after: u64,
        current: impl Fn(&ChannelKeyAccess) -> u64 + Send,
    ) -> u64 {
        // Subscribed before looking, so an installation between the two is
        // still seen.
        let mut installed = self.installed.subscribe();
        let Some((access, mut changes)) = self
            .get()
            .and_then(|access| access.changes().map(|changes| (access, changes)))
        else {
            // No service: nothing changes until one is installed. The
            // installation counter lives as long as `self`.
            let _ = installed.changed().await;
            return self.get().map_or(0, |access| current(&access));
        };
        loop {
            let change = current(&access);
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
                _ = installed.changed() => return self.get().map_or(0, |access| current(&access)),
            }
        }
    }

    /// [`Self::record_keys`] as a workspace host running on this device asks
    /// for them, by the workspace's id as its records name it: opening the
    /// channel when it has none yet and `create`, and waiting up to `wait` for
    /// the group. `None` when this device takes no part in the workspace's
    /// channels; [`Err`] when it takes part and holds no keys yet.
    pub async fn channel_keys(
        &self,
        workspace: &str,
        create: bool,
        wait: Duration,
    ) -> Option<Result<ChannelContentKeys, String>> {
        let workspace = WorkspaceId::parse(workspace)?;
        if !create {
            let access = self.get()?;
            // Asked off the caller's task: the service takes its lock and
            // reads its state on that thread, never on the host's.
            #[cfg(not(target_family = "wasm"))]
            return tokio::task::spawn_blocking(move || access.record_keys(workspace))
                .await
                .unwrap_or_else(|error| Some(Err(error.to_string())));
            // A page has one thread: the read runs on it.
            #[cfg(target_family = "wasm")]
            return access.record_keys(workspace);
        }
        self.open_record(workspace, wait).await
    }

    /// `route`'s channel's keys, opening the channel when it has none yet
    /// ([`ChannelEncryption::access_record`]), and waiting up to `wait` for
    /// the group when the directory backs this device but no genesis has
    /// arrived or been admitted yet. An endpoint writing a record's first
    /// content asks this: the group is created within [`founding_wait`], and
    /// the write lands with its keys rather than failing.
    ///
    /// `None` when this device takes part in no channel of the route's
    /// workspace; [`Err`] when it waited and the keys still are not there,
    /// saying how long it waited.
    pub async fn open_record(
        &self,
        workspace: WorkspaceId,
        wait: Duration,
    ) -> Option<Result<ChannelContentKeys, String>> {
        let deadline = tilo_clock::now()
            .checked_add(wait)
            .unwrap_or_else(Instant::now);
        loop {
            let access = self.get()?;
            // Read before the ask: a change between the two is no wait at all.
            let after = access.workspace_change(workspace);
            let (state, _) = access.access(workspace);
            match state {
                ChannelAccess::Ready(keys) => return Some(Ok(keys)),
                ChannelAccess::Waiting => {}
                ChannelAccess::Removed => {
                    return Some(Err(
                        "this device was removed from the channel's group".into()
                    ));
                }
                ChannelAccess::Unavailable | ChannelAccess::NoSecretStore => return None,
            }
            let remaining = deadline.saturating_duration_since(tilo_clock::now());
            if remaining.is_zero() {
                return Some(Err(not_ready(wait)));
            }
            let changed = tokio::time::timeout(remaining, self.workspace_changed(workspace, after));
            if changed.await.is_err() {
                return Some(Err(not_ready(wait)));
            }
        }
    }
}

/// What a caller waiting `wait` for a channel's keys is told when they never
/// arrived: how long it waited, so whoever reads it knows what the wait was
/// and that nothing more is coming.
fn not_ready(wait: Duration) -> String {
    format!(
        "the channel's keys did not arrive within {}s",
        wait.as_secs()
    )
}

/// The identity service's route keys, from this host's channel service once
/// one runs.
impl tilo_identity_host::sources::ChannelKeySource for ChannelKeyProvider {
    fn access(
        &self,
        workspace: WorkspaceId,
        known_generation: u64,
    ) -> Option<Result<tilo_identity_host::sources::ChannelKeys, String>> {
        use runtime_identity::wire::ChannelAccess as Wire;
        let channels = self.get()?;
        let (access, generation) = channels.access(workspace);
        // A caller holding no keys asks with generation 0, which is also
        // the generation of a service that has changed nothing since it
        // started (a channel it restored from disk, after a restart): it is
        // sent the keys, never told they are unchanged.
        let held = known_generation != 0 && generation == known_generation;
        Some(match access {
            ChannelAccess::Ready(keys) if !held => keys
                .to_bytes()
                .map(|bytes| (Wire::Ready, generation, bytes.to_vec()))
                .map_err(|error| error.to_string()),
            ChannelAccess::Ready(_) => Ok((Wire::Ready, generation, Vec::new())),
            ChannelAccess::Waiting => Ok((Wire::Waiting, generation, Vec::new())),
            ChannelAccess::Removed => Ok((Wire::Removed, generation, Vec::new())),
            ChannelAccess::Unavailable => Ok((Wire::Unavailable, generation, Vec::new())),
            ChannelAccess::NoSecretStore => Ok((Wire::NoSecretStore, generation, Vec::new())),
        })
    }
}

mod entry;

#[cfg(test)]
mod tests {
    include!("encryption_tests.rs");
}
