//! End-to-end encryption of workspaces on this device. Workspace groups follow the
//! trusted devices of a workspace. Reader sets and protected documents have groups
//! whose keys reach only their permitted readers.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_clock::time::Instant;

use collab_types::signer::{DeviceSigner, SignFuture};
use collab_types::{
    Ed25519PublicKey, EncryptionMetadata, MlsAccessRequestV1, MlsGovernanceFrame, SignedOp,
};
use key_group::{Access, ContentKeys, GroupPeer, PeerEvent, VaultKey, WelcomeRefusal};
use neighbour_wire::{KeyScope, NeighbourMsg};

use crate::schedule::{PolicySchedule, RECHECK_INTERVAL};
use crate::seal_checks::SealCheckRegistry;
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
/// How long each member before this one in turn ([`GroupPeer::turn`]) has
/// to commit a change of a group's group before this one does: long enough
/// for one member's commit to reach the others, so they follow it rather than
/// commit the same change beside it and fork the group.
const TURN: Duration = Duration::from_secs(2);
/// The least time between two commits this device writes for one group.
/// Every member follows each commit, and in a group of hundreds a commit costs
/// each of them work that grows with the group: people arriving within this
/// of each other are admitted in one commit rather than one each. A change
/// after a quiet spell is committed at once.
const COMMIT_SPACING: Duration = Duration::from_millis(500);
/// Groups remembered as having a group this device takes no part in.
const MAX_GROUPS_SEEN: usize = 1 << 16;
/// How long a waiting device holds a Welcome whose tree names a device its
/// view of the membership does not, judging it again as that view changes,
/// before it gives up and asks with a fresh member key. Such a tree is an
/// honest one whenever this device's view is behind the committer's (a device
/// admitted a moment ago) or ahead of it (one removed a moment ago), and
/// asking again costs the whole group a commit: a lag of a moment is waited
/// out, and only a view that does not come round in this long, or a Welcome
/// the group moved past, is given up on.
const WELCOME_WAIT: Duration = Duration::from_secs(10);
/// Frames one group keeps for the policy that has not named their signer
/// yet. Bounded, newest kept: a frame this device's policy still does not
/// back after that many later ones is not going to be applied.
const MAX_HELD_FRAMES: usize = 8;

/// The widest a device's wait to create a group's group spreads past
/// [`REQUEST_FIRST`], in milliseconds: devices that hold no state for the same
/// group wait within these of each other, so one creates the group and the
/// others hear of it in time.
const FOUNDING_SPREAD_MS: u64 = 2_000;

/// The longest a device that holds no state for a group waits for its genesis
/// (from the workspace's retained frames, or a key holder answering its
/// request) before it creates the group itself: [`REQUEST_FIRST`] plus the
/// widest spread between devices. Whoever waits for a fresh group's keys
/// waits longer than this, or it gives up on a group about to be created.
#[must_use]
pub const fn founding_wait() -> Duration {
    REQUEST_FIRST.saturating_add(Duration::from_millis(FOUNDING_SPREAD_MS))
}

/// The longest a device that is waiting for a group's keys goes between asks
/// ([`REQUEST_LONGEST`]): each ask it leaves unanswered doubles the wait. A
/// reader of a record sealed in that group reads nothing until the keys are
/// here, so whoever waits for it waits past this: an ask on a busy machine is
/// answered late, and a reader that gave up before the next one would call a
/// record that is arriving one that never came.
#[must_use]
pub const fn asking_again_wait() -> Duration {
    REQUEST_LONGEST.saturating_add(REQUEST_FIRST)
}

/// How long a device that took a group up for another one that asked
/// ([`PeerEntry::wanted`]) waits to create its group, once no genesis is on
/// its way: the asker creates it at once if it can, this one only for an
/// asker that cannot (a reader). Each device waits its own while, set by its
/// key and the group, so one creates the group and the others hear of it.
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
        store: &KeysConfig,
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
fn give_up_welcome(entry: &mut PeerEntry, store: &KeysConfig, now: Instant) {
    entry.welcome = None;
    if let Err(error) = entry.peer.refuse_welcome(&store.vault) {
        tracing::warn!(%error, workspace = %entry.workspace, "group state could not be written");
    }
    entry.request = None;
    entry.retry = Backoff::now(now, REQUEST_FIRST);
    entry.fresh_at = now;
}

/// What an application endpoint may do with a route's content right now.
#[derive(Clone, Debug)]
pub enum KeyAccess {
    /// Admitted: seal new content and open history with these keys.
    Ready(ContentKeys),
    /// Asked the key holders for access and not admitted yet. Nothing can
    /// be sealed or opened; the endpoint shows a waiting state.
    Waiting,
    Removed,
    /// Nobody can admit this device: the workspace's governance is unknown here.
    Unavailable,
    /// This device has no secret store open, so it keeps no group state
    /// of its own and cannot join a group. Asking again changes nothing
    /// while the session runs.
    NoSecretStore,
}

/// A group governance message received from the workspace transport. Every
/// frame and request is signed by a device key and verified by the group
/// service before it changes any state.
#[derive(Debug, Clone)]
pub enum InboundGovernance {
    Frame {
        workspace: WorkspaceId,
        frame: Box<collab_types::MlsGovernanceFrame>,
    },
    /// A device asking the workspace's key holders for membership.
    AccessRequest {
        workspace: WorkspaceId,
        request: Box<collab_types::MlsAccessRequestV1>,
    },
    DocumentFrame {
        scope: neighbour_wire::KeyScope,
        frame: Box<MlsGovernanceFrame>,
    },
    DocumentAccessRequest {
        scope: neighbour_wire::KeyScope,
        request: Box<MlsAccessRequestV1>,
    },
}

impl InboundGovernance {
    /// The governance `message` carries, if it is one for a valid workspace.
    #[must_use]
    pub fn from_message(message: NeighbourMsg) -> Option<Self> {
        match message {
            NeighbourMsg::MlsGovernance {
                workspace, frame, ..
            } => match neighbour_wire::KeyScope::parse(&workspace)? {
                neighbour_wire::KeyScope::Workspace(workspace) => {
                    Some(Self::Frame { workspace, frame })
                }
                scope @ KeyScope::Document { .. } => Some(Self::DocumentFrame { scope, frame }),
            },
            NeighbourMsg::MlsAccessRequest {
                workspace, request, ..
            } => match neighbour_wire::KeyScope::parse(&workspace)? {
                neighbour_wire::KeyScope::Workspace(workspace) => {
                    Some(Self::AccessRequest { workspace, request })
                }
                scope @ KeyScope::Document { .. } => {
                    Some(Self::DocumentAccessRequest { scope, request })
                }
            },
            _ => None,
        }
    }
}

/// Where the group protocol's messages travel: the workspace transport of the
/// host (the desktop session's network, the runtime's server sessions).
pub trait GovernanceTransport {
    /// Hand one message to the transport. `false` when it was not accepted.
    fn publish(&self, message: NeighbourMsg) -> bool;
    /// Take every message received since the last call.
    fn take_inbound(&self) -> Vec<InboundGovernance>;
}

/// The access policy inputs this service needs from the workspace model.
pub trait KeyDirectory: collab_types::signer::MaybeSendSync {
    /// The certified devices whose current policy allows them to read an
    /// exact document. Until a directory can answer it, the document has no
    /// key directory.
    fn document_devices(
        &self,
        workspace: WorkspaceId,
        app: &str,
        document: &str,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        let _ = (workspace, app, document, now);
        None
    }

    /// Whether this document has or must start using its own group.
    /// An unsettled policy cannot choose the group to hand to an endpoint.
    fn requires_document_keys(
        &self,
        _workspace: WorkspaceId,
        _app: &str,
        _document: &str,
    ) -> Option<bool> {
        Some(false)
    }

    /// Every device that holds `workspace`'s keys at wall-clock `now` (seconds since the Unix
    /// epoch): the devices of its members that read and run work there. `None` while this device
    /// cannot tell: the workspace's governance is unknown here, and it then admits and removes no
    /// one.
    ///
    /// The person's own workspace ([`collab_types::person_workspace`]) is read by the devices
    /// of its person (certified, not revoked).
    fn allowed_devices(
        &self,
        workspace: WorkspaceId,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>>;
    /// The devices of the readers `named` names among those that hold `workspace`'s keys at
    /// wall-clock `now` ([`Self::allowed_devices`]): who holds the keys of a group only they read
    /// ([`collab_types::reader_group`]) — its people, and every member whose role includes the
    /// role it names, as the workspace's governance has them at `now`. Never a device outside
    /// [`Self::allowed_devices`], so no group of a workspace reaches a device the workspace does
    /// not admit. `None` while this device cannot tell, as there; a directory that names no one's
    /// devices answers `None` always, and so admits and removes no one in such a group.
    fn reader_devices(
        &self,
        workspace: WorkspaceId,
        named: &collab_types::NamedReaders,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        let _ = (workspace, named, now);
        None
    }
    /// The [`tilo_identity::policy_change`] epoch (or a test's own) when
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
        tilo_identity::policy_change::wake_at(at.saturating_mul(1_000_000_000));
    }
    /// Whether this device created `workspace` and so founds its group. Every
    /// other device waits for the genesis and joins by Welcome.
    fn founds(&self, workspace: WorkspaceId) -> bool;
}

/// The platform secret store's namespace holding a device's group vault
/// master key: the key the sealed group state of every workspace is
/// derived from. A host keeps it the way it keeps its other secrets — the
/// system keychain on a desktop, a non-extractable `WebCrypto` key in a
/// page — so the state's own store never holds the key that opens it.
pub const VAULT_SECRET_APP: &str = "so.tilo.collaboration.mls";

/// Where a host keeps this device's group state: the peers of each
/// workspace's group. A desktop keeps
/// it in its groups directory ([`KeyDir`]); a page keeps it in its store.
pub trait KeyStorage: std::fmt::Debug + collab_types::signer::MaybeSendSync {
    /// Make the store ready for the state a group writes: a directory that
    /// pays for it is created owner-only here, so a write cannot land in one a
    /// person's umask left open. A store that always exists answers nothing.
    ///
    /// # Errors
    /// The store cannot be created.
    fn prepare(&self) -> Result<(), String> {
        Ok(())
    }
    /// Where the peer of each workspace's group is kept.
    fn peers(&self) -> Arc<dyn key_group::PeerStates>;
    /// Every workspace this store holds a peer for, so a service registers
    /// their checks before the first op after a restart. A store that cannot
    /// list them answers none: its groups open as an endpoint asks about them.
    fn held(&self) -> Vec<WorkspaceId> {
        Vec::new()
    }
    /// Document routes of the groups this store keeps.
    fn scopes(&self) -> Vec<KeyScope> {
        Vec::new()
    }

    /// Keep the route before saving the group's peer state.
    fn keep_scope(&self, _scope: &KeyScope) -> Result<(), String> {
        Ok(())
    }

    /// Whether this store already keeps group state: a vault whose master
    /// key is gone opens none of it, so it is never replaced by a new one. A
    /// store whose state lives elsewhere answers `false`.
    ///
    /// # Errors
    /// The store cannot be read.
    fn holds_key_state(&self) -> Result<bool, String> {
        Ok(false)
    }
    /// The reader sets' groups this device takes part in, as [`Self::keep_reader_groups`] last
    /// kept them: each one's workspace and readers ([`collab_types::reader_group`]). A store
    /// that keeps none answers none: its groups are known again as endpoints ask for them.
    fn reader_groups(&self) -> Vec<(WorkspaceId, String)> {
        Vec::new()
    }
    /// Keep `groups`, every reader set's group this device takes part in, in place of what was
    /// kept.
    ///
    /// # Errors
    /// They could not be kept.
    fn keep_reader_groups(&self, groups: &[(WorkspaceId, String)]) -> Result<(), String> {
        let _ = groups;
        Ok(())
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

/// The file of a [`KeyDir`] keeping its reader sets' groups.
#[cfg(not(target_arch = "wasm32"))]
const READER_GROUPS: &str = "reader-groups.json";

/// A desktop's key directory: one `<workspace>.peer` file per workspace, and the reader sets'
/// groups it takes part in.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct KeyDir {
    state: PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl KeyDir {
    /// The state kept under `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { state: root.into() }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl KeyStorage for KeyDir {
    fn prepare(&self) -> Result<(), String> {
        tilo_files::native::atomic::create_private_directory(&self.state)
            .map_err(|error| error.to_string())
    }

    fn peers(&self) -> Arc<dyn key_group::PeerStates> {
        Arc::new(key_group::Files::new(self.state.clone()))
    }

    fn held(&self) -> Vec<WorkspaceId> {
        let entries = match std::fs::read_dir(&self.state) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                tracing::warn!(dir = %self.state.display(), %error, "group state cannot be listed");
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

    fn scopes(&self) -> Vec<KeyScope> {
        let Ok(entries) = std::fs::read_dir(&self.state) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension()?.to_str()? != "scope" {
                    return None;
                }
                let group = WorkspaceId::parse(path.file_stem()?.to_str()?)?;
                let scope = KeyScope::parse(&std::fs::read_to_string(path).ok()?)?;
                (KeyScope::requires_document(group) && scope.group() == group).then_some(scope)
            })
            .collect()
    }

    fn keep_scope(&self, scope: &KeyScope) -> Result<(), String> {
        let path = self.state.join(format!("{}.scope", scope.group()));
        tilo_files::native::atomic::write_private(&path, scope.to_string().as_bytes())
            .map_err(|error| error.to_string())
    }

    fn reader_groups(&self) -> Vec<(WorkspaceId, String)> {
        tilo_files::native::atomic::json::load_or_default(&self.state.join(READER_GROUPS))
    }

    fn keep_reader_groups(&self, groups: &[(WorkspaceId, String)]) -> Result<(), String> {
        tilo_files::native::atomic::json::write_private(&self.state.join(READER_GROUPS), groups)
            .map_err(|error| error.to_string())
    }

    fn forget(&self, group: WorkspaceId) -> Result<(), String> {
        self.peers()
            .forget(group)
            .map_err(|error| error.to_string())?;
        match std::fs::remove_file(self.state.join(format!("{group}.scope"))) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn holds_key_state(&self) -> Result<bool, String> {
        match std::fs::read_dir(&self.state) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry.map_err(|error| error.to_string())?.path();
                    if path.extension().and_then(|ext| ext.to_str()) != Some("scope")
                        && path.file_name().and_then(|name| name.to_str()) != Some(READER_GROUPS)
                    {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!("{}: {error}", self.state.display())),
        }
    }
}

/// Durable location and keys of this device's own group state: what a
/// key holder of a group needs, and only workspaces this device's own workspace
/// host does not take part in need it.
pub struct KeysConfig {
    /// Where this host keeps each workspace's group state.
    pub state: Arc<dyn KeyStorage>,
    /// Key sealing that state, held by the platform secret store.
    pub vault: VaultKey,
    /// This device's certified key, kept in its ownership: the request and frame signer.
    pub signer: Arc<dyn DeviceSigner>,
    /// Whether this device settles its groups or only follows them.
    pub role: KeyRole,
}

/// What a device does in the groups it takes part in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRole {
    /// Founds groups and commits them to the access policy.
    Committer,
    /// Asks for access and follows the commits others write, but founds and commits no group
    /// but a call's media group ([`WorkspaceKeys::settles`]): a page or an agent, which may not
    /// hold the grants a commit of any other group is decided on.
    Follower,
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

/// When this device may create a group's group itself.
struct Founding {
    /// When a device that took the group up for another one creates the
    /// group, if no genesis has arrived.
    at: Instant,
    /// Whether the policy, when last read, let this device read the group
    /// and so create its group.
    allowed: bool,
}

/// One group this device takes part in.
struct PeerEntry {
    workspace: WorkspaceId,
    peer: GroupPeer,
    /// The request this device asks with while it waits, re-sent on `retry`.
    request: Option<MlsAccessRequestV1>,
    /// A request, or a frame its peer staged, whose signature the device's ownership has not
    /// answered yet.
    signing: Option<(Owed, SignFuture<'static>)>,
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
    /// When this device last committed a change of the workspace's group: the
    /// next waits for [`COMMIT_SPACING`] after it.
    committed_at: Option<Instant>,
    /// An endpoint opened the route after this device was removed: ask
    /// again, and the policy decides whether access resumes.
    rejoin: bool,
    founding: Founding,
    /// An endpoint of this device opened the route. A group taken up only
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
}

/// What a signature the device's ownership owes is for ([`WorkspaceKeys::take_signing`]).
pub struct Owed(Signing);

enum Signing {
    /// An access request.
    Request(MlsAccessRequestV1),
    /// The frame the group's peer staged: its genesis or a commit.
    Frame,
}

/// Whether `scope` is a call's media group ([`collab_types::calls::media_document`]): the only
/// group a [`KeyRole::Follower`] founds and commits.
#[must_use]
pub fn is_call_media(scope: &KeyScope) -> bool {
    matches!(
        scope,
        KeyScope::Document { app, document, readers: None, .. }
            if app == collab_types::scope::MEDIA_APP
                && collab_types::calls::parse_media_document(document).is_some()
    )
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

/// A group an endpoint asked about that this device could not open: no
/// one can name its readers here, or opening its state failed. Its answer
/// changes with nothing arriving for it, so the service tells the route itself
/// ([`WorkspaceKeys::bump`]) rather than leaving the endpoint to ask on
/// a timer.
struct Unopened {
    workspace: WorkspaceId,
    /// `None` while no one can name the group's readers here: re-checked
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

/// This device's side of every encrypted group it takes part in.
///
/// Taking part in a workspace's group needs this device's own group state,
/// kept under a key from the platform secret store; a service opened
/// without it ([`Self::without_store`]) answers every workspace
/// [`KeyAccess::NoSecretStore`].
pub struct WorkspaceKeys {
    /// This device's own group state; `None` when no secret store opened.
    store: Option<KeysConfig>,
    directory: reader_groups::Directory,
    seal_checks: Arc<dyn SealCheckRegistry>,
    peers: HashMap<WorkspaceId, PeerEntry>,
    /// When the groups last re-checked the access policy, and what the next
    /// re-check waits for.
    reconcile: PolicySchedule,
    delivery: u64,
    generation: u64,
    /// The [`Self::generation`] each group last changed at.
    changed: HashMap<WorkspaceId, u64>,
    /// [`Self::generation`], announced to whoever waits for a change.
    changes: tokio::sync::watch::Sender<u64>,
    /// An endpoint's [`Self::access`] queued governance the next pump sends
    /// (a genesis to announce, an access request to make). A host that parks
    /// its pump while idle wakes it on this; see [`SharedKeys::with_wake`].
    queued: bool,
    /// Groups asked about that could not be opened; see [`Unopened`].
    unopened: HashMap<WorkspaceId, Unopened>,
    /// The check this service last registered with the broker for each
    /// group. Every access re-asserts its group's check, and a group's
    /// keys are read for every live frame sealed or opened; taking the
    /// broker's lock for each made every such read wait for as long as a
    /// broker thread held it to append and sync durable history, which froze
    /// the desktop while its frame loop was the reader. Only a changed check
    /// takes the lock.
    registered: HashMap<WorkspaceId, key_group::SealCheck>,
    /// Groups whose frames arrived while this device took no part in
    /// them: they have a group, so this device never creates one.
    groups_seen: std::collections::HashSet<WorkspaceId>,
    /// The policy epoch the last pass read, while the directory announces
    /// its changes: a request that went unanswered under the policy before
    /// it is asked again at once when it moves, rather than waiting out a
    /// backoff that has doubled past what the caller waits
    /// ([`Self::ask_waiting_now`]).
    policy_epoch: Option<u64>,
    /// The keys of the group state this machine was copied with, which names the device it was
    /// copied from: the copy opens what it holds with them until the workspace admits it, then
    /// forgets the state and joins as itself. It never seals with them.
    copied: HashMap<WorkspaceId, ContentKeys>,
}

impl WorkspaceKeys {
    /// Forget `workspace`, deleted: its group and keys,
    /// here and in the store.
    ///
    /// # Errors
    /// What the store kept could not be removed.
    pub fn forget_workspace(&mut self, workspace: WorkspaceId) -> Result<(), String> {
        let groups = self.directory.forget(workspace);
        if !groups.is_empty() {
            self.keep_reader_groups();
        }
        for group in groups {
            self.forget_group(group)?;
            self.bump(group);
        }
        self.forget_group(workspace)
    }

    /// Forget one group: its state and keys, here and in the store.
    fn forget_group(&mut self, workspace: WorkspaceId) -> Result<(), String> {
        self.peers.remove(&workspace);
        self.changed.remove(&workspace);
        self.unopened.remove(&workspace);
        self.registered.remove(&workspace);
        self.groups_seen.remove(&workspace);
        self.copied.remove(&workspace);
        self.store
            .as_ref()
            .map_or(Ok(()), |store| store.state.forget(workspace))
    }

    /// Open every group state the host keeps and register each group's
    /// check with the broker, so the plaintext check holds from the first op after
    /// a restart.
    pub fn open(
        config: KeysConfig,
        directory: Box<dyn KeyDirectory>,
        seal_checks: Arc<dyn SealCheckRegistry>,
    ) -> std::io::Result<Self> {
        config.state.prepare().map_err(std::io::Error::other)?;
        let mut service = Self::with_store(Some(config), directory, seal_checks);
        service.take_in_kept();
        Ok(service)
    }

    /// The service of a device whose secret store did not open: it answers
    /// every workspace [`KeyAccess::NoSecretStore`].
    pub fn without_store(
        directory: Box<dyn KeyDirectory>,
        seal_checks: Arc<dyn SealCheckRegistry>,
    ) -> Self {
        Self::with_store(None, directory, seal_checks)
    }

    fn with_store(
        store: Option<KeysConfig>,
        directory: Box<dyn KeyDirectory>,
        seal_checks: Arc<dyn SealCheckRegistry>,
    ) -> Self {
        Self {
            store,
            directory: reader_groups::Directory::new(directory),
            seal_checks,
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
            copied: HashMap::new(),
        }
    }

    /// Increases whenever any group's keys or access change.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The [`Self::generation`] at which `workspace`'s keys or access last
    /// changed; 0 while they never did in this service.
    pub fn workspace_change(&self, workspace: WorkspaceId) -> u64 {
        self.changed.get(&workspace).copied().unwrap_or(0)
    }

    /// The key group of `readers` in `workspace` ([`collab_types::reader_group`]), which
    /// [`Self::access`] then answers for: this device settles it from now on as it settles the
    /// workspace's own. `None` when `readers` names no set.
    pub fn reader_group(&mut self, workspace: WorkspaceId, readers: &str) -> Option<WorkspaceId> {
        let known = self.directory.kept().len();
        let group = self.directory.group(workspace, readers)?;
        if self.directory.kept().len() != known {
            self.keep_reader_groups();
        }
        Some(group)
    }

    /// Keep every reader set's group known here with this device's group state.
    fn keep_reader_groups(&self) {
        if let Some(store) = &self.store
            && let Err(error) = store.state.keep_reader_groups(&self.directory.kept())
        {
            tracing::warn!(%error, "reader groups could not be kept");
        }
    }

    /// The groups of `workspace`'s reader sets this device holds keys of, each with the epoch it
    /// holds: what a device asks the person's other devices to catch up on beside the
    /// workspace's own group.
    pub fn reader_groups_of(&self, workspace: WorkspaceId) -> Vec<(WorkspaceId, u64)> {
        self.directory
            .of(workspace)
            .into_iter()
            .filter_map(|group| Some((group, self.existing_keys(group)?.epoch())))
            .collect()
    }

    /// The workspace whose transport carries `group`'s governance: a reader set's workspace, or
    /// the group's own.
    pub fn carrier(&self, group: WorkspaceId) -> WorkspaceId {
        self.directory.carrier(group)
    }

    /// What an endpoint of this device may do with `workspace`'s group. The
    /// first call starts taking part in it: this device asks the key holders
    /// for access, and creates the group when no genesis arrives. The caller
    /// has already decided the endpoint may read the workspace.
    pub fn access(&mut self, workspace: WorkspaceId, now: Instant) -> KeyAccess {
        let document_readers = if neighbour_wire::KeyScope::requires_document(workspace) {
            let Some(readers) = self.current_document_readers(workspace) else {
                // The governance that names the document's readers may not
                // have reached this device yet: its route hears when it does.
                self.note_unopened(workspace, Unopened::unnamed(workspace));
                return KeyAccess::Unavailable;
            };
            Some(readers)
        } else {
            None
        };
        if self.store.is_none() {
            self.note_unopened(workspace, Unopened::unnamed(workspace));
            return KeyAccess::NoSecretStore;
        }
        if !self.peers.contains_key(&workspace) {
            match self.start_peer(workspace, now) {
                Ok(true) => self.queued = true,
                Ok(false) => {
                    self.note_unopened(workspace, Unopened::unnamed(workspace));
                    return KeyAccess::Unavailable;
                }
                Err(error) => {
                    tracing::warn!(%error, %workspace, "group state could not be created");
                    self.note_unopened(workspace, Unopened::failed(workspace, now));
                    return KeyAccess::Unavailable;
                }
            }
        }
        self.unopened.remove(&workspace);
        let Some(entry) = self.peers.get_mut(&workspace) else {
            return KeyAccess::Unavailable;
        };
        let was_wanted = entry.wanted;
        entry.wanted = true;
        let access = match entry.peer.access() {
            Access::Ready { .. }
                if document_readers
                    .as_ref()
                    .is_some_and(|readers| !entry.peer.members().is_subset(readers)) =>
            {
                // Until the group removes excluded devices, its exporter
                // can still be held by them.
                KeyAccess::Waiting
            }
            Access::Ready { .. } => entry
                .peer
                .content_keys()
                .map_or(KeyAccess::Waiting, KeyAccess::Ready),
            Access::Waiting => KeyAccess::Waiting,
            Access::Removed => {
                if !entry.rejoin {
                    entry.rejoin = true;
                    entry.request = None;
                    entry.retry = Backoff::now(now, REQUEST_FIRST);
                    self.queued = true;
                }
                KeyAccess::Removed
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

    fn takes_part(&self, group: WorkspaceId) -> bool {
        self.allowed_devices(group, tilo_clock::unix_now())
            .zip(self.device())
            .is_some_and(|(allowed, device)| allowed.contains(&device))
    }

    fn current_document_readers(&self, group: WorkspaceId) -> Option<BTreeSet<Ed25519PublicKey>> {
        let readers = self.allowed_devices(group, tilo_clock::unix_now())?;
        readers.contains(&self.device()?).then_some(readers)
    }

    fn allowed_devices(&self, group: WorkspaceId, now: u64) -> Option<BTreeSet<Ed25519PublicKey>> {
        self.directory.allowed_devices(group, now)
    }

    fn parent_workspace(&self, group: WorkspaceId) -> WorkspaceId {
        self.directory.carrier(group)
    }

    fn remember_scope(&mut self, scope: neighbour_wire::KeyScope) -> Option<WorkspaceId> {
        let group = scope.group();
        if matches!(scope, neighbour_wire::KeyScope::Workspace(_)) {
            return (!neighbour_wire::KeyScope::requires_document(group)).then_some(group);
        }
        if let Some(known) = self.directory.document_scope(group) {
            return (known == &scope).then_some(group);
        }
        if self.peers.contains_key(&group)
            && let Some(store) = &self.store
            && let Err(error) = store.state.keep_scope(&scope)
        {
            tracing::warn!(%group, %error, "the document key route could not be saved");
            return None;
        }
        self.directory.register_scope(scope)
    }

    /// Open the MLS group for this exact scope, without falling back to its
    /// workspace's group.
    pub fn access_scope(&mut self, scope: neighbour_wire::KeyScope, now: Instant) -> KeyAccess {
        let Some(group) = self.remember_scope(scope) else {
            return KeyAccess::Unavailable;
        };
        self.access(group, now)
    }

    /// Remember that `group` could not be opened, so its route hears when
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

    /// Tell the routes of unopened groups whose answer may now differ:
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
                    let opens = stored
                        && self
                            .directory
                            .allowed_devices(entry.workspace, tilo_clock::unix_now())
                            .zip(device)
                            .is_some_and(|(allowed, device)| allowed.contains(&device));
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

    /// Whether an unopened group waits for a re-check of the policy that
    /// may name it. Without a secret store nothing opens, so a device that
    /// holds none has nothing to re-check and parks.
    fn unopened_waits_for_policy(&self) -> bool {
        self.store.is_some() && self.unopened.values().any(|entry| entry.retry.is_none())
    }

    /// Observe a current key without opening a group, requesting access or
    /// changing a removed member's rejoin state.
    fn existing_keys(&self, workspace: WorkspaceId) -> Option<ContentKeys> {
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
    ) -> Option<Result<ContentKeys, String>> {
        if neighbour_wire::KeyScope::requires_document(workspace) {
            let readers = self.current_document_readers(workspace)?;
            if self
                .peers
                .get(&workspace)
                .is_some_and(|entry| !entry.peer.members().is_subset(&readers))
            {
                return Some(Err("the document group is updating its readers".into()));
            }
        }
        if let Some(keys) = self.existing_keys(workspace) {
            return Some(Ok(keys));
        }
        if self.store.is_none()
            || self
                .allowed_devices(workspace, tilo_clock::unix_now())
                .is_none()
        {
            return None;
        }
        Some(Err("this device holds no group keys yet".into()))
    }

    /// [`Self::record_keys`], or the keys of the state this machine was copied with while the
    /// workspace has not admitted this device: what opens sealed history, never what seals it.
    pub(crate) fn history_keys(
        &self,
        workspace: WorkspaceId,
    ) -> Option<Result<ContentKeys, String>> {
        if neighbour_wire::KeyScope::requires_document(workspace) {
            return self.record_keys(workspace);
        }
        match (self.existing_keys(workspace), self.copied.get(&workspace)) {
            (None, Some(copied)) => Some(Ok(copied.clone())),
            _ => self.record_keys(workspace),
        }
    }

    fn device(&self) -> Option<Ed25519PublicKey> {
        self.store
            .as_ref()
            .map(|store| Ed25519PublicKey::from_bytes(store.signer.device_public_key()))
    }

    /// Whether this device only follows its groups ([`KeyRole::Follower`]).
    fn reads_only(&self) -> bool {
        self.store
            .as_ref()
            .is_some_and(|store| store.role == KeyRole::Follower)
    }

    /// Whether this device founds `group` and commits it to its readers, as well as asking for
    /// it. A committer settles every group it takes part in. A follower settles only a call's
    /// media group: who reads one is the devices of the people the current generation of the
    /// call's rule admits, which a follower reads from the workspace's governance as every member
    /// does, and every member judges each frame of it against that rule ([`key_group::backs`]).
    /// No grant of a role decides it, so a page in a call founds its keys and admits the others
    /// in it when no other device of the call would.
    fn settles(&self, group: WorkspaceId) -> bool {
        !self.reads_only()
            || self
                .directory
                .document_scope(group)
                .is_some_and(is_call_media)
    }

    /// Whether a request waits on its signature from the device's ownership: [`Self::signatures`]
    /// then has something to await.
    #[must_use]
    pub fn signing(&self) -> bool {
        self.peers.values().any(|entry| entry.signing.is_some())
    }

    /// Await every signature the device's ownership owes, keeping each signed request for the next
    /// [`Self::pump`] to send and settling each signed frame.
    pub async fn signatures(&mut self) {
        for (workspace, owed, signing) in self.take_signing() {
            let signed = signing.await;
            self.signed(workspace, owed, signed, tilo_clock::now());
        }
    }

    /// The signatures the device's ownership owes, taken out of the service so a shared one is
    /// not held while they are awaited; each answer goes back with [`Self::signed`].
    pub fn take_signing(
        &mut self,
    ) -> Vec<(WorkspaceId, Owed, collab_types::signer::SignFuture<'static>)> {
        self.peers
            .iter_mut()
            .filter_map(|(workspace, entry)| {
                let (owed, signing) = entry.signing.take()?;
                Some((*workspace, owed, signing))
            })
            .collect()
    }

    /// Take the device's ownership's answer to what it `owed` for `workspace`: keep a signed
    /// request for the next [`Self::pump`] to send, and found or commit with a signed frame.
    pub fn signed(
        &mut self,
        workspace: WorkspaceId,
        owed: Owed,
        signed: Result<[u8; 64], collab_types::signer::SignError>,
        now: Instant,
    ) {
        let (Some(entry), Some(store)) = (self.peers.get_mut(&workspace), &self.store) else {
            return;
        };
        match (owed.0, signed) {
            (Signing::Request(mut request), Ok(signature)) => {
                request.signature = collab_types::Signature::from_bytes(signature);
                entry.request = Some(request);
                self.queued = true;
            }
            (Signing::Frame, Ok(signature)) => {
                let finished = entry.peer.signed(signature, &store.vault);
                self.finish(workspace, finished, now);
            }
            (_, Err(error)) => {
                tracing::warn!(%error, %workspace, "group frame or request was not signed");
            }
        }
    }

    /// Have the device's ownership sign the frame `workspace`'s peer staged (`message`): at once
    /// when it answers in this pass, or later through [`Self::signatures`].
    fn sign_staged(&mut self, workspace: WorkspaceId, message: Vec<u8>, now: Instant) {
        let (Some(entry), Some(store)) = (self.peers.get_mut(&workspace), &self.store) else {
            return;
        };
        let key = Arc::clone(&store.signer);
        let mut signing: SignFuture<'static> = Box::pin(async move { key.sign(message).await });
        match signing
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
        {
            std::task::Poll::Ready(answer) => {
                self.signed(workspace, Owed(Signing::Frame), answer, now);
            }
            std::task::Poll::Pending => {
                entry.signing = Some((Owed(Signing::Frame), signing));
                self.queued = true;
            }
        }
    }

    /// Settle what a signed frame of `workspace`'s did: a group founded admits the requests it
    /// holds at once, and a commit is spaced from the next.
    fn finish(
        &mut self,
        workspace: WorkspaceId,
        finished: key_group::Result<Option<key_group::Signed>>,
        now: Instant,
    ) {
        let Some(entry) = self.peers.get_mut(&workspace) else {
            return;
        };
        match finished {
            Ok(Some(key_group::Signed::Founded(true))) => {
                entry.dirty = true;
                entry.wanted = true;
            }
            Ok(Some(key_group::Signed::Reconciled(outcome))) => {
                if !outcome.refused.is_empty() {
                    tracing::warn!(%workspace, refused = outcome.refused.len(), "malformed group access requests refused");
                }
                if !outcome.changed() {
                    return;
                }
                tracing::debug!(%workspace, admitted = ?outcome.admitted, removed = ?outcome.removed, "key group changed");
                entry.committed_at = Some(now);
            }
            Ok(Some(key_group::Signed::Founded(false)) | None) => {
                // The group moved while the frame waited on its signature: look again.
                entry.dirty = true;
                self.queued = true;
                return;
            }
            Err(error) => {
                tracing::warn!(%error, %workspace, "group could not be founded or committed");
                return;
            }
        }
        let seal_check = entry.peer.seal_check();
        self.register_seal_check(workspace, seal_check);
        self.bump(workspace);
        self.queued = true;
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
        inbound: Vec<InboundGovernance>,
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
        }
        // Unopened groups are re-checked with the policy, and a failed
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
    /// arrives or an endpoint's access wakes it ([`SharedKeys::with_wake`]).
    ///
    /// A group re-checks its access policy when the directory announces a
    /// change (`Self::waits_for_policy_change`) and at the next expiry the
    /// last re-check read, which [`Self::pump`] registers with
    /// [`KeyDirectory::announce_at`]; neither is a deadline here. While
    /// this device has a group to settle, a directory that announces
    /// nothing re-checks every `RECONCILE_INTERVAL`, and frames the
    /// transport did not take are retried at the same cadence; that re-check
    /// is what lets a group this device only waits on be founded, or
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
        policy
            .into_iter()
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
    /// announcement wakes it ([`SharedKeys::arm_policy_watch`]).
    fn waits_for_policy_change(&self) -> bool {
        (!self.peers.is_empty() || self.unopened_waits_for_policy())
            && self
                .directory
                .policy_epoch()
                .is_some_and(|epoch| self.reconcile.waits_for_change(epoch))
    }

    /// Register the first instant after `wall` at which the access policy of
    /// a workspace this device takes part in can change by time alone, so the
    /// groups re-check then ([`KeyDirectory::announce_at`]).
    fn register_next_policy_change(&self, wall: u64) {
        let workspaces: BTreeSet<WorkspaceId> =
            self.peers.values().map(|entry| entry.workspace).collect();
        let next = workspaces
            .into_iter()
            .filter_map(|workspace| {
                self.directory
                    .next_policy_change(self.parent_workspace(workspace), wall)
            })
            .filter(|at| *at > wall)
            .min();
        if let Some(at) = next {
            self.directory.announce_at(at);
        }
    }

    fn receive(&mut self, inbound: InboundGovernance, now: Instant, wall: u64) {
        match inbound {
            // Governance arrives in the workspace it is for and names its group: the
            // workspace's own, or the group of a reader set of it.
            InboundGovernance::Frame { workspace, frame } => {
                if let Some(group) = self.directory.arrived(workspace, frame.workspace) {
                    self.receive_frame(group, *frame, now, wall);
                }
            }
            InboundGovernance::AccessRequest { workspace, request } => {
                if let Some(group) = self.directory.arrived(workspace, request.workspace) {
                    self.receive_request(group, *request, now, wall);
                }
            }
            InboundGovernance::DocumentFrame { scope, frame } => {
                if frame.workspace == scope.group()
                    && frame.check().is_ok()
                    && let Some(group) = self.accept_document_scope(scope, frame.signed_by, wall)
                {
                    self.receive_frame(group, *frame, now, wall);
                }
            }
            InboundGovernance::DocumentAccessRequest { scope, request } => {
                if request.workspace == scope.group()
                    && request.verify().is_ok()
                    && let Some(group) = self.accept_document_scope(scope, request.device, wall)
                {
                    self.receive_request(group, *request, now, wall);
                }
            }
        }
    }

    fn accept_document_scope(
        &mut self,
        scope: neighbour_wire::KeyScope,
        sender: Ed25519PublicKey,
        wall: u64,
    ) -> Option<WorkspaceId> {
        let readers = self.directory.scope_devices(&scope, wall)?;
        if !readers.contains(&self.device()?) || !readers.contains(&sender) {
            return None;
        }
        self.remember_scope(scope)
    }

    /// Judge the frames each group held for the policy ([`Self::receive_frame`])
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
            let allowed = self.allowed_devices(workspace, wall);
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
            // A device that was away keeps the state of the groups it takes
            // part in, and its host replays the governance it retained when
            // the device comes back, before its apps read anything: take that
            // in rather than forget it, so the group is current before the
            // first read of its record. A group this device holds no state
            // of is one it takes no part in: it never creates a second group
            // of a group whose group it has heard, and if it comes to take
            // part it asks to join.
            let opened = self.store.is_some()
                && match self.open_peer(workspace, now) {
                    Ok(opened) => opened,
                    Err(error) => {
                        tracing::warn!(%error, %workspace, "group state could not be opened");
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
        let allowed = self.allowed_devices(workspace, wall);
        let (Some(entry), Some(store)) = (self.peers.get_mut(&workspace), &self.store) else {
            return;
        };
        let unjudged = match &allowed {
            // The workspace's governance reached this device before the
            // policy here named the frame's signer.
            Some(allowed) => !key_group::backs(&frame, allowed),
            // A group competing with this device's, while it cannot name who
            // reads the group: whether its founder may is not known here.
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
                tracing::warn!(%error, %workspace, "group frame refused");
                return;
            }
        };
        tracing::debug!(%workspace, epoch, ?signer, ?event, access = ?entry.peer.access(), "key frame taken in");
        if !entry.wanted && entry.peer.access() == Access::Waiting && entry.peer.knows_group() {
            // Taken up only to create the group if no one had: someone has,
            // and its key holders admit whoever asked.
            self.peers.remove(&workspace);
            self.groups_seen.insert(workspace);
            if let Err(error) = store.state.peers().forget(workspace) {
                tracing::warn!(%error, %workspace, "unwanted group state could not be removed");
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
                // Ask again with a fresh member key; any key holder admits
                // a member asking with another key than it joined with (the
                // members that took a refused Welcome's commit hold this
                // device admitted with the key it asked with).
                entry.request = None;
                entry.retry = Backoff::now(now, REQUEST_FIRST);
                entry.fresh_at = now;
                true
            }
            PeerEvent::AdmittedWithAnotherKey => {
                // The key holders admit it again with the key it asks with
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
        let seal_check = entry.peer.seal_check();
        self.register_seal_check(workspace, seal_check);
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
        // A reader admits no one into a group it does not settle.
        if !self.settles(workspace) || request.workspace != workspace {
            return;
        }
        if !self.peers.contains_key(&workspace)
            && request.verify().is_ok()
            && self
                .allowed_devices(workspace, wall)
                .is_some_and(|allowed| {
                    allowed.contains(&request.device)
                        && self
                            .device()
                            .is_some_and(|device| allowed.contains(&device))
                })
        {
            // A device the policy allows asks for a group this device may
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
                    tracing::warn!(%error, %workspace, "group state could not be created");
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
            Err(error) => tracing::warn!(%error, %workspace, "group access request refused"),
        }
    }

    /// Take `request` and commit its device into `workspace`'s group now, rather than on this
    /// device's turn: what a device handing the workspace to a server does, so the frames the
    /// server joins from travel with the hand-over ([`Self::admitted_frames`]). Judged as every
    /// request is ([`Self::receive_request`]): the policy names both devices. The commit's
    /// signature may be owed yet ([`Self::take_signing`]).
    ///
    /// # Errors
    /// This device settles no group of the workspace, or holds no keys of it to admit with.
    pub fn admit_now(
        &mut self,
        workspace: WorkspaceId,
        request: MlsAccessRequestV1,
        now: Instant,
        wall: u64,
    ) -> Result<(), String> {
        let device = request.device;
        self.receive_request(workspace, request, now, wall);
        let entry = self
            .peers
            .get_mut(&workspace)
            .ok_or("this device takes no part in the workspace's keys")?;
        if !matches!(entry.peer.access(), Access::Ready { .. }) {
            return Err("this device holds no keys of the workspace to admit another with".into());
        }
        if entry.peer.members().contains(&device) {
            return Ok(());
        }
        entry.turn_at = Some(now);
        entry.committed_at = None;
        self.reconcile(workspace, now, wall);
        Ok(())
    }

    /// The frames `device` joins `workspace`'s group from, once this device committed it in
    /// ([`Self::admit_now`]): the group's genesis and head, the head's join material with them.
    ///
    /// # Errors
    /// The device is not in the group, or the commit still waits on its signature.
    pub fn admitted_frames(
        &self,
        workspace: WorkspaceId,
        device: &Ed25519PublicKey,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let entry = self
            .peers
            .get(&workspace)
            .ok_or("this device takes no part in the workspace's keys")?;
        if entry.signing.is_some() {
            return Err("the commit admitting the device waits on its signature".into());
        }
        if !entry.peer.members().contains(device) {
            return Err("the workspace's policy does not let this device admit that one".into());
        }
        let scope = self
            .wire_scope(workspace)
            .ok_or("the workspace's keys have no route")?;
        Ok(entry
            .peer
            .current_frames()
            .into_iter()
            .map(|frame| NeighbourMsg::MlsGovernance {
                workspace: scope.clone(),
                frame: Box::new(frame),
                delivery: 0,
            })
            .collect())
    }

    /// The frames of `workspace`'s group from `epoch` on that `asker`, a device that holds the group
    /// at `epoch`, follows to this device's head ([`GroupPeer::frames_from`]): what the workspace's
    /// host replays to a device that was away, answered by a member when the host is not there.
    /// None unless the policy at `wall` admits both the asker and this device, as an access request
    /// is judged ([`Self::receive_request`]).
    pub fn frames_for(
        &self,
        workspace: WorkspaceId,
        asker: &Ed25519PublicKey,
        epoch: u64,
        wall: u64,
    ) -> Vec<MlsGovernanceFrame> {
        let admitted = self
            .allowed_devices(workspace, wall)
            .is_some_and(|allowed| {
                allowed.contains(asker)
                    && self
                        .device()
                        .is_some_and(|device| allowed.contains(&device))
            });
        if !admitted {
            return Vec::new();
        }
        self.peers
            .get(&workspace)
            .map(|entry| entry.peer.frames_from(epoch))
            .unwrap_or_default()
    }

    fn reconcile(&mut self, workspace: WorkspaceId, now: Instant, wall: u64) {
        if !self.peers.contains_key(&workspace) {
            return;
        }
        let allowed = self.allowed_devices(workspace, wall);
        let (Some(entry), Some(store)) = (self.peers.get_mut(&workspace), &self.store) else {
            return;
        };
        entry.dirty = false;
        if workspace.is_document_key_group()
            && allowed
                .as_ref()
                .is_some_and(|readers| !readers.contains(&entry.peer.device()))
        {
            entry.wanted = false;
            entry.turn_at = None;
            entry.request = None;
            entry.signing = None;
            match entry.peer.withdraw_for_policy(&store.vault) {
                Ok(true) => self.bump(workspace),
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, group = %workspace, "could not save the key state after losing access");
                }
            }
            return;
        }
        // While this device cannot tell who holds the workspace's keys it
        // commits nothing and removes no one; nor does a reader in a group it
        // does not settle, nor a device whose last frame waits on its signature.
        let settles = store.role == KeyRole::Committer
            || self
                .directory
                .document_scope(workspace)
                .is_some_and(is_call_media);
        let Some(allowed) = allowed.filter(|_| settles && entry.signing.is_none()) else {
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
        match entry.peer.stage_reconcile(&allowed, &store.vault) {
            Ok(key_group::Staged::Sign(message)) => self.sign_staged(workspace, message, now),
            Ok(key_group::Staged::Done(outcome)) => {
                self.finish(
                    workspace,
                    Ok(Some(key_group::Signed::Reconciled(outcome))),
                    now,
                );
            }
            Err(error) => tracing::warn!(%error, %workspace, "group membership change failed"),
        }
    }

    /// Hand the transport every frame this device wrote or must publish
    /// again; kept for the next pass while the transport refuses one.
    fn publish(&mut self, workspace: WorkspaceId, transport: &dyn GovernanceTransport) {
        let Some(wire_scope) = self.wire_scope(workspace) else {
            return;
        };
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
                workspace: wire_scope.clone(),
                frame: Box::new(frame.clone()),
                delivery: self.delivery,
            });
        }
        if accepted {
            entry.peer.published();
        }
    }

    fn wire_scope(&self, group: WorkspaceId) -> Option<String> {
        if let Some(scope) = self.directory.document_scope(group) {
            return Some(scope.to_string());
        }
        (!neighbour_wire::KeyScope::requires_document(group))
            .then(|| self.directory.carrier(group).to_string())
    }

    /// Ask again at once for the groups this device still waits on: the
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

    /// Ask the key holders for access on the request backoff, while waiting.
    fn ask(
        &mut self,
        workspace: WorkspaceId,
        transport: &dyn GovernanceTransport,
        now: Instant,
        wall: u64,
    ) {
        let Some(wire_scope) = self.wire_scope(workspace) else {
            return;
        };
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
                    tracing::warn!(%error, "group access request could not be prepared");
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
            // A key that answers at once (a native one) signs in this pass; ownership that answers
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
                    tracing::warn!(%error, "group access request was not signed");
                    return;
                }
                std::task::Poll::Pending => {
                    entry.signing = Some((Owed(Signing::Request(request)), signing));
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
            workspace: wire_scope,
            request: Box::new(request),
            delivery: self.delivery,
        };
        let _ = transport.publish(message);
    }

    /// Start taking part in `group`: wait for access, and for the group to
    /// be created ([`Self::found`]). `Ok(false)` while the directory cannot
    /// name the group's readers here: a workspace whose governance is unknown
    /// and which is no personal cluster of this device's.
    fn start_peer(&mut self, workspace: WorkspaceId, now: Instant) -> key_group::Result<bool> {
        if !self.takes_part(workspace) {
            return Ok(false);
        }
        if self.open_peer(workspace, now)? {
            return Ok(true);
        }
        let store = self.store.as_ref().ok_or_else(no_store)?;
        if let Some(scope) = self.directory.document_scope(workspace) {
            store
                .state
                .keep_scope(scope)
                .map_err(key_group::Error::Storage)?;
        }
        let signer = Box::new(Arc::clone(&store.signer));
        let peer = GroupPeer::waiting(Some(store.state.peers()), workspace, signer, &store.vault)?;
        self.insert_peer(workspace, peer, now);
        Ok(true)
    }

    /// Create `group`'s group when this device created the workspace
    /// ([`KeyDirectory::founds`]) and holds its keys. A device that has
    /// seen a genesis never creates one.
    fn found(&mut self, workspace: WorkspaceId, now: Instant, wall: u64) {
        let Some(entry) = self.peers.get(&workspace) else {
            return;
        };
        if entry.peer.knows_group()
            || entry.signing.is_some()
            || self.groups_seen.contains(&workspace)
            || !self.settles(workspace)
        {
            return;
        }
        let due = self.directory.founds(workspace) && (entry.wanted || now >= entry.founding.at);
        let reads = self
            .allowed_devices(workspace, wall)
            .zip(self.device())
            .is_some_and(|(allowed, device)| allowed.contains(&device));
        let Some(entry) = self
            .peers
            .get_mut(&workspace)
            .filter(|_| self.store.is_some())
        else {
            return;
        };
        // Looked at again on every pass, a policy re-check's included.
        entry.founding.allowed = reads;
        if !reads || !due {
            return;
        }
        match entry.peer.stage_found() {
            Ok(Some(message)) => self.sign_staged(workspace, message, now),
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, %workspace, "group could not be created"),
        }
    }

    /// Take in what the store keeps and this service has not opened: its reader groups, the
    /// routes of its document groups, and the state of each group it holds. What a service reads
    /// as it opens, and again once groups kept elsewhere were written into its store.
    fn take_in_kept(&mut self) {
        let Some(store) = &self.store else {
            return;
        };
        let held = store.state.held();
        let kept = store.state.reader_groups();
        let scopes = store.state.scopes();
        for (workspace, readers) in kept {
            self.directory.group(workspace, &readers);
        }
        for scope in scopes {
            self.directory.register_scope(scope);
        }
        let now = tilo_clock::now();
        for workspace in held {
            if self.peers.contains_key(&workspace) {
                continue;
            }
            if let Err(error) = self.open_peer(workspace, now) {
                tracing::warn!(%error, %workspace, "group state could not be opened");
            }
        }
    }

    /// Open the state kept for `group`. `Ok(false)` when there is none.
    fn open_peer(&mut self, workspace: WorkspaceId, now: Instant) -> key_group::Result<bool> {
        let store = self.store.as_ref().ok_or_else(no_store)?;
        let opened = match GroupPeer::open(
            Some(store.state.peers()),
            workspace,
            Box::new(Arc::clone(&store.signer)),
            &store.vault,
        ) {
            // A machine copied from another re-keyed its device: the state
            // names the device it was copied from, whose member it is, and
            // can never act for this one, so this device takes no leaf of its
            // source's. Until the workspace admits this device, its keys
            // open what the copy holds; then the state is dropped and the
            // device joins as itself, as any further device of the member
            // does.
            Err(key_group::Error::OtherDevice) => {
                if self.takes_part(workspace) {
                    tracing::info!(
                        %workspace,
                        "group state named the device this machine was copied from; joining as this device"
                    );
                    store.state.peers().forget(workspace)?;
                    self.copied.remove(&workspace);
                } else if let Some(keys) =
                    GroupPeer::kept_keys(&*store.state.peers(), workspace, &store.vault)?
                {
                    self.copied.insert(workspace, keys);
                }
                return Ok(false);
            }
            // State in a format this build does not write: authentic and this
            // device's own, but unreadable. Dropped, so the device can ask to
            // join again; what only this device held is gone, which is why it
            // is an error.
            Err(key_group::Error::OtherFormat) => {
                tracing::error!(
                    %workspace,
                    "group state is in a format no upgrade reaches: dropping it and joining again; \
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

    fn insert_peer(&mut self, workspace: WorkspaceId, peer: GroupPeer, now: Instant) {
        let seal_check = peer.seal_check();
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
                    allowed: self.settles(workspace),
                },
                wanted: true,
                held: Vec::new(),
                welcome: None,
            },
        );
        self.register_seal_check(workspace, seal_check);
    }

    fn register_seal_check(&mut self, workspace: WorkspaceId, seal_check: key_group::SealCheck) {
        if !seal_check.encryption_required || self.registered.get(&workspace) == Some(&seal_check) {
            return;
        }
        self.seal_checks.register_seal_check(workspace, seal_check);
        self.registered.insert(workspace, seal_check);
    }

    /// Record that `workspace`'s keys or access changed, and announce it.
    ///
    /// A document's or reader set's group is a change of the workspace that
    /// carries it too: the routes it keys name that workspace, not the group,
    /// and follow the workspace's changes ([`SharedKeys::workspace_changed`]).
    fn bump(&mut self, workspace: WorkspaceId) {
        self.generation = self.generation.wrapping_add(1);
        self.changed.insert(workspace, self.generation);
        let carrier = self.directory.carrier(workspace);
        if carrier != workspace {
            self.changed.insert(carrier, self.generation);
        }
        self.changes.send_replace(self.generation);
    }
}

/// Why a group of this device's own could not be opened: no secret store
/// keeps its state. Callers check for a store first.
fn no_store() -> std::io::Error {
    std::io::Error::other("no secret store keeps this device's group state")
}

/// How long a pumping thread parks after a pass at `now`, given the
/// service's [`WorkspaceKeys::next_pump_deadline`]: until the deadline,
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
pub struct SharedKeys {
    inner: Arc<Mutex<WorkspaceKeys>>,
    wake: Option<PumpWake>,
}

impl SharedKeys {
    pub fn new(service: WorkspaceKeys) -> Self {
        Self {
            inner: Arc::new(Mutex::new(service)),
            wake: None,
        }
    }

    /// Run `operation` on the service; `None` when the lock is poisoned.
    pub(crate) fn with_service<T>(
        &self,
        operation: impl FnOnce(&mut WorkspaceKeys) -> T,
    ) -> Option<T> {
        let mut service = self.inner.lock().ok()?;
        Some(operation(&mut service))
    }

    // The native owner can hold a workspace lock when it examines history.
    // The key pump reads governance in the opposite order, so defer if busy.
    pub(crate) fn held_history_keys(
        &self,
        group: WorkspaceId,
        readers: Option<&BTreeSet<[u8; 32]>>,
    ) -> Result<ContentKeys, String> {
        let service = self
            .inner
            .try_lock()
            .map_err(|_| "the key service is busy")?;
        let entry = service
            .peers
            .get(&group)
            .ok_or("this device holds no history keys")?;
        if let Some(readers) = readers {
            let device = service.device().ok_or("the key service has no device")?;
            if !readers.contains(device.as_bytes())
                || entry
                    .peer
                    .members()
                    .iter()
                    .any(|member| !readers.contains(member.as_bytes()))
            {
                return Err("the key group is updating its readers".into());
            }
        }
        let keys = entry.peer.content_keys();
        drop(service);
        keys.ok_or_else(|| "this device holds no history keys".into())
    }

    /// See [`WorkspaceKeys::ask_on_entry`]; the ownership's signature is awaited without
    /// holding the service.
    pub async fn ask_on_entry(
        &self,
        workspace: WorkspaceId,
        now: Instant,
        wall: u64,
    ) -> Option<MlsAccessRequestV1> {
        match self.with_service(|service| service.entry_request(workspace, now, wall))?? {
            entry::EntryRequest::Signed(request) => Some(request),
            entry::EntryRequest::Unsigned(workspace, mut request, signing) => {
                let signature = signing
                    .await
                    .inspect_err(
                        |error| tracing::warn!(%error, "group access request was not signed"),
                    )
                    .ok()?;
                request.signature = collab_types::Signature::from_bytes(signature);
                self.with_service(|service| service.entry_signed(workspace, request, now))
            }
        }
    }

    /// See [`WorkspaceKeys::admit_now`]: `request`'s device committed into `workspace`'s group,
    /// and the frames it joins from ([`WorkspaceKeys::admitted_frames`]). The ownership's
    /// signature of the commit is awaited without holding the service.
    ///
    /// # Errors
    /// As there, or the service is gone.
    pub async fn admit(
        &self,
        workspace: WorkspaceId,
        request: MlsAccessRequestV1,
        wall: u64,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let device = request.device;
        let now = tilo_clock::now();
        let owed = self
            .with_service(|service| {
                service.admit_now(workspace, request, now, wall)?;
                Ok::<_, String>(service.take_signing())
            })
            .ok_or("the key service is gone")??;
        for (group, owed, signing) in owed {
            let signed = signing.await;
            self.with_service(|service| service.signed(group, owed, signed, tilo_clock::now()))
                .ok_or("the key service is gone")?;
        }
        // The commit goes to the group's other members as every commit does: the pump publishes
        // it on its next pass.
        let frames = self
            .with_service(|service| service.admitted_frames(workspace, &device))
            .ok_or("the key service is gone")?;
        if let Some(wake) = &self.wake {
            wake();
        }
        frames
    }

    /// Forget `workspace`, deleted ([`WorkspaceKeys::forget_workspace`]).
    ///
    /// # Errors
    /// What the store kept could not be removed, or the service is gone.
    pub fn forget_workspace(&self, workspace: WorkspaceId) -> Result<(), String> {
        self.with_service(|service| service.forget_workspace(workspace))
            .unwrap_or_else(|| Err("the key service is gone".into()))
    }

    /// As [`Self::new`], for a host that parks its pump until it is poked:
    /// `wake` runs whenever an endpoint's access queues governance the next
    /// pump must send, so a first group is announced or requested without
    /// waiting for a timer.
    pub fn with_wake(service: WorkspaceKeys, wake: PumpWake) -> Self {
        Self {
            inner: Arc::new(Mutex::new(service)),
            wake: Some(wake),
        }
    }

    /// `workspace`'s keys as this device holds them
    /// ([`WorkspaceKeys::record_keys`]), never opening, joining or
    /// creating anything. `None` when this device takes part in no group
    /// of the workspace; [`Err`] when it does and holds none yet.
    pub fn record_keys(&self, workspace: WorkspaceId) -> Option<Result<ContentKeys, String>> {
        self.with_service(|service| service.record_keys(workspace))
            .unwrap_or_else(|| Some(Err("the key service is unavailable".into())))
    }

    /// See [`WorkspaceKeys::reader_groups_of`]; none when the service is unavailable.
    #[must_use]
    pub fn reader_groups_of(&self, workspace: WorkspaceId) -> Vec<(WorkspaceId, u64)> {
        self.inner
            .lock()
            .map(|service| service.reader_groups_of(workspace))
            .unwrap_or_default()
    }

    /// See [`WorkspaceKeys::carrier`]; `group` itself when the service is unavailable.
    #[must_use]
    pub fn carrier(&self, group: WorkspaceId) -> WorkspaceId {
        self.inner
            .lock()
            .map_or(group, |service| service.carrier(group))
    }

    /// The scope `group`'s governance travels under ([`WorkspaceKeys::wire_scope`]): its document's
    /// route, else the workspace that carries it. `None` for a document group the service holds no
    /// route of, whose frames go as its carrier's.
    #[must_use]
    pub fn scope_of(&self, group: WorkspaceId) -> Option<String> {
        self.inner.lock().ok()?.wire_scope(group)
    }

    /// See [`WorkspaceKeys::reader_group`]; `None` also when the service is unavailable.
    pub fn reader_group(&self, workspace: WorkspaceId, readers: &str) -> Option<WorkspaceId> {
        self.inner.lock().ok()?.reader_group(workspace, readers)
    }

    /// Find the declared reader set from this device's retained group routes.
    #[must_use]
    pub fn named_readers(&self, workspace: WorkspaceId, document: &str) -> Option<String> {
        self.inner
            .try_lock()
            .ok()?
            .directory
            .named_readers(workspace, document)
    }

    /// Take in the groups written into this service's store since it opened, as it takes in
    /// those it found there when it opened: a device taking in the groups its desktop kept.
    pub fn take_in_kept(&self) {
        self.with_service(WorkspaceKeys::take_in_kept);
    }

    /// Set the exact route for a group. The directory supplies the scope's
    /// current access when the group is used.
    pub fn register_scope(&self, scope: neighbour_wire::KeyScope) -> Option<WorkspaceId> {
        self.with_service(|service| service.remember_scope(scope))
            .flatten()
    }

    /// See [`WorkspaceKeys::frames_for`], under the policy now.
    #[must_use]
    pub fn frames_for(
        &self,
        workspace: WorkspaceId,
        asker: &Ed25519PublicKey,
        epoch: u64,
    ) -> Vec<MlsGovernanceFrame> {
        self.with_service(|service| {
            service.frames_for(workspace, asker, epoch, tilo_clock::unix_now())
        })
        .unwrap_or_default()
    }

    /// See [`WorkspaceKeys::access`]; also returns the service's
    /// generation, which changes whenever any group's keys or access do.
    pub fn access(&self, workspace: WorkspaceId) -> (KeyAccess, u64) {
        self.access_at(workspace, tilo_clock::now())
    }

    /// [`Self::access`] at the caller's own `now`.
    pub fn access_at(&self, workspace: WorkspaceId, now: Instant) -> (KeyAccess, u64) {
        let Some((access, generation, queued)) = self.with_service(|service| {
            let access = service.access(workspace, now);
            (
                access,
                service.generation(),
                std::mem::take(&mut service.queued),
            )
        }) else {
            return (KeyAccess::Unavailable, 0);
        };
        if queued && let Some(wake) = &self.wake {
            wake();
        }
        (access, generation)
    }

    /// [`WorkspaceKeys::signatures`] on the shared service, never holding it while the
    /// device's ownership signs.
    pub async fn signatures(&self) {
        let Some(owed) = self.with_service(WorkspaceKeys::take_signing) else {
            return;
        };
        let mut answered = Vec::with_capacity(owed.len());
        for (workspace, owed, signing) in owed {
            answered.push((workspace, owed, signing.await));
        }
        let queued = self.with_service(|service| {
            for (workspace, owed, signed) in answered {
                service.signed(workspace, owed, signed, tilo_clock::now());
            }
            std::mem::take(&mut service.queued)
        });
        if queued == Some(true)
            && let Some(wake) = &self.wake
        {
            wake();
        }
    }

    /// Whether the device's ownership owes a signature ([`WorkspaceKeys::signing`]).
    pub fn signing(&self) -> bool {
        self.with_service(|service| service.signing())
            .unwrap_or(false)
    }

    /// Open `envelope`, a new sealed write to one of `workspace`'s groups, for
    /// the durable owner that holds the group's keys. Only under the
    /// group's current epoch, and only while this device reads the
    /// group; anything else is refused. Call it only once the envelope's
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
    ) -> Result<(ContentKeys, EncryptionMetadata, Cow<'a, [u8]>), String> {
        let metadata = envelope.encryption.ok_or("the write is not sealed")?;
        let ciphertext =
            collab_types::sealed_ciphertext(envelope).map_err(|error| error.to_string())?;
        let (KeyAccess::Ready(keys), _) = self.access(workspace) else {
            return Err("this device holds no key for the write's group".into());
        };
        Ok((keys, metadata, ciphertext))
    }

    /// The payload a renewal of `envelope`, a sealed write of this device
    /// to one of `workspace`'s groups, carries under a fresh timestamp and
    /// sequence: the identical ciphertext while its epoch is the group's
    /// current one, else its content re-sealed under the current epoch. The
    /// plaintext never leaves this call.
    pub fn renew_sealed(
        &self,
        workspace: WorkspaceId,
        envelope: &SignedOp,
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        let (KeyAccess::Ready(keys), _) = self.access(workspace) else {
            return Err("this device holds no key for the write's group".into());
        };
        crate::history::HistoryKeys::from(keys).renew(envelope)
    }

    /// See [`WorkspaceKeys::pump`].
    pub fn pump(&self, transport: &dyn GovernanceTransport) {
        let _ = self.pump_until_due(transport);
    }

    /// [`Self::pump`] at the caller's own `now`, for a host that keeps a
    /// clock of its own.
    pub fn pump_at(&self, transport: &dyn GovernanceTransport, now: Instant) {
        self.with_service(|service| service.pump(transport, now, tilo_clock::unix_now()));
    }

    /// [`Self::pump`], then how long until the protocol next has work that
    /// no message brings ([`WorkspaceKeys::next_pump_deadline`]); `None`
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
        let deadline = self.with_service(|service| {
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
    /// A device taking part in no group never arms it, and so sleeps
    /// through every announcement.
    pub fn arm_policy_watch(&self, watch: &tilo_identity::policy_change::Watch) -> bool {
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

    /// See [`WorkspaceKeys::generation`].
    pub fn generation(&self) -> u64 {
        self.inner.lock().map_or(0, |service| service.generation())
    }

    /// See [`WorkspaceKeys::workspace_change`].
    pub fn workspace_change(&self, workspace: WorkspaceId) -> u64 {
        self.inner
            .lock()
            .map_or(0, |service| service.workspace_change(workspace))
    }

    /// Wakes whenever any group's keys or access change; `None` when the
    /// service is unavailable after a poisoned lock.
    pub fn changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.inner
            .lock()
            .ok()
            .map(|service| service.changes.subscribe())
    }
}

/// Where the identity service finds this session's group encryption
/// service. The broker that owns the service can start after the identity
/// service (onboarding defers it), so the slot is filled when it does.
#[derive(Clone, Default)]
pub struct KeyProvider {
    slot: Arc<Mutex<Option<SharedKeys>>>,
    /// Counts [`Self::set`] calls, for whoever waits on the service.
    installed: Arc<tokio::sync::watch::Sender<u64>>,
}

impl KeyProvider {
    /// The slot of this session: its compositor starts one collaboration
    /// broker and one identity service, which meet here.
    pub fn session() -> Self {
        #[cfg(not(target_family = "wasm"))]
        {
            static SESSION: std::sync::OnceLock<KeyProvider> = std::sync::OnceLock::new();
            SESSION.get_or_init(Self::default).clone()
        }
        #[cfg(target_family = "wasm")]
        {
            // A page's one thread: the slot is the page's own, not a shared static.
            thread_local! {
                static SESSION: KeyProvider = KeyProvider::default();
            }
            SESSION.with(Clone::clone)
        }
    }

    /// Point the slot at a broker's service (or at none).
    pub fn set(&self, access: Option<SharedKeys>) {
        if let Ok(mut slot) = self.slot.lock() {
            *slot = access;
        }
        self.installed
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// The current service, if a broker with one is running.
    pub fn get(&self) -> Option<SharedKeys> {
        self.slot.lock().ok().and_then(|slot| slot.clone())
    }

    /// [`SharedKeys::generation`] of the service installed now, 0
    /// while none is: what a first [`Self::changed`] waits from.
    pub fn generation(&self) -> u64 {
        self.get().map_or(0, |access| access.generation())
    }

    /// Wait until any group's keys or this device's access to them may have
    /// changed since `after`, the value the previous wait returned (or
    /// [`Self::generation`] read before looking at the keys), and return the
    /// value to wait from next. As [`Self::workspace_changed`], for every
    /// workspace at once.
    pub async fn changed(&self, after: u64) -> u64 {
        self.changed_where(after, SharedKeys::generation).await
    }

    /// Wait until `workspace`'s keys or this device's access to them may have
    /// changed since `after`, the value the previous wait returned (0 at
    /// first), and return the value to wait from next.
    ///
    /// Resolves at once when the workspace changed since `after`; otherwise
    /// when it next changes, or when the service is installed, replaced or
    /// goes away. It reports only that this group, or a document's or reader
    /// set's group the workspace carries, changed, nothing about its keys: the endpoint then asks the
    /// identity service, which checks the device's trust and the person's
    /// role on every answer. Costs no wakeup while nothing changes.
    pub async fn workspace_changed(&self, workspace: WorkspaceId, after: u64) -> u64 {
        self.changed_where(after, |access| access.workspace_change(workspace))
            .await
    }

    /// Wait until `current` of the installed service differs from `after`,
    /// or the service is installed, replaced or goes away.
    async fn changed_where(&self, after: u64, current: impl Fn(&SharedKeys) -> u64 + Send) -> u64 {
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

    /// The keys of the group of `readers` in `workspace`
    /// ([`collab_types::reader_group`]; [`collab_types::EVERY_MEMBER`] is the workspace's own
    /// group) as the workspace service on this device asks for them, by the workspace's id as
    /// its documents name it: opening the group when it has none yet and `create`, and waiting up
    /// to `wait` for the group. A document's content seals under them scoped to the document
    /// ([`collab_types::content_artifact_id`]). `None` when this device takes no part in the
    /// group, or `readers` names no set; [`Err`] when it takes part and holds no keys yet.
    pub async fn group_keys(
        &self,
        workspace: &str,
        readers: &str,
        create: bool,
        wait: Duration,
    ) -> Option<Result<ContentKeys, String>> {
        let service = self.get()?;
        let scope = KeyScope::parse(workspace)?;
        let workspace = match scope {
            KeyScope::Workspace(workspace) => service.reader_group(workspace, readers)?,
            KeyScope::Document {
                readers: ref named, ..
            } if readers == named.as_deref().unwrap_or(collab_types::EVERY_MEMBER) => {
                service.register_scope(scope)?
            }
            KeyScope::Document { .. } => return None,
        };
        if !create {
            let access = self.get()?;
            // Read on the caller's thread: the service takes its lock and
            // reads its state there. The caller may hold a workspace's lock (a
            // resource write asks this while it does), so the read never waits
            // for a thread of the runtime's blocking pool: with that pool full
            // of threads that themselves wait for the same workspace, the read
            // would never start and the lock would never be let go. A worker of
            // a runtime of several threads hands its other tasks on while it
            // reads; a runtime of one thread, or a thread of its own (a
            // session's main thread, its search index), reads where it is.
            #[cfg(not(target_family = "wasm"))]
            {
                let read = move || access.record_keys(workspace);
                return match tokio::runtime::Handle::try_current() {
                    Ok(runtime)
                        if runtime.runtime_flavor()
                            == tokio::runtime::RuntimeFlavor::MultiThread =>
                    {
                        tokio::task::block_in_place(read)
                    }
                    _ => read(),
                };
            }
            // A page has one thread: the read runs on it.
            #[cfg(target_family = "wasm")]
            return access.record_keys(workspace);
        }
        self.open_record(workspace, wait).await
    }

    /// `route`'s group's keys, opening the group when it has none yet
    /// ([`WorkspaceKeys::access_record`]), and waiting up to `wait` for
    /// the group when the directory backs this device but no genesis has
    /// arrived or been admitted yet. An endpoint writing a record's first
    /// content asks this: the group is created within [`founding_wait`], and
    /// the write lands with its keys rather than failing.
    ///
    /// `None` when this device takes part in no group of the route's
    /// workspace; [`Err`] when it waited and the keys still are not there,
    /// saying how long it waited.
    pub async fn open_record(
        &self,
        workspace: WorkspaceId,
        wait: Duration,
    ) -> Option<Result<ContentKeys, String>> {
        let deadline = tilo_clock::now()
            .checked_add(wait)
            .unwrap_or_else(Instant::now);
        loop {
            let access = self.get()?;
            // Read before the ask: a change between the two is no wait at all.
            let after = access.workspace_change(workspace);
            let (state, _) = access.access(workspace);
            match state {
                KeyAccess::Ready(keys) => return Some(Ok(keys)),
                KeyAccess::Waiting => {}
                KeyAccess::Removed => {
                    return Some(Err(
                        "this device was removed from the workspace's group".into()
                    ));
                }
                KeyAccess::Unavailable | KeyAccess::NoSecretStore => return None,
            }
            let remaining = deadline.saturating_duration_since(tilo_clock::now());
            if remaining.is_zero() {
                return Some(Err(not_ready(wait)));
            }
            if !within(remaining, self.workspace_changed(workspace, after)).await {
                return Some(Err(not_ready(wait)));
            }
        }
    }
}

/// Whether `changed` finishes before `wait` passes. On a Tokio runtime its timer measures the
/// wait. A caller that polls on a thread of its own (a session's main thread, its search index)
/// holds no runtime and no timer, so a thread measures the wait for it, and ends as soon as
/// `changed` does.
async fn within<F: std::future::Future>(wait: Duration, changed: F) -> bool {
    #[cfg(not(target_family = "wasm"))]
    if tokio::runtime::Handle::try_current().is_err() {
        let (passed, deadline) = tokio::sync::oneshot::channel::<()>();
        // Dropped when this returns, which ends the thread's wait at once.
        let (done, ended) = std::sync::mpsc::channel::<()>();
        let measuring = std::thread::Builder::new()
            .name("tilo-keys-wait".into())
            .spawn(move || {
                if ended.recv_timeout(wait) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
                    let _ = passed.send(());
                }
            });
        if let Err(error) = measuring {
            tracing::error!(%error, "no thread measures the wait for a group's keys: it ends now");
            return false;
        }
        let changed = std::pin::pin!(changed);
        let finished = tokio::select! {
            _ = changed => true,
            _ = deadline => false,
        };
        drop(done);
        return finished;
    }
    tokio::time::timeout(wait, changed).await.is_ok()
}

/// What a caller waiting `wait` for a group's keys is told when they never
/// arrived: how long it waited, so whoever reads it knows what the wait was
/// and that nothing more is coming.
fn not_ready(wait: Duration) -> String {
    format!("the group's keys did not arrive within {}s", wait.as_secs())
}

/// The identity service's route keys, from this host's key service once
/// one runs.
impl tilo_identity::sources::GroupKeySource for KeyProvider {
    fn access(
        &self,
        workspace: WorkspaceId,
        known_generation: u64,
    ) -> Option<Result<tilo_identity::sources::GroupKeys, String>> {
        let groups = self.get()?;
        let (access, generation) = groups.access(workspace);
        Some(wire_keys(access, generation, known_generation))
    }

    fn document_access(
        &self,
        workspace: WorkspaceId,
        app: &str,
        document: &str,
        known_generation: u64,
    ) -> Option<Result<tilo_identity::sources::GroupKeys, String>> {
        let groups = self.get()?;
        let Some(scoped) = groups.with_service(|service| {
            service
                .directory
                .requires_document_keys(workspace, app, document)
        })?
        else {
            return Some(wire_keys(
                KeyAccess::Waiting,
                groups.generation(),
                known_generation,
            ));
        };
        let named = if KeyScope::has_named_readers(document) {
            let Some(readers) = groups.named_readers(workspace, document) else {
                return Some(wire_keys(
                    KeyAccess::Waiting,
                    groups.generation(),
                    known_generation,
                ));
            };
            Some(readers)
        } else {
            None
        };
        let original = match &named {
            Some(readers) => groups.reader_group(workspace, readers)?,
            None => workspace,
        };
        if !scoped {
            return self.access(original, known_generation);
        }
        let scope = KeyScope::document(workspace, app, document, named.as_deref())?;
        let group = groups.register_scope(scope)?;
        let (mut access, _) = groups.access(group);
        if let KeyAccess::Ready(keys) = &mut access
            && let (KeyAccess::Ready(history), _) = groups.access(original)
        {
            *keys = keys.clone().with_workspace_history(history);
        }
        // A policy edit can select a different group without moving either
        // group yet. Distinguish that transition from an unchanged answer.
        let generation = groups.with_service(|service| service.generation())? | (1 << 63);
        Some(wire_keys(access, generation, known_generation))
    }
}

fn wire_keys(
    access: KeyAccess,
    generation: u64,
    known_generation: u64,
) -> Result<tilo_identity::sources::GroupKeys, String> {
    use tilo_identity_client::wire::KeyAccess as Wire;
    // A caller holding no keys asks with generation 0, which is also
    // the generation of a service that has changed nothing since it
    // started (a group it restored from disk, after a restart): it is
    // sent the keys, never told they are unchanged.
    let held = known_generation != 0 && generation == known_generation;
    match access {
        KeyAccess::Ready(keys) if !held => keys
            .to_bytes()
            .map(|bytes| (Wire::Ready, generation, bytes.to_vec()))
            .map_err(|error| error.to_string()),
        KeyAccess::Ready(_) => Ok((Wire::Ready, generation, Vec::new())),
        KeyAccess::Waiting => Ok((Wire::Waiting, generation, Vec::new())),
        KeyAccess::Removed => Ok((Wire::Removed, generation, Vec::new())),
        KeyAccess::Unavailable => Ok((Wire::Unavailable, generation, Vec::new())),
        KeyAccess::NoSecretStore => Ok((Wire::NoSecretStore, generation, Vec::new())),
    }
}

mod entry;
mod handover;
mod reader_groups;

#[cfg(test)]
mod tests {
    include!("encryption_tests.rs");
}
