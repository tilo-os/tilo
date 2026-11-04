//! End-to-end encryption of collaboration channels on this device.
//!
//! Every document route is a channel with one MLS group, whose members are
//! exactly the devices the workspace's access policy lets read the route
//! ([`ChannelDirectory::allowed_devices`]). Every such device is a
//! participant and runs the same [`ChannelPeer`]: any
//! of them writes the commit that brings the group in line with the policy,
//! and every one refuses a commit the policy does not back (see
//! [`channel_crypto::peer`]). A device that cannot name a channel's record
//! cannot tell who may read it: it commits nothing to that channel, and only
//! follows the commits of participants that can.
//!
//! [`ChannelEncryption::access`] answers an application endpoint that wants
//! the content keys of a route, starting to take part in its channel as
//! needed: the device asks the participants for access, and creates the
//! group itself when no genesis of the channel has arrived a short while
//! later ([`found_after`]). A device that holds no state for a channel
//! another device asks about waits the same while, to create the group if
//! no one has; it leaves the channel alone once a genesis arrives. Two
//! geneses made at once resolve to one, and nothing sealed under the other
//! is lost (see [`channel_crypto::peer`]). [`ChannelEncryption::pump`] runs
//! the protocol over the workspace transport:
//!
//! - inbound frames advance the group, inbound access requests are kept for
//!   the next commit;
//! - every participant moves its group to the access policy, so an invite,
//!   a removal, a role change that ends access, a new certified device or a
//!   revoked one becomes a commit from whichever participant gets there
//!   first (of two on one epoch, every device keeps the lower digest);
//! - frames the transport did not take are published again, and waiting
//!   devices re-send their request with backoff. The workspace's host keeps each
//!   channel's newest frame for whoever was away.
//!
//! Every change registers the channel's plaintext gate with the broker,
//! which refuses plaintext and ciphertext bound to another channel on every
//! ingress path. The broker itself never holds a key.
//!
//! Every change is also announced ([`ChannelKeyProvider::channel_changed`]):
//! an application endpoint holding a route's keys waits on that signal and
//! asks the identity service again only when it fires, rather than on a
//! timer. A policy change becomes a commit within `RECONCILE_INTERVAL` (on
//! its announcement, or at the expiry, when the directory announces changes:
//! [`ChannelDirectory::policy_epoch`]); the commit is announced on the
//! device that wrote it the moment it is made and on every other the moment
//! its frame arrives, and the endpoint asks for keys at once.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use crate::governance_pump::{GovernancePark, GovernancePump};

use channel_crypto::{Access, ChannelContentKeys, ChannelPeer, PeerEvent, VaultKey};
use collab_types::{
    ChannelId, ChannelRoute, Ed25519PublicKey, EncryptionMetadata, MlsAccessRequestV1,
    MlsGovernanceFrame, SignedOp,
};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;

use crate::collab_core::BrokerChannels;
use crate::policy_schedule::{PolicySchedule, RECHECK_INTERVAL};
use tilo_desktop_model::WorkspaceId;

/// How often groups re-check the access policy when their directory
/// announces no changes, the longest an announced change waits, and how soon
/// a pass that left work due tries again ([`crate::policy_schedule`]).
const RECONCILE_INTERVAL: Duration = RECHECK_INTERVAL;
/// First and longest wait before a waiting device asks again.
const REQUEST_FIRST: Duration = Duration::from_secs(2);
const REQUEST_LONGEST: Duration = Duration::from_secs(120);
/// How long a waiting device asks with one member key before it asks with
/// a fresh one: an admission whose Welcome never reached it is then answered
/// again.
const REQUEST_FRESH: Duration = Duration::from_secs(60);
/// Under the state root, the directory of a workspace's channels.
const CHANNELS_DIR: &str = "channels";
/// Channels remembered as having a group this device takes no part in.
const MAX_GROUPS_SEEN: usize = 1 << 16;
/// Frames one channel keeps for the policy that has not named their signer
/// yet. Bounded, newest kept: a frame this device's policy still does not
/// back after that many later ones is not going to be applied.
const MAX_HELD_FRAMES: usize = 8;

/// How long a device that holds no state for a channel waits for its genesis
/// (from the workspace's retained frames, or a participant answering its request)
/// before it creates the group itself. Each device waits its own while
/// within a second or two of the next, set by its key and the channel, so
/// one creates the group and the others hear of it in time.
fn found_after(device: &Ed25519PublicKey, channel: ChannelId) -> Duration {
    let spread =
        device
            .as_bytes()
            .iter()
            .zip(channel.0.as_bytes())
            .fold(0_u64, |acc, (device, channel)| {
                acc.wrapping_mul(31)
                    .wrapping_add(u64::from(device ^ channel))
            })
            % 2_000;
    REQUEST_FIRST.saturating_add(Duration::from_millis(spread))
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
/// variant is signed by a device key and verified by the channel service
/// before it changes any state.
#[derive(Debug, Clone)]
pub enum InboundChannelGovernance {
    Frame {
        workspace: WorkspaceId,
        frame: Box<collab_types::MlsGovernanceFrame>,
    },
    /// A device asking a channel's participants for membership, with the
    /// route of the channel's record when the device sent it
    /// ([`NeighbourMsg::MlsRoutedAccessRequest`]).
    AccessRequest {
        workspace: WorkspaceId,
        request: Box<collab_types::MlsAccessRequestV1>,
        route: Option<Box<ChannelRoute>>,
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
            } => WorkspaceId::parse(&workspace).map(|workspace| Self::AccessRequest {
                workspace,
                request,
                route: None,
            }),
            NeighbourMsg::MlsRoutedAccessRequest {
                room: workspace,
                request,
                route,
                ..
            } => WorkspaceId::parse(&workspace).map(|workspace| Self::AccessRequest {
                workspace,
                request,
                route: Some(route),
            }),
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
pub trait ChannelDirectory: Send {
    /// Every device that may read `channel`, one of `workspace`'s channels, at
    /// wall-clock `now` (seconds since the Unix epoch). A channel is one
    /// record's, so a Read Deny on that record, on its app or on any pattern
    /// covering it takes its person's devices out of this channel and no
    /// other: no participant then hands them the record's key, and a device
    /// already holding it is removed in a new epoch.
    ///
    /// `route` is the channel's record when this device knows it: proven
    /// against the channel ([`ChannelRoute::proves`]), from a device's routed
    /// request or this device's own endpoint. Without it the directory names
    /// the record only from grants naming it. `None` for a channel it cannot
    /// name: it may be a member's own record, so who reads it is unknown, and
    /// this device admits and removes no one.
    ///
    /// A device takes part in a channel exactly when this names it: a
    /// workspace's host has no place in its channels that its membership
    /// does not give it.
    ///
    /// A personal cluster ([`collab_types::personal_cluster`]) has no record grants:
    /// its channels are read by the devices of the identity it derives from
    /// (certified, not revoked), and a directory holding that identity
    /// answers them for every channel. A channel is then taken up once the
    /// directory can name its readers.
    fn allowed_devices(
        &self,
        workspace: WorkspaceId,
        channel: ChannelId,
        route: Option<&ChannelRoute>,
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
}

/// Durable location and keys of this device's own channel state: what a
/// participant of a channel needs, and only workspaces this device's own workspace
/// host does not take part in need it.
pub struct ChannelEncryptionConfig {
    /// Private directory holding sealed channel state.
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

/// When this device may create a channel's group itself.
struct Founding {
    /// When it creates the group if no genesis has arrived.
    at: Instant,
    /// Whether the policy, when last read, let this device read the channel
    /// and so create its group.
    allowed: bool,
}

/// One channel this device takes part in.
struct PeerEntry {
    workspace: WorkspaceId,
    peer: ChannelPeer,
    /// The channel's record, once proven: decides Read on the record, and
    /// goes with every access request so other participants can too.
    route: Option<ChannelRoute>,
    /// The request this device asks with while it waits, re-sent on `retry`.
    request: Option<MlsAccessRequestV1>,
    retry: Backoff,
    /// When the next request mints a fresh member key: an admission whose
    /// Welcome never arrived here is then answered again.
    fresh_at: Instant,
    /// The group may differ from the policy: reconcile on the next pass.
    dirty: bool,
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
    broker: Arc<Mutex<BrokerChannels>>,
    peers: HashMap<ChannelId, PeerEntry>,
    /// When the groups last re-checked the access policy, and what the next
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
    /// Channels asked about that could not be opened; see [`Unopened`].
    unopened: HashMap<ChannelId, Unopened>,
    /// The gate this service last registered with the broker for each
    /// channel. Every access re-asserts its channel's gate, and a channel's
    /// keys are read for every live frame sealed or opened; taking the
    /// broker's lock for each made every such read wait for as long as a
    /// broker thread held it to append and sync durable history, which froze
    /// the desktop while its frame loop was the reader. Only a changed gate
    /// takes the lock.
    registered: HashMap<ChannelId, channel_crypto::ChannelGate>,
    /// Channels whose frames arrived while this device took no part in
    /// them: they have a group, so this device never creates one.
    groups_seen: std::collections::HashSet<ChannelId>,
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
        let state_root = config.root.clone();
        let mut service = Self::with_store(Some(config), directory, broker);
        let now = Instant::now();
        for workspace_dir in std::fs::read_dir(&state_root)? {
            let workspace_dir = workspace_dir?.path();
            let Some(workspace) = workspace_dir
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(WorkspaceId::parse)
            else {
                continue;
            };
            for channel in channels_in(&workspace_dir.join(CHANNELS_DIR), "peer") {
                if let Err(error) = service.open_peer(workspace, channel, now) {
                    tracing::warn!(%error, %workspace, "channel state could not be opened");
                }
            }
        }
        Ok(service)
    }

    /// The service of a device whose secret store did not open: it answers
    /// every workspace [`ChannelAccess::NoSecretStore`].
    pub fn without_store(
        directory: Box<dyn ChannelDirectory>,
        broker: Arc<Mutex<BrokerChannels>>,
    ) -> Self {
        Self::with_store(None, directory, broker)
    }

    fn with_store(
        store: Option<ChannelEncryptionConfig>,
        directory: Box<dyn ChannelDirectory>,
        broker: Arc<Mutex<BrokerChannels>>,
    ) -> Self {
        Self {
            store,
            directory,
            broker,
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
        }
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

    /// What an endpoint of this device may do with `channel` in `workspace`.
    /// The first call for a route starts taking part in its channel: this
    /// device asks the participants for access, and creates the group when
    /// no genesis arrives. The caller has already decided the endpoint may
    /// read the route.
    pub fn access(
        &mut self,
        workspace: WorkspaceId,
        channel: ChannelId,
        now: Instant,
    ) -> ChannelAccess {
        self.access_routed(workspace, channel, None, now)
    }

    fn access_routed(
        &mut self,
        workspace: WorkspaceId,
        channel: ChannelId,
        route: Option<&ChannelRoute>,
        now: Instant,
    ) -> ChannelAccess {
        if self.store.is_none() {
            self.note_unopened(
                channel,
                Unopened {
                    workspace,
                    retry: None,
                },
            );
            return ChannelAccess::NoSecretStore;
        }
        if !self.peers.contains_key(&channel) {
            match self.start_peer(workspace, channel, route, now) {
                Ok(true) => self.queued = true,
                Ok(false) => {
                    self.note_unopened(
                        channel,
                        Unopened {
                            workspace,
                            retry: None,
                        },
                    );
                    return ChannelAccess::Unavailable;
                }
                Err(error) => {
                    tracing::warn!(%error, %workspace, "channel state could not be created");
                    self.note_unopened(channel, Unopened::failed(workspace, now));
                    return ChannelAccess::Unavailable;
                }
            }
        }
        self.unopened.remove(&channel);
        // Learned before it is founded: who may read the channel is read from
        // its record, and the request this device sends carries the route.
        if let Some(route) = route {
            self.learn_route(workspace, channel, route);
        }
        let Some(entry) = self.peers.get_mut(&channel) else {
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
        // The read that first wants a channel wakes the pass that asks for
        // it: a peer another member's access request opened is nobody's
        // wanted channel until then, and the pump would park with the
        // request never sent.
        if !was_wanted {
            self.queued = true;
        }
        access
    }

    /// [`Self::access`] for the channel of `route`'s record in `workspace`,
    /// remembering the route: this device then decides Read on the record
    /// itself, and sends the route with its access request.
    pub fn access_record(
        &mut self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        now: Instant,
    ) -> (ChannelId, ChannelAccess) {
        let channel = route.channel(&workspace.to_string());
        (
            channel,
            self.access_routed(workspace, channel, Some(route), now),
        )
    }

    /// Keep `route`, proven to be `channel`'s in `workspace`, with the channel's
    /// state, durably, and re-check the channel's readers with it.
    fn learn_route(&mut self, workspace: WorkspaceId, channel: ChannelId, route: &ChannelRoute) {
        if !route.proves(&workspace.to_string(), channel) {
            return;
        }
        let Some(store) = &self.store else {
            return;
        };
        if let Some(entry) = self.peers.get_mut(&channel)
            && entry.workspace == workspace
            && entry.route.is_none()
        {
            entry.route = Some(route.clone());
            entry.dirty = true;
            save_route(&route_path(&store.root, workspace, channel), route);
        }
    }

    /// Remember that `channel` could not be opened, so its route hears when
    /// that may have changed. A failed open keeps the backoff it already
    /// has; a first entry wakes a parked pump, which then arms its policy
    /// watch or retry deadline for it.
    fn note_unopened(&mut self, channel: ChannelId, entry: Unopened) {
        match self.unopened.get_mut(&channel) {
            Some(known) if known.workspace == entry.workspace => {
                if entry.retry.is_none() {
                    known.retry = None;
                } else if known.retry.is_none() {
                    known.retry = entry.retry;
                }
            }
            _ => {
                self.unopened.insert(channel, entry);
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
        for (channel, entry) in &mut self.unopened {
            match &mut entry.retry {
                None => {
                    // An unopened channel keeps no route, so only a
                    // directory that names its readers by channel alone can
                    // open it here; a request carrying the route opens it
                    // through `start_peer` instead.
                    let opens = stored
                        && takes_part(&*self.directory, device, entry.workspace, *channel, None);
                    if recheck && opens {
                        changed.push((*channel, true));
                    }
                }
                Some(retry) => {
                    if retry.is_due(now) {
                        retry.fired(now, REQUEST_LONGEST);
                        changed.push((*channel, false));
                    }
                }
            }
        }
        for (channel, settled) in changed {
            // A workspace whose governance is now known is told once; its route
            // asks and opens the channel. A failed open stays until one
            // succeeds.
            if settled {
                self.unopened.remove(&channel);
            }
            self.bump(channel);
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
    fn existing_keys(
        &self,
        workspace: WorkspaceId,
        channel: ChannelId,
    ) -> Option<ChannelContentKeys> {
        self.peers
            .get(&channel)
            .filter(|entry| entry.workspace == workspace)
            .and_then(|entry| entry.peer.content_keys())
    }

    /// `channel`'s keys as this device holds them, opening, joining or
    /// creating nothing: `None` when this device takes part in no channel of
    /// `workspace`, `Some(Err(_))` when it does and holds none of this one yet.
    fn record_keys(
        &self,
        workspace: WorkspaceId,
        channel: ChannelId,
    ) -> Option<Result<ChannelContentKeys, String>> {
        if let Some(keys) = self.existing_keys(workspace, channel) {
            return Some(Ok(keys));
        }
        // The directory names a channel's readers only for a device that may
        // read it: an unnameable channel is no channel of this device's.
        if self.store.is_none()
            || self
                .directory
                .allowed_devices(workspace, channel, None, tilo_clock::unix_now())
                .is_none()
        {
            return None;
        }
        Some(Err("this device holds no channel keys yet".into()))
    }

    fn device(&self) -> Option<Ed25519PublicKey> {
        self.store
            .as_ref()
            .map(|store| Ed25519PublicKey::from_bytes(store.device_key.verifying_key().to_bytes()))
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
        // Read before the policy is: a change announced while the groups
        // reconcile has a later epoch and makes the next re-check due.
        let epoch = self.directory.policy_epoch();
        let reconcile_due = self
            .reconcile
            .due(now, epoch.unwrap_or(0))
            .is_some_and(|due| due <= now);
        let channels: Vec<ChannelId> = self.peers.keys().copied().collect();
        let mut reconciled = false;
        for channel in channels {
            let due = reconcile_due || self.peers.get(&channel).is_some_and(|entry| entry.dirty);
            self.found(channel, now, wall);
            if due {
                self.reconcile(channel, wall);
                reconciled = true;
            }
            self.publish(channel, transport);
            self.ask(channel, transport, now, wall);
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
            .filter(|(channel, entry)| {
                entry.founding.allowed
                    && entry.founding.at > now
                    && !entry.peer.knows_group()
                    && !self.groups_seen.contains(channel)
            })
            .map(|(_, entry)| entry.founding.at)
            .min();
        let unopened = self
            .unopened
            .values()
            .filter_map(|entry| entry.retry.as_ref().map(|retry| retry.due))
            .min();
        policy
            .into_iter()
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
            InboundChannelGovernance::AccessRequest {
                workspace,
                request,
                route,
            } => {
                self.receive_request(workspace, *request, route.as_deref(), now, wall);
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

    fn receive_frame(
        &mut self,
        workspace: WorkspaceId,
        frame: MlsGovernanceFrame,
        now: Instant,
        wall: u64,
    ) {
        let channel = frame.channel;
        let Some(route) = self
            .peers
            .get(&channel)
            .filter(|entry| entry.workspace == workspace)
            .map(|entry| entry.route.clone())
        else {
            // A group this device takes no part in yet: if it comes to, it
            // asks to join and never creates a second one.
            if self.groups_seen.len() < MAX_GROUPS_SEEN {
                self.groups_seen.insert(channel);
            }
            return;
        };
        let allowed = self
            .directory
            .allowed_devices(workspace, channel, route.as_ref(), wall);
        let (Some(entry), Some(store)) = (self.peers.get_mut(&channel), &self.store) else {
            return;
        };
        if let Some(allowed) = &allowed
            && !channel_crypto::backs(&frame, allowed)
        {
            // The workspace's governance reached this device before the
            // policy here named the frame's signer. Keep the frame for the
            // policy that moves instead of refusing a write from a device
            // admitted a moment ago for good.
            if !entry.held.contains(&frame) {
                if entry.held.len() == MAX_HELD_FRAMES {
                    entry.held.remove(0);
                }
                entry.held.push(frame);
            }
            return;
        }
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
        if !entry.wanted && entry.peer.access() == Access::Waiting && entry.peer.knows_group() {
            // Taken up only to create the group if no one had: someone has,
            // and its participants admit whoever asked.
            let dir = channels_dir(&store.root, workspace);
            self.peers.remove(&channel);
            self.groups_seen.insert(channel);
            if let Err(error) = std::fs::remove_file(ChannelPeer::path_in(&dir, channel)) {
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
            PeerEvent::Desynchronised => {
                // Ask again with a fresh member key; any participant admits
                // a member asking with another key than it joined with.
                entry.request = None;
                entry.retry = Backoff::now(now, REQUEST_FIRST);
                entry.fresh_at = now;
                true
            }
            PeerEvent::Genesis | PeerEvent::Duplicate | PeerEvent::Ignored => false,
        };
        let gate = entry.peer.gate();
        self.register_gate(channel, gate);
        if changed {
            self.bump(channel);
        }
    }

    fn receive_request(
        &mut self,
        workspace: WorkspaceId,
        request: MlsAccessRequestV1,
        route: Option<&ChannelRoute>,
        now: Instant,
        wall: u64,
    ) {
        let channel = request.channel;
        // A route that is not the channel's is a forgery: the request goes
        // with it, rather than being decided as if it named no record.
        if route.is_some_and(|route| !route.proves(&workspace.to_string(), channel)) {
            return;
        }
        if !self.peers.contains_key(&channel)
            && request.verify().is_ok()
            && self
                .directory
                .allowed_devices(workspace, channel, route, wall)
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
            match self.start_peer(workspace, channel, route, now) {
                Ok(true) => {
                    if let Some(entry) = self.peers.get_mut(&channel) {
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
        if let Some(route) = route {
            self.learn_route(workspace, channel, route);
        }
        let (Some(entry), Some(store)) = (self.peers.get_mut(&channel), &self.store) else {
            return;
        };
        if entry.workspace != workspace {
            return;
        }
        match entry.peer.learn_request(request, &store.vault) {
            Ok(true) => entry.dirty = true,
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, %workspace, "channel access request refused"),
        }
    }

    fn reconcile(&mut self, channel: ChannelId, wall: u64) {
        let Some((workspace, route)) = self
            .peers
            .get(&channel)
            .map(|entry| (entry.workspace, entry.route.clone()))
        else {
            return;
        };
        let allowed = self
            .directory
            .allowed_devices(workspace, channel, route.as_ref(), wall);
        let (Some(entry), Some(store)) = (self.peers.get_mut(&channel), &self.store) else {
            return;
        };
        entry.dirty = false;
        // Without the channel's record this device cannot tell who may read
        // it: it commits nothing, and removes no one.
        let Some(allowed) = allowed else {
            return;
        };
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
            let gate = entry.peer.gate();
            self.register_gate(channel, gate);
            self.bump(channel);
        }
    }

    /// Hand the transport every frame this device wrote or must publish
    /// again; kept for the next pass while the transport refuses one.
    fn publish(&mut self, channel: ChannelId, transport: &dyn GovernanceTransport) {
        let (Some(entry), Some(store)) = (self.peers.get_mut(&channel), &self.store) else {
            return;
        };
        if entry.peer.outbox().is_empty() {
            return;
        }
        let mut accepted = true;
        for frame in entry.peer.outbox() {
            self.delivery = self.delivery.wrapping_add(1);
            accepted &= transport.publish(NeighbourMsg::MlsGovernance {
                workspace: entry.workspace.to_string(),
                frame: Box::new(frame.clone()),
                delivery: self.delivery,
            });
        }
        if accepted && let Err(error) = entry.peer.published(&store.vault) {
            tracing::warn!(%error, workspace = %entry.workspace, "channel outbox could not be written");
        }
    }

    /// Ask the participants for access on the request backoff, while waiting.
    fn ask(
        &mut self,
        channel: ChannelId,
        transport: &dyn GovernanceTransport,
        now: Instant,
        wall: u64,
    ) {
        let (Some(entry), Some(store)) = (self.peers.get_mut(&channel), &self.store) else {
            return;
        };
        let asking = match entry.peer.access() {
            Access::Waiting => entry.wanted,
            Access::Removed => entry.rejoin,
            Access::Ready { .. } => false,
        };
        if !asking || !entry.retry.is_due(now) {
            return;
        }
        let fresh = now >= entry.fresh_at;
        if entry.request.is_none() || fresh {
            match entry.peer.access_request_now(wall, fresh, &store.vault) {
                Ok(request) => {
                    entry.request = request;
                    entry.rejoin = false;
                    if fresh {
                        entry.fresh_at = now.checked_add(REQUEST_FRESH).unwrap_or(now);
                    }
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
        // Routed when the record is known, so participants decide Read on it.
        self.delivery = self.delivery.wrapping_add(1);
        let message = match entry.route.clone() {
            Some(route) => NeighbourMsg::MlsRoutedAccessRequest {
                room: entry.workspace.to_string(),
                request: Box::new(request),
                route: Box::new(route),
                delivery: self.delivery,
            },
            None => NeighbourMsg::MlsAccessRequest {
                workspace: entry.workspace.to_string(),
                request: Box::new(request),
                delivery: self.delivery,
            },
        };
        let _ = transport.publish(message);
    }

    /// Start taking part in `channel`: wait for access, and for the group to
    /// be created ([`Self::found`]). `Ok(false)` while the directory cannot
    /// name the channel's readers here: a workspace whose governance is unknown
    /// and which is no personal cluster of this device's.
    fn start_peer(
        &mut self,
        workspace: WorkspaceId,
        channel: ChannelId,
        route: Option<&ChannelRoute>,
        now: Instant,
    ) -> channel_crypto::Result<bool> {
        if !takes_part(&*self.directory, self.device(), workspace, channel, route) {
            return Ok(false);
        }
        if self.open_peer(workspace, channel, now)? {
            return Ok(true);
        }
        let store = self.store.as_ref().ok_or_else(no_store)?;
        let dir = channels_dir(&store.root, workspace);
        create_private_dir(&dir)?;
        let signer = Box::new(store.device_key.clone());
        let peer = ChannelPeer::waiting(Some(&dir), channel, signer, &store.vault)?;
        self.insert_peer(workspace, channel, peer, now);
        Ok(true)
    }

    /// Create `channel`'s group when this device waited its while and no
    /// genesis of the channel arrived, and the policy lets it read the
    /// channel. A device that has seen a genesis never creates one.
    fn found(&mut self, channel: ChannelId, now: Instant, wall: u64) {
        let Some(entry) = self.peers.get(&channel) else {
            return;
        };
        if entry.peer.knows_group() || self.groups_seen.contains(&channel) {
            return;
        }
        let (workspace, route) = (entry.workspace, entry.route.clone());
        let reads = self
            .directory
            .allowed_devices(workspace, channel, route.as_ref(), wall)
            .zip(self.device())
            .is_some_and(|(allowed, device)| allowed.contains(&device));
        let (Some(entry), Some(store)) = (self.peers.get_mut(&channel), &self.store) else {
            return;
        };
        // Looked at again on every pass, a policy re-check's included.
        entry.founding.allowed = reads;
        if !reads || now < entry.founding.at {
            return;
        }
        match entry.peer.found_now(&store.vault) {
            Ok(true) => {
                // The requests it holds are admitted at once.
                entry.dirty = true;
                entry.wanted = true;
                let gate = entry.peer.gate();
                self.register_gate(channel, gate);
                self.bump(channel);
            }
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, %workspace, "channel group could not be created"),
        }
    }

    /// Open the state kept for `channel`. `Ok(false)` when there is none.
    fn open_peer(
        &mut self,
        workspace: WorkspaceId,
        channel: ChannelId,
        now: Instant,
    ) -> channel_crypto::Result<bool> {
        let store = self.store.as_ref().ok_or_else(no_store)?;
        let dir = channels_dir(&store.root, workspace);
        let Some(peer) = ChannelPeer::open(
            &dir,
            channel,
            Box::new(store.device_key.clone()),
            &store.vault,
        )?
        else {
            return Ok(false);
        };
        self.insert_peer(workspace, channel, peer, now);
        Ok(true)
    }

    fn insert_peer(
        &mut self,
        workspace: WorkspaceId,
        channel: ChannelId,
        peer: ChannelPeer,
        now: Instant,
    ) {
        let route = self.store.as_ref().and_then(|store| {
            load_route(
                &route_path(&store.root, workspace, channel),
                workspace,
                channel,
            )
        });
        let gate = peer.gate();
        let found_at = now
            .checked_add(found_after(&peer.device(), channel))
            .unwrap_or(now);
        self.peers.insert(
            channel,
            PeerEntry {
                workspace,
                peer,
                route,
                request: None,
                retry: Backoff::now(now, REQUEST_FIRST),
                fresh_at: now.checked_add(REQUEST_FRESH).unwrap_or(now),
                dirty: true,
                rejoin: false,
                founding: Founding {
                    at: found_at,
                    allowed: true,
                },
                wanted: true,
                held: Vec::new(),
            },
        );
        self.register_gate(channel, gate);
    }

    fn register_gate(&mut self, channel: ChannelId, gate: channel_crypto::ChannelGate) {
        if !gate.encryption_required || self.registered.get(&channel) == Some(&gate) {
            return;
        }
        if let Ok(mut broker) = self.broker.lock() {
            broker.register_channel_gate(channel, gate);
            self.registered.insert(channel, gate);
        }
    }

    /// Record that `channel`'s keys or access changed, and announce it.
    fn bump(&mut self, channel: ChannelId) {
        self.generation = self.generation.wrapping_add(1);
        self.changed.insert(channel, self.generation);
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
    channel: ChannelId,
    route: Option<&ChannelRoute>,
) -> bool {
    directory
        .allowed_devices(workspace, channel, route, tilo_clock::unix_now())
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
    fn run<T>(&self, operation: impl FnOnce(&mut ChannelEncryption) -> T) -> Option<T> {
        let mut service = self.inner.lock().ok()?;
        Some(operation(&mut service))
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

    /// `route`'s channel's keys as this device holds them
    /// ([`ChannelEncryption::record_keys`]), never opening, joining or
    /// creating anything. `None` when this device takes part in no channel
    /// of the route's workspace; [`Err`] when it does and holds none yet.
    pub fn record_keys(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
    ) -> Option<Result<ChannelContentKeys, String>> {
        let channel = route.channel(&workspace.to_string());
        self.run(|service| service.record_keys(workspace, channel))
            .unwrap_or_else(|| Some(Err("the channel service is unavailable".into())))
    }

    /// See [`ChannelEncryption::access`]; also returns the service's
    /// generation, which changes whenever any channel's keys or access do.
    pub fn access(&self, workspace: WorkspaceId, channel: ChannelId) -> (ChannelAccess, u64) {
        let Some((access, generation, queued)) = self.run(|service| {
            let access = service.access(workspace, channel, Instant::now());
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

    /// See [`ChannelEncryption::access_record`]; also returns the service's
    /// generation, as [`Self::access`] does.
    pub fn access_record(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
    ) -> (ChannelAccess, u64) {
        // As `ChannelEncryption::access_record` asks: the route only when it
        // is the channel's.
        let Some((access, generation, queued)) = self.run(|service| {
            let (_, access) = service.access_record(workspace, route, Instant::now());
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
        let (ChannelAccess::Ready(keys), _) = self.access(workspace, metadata.channel) else {
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
        let (ChannelAccess::Ready(keys), _) = self.access(workspace, metadata.channel) else {
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
        let now = Instant::now();
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

    /// See [`ChannelEncryption::channel_change`].
    pub fn channel_change(&self, channel: ChannelId) -> u64 {
        self.inner
            .lock()
            .map_or(0, |service| service.channel_change(channel))
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

    /// [`Self::record_keys`] as a workspace host running on this device asks
    /// for them, by the workspace's id as its records name it: opening the
    /// channel when it has none yet and `create`, and waiting up to `wait` for
    /// the group. `None` when this device takes no part in the workspace's
    /// channels; [`Err`] when it takes part and holds no keys yet.
    pub async fn channel_keys(
        &self,
        workspace: &str,
        route: &ChannelRoute,
        create: bool,
        wait: Duration,
    ) -> Option<Result<ChannelContentKeys, String>> {
        let workspace = WorkspaceId::parse(workspace)?;
        if !create {
            let access = self.get()?;
            // Asked off the caller's task: the service takes its lock and
            // reads its state on that thread, never on the host's.
            let route = route.clone();
            return tokio::task::spawn_blocking(move || access.record_keys(workspace, &route))
                .await
                .unwrap_or_else(|error| Some(Err(error.to_string())));
        }
        self.open_record(workspace, route, wait).await
    }

    /// `route`'s channel's keys, opening the channel when it has none yet
    /// ([`ChannelEncryption::access_record`]), and waiting up to `wait` for
    /// the group when the directory backs this device but no genesis has
    /// arrived or been admitted yet. An endpoint writing a record's first
    /// content asks this: the group is created within a second or two, and
    /// the write lands with its keys rather than failing.
    ///
    /// `None` when this device takes part in no channel of the route's
    /// workspace; [`Err`] when it waited and the keys still are not there.
    pub async fn open_record(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        wait: Duration,
    ) -> Option<Result<ChannelContentKeys, String>> {
        let channel = route.channel(&workspace.to_string());
        let deadline = Instant::now()
            .checked_add(wait)
            .unwrap_or_else(Instant::now);
        loop {
            let access = self.get()?;
            // Read before the ask: a change between the two is no wait at all.
            let after = access.channel_change(channel);
            let (state, _) = access.access_record(workspace, route);
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
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Some(Err("the channel's keys are not ready".into()));
            }
            let changed = tokio::time::timeout(remaining, self.channel_changed(channel, after));
            if changed.await.is_err() {
                return Some(Err("the channel's keys are not ready".into()));
            }
        }
    }
}

/// The identity service's route keys, from this host's channel service once
/// one runs.
impl tilo_identity_host::broker::ChannelKeySource for ChannelKeyProvider {
    fn access(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        known_generation: u64,
    ) -> Option<Result<tilo_identity_host::broker::ChannelKeys, String>> {
        use runtime_identity::wire::ChannelAccess as Wire;
        let channels = self.get()?;
        let (access, generation) = channels.access_record(workspace, route);
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

/// Where the state of `workspace`'s channels is kept.
fn channels_dir(state_root: &Path, workspace: WorkspaceId) -> PathBuf {
    state_root.join(workspace.to_string()).join(CHANNELS_DIR)
}

/// Where the route of `channel`'s record is kept beside its state.
fn route_path(state_root: &Path, workspace: WorkspaceId, channel: ChannelId) -> PathBuf {
    channels_dir(state_root, workspace).join(format!("{}.route", channel.0))
}

/// The route kept at `path`, only if it is still `channel`'s in `workspace`: a
/// route proves itself, so a damaged or swapped file names no record rather
/// than a wrong one, and the channel is then decided as one whose record is
/// unknown, which withholds more, never less.
/// A missing file is a channel with no route kept; one that cannot be read
/// or decoded, or proves another channel, is logged.
fn load_route(path: &Path, workspace: WorkspaceId, channel: ChannelId) -> Option<ChannelRoute> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "channel route cannot be read");
            return None;
        }
    };
    let route = match postcard::from_bytes::<ChannelRoute>(&bytes) {
        Ok(route) => route,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "channel route does not decode");
            return None;
        }
    };
    if !route.proves(&workspace.to_string(), channel) {
        tracing::warn!(path = %path.display(), "channel route names another channel");
        return None;
    }
    Some(route)
}

fn save_route(path: &Path, route: &ChannelRoute) {
    let saved = postcard::to_allocvec(route)
        .map_err(std::io::Error::other)
        .and_then(|bytes| tilo_files::native::atomic::write_private(path, &bytes));
    if let Err(error) = saved {
        tracing::warn!(%error, "channel route could not be saved");
    }
}

/// Channel ids of the `*.{extension}` files in `dir`. A missing directory
/// holds none; a file whose name is no channel id is logged and skipped.
fn channels_in(dir: &Path, extension: &str) -> Vec<ChannelId> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            tracing::warn!(dir = %dir.display(), %error, "channel state cannot be listed");
            return Vec::new();
        }
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some(extension) {
                return None;
            }
            let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
            match uuid::Uuid::parse_str(stem) {
                Ok(channel) => Some(ChannelId(channel)),
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "channel state file names no channel");
                    None
                }
            }
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

/// Fixtures other hosts' channel tests share: an in-memory workspace transport
/// and a fixed access policy.
#[cfg(any(test, feature = "test-support"))]
#[allow(
    clippy::wildcard_imports,
    clippy::missing_panics_doc,
    clippy::unwrap_used,
    reason = "test fixtures: a poisoned lock is the test failing"
)]
pub mod testing;

mod history;

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use collab_types::{ArtifactId, ContentRetention};
    use std::sync::atomic::Ordering;

    struct Device {
        service: ChannelEncryption,
        broker: Arc<Mutex<BrokerChannels>>,
        endpoint: Endpoint,
    }

    /// Device `seed` of workspace `workspace_id`, with its state under `dir`
    /// and transport endpoint `id` of `workspace`.
    fn device(
        workspace: &Arc<Workspace>,
        workspace_id: WorkspaceId,
        id: usize,
        seed: u8,
        policy: &Policy,
        dir: &Path,
    ) -> Device {
        workspace.queues.lock().unwrap().entry(id).or_default();
        let broker = Arc::new(Mutex::new(BrokerChannels::default()));
        let service = ChannelEncryption::open(
            ChannelEncryptionConfig {
                root: dir.join("channels"),
                vault: VaultKey::new([seed; 32]),
                device_key: key(seed),
            },
            Box::new(Directory {
                policy: policy.clone(),
            }),
            Arc::clone(&broker),
        )
        .unwrap();
        Device {
            service,
            broker,
            endpoint: Endpoint {
                workspace: Arc::clone(workspace),
                id,
                workspace_id,
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

    /// `device` opens `channel` and creates its group, as it does once it
    /// waited its while and no genesis arrived.
    fn create(
        device: &mut Device,
        workspace_id: WorkspaceId,
        channel: ChannelId,
        now: Instant,
    ) -> ChannelAccess {
        device.service.access(workspace_id, channel, now);
        if let Some(entry) = device.service.peers.get_mut(&channel) {
            entry.founding.at = now;
        }
        device.service.found(channel, now, 1_000);
        device.service.access(workspace_id, channel, now)
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
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(public(2));
        policy.allowed.lock().unwrap().insert(public(1));
        let (owner_dir, member_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
        let mut member = device(&workspace, workspace_id, 1, 2, &policy, member_dir.path());
        for service in [&owner.service, &member.service] {
            let generation = service.generation();
            assert!(service.existing_keys(workspace_id, channel).is_none());
            assert!(service.peers.is_empty());
            assert_eq!(service.generation(), generation);
        }

        // Only this explicit fixture admission creates or requests keys.
        let mut now = Instant::now();
        owner.service.access(workspace_id, channel, now);
        member.service.access(workspace_id, channel, now);
        run(&mut [&mut owner, &mut member], &mut now, 5);
        assert!(
            member
                .service
                .existing_keys(workspace_id, channel)
                .is_some()
        );
        assert!(
            member
                .service
                .existing_keys(WorkspaceId::new(), channel)
                .is_none()
        );
        assert!(
            owner
                .service
                .existing_keys(WorkspaceId::new(), channel)
                .is_none()
        );

        policy.allowed.lock().unwrap().remove(&public(2));
        run(&mut [&mut owner, &mut member], &mut now, 3);
        let generation = member.service.generation();
        let rejoin = member.service.peers.get(&channel).unwrap().rejoin;
        assert!(!rejoin);
        assert!(
            member
                .service
                .existing_keys(workspace_id, channel)
                .is_none()
        );
        assert_eq!(member.service.generation(), generation);
        assert!(!member.service.peers.get(&channel).unwrap().rejoin);
    }

    #[test]
    fn opening_a_channel_wakes_a_parked_pump_and_reading_it_does_not() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let dir = tempfile::tempdir().unwrap();
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&wakes);
        let owner = device(
            &workspace,
            workspace_id,
            0,
            1,
            &Policy::readers([1, 2]),
            dir.path(),
        );
        let access = ChannelKeyAccess::with_wake(
            owner.service,
            Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let channel = ChannelId::new();
        access.access(workspace_id, channel);
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "a new group has a genesis to announce"
        );
        access.access(workspace_id, channel);
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "an open channel queues nothing new"
        );

        let member_dir = tempfile::tempdir().unwrap();
        let member = device(
            &workspace,
            workspace_id,
            1,
            2,
            &Policy::readers([1, 2]),
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
            member.access(workspace_id, channel).0,
            ChannelAccess::Waiting
        ));
        assert_eq!(
            member_wakes.load(Ordering::SeqCst),
            1,
            "a new member has an access request to make"
        );
        member.access(workspace_id, channel);
        assert_eq!(member_wakes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_new_channel_is_encrypted_from_creation_and_follows_the_policy() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let policy = Policy::readers([1]);
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
        let mut laptop = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());

        assert!(ready_epoch(&create(&mut owner, workspace_id, channel, now)).is_some());
        assert!(
            refuses_plaintext(&owner, channel),
            "encrypted from creation"
        );
        assert!(matches!(
            laptop.service.access(workspace_id, channel, now),
            ChannelAccess::Unavailable
        ));

        // Not named yet: nobody can admit it, and its route hears when that
        // changes.
        run(&mut [&mut owner, &mut laptop], &mut now, 5);
        assert!(matches!(
            laptop.service.access(workspace_id, channel, now),
            ChannelAccess::Unavailable
        ));

        policy.allowed.lock().unwrap().insert(public(2));
        run(&mut [&mut owner, &mut laptop], &mut now, 5);
        assert!(matches!(
            laptop.service.access(workspace_id, channel, now),
            ChannelAccess::Waiting
        ));
        run(&mut [&mut owner, &mut laptop], &mut now, 8);
        let owner_epoch = ready_epoch(&owner.service.access(workspace_id, channel, now));
        assert_eq!(
            ready_epoch(&laptop.service.access(workspace_id, channel, now)),
            owner_epoch
        );
        assert!(
            refuses_plaintext(&laptop, channel),
            "gate: {:?} peer: {:?}",
            laptop.service.registered.get(&channel),
            laptop
                .service
                .peers
                .get(&channel)
                .map(|e| (e.peer.access(), e.peer.gate()))
        );

        policy.allowed.lock().unwrap().remove(&public(2));
        run(&mut [&mut owner, &mut laptop], &mut now, 3);
        assert!(matches!(
            laptop.service.access(workspace_id, channel, now),
            ChannelAccess::Removed
        ));
    }

    /// A participant that cannot name a channel's record (it lost the route,
    /// and no grant names the record) cannot tell who may read it: it removes
    /// no one, though the policy it can read names no one. Named
    /// again, the group follows the policy once more.
    #[test]
    fn a_participant_that_cannot_name_the_record_removes_no_one() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let route = ChannelRoute::new("notes", "plan");
        let channel = route.channel(&workspace_id.to_string());
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(public(2));
        policy.allowed.lock().unwrap().insert(public(1));
        let dirs = [(); 2].map(|()| tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&workspace, workspace_id, 0, 1, &policy, dirs[0].path());
        let mut laptop = device(&workspace, workspace_id, 1, 2, &policy, dirs[1].path());
        for device in [&mut owner, &mut laptop] {
            device.service.access(workspace_id, channel, now);
        }
        run(&mut [&mut owner, &mut laptop], &mut now, 8);
        let epoch = ready_epoch(&laptop.service.access(workspace_id, channel, now));
        assert!(epoch.is_some());
        assert_eq!(
            ready_epoch(&owner.service.access(workspace_id, channel, now)),
            epoch
        );

        // Everyone loses the route; the record is no longer named.
        policy.unnamed.lock().unwrap().insert(channel);
        run(&mut [&mut owner, &mut laptop], &mut now, 5);
        assert_eq!(
            ready_epoch(&owner.service.access(workspace_id, channel, now)),
            epoch
        );
        assert_eq!(
            ready_epoch(&laptop.service.access(workspace_id, channel, now)),
            epoch
        );

        // Named again, the group follows the policy: the laptop is removed.
        owner.service.learn_route(workspace_id, channel, &route);
        policy.allowed.lock().unwrap().remove(&public(2));
        run(&mut [&mut owner, &mut laptop], &mut now, 3);
        assert!(matches!(
            laptop.service.access(workspace_id, channel, now),
            ChannelAccess::Removed
        ));
        assert!(ready_epoch(&owner.service.access(workspace_id, channel, now)) > epoch);
    }

    /// A Read Deny on one record takes the denied device out of that
    /// record's channel alone: a participant removes it in a new epoch there,
    /// the workspace's other channels keep their epoch and the device keeps their
    /// keys, and a request of the denied device never makes a participant
    /// create a group for the denied record.
    #[test]
    fn a_device_withheld_from_one_channel_loses_only_that_channels_key() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let (payroll, notes, budget) = (ChannelId::new(), ChannelId::new(), ChannelId::new());
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(public(2));
        policy.allowed.lock().unwrap().insert(public(1));
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
        let mut laptop = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());
        for channel in [payroll, notes] {
            owner.service.access(workspace_id, channel, now);
            laptop.service.access(workspace_id, channel, now);
        }
        run(&mut [&mut owner, &mut laptop], &mut now, 5);
        let notes_epoch = ready_epoch(&owner.service.access(workspace_id, notes, now));
        let payroll_epoch = ready_epoch(&owner.service.access(workspace_id, payroll, now));
        assert!(ready_epoch(&laptop.service.access(workspace_id, payroll, now)).is_some());
        assert_eq!(
            ready_epoch(&laptop.service.access(workspace_id, notes, now)),
            notes_epoch
        );

        policy
            .withheld
            .lock()
            .unwrap()
            .extend([payroll, budget].map(|channel| (channel, BTreeSet::from([public(2)]))));
        run(&mut [&mut owner, &mut laptop], &mut now, 3);
        assert!(matches!(
            laptop.service.access(workspace_id, payroll, now),
            ChannelAccess::Removed
        ));
        let rotated = ready_epoch(&owner.service.access(workspace_id, payroll, now));
        assert!(rotated > payroll_epoch, "the denied record's key rotates");
        assert_eq!(
            ready_epoch(&owner.service.access(workspace_id, notes, now)),
            notes_epoch,
            "another record's channel keeps its epoch"
        );
        assert_eq!(
            ready_epoch(&laptop.service.access(workspace_id, notes, now)),
            notes_epoch
        );
        // Asking again for the denied record, or first for another denied
        // one, admits nothing and creates no group at the owner.
        laptop.service.access(workspace_id, budget, now);
        run(&mut [&mut owner, &mut laptop], &mut now, 5);
        for channel in [payroll, budget] {
            assert!(ready_epoch(&laptop.service.access(workspace_id, channel, now)).is_none());
        }
        assert!(!owner.service.peers.contains_key(&budget));
        assert!(
            !owner.service.peers[&payroll]
                .peer
                .members()
                .contains(&public(2))
        );

        policy.withheld.lock().unwrap().clear();
        run(&mut [&mut owner, &mut laptop], &mut now, 5);
        assert_eq!(
            ready_epoch(&laptop.service.access(workspace_id, payroll, now)),
            ready_epoch(&owner.service.access(workspace_id, payroll, now))
        );
    }

    /// A Read Deny on an app takes the denied device out of every channel
    /// of that app whose route a participant knows (from its own endpoint or
    /// from the device's routed request) in a new epoch of each, and out of
    /// no other app's channel. A record of the app first asked for after the
    /// Deny, and a channel asked for without a route, admit nothing and
    /// create no group. A route that is not its channel's is refused. The
    /// owner keeps the routes: opened again, it still withholds the app's
    /// channels and admits the device to nothing of it.
    #[test]
    fn a_read_deny_on_an_app_withholds_every_routed_channel_of_it() {
        let workspace = Arc::new(Workspace::default());
        let id = WorkspaceId::new();
        let workspace_name = id.to_string();
        let routes = [
            ChannelRoute::new("finance", "payroll"),
            ChannelRoute::new("finance", "q3"),
            ChannelRoute::new("notes", "draft"),
        ];
        let [payroll, q3, draft] = routes.clone().map(|route| route.channel(&workspace_name));
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(public(2));
        policy.allowed.lock().unwrap().insert(public(1));
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&workspace, id, 0, 1, &policy, owner_dir.path());
        let mut laptop = device(&workspace, id, 1, 2, &policy, laptop_dir.path());
        // The owner's own endpoint opens payroll; q3 and draft the owner
        // learns from the laptop's routed requests alone.
        owner.service.access_record(id, &routes[0], now);
        for route in &routes {
            laptop.service.access_record(id, route, now);
        }
        run(&mut [&mut owner, &mut laptop], &mut now, 5);
        // Whichever of the two created q3's and draft's groups, the owner
        // takes part in them too.
        for channel in [q3, draft] {
            owner.service.access(id, channel, now);
        }
        run(&mut [&mut owner, &mut laptop], &mut now, 8);
        let epoch = |device: &mut Device, channel, now| {
            ready_epoch(&device.service.access(id, channel, now))
        };
        let before: Vec<Option<u64>> = [payroll, q3, draft]
            .into_iter()
            .map(|channel| epoch(&mut owner, channel, now))
            .collect();
        for channel in [payroll, q3, draft] {
            assert!(epoch(&mut laptop, channel, now).is_some());
            assert!(owner.service.peers[&channel].route.is_some());
        }

        policy
            .withheld_apps
            .lock()
            .unwrap()
            .insert("finance".into(), BTreeSet::from([public(2)]));
        run(&mut [&mut owner, &mut laptop], &mut now, 3);
        for (index, channel) in [payroll, q3].into_iter().enumerate() {
            assert!(matches!(
                laptop.service.access(id, channel, now),
                ChannelAccess::Removed
            ));
            assert!(
                epoch(&mut owner, channel, now) > before[index],
                "the denied app's key rotates"
            );
        }
        assert_eq!(epoch(&mut owner, draft, now), before[2]);
        assert_eq!(epoch(&mut laptop, draft, now), before[2]);

        // A new finance record, a channel asked for with no route, and one
        // whose route is a forgery admit nothing and create no group.
        let budget = ChannelRoute::new("finance", "budget");
        laptop.service.access_record(id, &budget, now);
        let unrouted = ChannelRoute::new("notes", "plan").channel(&workspace_name);
        laptop.service.access(id, unrouted, now);
        let forged = ChannelRoute::new("notes", "forged").channel(&workspace_name);
        let request = member_request(&laptop_dir.path().join("forged"), forged, 2);
        owner
            .service
            .receive_request(id, request, Some(&routes[2]), now, 0);
        run(&mut [&mut owner, &mut laptop], &mut now, 5);
        for channel in [budget.channel(&workspace_name), unrouted, forged] {
            assert!(!owner.service.peers.contains_key(&channel));
            assert!(epoch(&mut laptop, channel, now).is_none());
        }

        // Opened again, the owner still knows every route and admits the
        // device to nothing of the app, however often it asks.
        drop(owner);
        let mut owner = device(&workspace, id, 0, 1, &policy, owner_dir.path());
        for channel in [payroll, q3, draft] {
            assert!(owner.service.peers[&channel].route.is_some());
        }
        for route in &routes {
            laptop.service.access_record(id, route, now);
        }
        run(&mut [&mut owner, &mut laptop], &mut now, 5);
        for channel in [payroll, q3] {
            assert!(epoch(&mut laptop, channel, now).is_none());
            assert!(
                !owner.service.peers[&channel]
                    .peer
                    .members()
                    .contains(&public(2))
            );
        }
        assert!(epoch(&mut laptop, draft, now).is_some());

        policy.withheld_apps.lock().unwrap().clear();
        run(&mut [&mut owner, &mut laptop], &mut now, 5);
        for channel in [payroll, q3] {
            assert_eq!(
                epoch(&mut laptop, channel, now),
                epoch(&mut owner, channel, now)
            );
        }
    }

    /// The access request device `seed` makes for `channel`, with its state
    /// under `dir`.
    fn member_request(dir: &Path, channel: ChannelId, seed: u8) -> MlsAccessRequestV1 {
        std::fs::create_dir_all(dir).unwrap();
        ChannelPeer::waiting(
            Some(dir),
            channel,
            Box::new(key(seed)),
            &VaultKey::new([2; 32]),
        )
        .unwrap()
        .access_request_now(1, false, &VaultKey::new([2; 32]))
        .unwrap()
        .unwrap()
    }

    /// Run every device's protocol for `seconds` of simulated time the way
    /// the broker's drain thread does: a device runs a pass only
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
                    .workspace
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

    /// #3998: a device in a channel's group re-checks its policy once a
    /// reconcile interval, whoever created the group; a device with no
    /// channel has nothing to pump.
    #[test]
    fn only_a_group_member_or_an_unfinished_member_has_a_pump_deadline() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(public(2));
        let (owner_dir, member_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
        let mut member = device(&workspace, workspace_id, 1, 2, &policy, member_dir.path());
        assert_eq!(owner.service.next_pump_deadline(now), None, "no channel");
        assert_eq!(member.service.next_pump_deadline(now), None, "no channel");

        owner.service.access(workspace_id, channel, now);
        member.service.access(workspace_id, channel, now);
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
        assert!(ready_epoch(&member.service.access(workspace_id, channel, now)).is_some());
        for device in [&owner, &member] {
            let reconcile = device.service.next_pump_deadline(now).unwrap();
            assert!(reconcile > now.checked_sub(RECONCILE_INTERVAL).unwrap());
            assert!(reconcile <= now.checked_add(RECONCILE_INTERVAL).unwrap());
        }
        assert_eq!(
            wait_after_pass(Some(now), now),
            Some(RECONCILE_INTERVAL),
            "work a pass left due is retried at the old cadence, not spun on"
        );
        assert_eq!(wait_after_pass(None, now), None);
    }

    /// #3998: a device waiting to be admitted to a group it did not found
    /// asks again on its backoff, and is due exactly then.
    #[test]
    fn a_waiting_member_is_due_on_its_request_backoff() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let dir = tempfile::tempdir().unwrap();
        // The directory announces its policy changes, so nothing but the
        // request backoff is ever due here.
        let policy = Policy::readers([1, 2]);
        *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::new(
            std::sync::atomic::AtomicU64::new(1),
        )));
        let mut member = device(&workspace, workspace_id, 1, 2, &policy, dir.path());
        // The channel's group exists: this device saw its genesis, so it
        // waits to be admitted instead of founding one of its own.
        member.service.groups_seen.insert(channel);
        let now = Instant::now();
        assert!(matches!(
            member.service.access(workspace_id, channel, now),
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

    /// A peer another member's access request opened answers that member
    /// and asks nothing itself. The first read that wants the channel asks
    /// for it: the read queues the pass, so a parked pump sends the request
    /// instead of sleeping with the keys never asked for.
    #[test]
    fn a_read_that_first_wants_a_channel_queues_the_pass_that_asks() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let policy = Policy::readers([1, 2]);
        let dir = tempfile::tempdir().unwrap();
        let mut member = device(&workspace, workspace_id, 1, 2, &policy, dir.path());
        // The group exists; this device only learns of it on admission.
        member.service.groups_seen.insert(channel);
        let now = Instant::now();
        let request = member_request(&dir.path().join("asking"), channel, 1);
        member
            .service
            .receive_request(workspace_id, request, None, now, 1_000);
        {
            let entry = member
                .service
                .peers
                .get_mut(&channel)
                .expect("another member's request opened the channel");
            assert!(!entry.wanted, "answering a request wants nothing here");
            // The pass that took the request in has run: nothing is owed.
            entry.dirty = false;
        }
        // The pass that took the request in: it answers, and settles nothing
        // of its own.
        member.service.pump(&member.endpoint, now, 1_000);
        member.service.queued = false;
        assert_eq!(
            member.service.next_pump_deadline(now),
            Some(now.checked_add(RECONCILE_INTERVAL).unwrap()),
            "a pass that answered only another member's request has no work of its own, only the \
             re-check of a channel whose directory announces nothing"
        );
        let woke = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&woke);
        let keys = ChannelKeyAccess::with_wake(
            member.service,
            Arc::new(move || flag.store(true, Ordering::Relaxed)),
        );
        assert!(matches!(
            keys.access(workspace_id, channel).0,
            ChannelAccess::Waiting
        ));
        assert!(
            woke.load(Ordering::Relaxed),
            "reading the channel wakes the pass that asks for it"
        );
    }

    /// #3998: devices that park between deadlines reach the same outcome
    /// in the same simulated time as devices pumped on a fixed tick:
    /// admission, then removal once the policy drops the member.
    #[test]
    fn parked_pumps_follow_the_policy_like_a_fixed_tick() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let policy = Policy::readers([1]);
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
        let mut laptop = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());

        assert!(ready_epoch(&create(&mut owner, workspace_id, channel, now)).is_some());
        assert!(matches!(
            laptop.service.access(workspace_id, channel, now),
            ChannelAccess::Unavailable
        ));
        let mut parked = [Some(now), Some(now)];
        run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 5);
        assert!(matches!(
            laptop.service.access(workspace_id, channel, now),
            ChannelAccess::Unavailable
        ));

        policy.allowed.lock().unwrap().insert(public(2));
        run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 5);
        assert!(matches!(
            laptop.service.access(workspace_id, channel, now),
            ChannelAccess::Waiting
        ));
        parked[1] = Some(now); // what the endpoint's access does to its pump
        run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 5);
        let owner_epoch = ready_epoch(&owner.service.access(workspace_id, channel, now));
        assert_eq!(
            ready_epoch(&laptop.service.access(workspace_id, channel, now)),
            owner_epoch
        );
        assert!(
            parked[1].is_some(),
            "the admitted member re-checks the policy as every group member does"
        );

        // Ten idle seconds cost each group member one policy re-check a
        // reconcile interval.
        let idle = run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 10);
        assert!(idle <= 22, "{idle} passes in ten idle seconds");

        policy.allowed.lock().unwrap().remove(&public(2));
        run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 3);
        assert!(matches!(
            laptop.service.access(workspace_id, channel, now),
            ChannelAccess::Removed
        ));
    }

    /// #4022: with a directory that announces its changes, an idle group
    /// has no deadline at all, and a revocation is enforced as soon as it is
    /// announced, with no clock having run in between.
    #[test]
    fn an_announced_revocation_is_enforced_at_once_and_idle_groups_park() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let policy = Policy::default();
        *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::clone(&epoch)));
        policy.allowed.lock().unwrap().insert(public(2));
        policy.allowed.lock().unwrap().insert(public(1));
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
        let mut laptop = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());
        assert!(ready_epoch(&create(&mut owner, workspace_id, channel, now)).is_some());
        assert!(matches!(
            laptop.service.access(workspace_id, channel, now),
            ChannelAccess::Waiting
        ));
        let mut parked = [Some(now), Some(now)];
        run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 5);
        assert!(ready_epoch(&laptop.service.access(workspace_id, channel, now)).is_some());
        assert_eq!(parked, [None, None], "both park once the member holds keys");

        // A minute with nothing announced runs no pass at all.
        let idle = run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 60);
        assert_eq!(idle, 0, "an idle group polls nothing");
        assert!(owner.service.waits_for_policy_change());

        // The revocation, announced. The owner is due at once, and one
        // pass removes the member.
        policy.allowed.lock().unwrap().remove(&public(2));
        epoch.fetch_add(1, Ordering::SeqCst);
        assert!(!owner.service.waits_for_policy_change());
        assert_eq!(owner.service.next_pump_deadline(now), Some(now));
        parked[0] = Some(now); // what the armed watch's wake does
        run_parked(&mut [&mut owner, &mut laptop], &mut parked, &mut now, 1);
        assert!(matches!(
            laptop.service.access(workspace_id, channel, now),
            ChannelAccess::Removed
        ));
    }

    /// #4022: announcements in quick succession cost at most one re-check
    /// a reconcile interval, and none waits longer than that.
    #[test]
    fn announcements_in_quick_succession_coalesce() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(public(1));
        *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::clone(&epoch)));
        let dir = tempfile::tempdir().unwrap();
        let mut now = Instant::now();
        let mut owner = device(&workspace, workspace_id, 0, 1, &policy, dir.path());
        assert!(ready_epoch(&create(&mut owner, workspace_id, channel, now)).is_some());
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

    /// #4022: a grant that expires while nothing else happens is enforced
    /// when it expires: the owner registers the expiry to be announced
    /// at, parks with no timer of its own, and the announcement at the
    /// expiry wakes the one pass that removes the member. The test thread's
    /// wall clock is held, so the expiry is reached when the test moves it
    /// there; no other test sees that time.
    #[test]
    fn a_grant_expiring_while_idle_is_enforced_at_its_expiry() {
        let start = tilo_clock::unix_now();
        let at = |second: u64| std::time::UNIX_EPOCH + Duration::from_secs(second);
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let policy = Policy::default();
        *policy.announces.lock().unwrap() = Some(Announcements::Process);
        policy.allowed.lock().unwrap().insert(public(2));
        policy.allowed.lock().unwrap().insert(public(1));
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
        let laptop = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());
        create(&mut owner, workspace_id, channel, Instant::now());
        let (owner_endpoint, laptop_endpoint) = (owner.endpoint, laptop.endpoint);
        let owner = ChannelKeyAccess::new(owner.service);
        let laptop = ChannelKeyAccess::new(laptop.service);
        // Match the drainer's Notify: concurrent announcements leave one
        // pending wake, not a queue of already-reconciled epochs to pump again.
        let (woke, wakes) = std::sync::mpsc::sync_channel(1);
        let watch = tilo_identity_host::policy_change::Watch::new(move || {
            let _ = woke.try_send(());
        });
        let parked = |owner: &ChannelKeyAccess| {
            tilo_test_env::wait_until("the owner parked", || {
                match owner.pump_until_due(&owner_endpoint) {
                    Some(wait) => {
                        let _ = wakes.recv_timeout(wait);
                        false
                    }
                    None => owner.arm_policy_watch(&watch),
                }
            });
        };
        let expiry = start.saturating_add(3);

        // This thread pumps both devices, so the time it reads is the time
        // the owner judges the grant by.
        tilo_clock::testing::with_wall(at(start), || {
            assert!(ready_epoch(&owner.access(workspace_id, channel).0).is_some());
            tilo_test_env::wait_until("the laptop joined", || {
                let _ = owner.pump_until_due(&owner_endpoint);
                let _ = laptop.pump_until_due(&laptop_endpoint);
                ready_epoch(&laptop.access(workspace_id, channel).0).is_some()
            });

            // Replace the admitted member's permission with an expiring
            // grant, announced through the production watch, and let the
            // owner register the expiry and park.
            policy.until.lock().unwrap().insert(public(2), expiry);
            policy.allowed.lock().unwrap().remove(&public(2));
            tilo_identity_host::policy_change::announce();
            parked(&owner);
            assert_eq!(
                policy.announce_at.lock().unwrap().last(),
                Some(&expiry),
                "the owner registered the expiry"
            );
            let _ = laptop.pump_until_due(&laptop_endpoint);
            assert!(ready_epoch(&laptop.access(workspace_id, channel).0).is_some());
        });

        // Short of the expiry, a pass (another announcement) keeps the member.
        tilo_clock::testing::with_wall(at(expiry - 1), || {
            tilo_identity_host::policy_change::announce();
            assert!(wakes.try_recv().is_ok(), "the armed watch woke");
            parked(&owner);
            let _ = laptop.pump_until_due(&laptop_endpoint);
            assert!(
                ready_epoch(&laptop.access(workspace_id, channel).0).is_some(),
                "removed before the grant ended"
            );
        });

        // The expiry: its announcement wakes the parked owner, whose
        // re-check (at most a reconcile interval after the last) removes the
        // member.
        tilo_clock::testing::with_wall(at(expiry), || {
            tilo_identity_host::policy_change::announce();
            assert!(wakes.try_recv().is_ok(), "the expiry woke the owner");
            tilo_test_env::wait_until("the member applied the removal", || {
                let _ = owner.pump_until_due(&owner_endpoint);
                let _ = laptop.pump_until_due(&laptop_endpoint);
                matches!(
                    laptop.access(workspace_id, channel).0,
                    ChannelAccess::Removed
                )
            });
        });
    }

    /// #3964: an app endpoint waiting on its route's channel hears of its
    /// admission and its removal the moment this device's service records
    /// them, and of nothing that happens to another channel.
    #[test]
    fn a_route_hears_of_its_own_channel_changes_and_only_those() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let (channel, other) = (ChannelId::new(), ChannelId::new());
        let policy = Policy::readers([1]);
        let announced = Arc::new(std::sync::atomic::AtomicU64::new(0));
        *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::clone(&announced)));
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&workspace, workspace_id, 0, 1, &policy, owner_dir.path());
        let Device {
            service, endpoint, ..
        } = device(&workspace, workspace_id, 1, 2, &policy, laptop_dir.path());
        let keys = ChannelKeyProvider::default();
        keys.set(Some(ChannelKeyAccess::new(service)));
        let laptop = keys.get().unwrap();
        assert!(ready_epoch(&create(&mut owner, workspace_id, channel, now)).is_some());
        assert!(matches!(
            laptop.access(workspace_id, channel).0,
            ChannelAccess::Unavailable
        ));
        let mut pump = |rounds: usize| {
            for _ in 0..rounds {
                now = now.checked_add(Duration::from_millis(250)).unwrap();
                owner.service.pump(&owner.endpoint, now, 1_000);
                laptop.pump_at(&endpoint, now);
            }
        };
        // A wait, polled by hand: parked on the service when this returns,
        // and woken (its waker called) only by a change it has to look at.
        let wait = |after: u64| {
            let keys = keys.clone();
            let mut waiting =
                Waiting::new(async move { keys.channel_changed(channel, after).await });
            assert_eq!(waiting.poll(), None, "nothing changed since {after}");
            waiting
        };

        let before = laptop.channel_change(channel);
        let mut waiting = wait(before);
        pump(8);
        assert_eq!(waiting.poll(), None, "not admitted, nothing changed");

        policy.allowed.lock().unwrap().insert(public(2));
        announced.fetch_add(1, Ordering::SeqCst);
        pump(8);
        assert!(waiting.woken(), "admission wakes the wait");
        let admitted = waiting.poll().expect("admission is announced");
        assert_ne!(admitted, before);
        assert!(matches!(
            laptop.access(workspace_id, channel).0,
            ChannelAccess::Waiting
        ));
        pump(8);
        assert!(ready_epoch(&laptop.access(workspace_id, channel).0).is_some());

        let mut waiting = wait(laptop.channel_change(channel));
        assert!(matches!(
            laptop.access(workspace_id, other).0,
            ChannelAccess::Waiting
        ));
        pump(20);
        assert!(ready_epoch(&laptop.access(workspace_id, other).0).is_some());
        assert_ne!(laptop.channel_change(other), 0, "the other channel changed");
        if let Some(change) = waiting.poll() {
            panic!("woken by another channel's change ({change})");
        }

        policy.allowed.lock().unwrap().remove(&public(2));
        announced.fetch_add(1, Ordering::SeqCst);
        pump(8);
        assert!(waiting.woken(), "removal wakes the wait");
        assert!(waiting.poll().is_some(), "removal is announced");
        assert!(matches!(
            laptop.access(workspace_id, channel).0,
            ChannelAccess::Removed
        ));
    }

    /// A future polled by hand, which counts the wakes it asks for.
    struct Waiting {
        future: std::pin::Pin<Box<dyn std::future::Future<Output = u64>>>,
        wakes: Arc<WakeCount>,
    }

    #[derive(Default)]
    struct WakeCount(std::sync::atomic::AtomicUsize);

    impl std::task::Wake for WakeCount {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Waiting {
        fn new(future: impl std::future::Future<Output = u64> + 'static) -> Self {
            Self {
                future: Box::pin(future),
                wakes: Arc::default(),
            }
        }

        /// Whether the future asked to be polled since it was last polled.
        fn woken(&self) -> bool {
            self.wakes.0.load(Ordering::SeqCst) > 0
        }

        fn poll(&mut self) -> Option<u64> {
            self.wakes.0.store(0, Ordering::SeqCst);
            let waker = std::task::Waker::from(Arc::clone(&self.wakes));
            match self
                .future
                .as_mut()
                .poll(&mut std::task::Context::from_waker(&waker))
            {
                std::task::Poll::Ready(change) => Some(change),
                std::task::Poll::Pending => None,
            }
        }
    }

    /// A device that is not a member yet (a browser, which learns nothing
    /// about the workspace's governance but what the host carries) learns from
    /// the workspace that the channel exists and is encrypted.
    #[test]
    fn a_new_channel_announces_its_genesis_to_the_workspace() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(public(5));
        let owner_dir = tempfile::tempdir().unwrap();
        let mut now = Instant::now();
        let mut owner = device(&workspace, workspace_id, 0, 5, &policy, owner_dir.path());
        let observer = Endpoint {
            workspace: Arc::clone(&workspace),
            id: 1,
            workspace_id,
        };
        workspace.queues.lock().unwrap().entry(1).or_default();
        assert!(ready_epoch(&create(&mut owner, workspace_id, channel, now)).is_some());
        run(&mut [&mut owner], &mut now, 3);
        let genesis: Vec<_> = observer
            .take_inbound()
            .into_iter()
            .filter_map(|inbound| match inbound {
                InboundChannelGovernance::Frame { frame, .. } => Some(frame),
                InboundChannelGovernance::AccessRequest { .. } => None,
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
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(public(4));
        policy.allowed.lock().unwrap().insert(public(3));
        let (owner_dir, phone_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&workspace, workspace_id, 0, 3, &policy, owner_dir.path());
        let mut phone = device(&workspace, workspace_id, 1, 4, &policy, phone_dir.path());
        create(&mut owner, workspace_id, channel, now);
        phone.service.access(workspace_id, channel, now);
        // The request gets through, then the network fails before the
        // genesis and the Welcome can. The phone, hearing of no group,
        // creates its own.
        run(&mut [&mut phone], &mut now, 1);
        workspace.down.store(true, Ordering::SeqCst);
        run(&mut [&mut owner, &mut phone], &mut now, 30);
        assert!(!owner.service.peers[&channel].peer.outbox().is_empty());
        workspace.down.store(false, Ordering::SeqCst);
        run(&mut [&mut owner, &mut phone], &mut now, 130);
        let epoch = ready_epoch(&phone.service.access(workspace_id, channel, now));
        assert!(epoch.is_some());
        assert_eq!(
            ready_epoch(&owner.service.access(workspace_id, channel, now)),
            epoch
        );
        assert_eq!(
            owner.service.peers[&channel].peer.members(),
            phone.service.peers[&channel].peer.members(),
            "one group"
        );
        assert!(
            owner.service.peers[&channel].peer.outbox().is_empty(),
            "the outbox drains once the transport takes it"
        );
    }

    #[test]
    fn channel_state_and_gates_survive_a_restart() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(public(6));
        policy.allowed.lock().unwrap().insert(public(5));
        let (owner_dir, tablet_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        {
            let mut owner = device(&workspace, workspace_id, 0, 5, &policy, owner_dir.path());
            let mut tablet = device(&workspace, workspace_id, 1, 6, &policy, tablet_dir.path());
            owner.service.access(workspace_id, channel, now);
            tablet.service.access(workspace_id, channel, now);
            run(&mut [&mut owner, &mut tablet], &mut now, 5);
            assert!(ready_epoch(&tablet.service.access(workspace_id, channel, now)).is_some());
        }
        for (id, seed, dir) in [(0, 5, owner_dir.path()), (1, 6, tablet_dir.path())] {
            let mut restarted = device(&workspace, workspace_id, id, seed, &policy, dir);
            assert!(
                refuses_plaintext(&restarted, channel),
                "the plaintext gate holds from the first op after a restart"
            );
            assert!(ready_epoch(&restarted.service.access(workspace_id, channel, now)).is_some());
        }
    }

    /// Key loss: a laptop is lost with all its channel state. Restoring the
    /// person's identity backup on a replacement machine brings back the
    /// root identity, not the lost device key; onboarding certifies a new
    /// device key. Once the workspace's directory names that certificate, the
    /// policy allows the new device, a participant admits it, and its key
    /// history opens everything written before the loss. The lost device is
    /// removed, so it cannot read what is written afterwards.
    #[test]
    fn a_restored_identity_regains_the_whole_channel_on_its_new_device() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let artifact = collab_types::ArtifactId::new();
        let policy = Policy::readers([7]);
        policy.allowed.lock().unwrap().insert(public(8));
        let (owner_dir, lost_dir, new_dir) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let mut now = Instant::now();
        let mut owner = device(&workspace, workspace_id, 0, 7, &policy, owner_dir.path());
        let mut lost = device(&workspace, workspace_id, 1, 8, &policy, lost_dir.path());
        owner.service.access(workspace_id, channel, now);
        lost.service.access(workspace_id, channel, now);
        run(&mut [&mut owner, &mut lost], &mut now, 5);
        let ChannelAccess::Ready(keys) = create(&mut owner, workspace_id, channel, now) else {
            panic!("owner keys");
        };
        let before = keys
            .seal_content(
                artifact,
                collab_types::ContentRetention::Delta,
                b"before the loss",
            )
            .unwrap();
        let ChannelAccess::Ready(lost_keys) = lost.service.access(workspace_id, channel, now)
        else {
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
        let mut restored = device(&workspace, workspace_id, 2, 9, &policy, new_dir.path());
        assert!(matches!(
            restored.service.access(workspace_id, channel, now),
            ChannelAccess::Waiting
        ));
        run(&mut [&mut owner, &mut restored], &mut now, 5);
        let ChannelAccess::Ready(restored_keys) =
            restored.service.access(workspace_id, channel, now)
        else {
            panic!("the restored identity's new device is admitted");
        };
        assert!(
            restored_keys
                .open_content(artifact, &before.0, &before.1)
                .is_ok(),
            "its key history opens everything written before the loss"
        );
        let ChannelAccess::Ready(keys) = create(&mut owner, workspace_id, channel, now) else {
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
        kind: collab_types::OpKind,
    ) -> SignedOp {
        SignedOp::new_unsigned_with_encryption(
            payload,
            artifact,
            public(1),
            collab_types::AttestationId::default(),
            kind,
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
    fn a_key_holder_opens_history_and_a_current_checkpoint() {
        use collab_types::OpKind;
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let artifact = collab_types::ArtifactId::new();
        let policy = Policy::default();
        policy.allowed.lock().unwrap().insert(public(12));
        policy.allowed.lock().unwrap().insert(public(11));
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&workspace, workspace_id, 0, 11, &policy, owner_dir.path());
        let mut laptop = device(&workspace, workspace_id, 1, 12, &policy, laptop_dir.path());
        let ChannelAccess::Ready(first) = create(&mut owner, workspace_id, channel, now) else {
            panic!("owner keys");
        };
        let old = sealed_op(
            artifact,
            first
                .seal_content(artifact, ContentRetention::Delta, b"before the rotation")
                .unwrap(),
            OpKind::Change,
        );
        laptop.service.access(workspace_id, channel, now);
        run(&mut [&mut owner, &mut laptop], &mut now, 5);
        let keys = ChannelKeyAccess::new(owner.service);
        let (ChannelAccess::Ready(current), _) = keys.access(workspace_id, channel) else {
            panic!("owner keys");
        };
        assert!(current.epoch() > first.epoch(), "adding the laptop rotated");

        assert!(keys.open_new_write(workspace_id, &old).is_err());
        assert_eq!(
            keys.open_history_write(workspace_id, &old).unwrap(),
            b"before the rotation"
        );
        let sealed = current
            .seal_content(artifact, ContentRetention::Checkpoint, b"merged record")
            .unwrap();
        assert_eq!(sealed.0.epoch, current.epoch());
        assert_eq!(sealed.0.retention(), Some(ContentRetention::Checkpoint));
        let payload = collab_types::SealedCompaction::new(std::slice::from_ref(&old), sealed.1)
            .unwrap()
            .encode()
            .unwrap();
        let compaction = sealed_op(artifact, (sealed.0, payload), OpKind::Compaction);
        assert_eq!(
            keys.open_new_write(workspace_id, &compaction).unwrap(),
            b"merged record"
        );
        assert_eq!(
            keys.open_history_write(workspace_id, &compaction).unwrap(),
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
            collab_types::document_attestation_id("test.app", "workspace", ""),
            collab_types::OpKind::Change,
            std::time::SystemTime::now(),
            1,
            metadata,
        )
    }

    #[test]
    fn a_renewal_keeps_a_current_ciphertext_and_reseals_one_from_before_a_rotation() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let artifact = collab_types::ArtifactId::new();
        let policy = Policy::readers([10]);
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut now = Instant::now();
        let mut owner = device(&workspace, workspace_id, 0, 10, &policy, owner_dir.path());
        let mut laptop = device(&workspace, workspace_id, 1, 11, &policy, laptop_dir.path());
        let ChannelAccess::Ready(before) = create(&mut owner, workspace_id, channel, now) else {
            panic!("the owner holds the channel's keys");
        };
        let stale = sealed_write(artifact, &before);
        policy.allowed.lock().unwrap().insert(public(11));
        laptop.service.access(workspace_id, channel, now);
        run(&mut [&mut owner, &mut laptop], &mut now, 5);
        let access = ChannelKeyAccess::new(owner.service);
        let (ChannelAccess::Ready(current), _) = access.access(workspace_id, channel) else {
            panic!("the owner holds the channel's keys");
        };
        assert!(
            current.epoch() > before.epoch(),
            "adding a member rotated the epoch"
        );

        let fresh = sealed_write(artifact, &current);
        assert_eq!(
            access.renew_sealed(workspace_id, &fresh).unwrap(),
            (fresh.encryption.unwrap(), fresh.payload.clone()),
            "a current-epoch write renews with its identical ciphertext"
        );

        let (metadata, payload) = access.renew_sealed(workspace_id, &stale).unwrap();
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
        assert!(access.renew_sealed(workspace_id, &tampered).is_err());
        let mut plaintext = fresh;
        plaintext.encryption = None;
        assert!(access.renew_sealed(workspace_id, &plaintext).is_err());
    }

    #[test]
    fn a_device_without_ready_access_renews_nothing() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let artifact = collab_types::ArtifactId::new();
        let policy = Policy::readers([12, 13]);
        let (owner_dir, laptop_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let now = Instant::now();
        let mut owner = device(&workspace, workspace_id, 0, 12, &policy, owner_dir.path());
        let laptop = device(&workspace, workspace_id, 1, 13, &policy, laptop_dir.path());
        let ChannelAccess::Ready(keys) = create(&mut owner, workspace_id, channel, now) else {
            panic!("the owner holds the channel's keys");
        };
        let write = sealed_write(artifact, &keys);
        let waiting = ChannelKeyAccess::new(laptop.service);
        let state = waiting.access(workspace_id, channel).0;
        assert!(matches!(state, ChannelAccess::Waiting), "{state:?}");
        assert!(
            waiting.renew_sealed(workspace_id, &write).is_err(),
            "a device still waiting for access renews nothing"
        );
    }

    #[test]
    fn opening_history_never_creates_a_channel_without_an_existing_key() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        let artifact = ArtifactId::new();
        let owner_dir = tempfile::tempdir().unwrap();
        let mut owner = device(
            &workspace,
            workspace_id,
            0,
            1,
            &Policy::readers([1, 2]),
            owner_dir.path(),
        );
        create(&mut owner, workspace_id, channel, Instant::now());
        let keys = ChannelKeyAccess::new(owner.service);
        let (ChannelAccess::Ready(held), _) = keys.access(workspace_id, channel) else {
            panic!("explicit fixture admission provides the original key");
        };
        let sealed = sealed_op(
            artifact,
            held.seal_content(artifact, ContentRetention::Delta, b"existing history")
                .unwrap(),
            collab_types::OpKind::Change,
        );
        let generation = keys.generation();
        assert_eq!(
            keys.open_history_write(workspace_id, &sealed).unwrap(),
            b"existing history"
        );
        assert_eq!(keys.generation(), generation);
        assert!(
            keys.open_history_write(WorkspaceId::new(), &sealed)
                .is_err()
        );
        assert_eq!(keys.generation(), generation);

        let fresh_dir = tempfile::tempdir().unwrap();
        let fresh = device(
            &workspace,
            workspace_id,
            0,
            1,
            &Policy::readers([1, 2]),
            fresh_dir.path(),
        );
        let fresh = ChannelKeyAccess::new(fresh.service);
        let generation = fresh.generation();
        assert!(fresh.open_history_write(workspace_id, &sealed).is_err());
        let state = fresh.inner.lock().unwrap();
        assert!(
            state.peers.is_empty(),
            "reading history must not mint replacement keys or request access"
        );
        assert!(
            !state.queued,
            "reading history must not enqueue channel governance"
        );
        assert_eq!(state.generation(), generation);
        drop(state);
    }

    include!("channel_encryption_unopened_tests.rs");

    /// A device whose secret store did not open keeps no channel state of
    /// its own. A workspace is answered that it has no secret store; asking again
    /// creates nothing, sends nothing and leaves its pump nothing to do, so
    /// nobody asks on a timer.
    #[test]
    fn without_a_store_nothing_is_due() {
        let workspace = Arc::new(Workspace::default());
        let workspace_id = WorkspaceId::new();
        let channel = ChannelId::new();
        // The directory announces policy changes, as a session's does.
        let policy = Policy::default();
        *policy.announces.lock().unwrap() = Some(Announcements::Own(Arc::new(
            std::sync::atomic::AtomicU64::new(1),
        )));
        let broker = Arc::new(Mutex::new(BrokerChannels::default()));
        let mut service = ChannelEncryption::without_store(
            Box::new(Directory {
                policy: policy.clone(),
            }),
            Arc::clone(&broker),
        );
        let endpoint = Endpoint::join(&workspace, workspace_id, 0);
        let now = Instant::now();
        assert!(matches!(
            service.access(workspace_id, channel, now),
            ChannelAccess::NoSecretStore
        ));
        let generation = service.generation();
        for later in 1..=3 {
            let now = now.checked_add(Duration::from_secs(later * 60)).unwrap();
            service.pump(&endpoint, now, 1_000);
            assert!(matches!(
                service.access(workspace_id, channel, now),
                ChannelAccess::NoSecretStore
            ));
            assert_eq!(service.next_pump_deadline(now), None, "nothing is due");
        }
        assert_eq!(service.generation(), generation, "nothing changed");
        assert!(service.peers.is_empty());
        assert!(service.existing_keys(workspace_id, channel).is_none());
        assert!(
            workspace.queues.lock().unwrap().values().all(Vec::is_empty),
            "no governance was sent"
        );
        // The workspace is a governed one too: still no state of its own.
        let mut governed = ChannelEncryption::without_store(Box::new(Directory { policy }), broker);
        assert!(matches!(
            governed.access(workspace_id, channel, now),
            ChannelAccess::NoSecretStore
        ));
        assert!(governed.peers.is_empty());
    }

    include!("channel_encryption_personal_cluster_tests.rs");

    include!("channel_encryption_held_frame_tests.rs");
}
