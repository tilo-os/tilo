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
    /// The key scope the storage unit on `route` seals under in `workspace`:
    /// [`collab_types::workspace_scope`] while every device that may read the
    /// workspace may read the record too, the record's own channel when the
    /// policy narrows its readers ([`Self::allowed_devices`] then names the
    /// narrower set for that channel). The unit's keys come from this scope's
    /// group; content stays bound to the unit's own channel.
    ///
    /// `None` (the default) when this directory cannot decide it: the
    /// workspace's governance is unknown, and a guessed scope could hand a
    /// device keys the record's policy withholds.
    fn unit_scope(
        &self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        now: u64,
    ) -> Option<ChannelId> {
        let _ = (workspace, route, now);
        None
    }
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
    /// The record an endpoint asked about, when it knew one: a record's
    /// channel is named from its record, so the re-check that waits for the
    /// policy asks with it.
    route: Option<ChannelRoute>,
}

impl Unopened {
    fn failed(workspace: WorkspaceId, now: Instant) -> Self {
        let mut retry = Backoff::now(now, REQUEST_FIRST);
        retry.fired(now, REQUEST_LONGEST);
        Self {
            workspace,
            retry: Some(retry),
            route: None,
        }
    }

    fn unnamed(workspace: WorkspaceId, route: Option<ChannelRoute>) -> Self {
        Self {
            workspace,
            retry: None,
            route,
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
    /// The key scope carrying each storage unit this device knows the route
    /// of, by the unit's own channel. A unit seals under its scope's keys
    /// while naming its own channel, so a payload's channel is turned back
    /// into its group here.
    units: HashMap<ChannelId, ChannelId>,
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
        // The scope each unit seals under now, read again from the policy
        // rather than remembered: a unit whose state was kept from before key
        // scopes keeps it for reading while new content seals under the scope.
        let routes: Vec<(WorkspaceId, ChannelRoute)> = service
            .peers
            .values()
            .filter_map(|entry| Some((entry.workspace, entry.route.clone()?)))
            .collect();
        let wall = tilo_clock::unix_now();
        for (workspace, route) in routes {
            service.resolve_scope(workspace, &route, wall);
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
            units: HashMap::new(),
        }
    }

    /// Increases whenever any channel's keys or access change.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The [`Self::generation`] at which `channel`'s keys or access last
    /// changed; 0 while they never did in this service. A storage unit
    /// changes when the scope carrying it does, so asking by the unit's
    /// channel answers with its scope's change too.
    pub fn channel_change(&self, channel: ChannelId) -> u64 {
        if let Some(change) = self.changed.get(&channel) {
            return *change;
        }
        self.units
            .get(&channel)
            .and_then(|scope| self.changed.get(scope))
            .copied()
            .unwrap_or(0)
    }

    /// The key scope that carries every unit of `workspace` whose readers
    /// are the workspace's participants.
    fn workspace_scope(workspace: WorkspaceId) -> ChannelId {
        collab_types::workspace_scope(&workspace.to_string())
    }

    /// The scope the storage unit on `route` seals under in `workspace`,
    /// remembered so a payload naming only the unit finds its group, or
    /// `None` while this device cannot decide it
    /// ([`ChannelDirectory::unit_scope`]).
    fn resolve_scope(
        &mut self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        wall: u64,
    ) -> Option<ChannelId> {
        let unit = route.channel(&workspace.to_string());
        let scope = self.directory.unit_scope(workspace, route, wall)?;
        self.units.insert(unit, scope);
        Some(scope)
    }

    /// The group that carries `unit`'s keys here: the scope its route named,
    /// or else the unit's own group when this device holds one (a record
    /// whose readers policy narrows, or one whose group was made before key
    /// scopes), or else the workspace's scope.
    fn scope_here(&self, workspace: WorkspaceId, unit: ChannelId) -> ChannelId {
        self.units
            .get(&unit)
            .copied()
            .or_else(|| self.takes_part_in(workspace, unit).then_some(unit))
            .unwrap_or_else(|| Self::workspace_scope(workspace))
    }

    /// Whether this device holds the state of `channel`, one of `workspace`'s
    /// groups.
    fn takes_part_in(&self, workspace: WorkspaceId, channel: ChannelId) -> bool {
        self.peers
            .get(&channel)
            .is_some_and(|entry| entry.workspace == workspace)
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
            self.note_unopened(channel, Unopened::unnamed(workspace, route.cloned()));
            return ChannelAccess::NoSecretStore;
        }
        if !self.peers.contains_key(&channel) {
            match self.start_peer(workspace, channel, route, now) {
                Ok(true) => self.queued = true,
                Ok(false) => {
                    self.note_unopened(channel, Unopened::unnamed(workspace, route.cloned()));
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
    /// itself, and sends the route with its access request. The answer's
    /// channel is the record's own; its keys are the ones the record's key
    /// scope carries, named for the record.
    pub fn access_record(
        &mut self,
        workspace: WorkspaceId,
        route: &ChannelRoute,
        now: Instant,
    ) -> (ChannelId, ChannelAccess) {
        let unit = route.channel(&workspace.to_string());
        let Some(scope) = self.resolve_scope(workspace, route, tilo_clock::unix_now()) else {
            // Nothing here names the scope, and a guessed one could hand a
            // device keys the record's policy withholds.
            self.note_unopened(unit, Unopened::unnamed(workspace, Some(route.clone())));
            return (unit, ChannelAccess::Unavailable);
        };
        // A unit with readers of its own is carried by its own group, whose
        // record this device decides Read on; the workspace's scope is read
        // by the workspace's participants, and needs no route.
        let routed = (scope == unit).then_some(route);
        let access = self.access_routed(workspace, scope, routed, now);
        (unit, self.for_unit(workspace, unit, scope, access))
    }

    /// [`Self::access`] for the group carrying the storage unit `unit`, whose
    /// route this caller does not have: the scope is the one learned for it,
    /// its own group here, or the workspace's scope. The answer's keys are
    /// named for the unit.
    fn access_unit(
        &mut self,
        workspace: WorkspaceId,
        unit: ChannelId,
        now: Instant,
    ) -> ChannelAccess {
        let scope = self.scope_here(workspace, unit);
        let access = self.access_routed(workspace, scope, None, now);
        self.for_unit(workspace, unit, scope, access)
    }

    /// `access`, the answer for the group `scope`, as the storage unit `unit`
    /// uses it: its keys named for the unit, with the keys of a group the
    /// unit had before key scopes merged in, so its whole history opens.
    fn for_unit(
        &self,
        workspace: WorkspaceId,
        unit: ChannelId,
        scope: ChannelId,
        access: ChannelAccess,
    ) -> ChannelAccess {
        let ChannelAccess::Ready(keys) = access else {
            return access;
        };
        let mut keys = keys.for_unit(unit);
        if scope != unit
            && let Some(own) = self.existing_keys(workspace, unit)
            && let Ok(merged) = keys.clone().with_earlier(&own)
        {
            keys = merged;
        }
        ChannelAccess::Ready(keys)
    }

    /// Make the content keys of legacy per-record groups available under the
    /// key scope that now carries their units. A device that holds such a
    /// group hands its keys to the scope's members with the scope's next
    /// commit, and every later admission passes them on, so a device that
    /// joined after key scopes reads the unit's whole history and the holders
    /// of the old group can go offline once one of them has done it.
    ///
    /// Only units the workspace's scope carries: one a Read Deny narrows is
    /// sealed under a group of its own, whose keys no denied device gets, and
    /// those are never handed to the broader scope. A unit whose record this
    /// device has not learned the readers of waits for a policy re-check,
    /// which resolves it.
    fn hand_over_legacy_keys(&mut self, wall: u64, recheck: bool) {
        let mut handing = Vec::new();
        let mut unresolved = Vec::new();
        for (unit, entry) in &self.peers {
            let scope = Self::workspace_scope(entry.workspace);
            if *unit == scope {
                continue;
            }
            match self.units.get(unit) {
                Some(carried) if *carried == scope => handing.push((*unit, scope)),
                Some(_) => {}
                None => {
                    if let Some(route) = &entry.route {
                        unresolved.push((*unit, entry.workspace, route.clone()));
                    }
                }
            }
        }
        if recheck {
            for (unit, workspace, route) in unresolved {
                let scope = self.resolve_scope(workspace, &route, wall);
                if scope == Some(Self::workspace_scope(workspace)) {
                    handing.push((unit, Self::workspace_scope(workspace)));
                }
            }
        }
        let mut adopted = Vec::new();
        for (unit, scope) in handing {
            let [Some(scope_entry), Some(unit_entry)] =
                self.peers.get_disjoint_mut([&scope, &unit])
            else {
                continue;
            };
            if scope_entry.workspace != unit_entry.workspace
                || !matches!(scope_entry.peer.access(), Access::Ready { .. })
            {
                continue;
            }
            let Some(keys) = unit_entry.peer.keyring() else {
                continue;
            };
            if scope_entry.peer.adopt(keys) {
                // The scope's commit is due now: it is what hands the keys out.
                scope_entry.dirty = true;
                adopted.push(scope);
            }
        }
        for scope in adopted {
            self.bump(scope);
        }
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
        let mut learned = false;
        if let Some(entry) = self.peers.get_mut(&channel)
            && entry.workspace == workspace
            && entry.route.is_none()
        {
            entry.route = Some(route.clone());
            entry.dirty = true;
            learned = true;
            save_route(&route_path(&store.root, workspace, channel), route);
        }
        // The scope a unit seals under is read from its record, so a route
        // that arrives after the channel was opened resolves it now.
        if learned {
            self.resolve_scope(workspace, route, tilo_clock::unix_now());
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
                // An endpoint that asks with its record hands it over for
                // the re-check that needs it.
                if known.route.is_none() {
                    known.route = entry.route;
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
                    // A record's channel is named from its record, so the
                    // re-check asks with the route its endpoint gave; only a
                    // channel asked about without one opens on a directory
                    // that names its readers by channel alone.
                    let opens = stored
                        && takes_part(
                            &*self.directory,
                            device,
                            entry.workspace,
                            *channel,
                            entry.route.as_ref(),
                        );
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

    /// `unit`'s keys as this device holds them, opening, joining or creating
    /// nothing: `None` when this device takes part in no group that could
    /// carry `unit` of `workspace`, `Some(Err(_))` when it does and holds none
    /// of its keys yet. `scope` is the scope its route named, when the caller
    /// has one.
    fn record_keys(
        &self,
        workspace: WorkspaceId,
        unit: ChannelId,
        scope: Option<ChannelId>,
    ) -> Option<Result<ChannelContentKeys, String>> {
        let scope = scope.unwrap_or_else(|| self.scope_here(workspace, unit));
        if let Some(keys) = self.unit_keys(workspace, unit, scope) {
            return Some(Ok(keys));
        }
        // The directory names a channel's readers only for a device that may
        // read it: an unnameable scope is no group of this device's.
        if self.store.is_none()
            || self
                .directory
                .allowed_devices(workspace, scope, None, tilo_clock::unix_now())
                .is_none()
        {
            return None;
        }
        Some(Err("this device holds no channel keys yet".into()))
    }

    /// The keys that open `unit` here: those of the group carrying it, named
    /// for the unit, with the keys of a group the unit had before key scopes
    /// merged in. `None` while neither is here yet.
    fn unit_keys(
        &self,
        workspace: WorkspaceId,
        unit: ChannelId,
        scope: ChannelId,
    ) -> Option<ChannelContentKeys> {
        let held = self.existing_keys(workspace, scope);
        let own = (scope != unit)
            .then(|| self.existing_keys(workspace, unit))
            .flatten();
        match (held, own) {
            (Some(held), Some(own)) => {
                let keys = held.for_unit(unit);
                Some(keys.clone().with_earlier(&own).unwrap_or(keys))
            }
            (Some(held), None) => Some(held.for_unit(unit)),
            (None, Some(own)) => Some(own.for_unit(unit)),
            (None, None) => None,
        }
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
        // Before the groups reconcile, so the hand-over commit is written in
        // this pass.
        self.hand_over_legacy_keys(wall, reconcile_due);
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
        if let Some(entry) = self.peers.get(&channel)
            && entry.workspace != workspace
        {
            return;
        }
        if !self.peers.contains_key(&channel) {
            // A device that was away keeps the state of the channels it takes
            // part in, and its host replays the governance it retained when
            // the device comes back, before its apps read anything: take that
            // in rather than forget it, so the group is current before the
            // first read of its record. A channel this device holds no state
            // of is one it takes no part in: it never creates a second group
            // of a channel whose group it has heard, and if it comes to take
            // part it asks to join.
            let opened = self.store.is_some()
                && match self.open_peer(workspace, channel, now) {
                    Ok(opened) => opened,
                    Err(error) => {
                        tracing::warn!(%error, %workspace, "channel state could not be opened");
                        false
                    }
                };
            if !opened {
                if self.groups_seen.len() < MAX_GROUPS_SEEN {
                    self.groups_seen.insert(channel);
                }
                return;
            }
            self.receive_frame(workspace, frame, now, wall);
            return;
        }
        let Some(route) = self.peers.get(&channel).map(|entry| entry.route.clone()) else {
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
        let opened = match ChannelPeer::open(
            &dir,
            channel,
            Box::new(store.device_key.clone()),
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
                match std::fs::remove_file(ChannelPeer::path_in(&dir, channel)) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                        return Err(error.into());
                    }
                    _ => return Ok(false),
                }
            }
            opened => opened?,
        };
        let Some(peer) = opened else {
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
        let unit = route.channel(&workspace.to_string());
        self.run(|service| {
            let scope = service
                .directory
                .unit_scope(workspace, route, tilo_clock::unix_now());
            service.record_keys(workspace, unit, scope)
        })
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

    /// The keys of the group carrying the storage unit `unit`, as
    /// [`Self::access`] has them for a group, opened or joined when this
    /// device takes part and asked for when it does not.
    fn access_unit(&self, workspace: WorkspaceId, unit: ChannelId) -> (ChannelAccess, u64) {
        let Some((access, generation, queued)) = self.run(|service| {
            let access = service.access_unit(workspace, unit, Instant::now());
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
        let (ChannelAccess::Ready(keys), _) = self.access_unit(workspace, metadata.channel) else {
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
        let (ChannelAccess::Ready(keys), _) = self.access_unit(workspace, metadata.channel) else {
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
    include!("channel_encryption_tests.rs");
}
