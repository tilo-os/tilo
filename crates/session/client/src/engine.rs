//! This device's workspaces: the engine that holds the state of every workspace
//! the person joined, and the enrollment that brings a joined workspace's state
//! here.
//! A workspace's state — its people, what its apps recorded, its history — is held
//! by the peer-state engine beside the layout ([`NativeWorkspaceOwner`]), in a file of
//! its own under this device's state ([`FILE`]). Joining redeems the invitation its
//! entry answered with ([`tilo_peer_state::owner::enroll::prepare_and_complete`]),
//! and a late joiner reads the whole history, not only what is recorded from now
//! on. The engine keeps which workspaces are on the layout ([`Shown`]), because an enrollment
//! installs into the workspace made for it.

// The page's session constructs no engine: it wires its workspaces with its own host, so only a
// desktop's build reads the state held here.
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page's session wires its workspaces with its own host"
    )
)]
use std::collections::{BTreeMap, BTreeSet};
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tilo_clock::time::SystemTime;

use tilo_action_policy::WorkspacePolicy;
use tilo_desktop_model::WorkspaceId;
use tilo_identity::identity_store::DeviceIdentity;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_peer_state::join::Role;
#[cfg(not(target_arch = "wasm32"))]
use tilo_peer_state::owner::Change;
use tilo_peer_state::owner::NativeWorkspaceOwner;
use tilo_peer_state::owner::{DocumentRead, History, Holding};
#[cfg(not(target_arch = "wasm32"))]
use tilo_peer_state::owner::{FileHost, open_file_with, owner_file_of};
use tilo_peer_state::{KeptDocument, Member};
use tilo_user_identity::RevocationList;
use tilo_workspace_keys::{KeyAccess, KeyProvider};

#[cfg(not(target_arch = "wasm32"))]
use crate::identity;
use crate::membership_check::MembershipCheck;
use crate::workspace_keys;

mod resources;

/// The engine's record, under this device's state directory: the name an earlier build kept it
/// under, so the state it wrote is still read.
const FILE: &str = "workspaces.json";

/// The largest document the session reads out of a workspace.
const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

/// Why a workspace is not the person's to delete here, when it is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotDeletable {
    /// This device does not hold it.
    NotHeld,
    /// Someone else is still in it, so it is not this device's to delete.
    OthersInIt,
    /// Another of the person's devices holds it too: deleting it here would not delete it for them.
    OtherDevice,
    /// It belongs to someone else.
    NotOwner,
}

/// The part an invitation brings someone in at: a role, and whether it also makes them an
/// administrator (an Editor with every governance action).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Part {
    pub role: Role,
    pub administrator: bool,
}

impl Part {
    /// A part at `role` alone.
    #[must_use]
    pub const fn role(role: Role) -> Self {
        Self {
            role,
            administrator: false,
        }
    }

    /// An administrator: an Editor the invitation also makes an administrator.
    #[must_use]
    pub const fn administrator() -> Self {
        Self {
            role: Role::Editor,
            administrator: true,
        }
    }
}

/// An invitation this device made: the link a person hands over, the part it brings whoever redeems
/// it in at, and when it stops working.
#[derive(Clone, Debug)]
pub struct Issued {
    /// The invitation's id, as the workspace's own records name it.
    pub invite: String,
    /// The text a device joins with.
    pub link: String,
    /// The part it brings the person in at.
    pub role: Role,
    /// When it stops working, in nanoseconds since the Unix epoch.
    pub expires_at_ns: u64,
}

/// The workspaces on this device's layout: an enrollment installs into the workspace made for it.
#[derive(Default)]
pub struct Shown(std::sync::Mutex<BTreeSet<WorkspaceId>>);

impl std::fmt::Debug for Shown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.0.lock_recover().iter()).finish()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl FileHost for Shown {
    fn shows(&self, workspace: WorkspaceId) -> bool {
        self.0.lock_recover().contains(&workspace)
    }

    /// A workspace this device lets go of leaves its layout with the record that drops it.
    fn publish(
        &self,
        _next: &tilo_peer_state::owner::NativeAuthorityStorage,
        change: Change,
        write: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let written = write();
        if let Change::Released { workspace } = change {
            self.0.lock_recover().remove(&workspace);
        }
        written
    }
}

/// What this device may seal a workspace's record with ([`Engine::seal_keys`]): the channel's keys,
/// the wait for them, or this device's own lack of a secret store to hold any.
pub enum SealKeys {
    /// The keys this device seals the record's writes with.
    Here(key_group::ContentKeys),
    /// The keys are not here yet: the answer arrives as the workspace's other messages do, and the
    /// write waits for it.
    Waiting,
    /// This device has no secret store, so it holds no channel keys of any
    /// workspace: a sealed record is one it can neither write nor read here,
    /// and no workspace refused anything.
    NoSecretStore,
}

/// Who hosts a workspace this device holds, as its record says ([`Engine::hosting`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hosting {
    /// The host's name, as its person gave it; empty when the record names no host.
    pub host: String,
    /// The host is this device.
    pub here: bool,
    /// This device is the workspace's member: its host took it in.
    pub admitted: bool,
}

/// Whether a workspace admitted this device, as this device's copy of it says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admission {
    Admitted,
    /// This machine was copied from another device, and the workspace has not admitted it yet:
    /// its copy still names `copied_from`. It reads what the copy holds as that device could, and
    /// writes nothing until it is admitted.
    Awaiting {
        copied_from: [u8; 32],
    },
}

impl Admission {
    fn of(view: &tilo_peer_state::owner::NativeCommittedView, own: [u8; 32]) -> Self {
        match view.device().device_public_key {
            held if held == own => Self::Admitted,
            copied_from => Self::Awaiting { copied_from },
        }
    }

    /// The device whose permissions decide what this device reads of the workspace.
    fn reader(self, own: [u8; 32]) -> [u8; 32] {
        match self {
            Self::Admitted => own,
            Self::Awaiting { copied_from } => copied_from,
        }
    }
}

/// This device's workspaces, and the identity they are held with.
pub struct Engine {
    owner: NativeWorkspaceOwner,
    shown: Arc<Shown>,
    identity: DeviceIdentity,
    /// Where the workspaces' channels find this device's keys, once the channel service is running:
    /// the session opens the workspaces before it opens the service, and every reader of a sealed
    /// record goes through this.
    keys: KeyProvider,
    /// How this host proves this device still owns a workspace before it commits there.
    ownership: Arc<dyn crate::membership_check::MembershipCheck>,
    /// The workspace transport of this session's peer network, once it runs: a record the
    /// workspace's hosts send only once asked is asked for here, and the peer network is gone while this
    /// is `None` .
    link: std::sync::Mutex<Option<Arc<workspace_keys::KeyLink>>>,
    /// The layout's workspaces this device could not found here: founding one is asked for again only
    /// by a later session, not by every pass over the layout.
    unfounded: std::sync::Mutex<std::collections::BTreeSet<WorkspaceId>>,
    /// The layout's workspaces being founded now: a pass that comes while one is signed founds it
    /// no second time.
    founding: std::sync::Mutex<std::collections::BTreeSet<WorkspaceId>>,
    /// Ticks whenever a founding ends, founded or not: what an ask of a workspace being founded
    /// waits on ([`Self::ask_holder`]).
    founded: tokio::sync::watch::Sender<u64>,
    /// What lands here from the workspaces' other devices: what a read waiting for a record it
    /// holds none of wakes on.
    arrivals: Arc<crate::arrivals::Arrivals>,
    /// This device's own writes its replica has not taken in yet ([`HeldWrites`]).
    held: HeldWrites,
    /// Keys of workspaces whose group this device takes no part in, handed to it by their host
    /// ([`Self::hand_keys`]): a sealed record they open reads with them.
    handed: Arc<std::sync::Mutex<RouteKeys>>,
    /// The keys each record's channel last gave this device: they seal and open while the channel
    /// asks for access again.
    group_keys: std::sync::Mutex<RouteKeys>,
    /// Sealed records as this device last opened them for a reader, so the next read opens only
    /// what was sealed since: a record its apps follow is opened once per write, not whole per read.
    kept: std::sync::Mutex<KeptResources>,
    /// Where this device's own workspace service keeps the workspaces it holds, under the state
    /// the engine was opened on; `None` for an engine that keeps no state.
    service_root: Option<std::path::PathBuf>,
    /// Where a workspace this device makes is founded: its own workspace service, once it is open
    /// ([`Self::serve_founding`]).
    founder: std::sync::OnceLock<Arc<dyn Founder>>,
    /// When the links out of each workspace lapse, as its holder lists them: a joiner redeeming
    /// one dials this device, which stays dialable while one is open ([`Self::invitations_open`]).
    open_links: std::sync::Mutex<std::collections::BTreeMap<WorkspaceId, Vec<u64>>>,
    /// Each workspace's resources, as the store holds them ([`resources`]).
    stores: resources::Stores,
}

/// One holder of a workspace, as a member's device reads it off the holder's
/// own signed address ([`Engine::holders`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    /// Its device key, hex.
    pub device: String,
    /// What it calls itself: its server's name from its person's admin
    /// workspace, or its site's name; its device's label when it names none.
    pub label: String,
    /// Whether it holds the workspace's keys: it reads the workspace. A store
    /// holds its ciphertext and reads none of it.
    pub reads: bool,
    /// Whether the machine under it is rented from a cloud provider, whose
    /// operator can read what it holds.
    pub cloud: bool,
    /// Whether it has a moderation agent for the workspace, which a person
    /// who governs the workspace may turn on.
    pub moderates: bool,
}

/// One member's delegation of their own work to a server, as the workspace
/// holds it and every member reads it ([`Engine::delegations`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delegation {
    /// The row it is kept under: the server's device key and the member's
    /// user root, hex.
    pub key: String,
    /// Whose work it is: their user root, hex.
    pub person: String,
    /// The server that may take it: its device key, hex.
    pub server: String,
    /// The agent of theirs it acts as, when it acts as one.
    pub agent: Option<String>,
    /// What it may take, by the paths the workspace decides actions by.
    pub grants: Vec<tilo_action_policy::Grant>,
    /// When it stops working, ns since the epoch.
    pub expires_at_ns: u64,
    /// Whether this device signed it: only that device can revoke it.
    pub revocable: bool,
}

/// What a workspace's holder answers an ask of its governance, once it has.
#[cfg(not(target_arch = "wasm32"))]
pub type ServiceAnswer =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send>>;

/// What a workspace's holder answers an ask of its governance, once it has.
#[cfg(target_arch = "wasm32")]
pub type ServiceAnswer =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, String>>>>;

/// What this device asks of the workspace services, once its own is open: founding a workspace
/// this device made there, and asking a workspace one of its governance actions as this device,
/// a member of it, of whichever service holds it.
pub trait Founder: Send + Sync {
    /// Found `workspace` under `name`, answered once this device holds it.
    fn found(
        &self,
        workspace: WorkspaceId,
        name: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>>;

    /// Ask `workspace`'s governance action `action` with `arguments` of its holder: this
    /// device's own service when it holds `workspace`, else the holder its enrollment names, on
    /// the device action service. `None` when this device reaches no holder of it.
    fn ask(
        &self,
        workspace: WorkspaceId,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<ServiceAnswer>;

    /// Ask this device's own host its own action `action` with `arguments`, in process, as the
    /// person at this device: what a window of theirs asks through the app broker, such as the
    /// machines their Servers workspace holds. `None` when this device serves no such host of its own.
    fn ask_host(
        &self,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<ServiceAnswer> {
        let _ = (action, arguments);
        None
    }
}

/// The sealed resources this device keeps opened, by workspace, app and document id, with the
/// keys that opened each. Each is behind a lock of its own, held while it is read: a second read
/// of the same resource waits for the first and goes on from what it opened, rather than opening
/// it whole beside it.
type KeptResources = BTreeMap<(WorkspaceId, String, String), Arc<std::sync::Mutex<KeptResource>>>;

/// One sealed resource as this device last opened it, with the keys that opened it; `None` until
/// a read opened it.
type KeptResource = Option<(KeysUsed, KeptDocument)>;

/// The keys a sealed record was opened with: the channel and epoch of the record's own keys and of
/// the channel's history keys. A rotation moves the epoch.
type KeysUsed = [Option<(collab_types::WorkspaceId, u64)>; 2];

/// What was kept opened of a record, when it was opened with exactly the keys `used` now: a
/// record opened under other keys is forgotten.
fn kept_under<K: Default>(kept: Option<(KeysUsed, K)>, used: &KeysUsed) -> K {
    match kept {
        Some((opened_with, kept)) if opened_with == *used => kept,
        _ => K::default(),
    }
}

/// Read a document with what this device kept of it, holding `opening`, the lock a second read
/// of the same resource waits on, until what the read opened is kept under `used`.
fn opened_under<T>(
    opening: Option<&std::sync::Mutex<KeptResource>>,
    used: &KeysUsed,
    read: impl FnOnce(&mut KeptDocument) -> T,
) -> T {
    let Some(opening) = opening else {
        return read(&mut KeptDocument::default());
    };
    let mut held = opening.lock_recover();
    let mut kept = kept_under(held.take(), used);
    let value = read(&mut kept);
    if kept.is_open() {
        *held = Some((*used, kept));
    }
    value
}

/// Keys, by workspace.
type RouteKeys = BTreeMap<WorkspaceId, key_group::ContentKeys>;

/// Opens one sealed write of a record into its plaintext.
type OpenSealed<'a> = &'a mut dyn FnMut(&collab_types::SignedOp) -> Result<Vec<u8>, String>;

/// How many sealed records [`Engine::read_resource`] keeps opened between reads.
const KEPT_RECORDS: usize = 64;

/// A record's bytes, with the heads they were last saved at: what [`Engine::record_since`] answers
/// when the record changed since the caller last read it.
type SavedRecord = Option<(Vec<u8>, [u8; 32])>;

/// The writes this device made that its replica has not taken in yet, by record, as document
/// frames in the clear: they wait for their record's keys (nothing goes out unsealed) or for workspace
/// held. The session that writes them keeps them here, and its readers read them over the
/// replica ([`Engine::read_resource`]); nobody else sees them until they land.
#[derive(Clone, Default)]
pub struct HeldWrites(Arc<std::sync::Mutex<BTreeMap<DocumentRoute, Vec<Vec<u8>>>>>);

impl HeldWrites {
    /// The held writes, to read or change.
    pub fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<DocumentRoute, Vec<Vec<u8>>>> {
        self.0.lock_recover()
    }
}

/// What a read of one of a workspace's records found ([`Engine::read_resource`]).
pub enum ResourceReading {
    /// The record as the workspace holds it, sealed writes opened with the record's channel keys.
    Captured {
        /// Names exactly these heads: equal revisions are equal documents.
        revision: [u8; 32],
        /// The document's heads, sorted.
        heads: Vec<[u8; 32]>,
        bytes: Vec<u8>,
    },
    /// Only the changes since the heads the caller holds, which they complete to `heads`.
    Changes {
        revision: [u8; 32],
        heads: Vec<[u8; 32]>,
        bytes: Vec<u8>,
    },
    /// The record stands where the caller last read it.
    Unchanged([u8; 32]),
    /// The record is not here yet: the workspace's host was asked for it, or the record's keys are
    /// on their way, and it reads once they are.
    Pending,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("workspaces", &self.workspaces())
            .finish_non_exhaustive()
    }
}

/// This device's channel keys, as its owner compacts the sealed histories of its records with
/// them wherever it writes, linked or not
/// ([`NativeWorkspaceOwner::keep_sealed_histories`]): those the workspace's host handed it
/// ([`Engine::hand_keys`]), else its own channels'. A device that takes no part in a workspace's
/// group (the host's own actions, which act as the host device) writes with the keys it was
/// handed, so it compacts with them too: its records' histories fill with its writes, and a full
/// one takes no write until it is compacted.
pub struct HistoryKeys {
    keys: KeyProvider,
    handed: Arc<std::sync::Mutex<RouteKeys>>,
}

impl HistoryKeys {
    /// The keys of `keys`' channels alone: a device whose host hands it none.
    #[must_use]
    pub fn new(keys: KeyProvider) -> Self {
        Self {
            keys,
            handed: Arc::default(),
        }
    }

    fn handed(&self, workspace: WorkspaceId) -> Option<tilo_workspace_keys::HistoryKeys> {
        self.handed
            .lock_recover()
            .get(&workspace)
            .cloned()
            .map(tilo_workspace_keys::HistoryKeys::from)
    }

    fn service(&self) -> Result<tilo_workspace_keys::SharedKeys, String> {
        self.keys
            .get()
            .ok_or_else(|| "this device's channel service is not running".to_owned())
    }
}

impl tilo_peer_state::owner::SealedHistoryKeys for HistoryKeys {
    fn open(
        &self,
        workspace: WorkspaceId,
        write: &collab_types::SignedOp,
    ) -> Result<Vec<u8>, String> {
        match self.handed(workspace) {
            Some(handed) => handed.open(write),
            None => self.service()?.open_history_write(workspace, write),
        }
    }

    fn seal_checkpoint(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
        snapshot: &[u8],
    ) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> {
        match self.handed(workspace) {
            Some(handed) => handed.seal_checkpoint(app, record, snapshot),
            None => self
                .service()?
                .seal_checkpoint(workspace, app, record, snapshot),
        }
    }

    fn renew(
        &self,
        workspace: WorkspaceId,
        write: &collab_types::SignedOp,
    ) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> {
        match self.handed(workspace) {
            Some(handed) => handed.renew(write),
            None => self.service()?.renew_sealed(workspace, write),
        }
    }
}

impl Engine {
    /// This device's workspaces as `owner` holds them, with the identity they are held under: what
    /// a host supplies whichever file, store or record it keeps them in.
    #[must_use]
    pub fn holding(
        owner: NativeWorkspaceOwner,
        shown: Arc<Shown>,
        identity: DeviceIdentity,
        ownership: Arc<dyn MembershipCheck>,
    ) -> Self {
        let keys = KeyProvider::default();
        let handed = Arc::<std::sync::Mutex<RouteKeys>>::default();
        owner.keep_sealed_histories(Arc::new(HistoryKeys {
            keys: keys.clone(),
            handed: Arc::clone(&handed),
        }));
        Self {
            owner,
            shown,
            identity,
            keys,
            ownership,
            link: std::sync::Mutex::new(None),
            unfounded: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            founding: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            founded: tokio::sync::watch::Sender::new(0),
            arrivals: Arc::new(crate::arrivals::Arrivals::default()),
            handed,
            group_keys: std::sync::Mutex::default(),
            held: HeldWrites::default(),
            kept: std::sync::Mutex::default(),
            service_root: None,
            founder: std::sync::OnceLock::new(),
            open_links: std::sync::Mutex::default(),
            stores: std::sync::Mutex::default(),
        }
    }

    /// Open this device's workspaces under `state`, with the workspaces the layout
    /// already holds. A session that keeps no state (a screenshot, a test without a
    /// state directory) holds no workspace.
    /// # Errors
    /// The record holds no valid state, or the file cannot be written.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(
        identity: DeviceIdentity,
        state: Option<&Path>,
        workspaces: impl IntoIterator<Item = WorkspaceId>,
        ownership: Arc<dyn MembershipCheck>,
    ) -> Result<Self, String> {
        let state = state.ok_or("this session keeps no state, so it holds no workspace")?;
        let path = owner_path(state)?;
        let shown = Arc::new(Shown(std::sync::Mutex::new(
            workspaces.into_iter().collect(),
        )));
        let owner = open_file_with(&path, Arc::clone(&shown) as Arc<dyn FileHost>)?;
        let mut engine = Self::holding(owner, shown, identity, ownership);
        engine.service_root = Some(state.join("tilo").join("workspace-service"));
        Ok(engine)
    }

    /// This device's engine, when this computer has an identity and a state directory to keep its
    /// workspaces in.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn for_this_device(
        state: Option<&Path>,
        workspaces: impl IntoIterator<Item = WorkspaceId>,
        ownership: Arc<dyn MembershipCheck>,
    ) -> Result<Self, String> {
        let identity = identity::device_identity()
            .ok_or("this computer has no identity yet, so it holds no workspace")?;
        Self::open(identity, state, workspaces, ownership)
    }

    /// Where this device's own workspace service keeps its workspaces; `None` for an engine that
    /// keeps no state.
    #[must_use]
    pub fn service_root(&self) -> Option<std::path::PathBuf> {
        self.service_root.clone()
    }

    /// The workspace ids this device holds.
    pub fn workspaces(&self) -> Vec<WorkspaceId> {
        self.owner.workspaces()
    }

    /// Whether this device holds this workspace.
    #[must_use]
    pub fn holds(&self, workspace: WorkspaceId) -> bool {
        self.owner.contains_workspace(workspace)
    }

    /// The name `workspace`'s governance records: the workspace's one name, which every device and
    /// every list of it reads. `None` for a workspace this device does not hold, or one whose
    /// governance names it nowhere (a workspace a build before the name was governance made).
    #[must_use]
    pub fn workspace_name(&self, workspace: WorkspaceId) -> Option<String> {
        self.owner
            .with_current(workspace, |view| {
                Ok(view.peer().governance().workspace_name())
            })
            .ok()
            .flatten()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
    }

    /// The host `workspace` waits on to take this device in, by the name its person gave it,
    /// while this device holds the workspace but is not its member yet: the workspace's record
    /// names another device (the one this machine was copied from) until its host admits this
    /// one, and only the host can. `None` for a workspace this device is a member of, or does not
    /// hold.
    #[must_use]
    pub fn awaited_host(&self, workspace: WorkspaceId) -> Option<String> {
        self.hosting(workspace)
            .filter(|hosting| !hosting.admitted)
            .map(|hosting| hosting.host)
    }

    /// Who hosts `workspace` as this device's record of it says, whether that is this device,
    /// and whether this device is admitted to it yet ([`Self::awaited_host`]). `None` for a
    /// workspace this device does not hold.
    #[must_use]
    pub fn hosting(&self, workspace: WorkspaceId) -> Option<Hosting> {
        if !self.holds(workspace) {
            return None;
        }
        let own = self.identity.device_public_key();
        self.owner
            .with_current(workspace, |view| {
                let hint = view.transport_hint().ok();
                Ok(Hosting {
                    host: hint
                        .map(|hint| hint.host.device_label.clone())
                        .unwrap_or_default(),
                    here: hint.is_some_and(|hint| hint.host.is_of_device(&own)),
                    admitted: Admission::of(view, own) == Admission::Admitted,
                })
            })
            .ok()
    }

    /// What the workspace `workspace` holds of the app `app`'s `record`: the document
    /// as the workspace last admitted it, also what it held before this device joined.
    /// A sealed record opens with this device's channel keys, which reach back past the
    /// membership it joined in; a device that holds none yet asks for them
    /// ([`Self::seal_keys`]) and reads the record once they are here. A record the
    /// hosts send only once asked is asked for on its first read.
    /// # Errors
    /// The workspace is not held here, this device may not read the record, the record
    /// is sealed and this device holds no keys of it, or the document cannot be read.
    pub fn record(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        self.record_since(workspace, app, record, None)
            .map(|read| read.map(|(bytes, _)| bytes))
    }

    /// What `workspace` holds of `app`'s `record` for a reader that merged it last at the sorted
    /// `heads` an earlier read handed out, or holds nothing of it: only the changes since those
    /// heads where the record holds them ([`ResourceReading::Changes`]), the record whole where it
    /// does not, so a reader that follows a record merges what changed rather than all of it.
    /// # Errors
    /// As [`Self::record`].
    pub fn record_update(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
        heads: Option<&[[u8; 32]]>,
    ) -> Result<ResourceReading, String> {
        let heads = heads.unwrap_or_default();
        self.read_resource(
            workspace,
            (app, record),
            Holding {
                revision: (!heads.is_empty())
                    .then(|| tilo_peer_state::owner::revision_of_heads(heads)),
                heads,
            },
            MAX_DOCUMENT_BYTES,
        )
    }

    /// [`Self::record`], with the revision the bytes were saved at, and told the revision
    /// `known` this caller already holds: a record whose admitted heads still are those
    /// is answered as unchanged without capturing or saving the document at all.
    /// # Errors
    /// As [`Self::record`].
    pub fn record_since(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
        known: Option<[u8; 32]>,
    ) -> Result<SavedRecord, String> {
        Ok(
            match self.read_resource(
                workspace,
                (app, record),
                Holding {
                    revision: known,
                    heads: &[],
                },
                MAX_DOCUMENT_BYTES,
            )? {
                ResourceReading::Captured {
                    revision, bytes, ..
                } => Some((bytes, revision)),
                ResourceReading::Changes { .. }
                | ResourceReading::Unchanged(_)
                | ResourceReading::Pending => None,
            },
        )
    }

    /// What `workspace` holds of `app`'s `record` now, for the reader this device is: at most
    /// `maximum` bytes of it, and the heads it stands at; for a reader `holding` heads an earlier
    /// read handed it, only the changes since.
    ///
    /// # Errors
    /// The workspace is not held here, this device may not read the record, the record is sealed
    /// and this device holds no keys of it, or the document cannot be read.
    pub fn read_resource(
        &self,
        workspace: &str,
        (app, record): (&str, &str),
        holding: Holding<'_>,
        maximum: usize,
    ) -> Result<ResourceReading, String> {
        let Some(held) = self.own_writes(workspace, app, record) else {
            return self.read_replica(workspace, (app, record), holding, maximum);
        };
        if let Some(reading) =
            self.read_own_since(workspace, (app, record), holding, maximum, &held)?
        {
            return Ok(reading);
        }
        // This device's own writes the replica has not taken in yet read over what it holds.
        let base = match self.read_replica(workspace, (app, record), Holding::default(), maximum) {
            Ok(ResourceReading::Captured { bytes, .. }) => Some(bytes),
            _ => None,
        };
        let author = self.identity.device_cert.device_public_key;
        let tilo_peer_state::document_frame::OwnRead {
            bytes,
            heads,
            revision,
        } = tilo_peer_state::document_frame::with_own_writes(base.as_deref(), author, &held)?;
        if holding.revision == Some(revision) {
            return Ok(ResourceReading::Unchanged(revision));
        }
        Ok(ResourceReading::Captured {
            revision,
            heads,
            bytes,
        })
    }

    /// [`Self::read_resource`] of a record this device wrote to and its replica has not taken in yet
    /// (`held`), for a reader `holding` heads of an earlier read: what the replica changed since what
    /// the reader holds of it, then those of the writes the reader lacks, so a record that
    /// changes while this device's writes wait is never read whole. `None` where only a whole read
    /// answers: the reader holds no heads, or one the replica and the writes do not, or a write
    /// carries the record whole.
    fn read_own_since(
        &self,
        workspace: &str,
        route: (&str, &str),
        holding: Holding<'_>,
        maximum: usize,
        held: &[Vec<u8>],
    ) -> Result<Option<ResourceReading>, String> {
        if holding.heads.is_empty() {
            return Ok(None);
        }
        let author = self.identity.device_cert.device_public_key;
        let Ok(Some(own)) = tilo_peer_state::document_frame::own_changes(author, held) else {
            return Ok(None);
        };
        let mut failed = None;
        let read = tilo_peer_state::document_frame::read_after_own(holding.heads, &own, |base| {
            let replica = self.read_replica(
                workspace,
                route,
                Holding {
                    revision: Some(tilo_peer_state::owner::revision_of_heads(base)),
                    heads: base,
                },
                maximum,
            );
            match replica {
                Ok(ResourceReading::Changes { bytes, heads, .. }) => Some((bytes, heads)),
                Ok(ResourceReading::Unchanged(_)) => Some((Vec::new(), base.to_vec())),
                Ok(ResourceReading::Captured { .. } | ResourceReading::Pending) => None,
                Err(error) => {
                    failed = Some(error);
                    None
                }
            }
        });
        if let Some(error) = failed {
            return Err(error);
        }
        let Some((bytes, heads)) = read.filter(|(bytes, _)| bytes.len() <= maximum) else {
            return Ok(None);
        };
        let revision = tilo_peer_state::owner::revision_of_heads(&heads);
        if bytes.is_empty() && heads.as_slice() == holding.heads {
            return Ok(Some(ResourceReading::Unchanged(revision)));
        }
        Ok(Some(ResourceReading::Changes {
            revision,
            heads,
            bytes,
        }))
    }

    /// The revision `workspace`'s `app`'s `record` stands at for the reader this device is,
    /// decided as [`Self::read_resource`] decides it but reading nothing out: `None` while the
    /// record is not here yet, or only a read decides it (this device's own writes the replica
    /// has not taken in yet).
    ///
    /// # Errors
    /// As [`Self::read_resource`].
    pub fn document_revision(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<[u8; 32]>, String> {
        if self.own_writes(workspace, app, record).is_some() {
            return Ok(None);
        }
        let own = self.identity.device_cert.device_public_key;
        Ok(self
            .opened(workspace, (app, record), &mut |workspace, open, kept| {
                self.owner.with_current(workspace, |view| {
                    view.document_revision(
                        Admission::of(view, own).reader(own),
                        (app, record),
                        tilo_clock::wall(),
                        History { open, kept },
                    )
                })
            })?
            .flatten())
    }

    /// The first second after `now` at which a read of `workspace`'s records can be decided
    /// differently with nothing written ([`crate::workspace_keys::next_policy_change`]).
    ///
    /// # Errors
    /// `workspace` names no workspace.
    pub fn next_policy_change(&self, workspace: &str, now: u64) -> Result<Option<u64>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        Ok(crate::workspace_keys::next_policy_change(
            &self.owner,
            workspace,
            now,
        ))
    }

    /// This device's own writes to the record its replica has not taken in yet.
    fn own_writes(&self, workspace: &str, app: &str, record: &str) -> Option<Vec<Vec<u8>>> {
        let route = DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: workspace.to_owned(),
            document: record.to_owned(),
        };
        self.held
            .lock()
            .get(&route)
            .filter(|frames| !frames.is_empty())
            .cloned()
    }

    /// What this device's replica holds of the record, its own held writes left out.
    fn read_replica(
        &self,
        workspace: &str,
        route: (&str, &str),
        holding: Holding<'_>,
        maximum: usize,
    ) -> Result<ResourceReading, String> {
        Ok(self
            .opened(workspace, route, &mut |workspace, open, kept| {
                self.read_in(workspace, route, holding, maximum, History { open, kept })
            })?
            .unwrap_or(ResourceReading::Pending))
    }

    /// `read` of the record of `workspace` at `route`, with how its sealed writes open — the
    /// record's keys, then the channel's own history keys — and what this device kept opened of
    /// it: `None` while the keys of a sealed record are not here.
    fn opened<T>(
        &self,
        workspace: &str,
        (app, record): (&str, &str),
        read: &mut dyn FnMut(WorkspaceId, OpenSealed<'_>, &mut KeptDocument) -> Result<T, String>,
    ) -> Result<Option<T>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        let own = self.identity.device_cert.device_public_key;
        let (sealed, admission) = self.owner.with_current(workspace, |view| {
            Ok((
                view.peer().publication().is_sealed_document(app, record),
                Admission::of(view, own),
            ))
        })?;
        let keys: Option<tilo_workspace_keys::HistoryKeys> = if sealed {
            match self.route_access(workspace) {
                None => None,
                Some(KeyAccess::Ready(keys)) => Some(keys.into()),
                Some(KeyAccess::Waiting | KeyAccess::Removed) => {
                    // Nothing opened under keys this device no longer holds answers a read
                    // until the record's channel gives it keys again.
                    self.kept.lock_recover().remove(&(
                        workspace,
                        app.to_owned(),
                        record.to_owned(),
                    ));
                    return Ok(None);
                }
                Some(KeyAccess::NoSecretStore) => {
                    return Err(
                        "this device has no secret store, so it holds no channel keys".to_owned(),
                    );
                }
                // A copy the workspace has not admitted takes part in none of its key groups yet:
                // the keys it was copied with, among the history keys, open what it holds.
                Some(KeyAccess::Unavailable) if admission != Admission::Admitted => None,
                Some(KeyAccess::Unavailable) => {
                    return Err("this device takes part in no channel of the record".to_owned());
                }
            }
        } else {
            None
        };
        // Taken before the workspace's view is held: the channel service's pump settles the groups
        // under its own lock and reads the workspace's governance there, so a read that asked it
        // for keys while holding the view would wait on it as it waits on the view.
        let history = self
            .keys
            .get()
            .and_then(|keys| keys.history_keys(workspace));
        // A sealed write the keys this device holds do not open yet — its epoch's key, or the key
        // of the branch it was sealed on, has not reached this device — reads as what opened
        // before it until they have, as the app's own channel holds such a write until they do;
        // a record none of whose writes open yet is still arriving here. The record's keys open
        // what they carry; the channel's own history keys open the rest.
        let mut unopened = false;
        let key = (workspace, app.to_owned(), record.to_owned());
        // What was opened under other keys than these — the channel rotated, as removing a member
        // rotates it — is let go, and it opens whole under the keys held now.
        let used: KeysUsed = [
            keys.as_ref()
                .map(tilo_workspace_keys::HistoryKeys::generation),
            history
                .as_ref()
                .and_then(|history| history.as_ref().ok())
                .map(tilo_workspace_keys::HistoryKeys::generation),
        ];
        // A sealed resource another read is opening now is waited for, and read on from what that
        // one opened: taking it out while it was read made the second reader open it whole, which
        // for a chain part of a thousand items costs the second caller most of a second.
        let opening = sealed.then(|| {
            let mut kept = self.kept.lock_recover();
            if !kept.contains_key(&key) && kept.len() >= KEPT_RECORDS {
                kept.pop_first();
            }
            Arc::clone(kept.entry(key).or_default())
        });
        let read = opened_under(opening.as_deref(), &used, |kept| {
            read(
                workspace,
                &mut |write| {
                    let opened = match (&keys, &history) {
                        (Some(keys), Some(Ok(history))) => {
                            keys.open(write).or_else(|_| history.open(write))
                        }
                        (Some(keys), _) => keys.open(write),
                        (None, Some(history)) => {
                            history.as_ref().map_err(Clone::clone)?.open(write)
                        }
                        (None, None) => Err("this session's channel service is not running".into()),
                    };
                    unopened |= opened.is_err();
                    opened
                },
                kept,
            )
        });
        // The person's other devices may hold the key the workspace's server has not handed this
        // one yet, or cannot, being away: they are asked for the group's frames from the epoch
        // held.
        if unopened && let (Some(link), Some(Ok(history))) = (self.link(), &history) {
            link.want_frames(workspace, history.generation().1);
        }
        match read {
            Err(_) if unopened => Ok(None),
            read => read.map(Some),
        }
    }

    /// [`Self::read_resource`] under keys the caller holds itself rather than this device's channel
    /// service: what a caller that sealed a record's writes reads them back with. Nothing of it
    /// is kept: what these keys open is the caller's alone.
    fn read_in(
        &self,
        workspace: WorkspaceId,
        (app, record): (&str, &str),
        holding: Holding<'_>,
        maximum: usize,
        history: History<'_>,
    ) -> Result<ResourceReading, String> {
        let own = self.identity.device_cert.device_public_key;
        self.owner
            .with_current(workspace, |view| {
                view.read_document_with_history(
                    Admission::of(view, own).reader(own),
                    (app, record),
                    holding,
                    maximum,
                    tilo_clock::wall(),
                    history,
                )
            })
            .map(|read| match read {
                DocumentRead::Captured(document) => ResourceReading::Captured {
                    revision: document.revision,
                    heads: document.heads,
                    bytes: document.bytes,
                },
                DocumentRead::Changes(changes) => ResourceReading::Changes {
                    revision: changes.revision,
                    heads: changes.heads,
                    bytes: changes.bytes,
                },
                DocumentRead::Unchanged(revision) => ResourceReading::Unchanged(revision),
                DocumentRead::CheckpointPending => ResourceReading::Pending,
            })
    }

    /// The member's own record `app`'s `record` in `workspace` is, as the writes of it this
    /// device holds name it.
    #[must_use]
    pub fn own_resource(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Option<collab_types::OwnResource> {
        let workspace = WorkspaceId::parse(workspace)?;
        self.owner
            .with_current(workspace, |view| Ok(view.peer().own_resource(app, record)))
            .ok()
            .flatten()
    }

    /// Whether `workspace` holds `app`'s `record` here now: what a read of it opens, or a record
    /// this device holds nothing of that may still be on its way.
    #[must_use]
    pub fn holds_resource(&self, workspace: &str, app: &str, record: &str) -> bool {
        WorkspaceId::parse(workspace).is_some_and(|workspace| {
            self.owner.with_current(workspace, |view| {
                Ok(view.peer().document(app, record).is_some())
            }) == Ok(true)
        })
    }

    /// Whether `workspace`'s host finished replaying to this device what it retains: every record
    /// comes with it, so one that is not here then is one the workspace has none of — unless the
    /// host sends records only when they are opened
    /// ([`Self::documents_when_opened`]).
    #[must_use]
    pub fn host_replayed(&self, workspace: &str) -> bool {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        self.link
            .lock_recover()
            .as_ref()
            .is_some_and(|link| link.has_replayed(workspace))
    }

    /// Whether `workspace`'s host sends this device only the workspace's governance, its shared
    /// desktop and the heads of its chained records, and every other record once this device opens
    /// it ([`Self::open_resource`]): a record that is not here is not new until the host answered it
    /// holds none of it ([`Self::resource_answer`]).
    #[must_use]
    pub fn documents_when_opened(&self, workspace: &str) -> bool {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        self.link
            .lock_recover()
            .as_ref()
            .is_some_and(|link| link.documents_when_opened(workspace))
    }

    /// Ask `workspace`'s hosts for `app`'s `record`
    /// (`neighbour_wire::app_document_channel`): what they hold of it, and their changes to it from
    /// then on. Asked once per record while the peer network runs; `false` when there is no peer network to ask
    /// on, so nothing was asked.
    pub fn open_resource(&self, workspace: &str, app: &str, record: &str) -> bool {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        let Some(link) = self.link.lock_recover().clone() else {
            return false;
        };
        let own = tilo_server_protocol::OwnedChannel::recognise(
            app,
            record,
            self.identity.device_cert.user_public_key,
        );
        link.open_resource(
            workspace,
            neighbour_wire::app_document_channel(app, record),
            own,
        )
    }

    /// What `workspace`'s host answered about `app`'s `record`: `None` while the answer to
    /// [`Self::open_resource`] is owed; once it came, the frames it sent — none when the workspace
    /// has none of the record, which is then new here — or that it is another person's own.
    #[must_use]
    pub fn resource_answer(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Option<tilo_server_protocol::DocumentAnswer> {
        let workspace = WorkspaceId::parse(workspace)?;
        let channel = neighbour_wire::app_document_channel(app, record);
        self.link
            .lock_recover()
            .as_ref()?
            .resource_answer(workspace, &channel)
    }

    /// What ticks whenever a host answered for a record this device opened; `None` with no peer network.
    #[must_use]
    pub fn record_answers(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.link.lock_recover().as_ref().map(|link| link.answers())
    }

    /// What ticks whenever a host finished its replay; `None` with no peer network.
    #[must_use]
    pub fn replays(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.link.lock_recover().as_ref().map(|link| link.replays())
    }

    /// Whether `workspace` is held on this device, by its own workspace service: everything the
    /// workspace retains is here, and no other device replays any of it.
    #[must_use]
    pub fn is_holder(&self, workspace: &str) -> bool {
        WorkspaceId::parse(workspace).is_some_and(|workspace| {
            self.holder_of(workspace) == Some(self.identity.device_cert.device_public_key)
        })
    }

    /// Whether this device can ask its workspaces' hosts for what they retain: a session whose
    /// peer network is not running reads only what it holds.
    #[must_use]
    pub fn reaches_host(&self) -> bool {
        self.link.lock_recover().is_some()
    }

    /// What lands here from the workspaces' other devices: the event a read waiting for one waits
    /// on.
    #[must_use]
    pub fn arrivals(&self) -> Arc<crate::arrivals::Arrivals> {
        Arc::clone(&self.arrivals)
    }

    /// Whether `certificate`'s holder reads `app`'s `record` in `workspace` here now: the workspace
    /// is held with this device's ownership of it, its people certify the caller, and the caller's
    /// device is one the workspace's policy lets read the record.
    /// # Errors
    /// The workspace is not held here, this device has no ownership of it, or the caller may not
    /// read the record now.
    pub fn authorizes_read(
        &self,
        workspace: &str,
        certificate: &tilo_user_identity::DeviceCert,
        app: &str,
        record: &str,
    ) -> Result<(), String> {
        use tilo_broker_core::owner_service::OwnerService as _;
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.check_ownership(&self.owner, workspace)?;
        let reader = self.identity.device_cert.device_public_key;
        self.owner.with_current(workspace, |view| {
            let certified = view.peer().certificates().iter().any(|held| {
                held.device_public_key == certificate.device_public_key
                    && held.user_public_key == certificate.user_public_key
            });
            if !certified {
                return Err("the reader is no device the workspace certifies".to_owned());
            }
            view.peer()
                .check_document_read(reader, app, record, tilo_clock::wall())
        })
    }

    /// Every record `workspace` holds here, by its app and id. A workspace this device
    /// does not hold lists nothing.
    /// # Errors
    /// The workspace is not held here, or its documents cannot be listed now.
    pub fn documents(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        let id = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        let mut records = self
            .owner
            .with_current(id, |view| Ok(view.peer().records()))?;
        // A record this device wrote that its replica holds nothing of yet is one of them too.
        for route in self.held.lock().keys() {
            let named = (route.app_id.clone(), route.document.clone());
            if route.cluster_id == workspace && !records.contains(&named) {
                records.push(named);
            }
        }
        Ok(records)
    }

    /// Whether this device's person reads everything `workspace` holds here, so that it may be
    /// branched: this device holds it, and its governance names the person as its owner or as a
    /// member it has not removed.
    #[must_use]
    pub fn reads_whole(&self, workspace: WorkspaceId) -> bool {
        let person = hex::encode(self.identity.device_cert.user_public_key);
        let id = workspace.to_string();
        self.holds(workspace)
            && (self.owner_of(&id).is_ok_and(|owner| owner == person)
                || self.people(&id).is_ok_and(|people| {
                    people.iter().any(|member| {
                        member.id.person_id() == Some(person.as_str()) && !member.removed
                    })
                }))
    }

    /// Write `changes` to `record` of `app` in `workspace` as this device's own write, and publish
    /// it to the workspace's other devices: the record's founding write when `founding` (the whole
    /// document, which the workspace must not hold yet), a change of it otherwise. Sealed under
    /// `sealed` , the keys of the record's channel, where the workspace's records are its members'
    /// sealed channels — a founding write as a checkpoint, so the record is sealed from the start —
    /// and in the clear where they are documents this device commits.
    /// Answers the write as this device's apps take a member's: their copies of the record follow
    /// it only once it is handed to them.
    /// # Errors
    /// The workspace is not held here, a founding write finds the record held already, or the
    /// workspace refused the write: [`WriteRefusal::ChangeRefused`] names the document it refused,
    /// the changes it carried and why, as a server's refusal of a change does.
    pub async fn write_resource(
        &self,
        workspace: WorkspaceId,
        (app, record): (&str, &str),
        changes: &[u8],
        founding: bool,
        sealed: Option<&key_group::ContentKeys>,
    ) -> Result<tilo_peer_state::owner::AppWrite, tilo_peer_state::owner::WriteRefusal> {
        use tilo_broker_core::owner_service::OwnerService as _;
        let device = self.identity.device_cert.device_public_key;
        let now = tilo_clock::wall();
        let artifact = collab_types::document_artifact_id(app, &workspace.to_string(), record);
        let frame = if founding {
            collab_types::DocumentMutation::Snapshot(changes.to_vec())
        } else {
            collab_types::DocumentMutation::Incremental(changes.to_vec())
        }
        .encode()
        .map_err(|error| error.to_string())?;
        let mut author = None;
        let draft = |view: &tilo_peer_state::owner::NativeCommittedView| {
            if founding && view.peer().document(app, record).is_some() {
                return Err(format!("{record} of {app} is in the workspace already"));
            }
            if Admission::of(view, device) != Admission::Admitted {
                return Err("the workspace has not admitted this device yet".to_owned());
            }
            author = Some(
                view.peer()
                    .permissions(device, now)
                    .map_err(|error| error.to_string())?,
            );
            let mut seal = sealed.map(|keys| {
                move |plaintext: &[u8]| {
                    keys.seal_content(
                        artifact,
                        collab_types::ContentRetention::of_frame(plaintext),
                        plaintext,
                    )
                    .map_err(|error| error.to_string())
                }
            });
            let op = view.peer().draft_write(
                (app, record),
                &frame,
                seal.as_mut()
                    .map(|seal| seal as &mut tilo_peer_state::SealCheckpoint<'_>),
                device,
                0,
                now,
            )?;
            Ok(vec![tilo_peer_state::owner_commit::OwnerCommitRequest {
                workspace: workspace.to_string(),
                operation: tilo_peer_state::owner_commit::CommitOperation::Document {
                    app: app.to_owned(),
                    document: record.to_owned(),
                    envelope: Box::new(op),
                    own: view.peer().own_write(app, record, &device),
                },
            }])
        };
        let committed = self
            .owner
            .write(workspace, draft, self.identity.signer.as_ref(), now)
            .await?
            .pop()
            .ok_or_else(|| {
                tilo_peer_state::owner::WriteRefusal::Refused("the write was not committed".into())
            })?;
        self.schedule_publication(workspace);
        let tilo_peer_state::owner_commit::CommitOperation::Document { envelope, own, .. } =
            committed.0.operation
        else {
            return Err(tilo_peer_state::owner::WriteRefusal::Refused(
                "the write is not a document write".to_owned(),
            ));
        };
        Ok(tilo_peer_state::owner::AppWrite {
            peer: neighbour_wire::peer_handle(&device),
            app_id: app.to_owned(),
            cluster_id: workspace.to_string(),
            document_id: record.to_owned(),
            author: author.ok_or_else(|| {
                tilo_peer_state::owner::WriteRefusal::Refused("the write was not made".to_owned())
            })?,
            replayed: false,
            envelope: *envelope,
            own,
        })
    }

    /// The keys of `route`'s record in `workspace` here: those its host handed this device, else
    /// its channel's — taking part in it asks the workspace's devices for access the first time,
    /// and its governance answers with the group — and while the channel asks again, the ones it
    /// last gave. `None` without a channel service.
    fn route_access(&self, workspace: WorkspaceId) -> Option<KeyAccess> {
        let at = workspace;
        if let Some(handed) = self.handed.lock_recover().get(&at) {
            return Some(KeyAccess::Ready(handed.clone()));
        }
        let access = self.keys.get()?.access(workspace).0;
        let mut kept = self.group_keys.lock_recover();
        Some(match access {
            KeyAccess::Ready(keys) => {
                kept.insert(at, keys.clone());
                KeyAccess::Ready(keys)
            }
            KeyAccess::Waiting => kept
                .get(&at)
                .map_or(KeyAccess::Waiting, |keys| KeyAccess::Ready(keys.clone())),
            gone => {
                kept.remove(&at);
                gone
            }
        })
    }

    /// The keys this device seals writes in `workspace` with.
    /// [`SealKeys::Waiting`] says they are not here yet and will arrive as the
    /// workspace's other messages do, which includes a channel the governance does not
    /// name this device in yet. [`SealKeys::NoSecretStore`] is this device's own lack,
    /// not any workspace's verdict.
    /// # Errors
    /// This session has no key service, or the workspace removed this device from its
    /// group: it holds no keys, and none are coming.
    pub fn seal_keys(&self, workspace: WorkspaceId) -> Result<SealKeys, String> {
        Ok(match self.route_access(workspace) {
            None => return Err("this session's channel service is not running".to_owned()),
            Some(KeyAccess::Ready(keys)) => SealKeys::Here(keys),
            // Asked and not admitted yet, or admitted nowhere the governance here can name:
            // either way the keys are not here yet.
            Some(KeyAccess::Waiting | KeyAccess::Unavailable) => SealKeys::Waiting,
            Some(KeyAccess::Removed) => {
                return Err("the workspace removed this device from its group".to_owned());
            }
            Some(KeyAccess::NoSecretStore) => SealKeys::NoSecretStore,
        })
    }

    /// The devices the workspace `workspace` admits, by the publisher each of their
    /// sessions is known by ([`workspace_keys::roster_of`]).
    /// # Errors
    /// The workspace is not held here, or its governance cannot be read.
    pub fn roster(&self, workspace: &str) -> Result<Vec<String>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        workspace_keys::roster_of(self, workspace).map(|roster| {
            roster
                .into_iter()
                .map(|(device, _)| device.to_string())
                .collect()
        })
    }

    /// The devices the workspace admits, as a call's roster names them: whoever its governance
    /// certified and has not removed may reach a call in it.
    pub fn roster_devices(&self, workspace: &str) -> Result<Vec<[u8; 32]>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        workspace_keys::roster_of(self, workspace).map(|roster| {
            roster
                .into_iter()
                .map(|(device, _)| *device.as_bytes())
                .collect()
        })
    }

    /// Who the workspace has in it, as its governance tells it: every person it was shared with, by
    /// the name it knows them by and the part they take in it. A joined workspace brings its people
    /// with it, so this is how the layout knows them before any of them is here.
    pub fn people(&self, workspace: &str) -> Result<Vec<Member>, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.owner.with_current(workspace, |view| {
            Ok(view
                .peer()
                .governance()
                .governed_members(tilo_clock::wall())?
                .members)
        })
    }

    /// The people of `workspace` and the grants it makes, as its governance records
    /// them: the one source of who shares it, in what part, and what each may do.
    /// # Errors
    /// The workspace is not held here, or its governance cannot be read.
    pub fn policy(&self, workspace: &str) -> Result<WorkspacePolicy, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.owner
            .with_current(workspace, |view| view.peer().governance().governed_policy())
    }

    /// The workspace's own document as this device holds it, whole: its governance, and the layout
    /// it keeps at its root. `None` when this device holds no workspace by that id.
    #[must_use]
    pub fn workspace_document(&self, workspace: &str) -> Option<Vec<u8>> {
        let workspace = WorkspaceId::parse(workspace)?;
        self.owner
            .with_current(workspace, |view| Ok(view.peer().workspace().document()))
            .ok()
            .flatten()
    }

    /// Whether this device's person may administer `workspace` here: the governance gives their
    /// person the grant action. A workspace decided by another device's governance says no,
    /// whatever this device holds of it.
    #[must_use]
    pub fn administers(&self, workspace: &str) -> bool {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        let device = self.identity.device_cert.device_public_key;
        self.owner
            .with_current(workspace, |view| {
                Ok(view
                    .peer()
                    .workspace()
                    .check_workspace_administration(device, tilo_clock::wall())
                    .is_ok())
            })
            .unwrap_or(false)
    }

    /// The name `workspace` knows `person` by, when it knows one: what an invitation made for them
    /// records as its invitee.
    pub fn name_of(&self, workspace: &str, person: [u8; 32]) -> Option<String> {
        let person = hex::encode(person);
        self.people(workspace)
            .ok()?
            .into_iter()
            .find(|member| member.id.person_id() == Some(person.as_str()))
            .map(|member| member.name)
            .filter(|name| !name.is_empty())
    }

    /// Whether this device's person may invite into `workspace` , as its governance answers it.
    #[must_use]
    pub fn may_invite(&self, workspace: &str) -> bool {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        let key = self.identity().signer.as_ref();
        self.owner
            .with_current(workspace, |view| {
                Ok(view.peer().governance().may_invite(key, tilo_clock::wall()))
            })
            .unwrap_or(false)
    }

    /// Whether this device's person may make administrators in `workspace`, as its governance
    /// answers it: whether the invite sheet offers an administrator's invitation.
    #[must_use]
    pub fn may_invite_administrator(&self, workspace: &str) -> bool {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        let key = self.identity().signer.as_ref();
        self.owner
            .with_current(workspace, |view| {
                Ok(view
                    .peer()
                    .governance()
                    .may_invite_administrator(key, tilo_clock::wall()))
            })
            .unwrap_or(false)
    }

    /// Whether this device's person may delete `workspace` : they own it, no one else is in it, and
    /// this device is the only one holding it — so a workspace taken off this device is a workspace
    /// gone everywhere, and not one another of their devices would list again.
    #[must_use]
    pub fn may_delete(&self, workspace: WorkspaceId) -> bool {
        self.not_deletable(workspace).is_none()
    }

    /// Why `workspace` is not this device's person's to delete here, when it is not: what the
    /// surfaces offering deleting say beside a delete they cannot offer.
    #[must_use]
    pub fn not_deletable(&self, workspace: WorkspaceId) -> Option<NotDeletable> {
        if !self.holds(workspace) {
            return Some(NotDeletable::NotHeld);
        }
        let identity = self.identity();
        let person = hex::encode(identity.device_cert.user_public_key);
        let device = identity.device_cert.device_public_key;
        let workspace_id = workspace.to_string();
        let alone = self.people(&workspace_id).is_ok_and(|people| {
            people
                .iter()
                .all(|member| member.removed || member.id.person_id() == Some(person.as_str()))
        });
        if !alone {
            return Some(NotDeletable::OthersInIt);
        }
        let only_here = self
            .roster_devices(&workspace_id)
            .is_ok_and(|devices| devices == [device]);
        if !only_here {
            return Some(NotDeletable::OtherDevice);
        }
        if !self
            .owner_of(&workspace_id)
            .is_ok_and(|owner| owner == person)
        {
            return Some(NotDeletable::NotOwner);
        }
        None
    }

    /// Take `workspace` out of this device's record, with everything of it this device
    /// holds. Answers whether the record held it.
    /// # Errors
    /// The record cannot be published.
    pub fn delete(&self, workspace: WorkspaceId) -> Result<bool, String> {
        self.kept
            .lock_recover()
            .retain(|(held, _, _), _| *held != workspace);
        self.owner.release_workspace(workspace)
    }

    /// The parts this device's person may hand out in an invitation into `workspace` , from the
    /// least to the most: what the surfaces offering a part to grant list. Empty when the person
    /// may not invite at all, since no part is theirs to hand out then.
    #[must_use]
    pub fn invitable_roles(&self, workspace: &str) -> Vec<Role> {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return Vec::new();
        };
        let key = self.identity().signer.as_ref();
        self.owner
            .with_current(workspace, |view| {
                let now = tilo_clock::wall();
                Ok(Role::ASSIGNABLE
                    .into_iter()
                    .filter(|role| view.peer().governance().may_invite_role(key, *role, now))
                    .collect())
            })
            .unwrap_or_default()
    }

    /// The revocations the workspace's governance records, as it last admitted them. `None` when
    /// this device holds no such workspace. What is checked against this is whether a shortcut's
    /// saver may still start it here.
    #[must_use]
    pub fn revocations(&self, workspace: &str) -> Option<RevocationList> {
        let workspace = WorkspaceId::parse(workspace)?;
        self.owner
            .with_current(workspace, |view| {
                let mut list = RevocationList::new();
                for entry in view.peer().revocations() {
                    // The workspace verified every entry when it admitted it: an entry that does
                    // not verify now leaves its refusals unknown rather than trusting the rest.
                    list.insert(entry).map_err(|error| error.to_string())?;
                }
                Ok(list)
            })
            .ok()
    }

    /// Who governs the workspace, as the id its governance names. A joined workspace
    /// keeps the owner its host's governance has, never the person who joined.
    /// # Errors
    /// The workspace is not held here yet, or its governance cannot be read now.
    pub fn owner_of(&self, workspace: &str) -> Result<String, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.owner.with_current(workspace, |view| {
            let owner = view.peer().governance().owner_user_public_key();
            if owner == [0; 32] {
                // A workspace whose governance names nobody has no owner to carry: a claim made
                // here would be this device's, not the workspace's.
                return Err("the workspace's governance names no owner".to_owned());
            }
            Ok(hex::encode(owner))
        })
    }

    /// The verdicts `workspace` holds on writes: this device's own, and the ones other devices
    /// recorded on its writes. Each names the changes the refused write would have brought, so a
    /// history row reads where it is not.
    #[must_use]
    pub fn write_refusals(&self, workspace: &str) -> Vec<neighbour_wire::WriteRefusal> {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return Vec::new();
        };
        self.owner.write_refusals(workspace)
    }

    /// The verdicts other devices recorded on this device's writes, with the workspace each was
    /// filed in, where waiting changes nothing: what the person hears once per verdict, because
    /// their writes stop arriving. A verdict this device filed itself is not one of them: the
    /// app or agent that made the change was told there, where the edit was made.
    #[must_use]
    pub fn lasting_refusals(&self) -> Vec<(String, neighbour_wire::WriteRefusal)> {
        let mine = self.identity.device_cert.device_public_key;
        self.workspaces()
            .into_iter()
            .flat_map(|workspace| {
                let name = workspace.to_string();
                self.owner
                    .write_refusals(workspace)
                    .into_iter()
                    .filter(|refusal| {
                        refusal.author == mine && refusal.by != mine && refusal.reason.permanent()
                    })
                    .map(move |refusal| (name.clone(), refusal))
            })
            .collect()
    }

    /// What each device of `workspace` calls itself, by its key.
    #[must_use]
    pub fn device_labels(&self, workspace: &str) -> std::collections::BTreeMap<[u8; 32], String> {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return std::collections::BTreeMap::new();
        };
        self.owner.device_labels(workspace)
    }

    /// The engine's owner, for what holds the workspace's session.
    pub fn owner(&self) -> &NativeWorkspaceOwner {
        &self.owner
    }

    /// Where `workspace`'s holder is reached and the device its enrollment pins: the address its
    /// live session is dialled at.
    #[cfg(target_os = "linux")]
    pub fn holder_address(
        &self,
        workspace: WorkspaceId,
    ) -> Option<([u8; 32], iroh_base::EndpointAddr)> {
        self.owner.host_addr(workspace)
    }

    /// The device holding `workspace`: the one whose workspace service orders its commits,
    /// which is where the person's other devices enter it.
    pub fn holder_of(&self, workspace: WorkspaceId) -> Option<[u8; 32]> {
        self.owner
            .with_current(workspace, |view| Ok(view.workspace_host().writes_for()))
            .ok()
    }

    /// The holders of `workspace`, each as its own signed address names it:
    /// what a member reads of who keeps their workspace and which of them can
    /// read it. Empty while this device holds no governance for the workspace.
    pub fn holders(&self, workspace: &str) -> Vec<Holder> {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return Vec::new();
        };
        let Ok(hints) = self.owner.with_current(workspace, |view| {
            Ok(view.peer().governance().keeping_devices())
        }) else {
            return Vec::new();
        };
        let mut holders: Vec<Holder> = hints
            .into_iter()
            .filter_map(|hint| {
                let payload = tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::from_transport_hint(&hint).ok()?;
                let label = payload.label.trim();
                Some(Holder {
                    device: hex::encode(hint.host.device_public_key),
                    label: if label.is_empty() {
                        hint.host.device_label
                    } else {
                        label.to_owned()
                    },
                    reads: payload.holds_keys,
                    cloud: payload.cloud,
                    moderates: payload.moderates,
                })
            })
            .collect();
        holders.sort_by(|a, b| a.label.cmp(&b.label));
        holders
    }

    /// The work the members of `workspace` gave servers that is live now, each
    /// as the member's own device signed it: who gave a server which work,
    /// until when, and which of them this device may take back. Empty while
    /// this device holds no state for the workspace.
    pub fn delegations(&self, workspace: &str) -> Vec<Delegation> {
        let Some(workspace_id) = WorkspaceId::parse(workspace) else {
            return Vec::new();
        };
        let Ok(held) = self.owner.with_current(workspace_id, |view| {
            let now_ns = tilo_clock::unix_now_ns();
            Ok(view
                .peer()
                .workspace()
                .work_delegations()
                .into_iter()
                .filter(|delegation| delegation.verify(workspace, now_ns).is_ok())
                .filter(|delegation| delegation.revoked_at_ns.is_none())
                .collect::<Vec<_>>())
        }) else {
            return Vec::new();
        };
        let device = self.identity.device_cert.device_public_key;
        held.into_iter()
            .map(|delegation| Delegation {
                key: delegation.row_key(),
                person: hex::encode(delegation.person),
                server: hex::encode(delegation.server),
                agent: delegation.agent.clone(),
                grants: delegation.grants.clone(),
                expires_at_ns: delegation.expires_at_ns,
                revocable: delegation.device == device,
            })
            .collect()
    }

    /// Where the holders of the workspaces this device holds are reached, one entry per holder, by
    /// workspace id, postcard encoded: what the person's other devices need to enter a workspace
    /// listed here at any of the devices that keep it.
    pub fn host_hints(&self) -> Vec<(String, Vec<u8>)> {
        self.owner
            .workspaces()
            .into_iter()
            .flat_map(|workspace| {
                // The hint itself, as the device that reads it decodes it: not the answer
                // whether this device has one, whose tag would lead the bytes.
                let hints = self
                    .owner
                    .with_current(workspace, |view| Ok(view.keeping_devices()))
                    .unwrap_or_default();
                let named = workspace.to_string();
                hints
                    .into_iter()
                    .filter_map(move |hint| {
                        let bytes = postcard::to_stdvec(&hint).ok()?;
                        Some((named.clone(), bytes))
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Where a read asks the workspaces' hosts for a record they send only once asked: the peer network
    /// this session's channels travel on.
    pub fn attach_link(&self, link: Arc<workspace_keys::KeyLink>) {
        *self.link.lock_recover() = Some(link);
    }

    /// Where this device's channels hear its workspaces' governance and tell which devices each
    /// admits: the peer network's side of them, `None` when this session runs no channel service.
    pub fn link(&self) -> Option<Arc<workspace_keys::KeyLink>> {
        self.link.lock_recover().clone()
    }

    /// Where this session's channel service installs this device's keys: the broker, the workspace host
    /// and every sealed read meet here.
    /// This device's own writes its replica has not taken in yet.
    pub fn held_writes(&self) -> &HeldWrites {
        &self.held
    }

    /// Read `workspace`'s records with `keys` from now on: the workspace's host handed them to this
    /// device, which takes no part in the workspace's group.
    pub fn hand_keys(&self, workspace: WorkspaceId, keys: key_group::ContentKeys) {
        self.handed.lock_recover().insert(workspace, keys);
    }

    pub fn keys(&self) -> KeyProvider {
        self.keys.clone()
    }

    /// This device's collaboration identity: the key its endpoint and its channels sign with, and
    /// the certificate that says whose it is.
    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    /// Found `workspace`, made on this layout under `name`, when this device made it and nothing
    /// here holds it yet: it is placed on this device's own workspace service, and this device
    /// holds it as a member from then on. A workspace this device may not own, or one that could
    /// not be placed, stays the layout's own.
    #[cfg(target_os = "linux")]
    pub async fn found(&self, workspace: WorkspaceId, name: &str) {
        if self.holds(workspace)
            || self.unfounded.lock_recover().contains(&workspace)
            || !self.founding.lock_recover().insert(workspace)
        {
            return;
        }
        self.found_here(workspace, name).await;
        self.founding.lock_recover().remove(&workspace);
        self.founded
            .send_modify(|ended| *ended = ended.wrapping_add(1));
    }

    /// [`Self::found`] once nothing else founds `workspace`.
    #[cfg(target_os = "linux")]
    async fn found_here(&self, workspace: WorkspaceId, name: &str) {
        if !self.identity().device_cert.capabilities.can_own_workspaces {
            return;
        }
        let Some(founder) = self.founder.get().cloned() else {
            tracing::warn!(%workspace, "this device's workspace service is not open, so the workspace stays the layout's own");
            return;
        };
        // Founding redeems the service's invitation into this workspace, and an enrollment installs
        // only into a workspace on this device's layout: it is put there before the service is
        // asked, as a join puts the workspace it installs into. One made since the session started
        // is on no layout the engine opened with, and would be refused as gone.
        self.bind(workspace);
        match founder.found(workspace, name.to_owned()).await {
            Ok(()) => {}
            Err(error) => {
                tracing::warn!(%error, %workspace, "the workspace was not founded here");
                self.unfounded.lock_recover().insert(workspace);
            }
        }
    }

    /// Whether a founder is handed ([`Self::serve_founding`]): this device's own workspace service
    /// is open.
    #[must_use]
    pub fn founds(&self) -> bool {
        self.founder.get().is_some()
    }

    /// Found workspaces through `founder` from now on: this device's own workspace service, once
    /// it is open. The first one handed stays.
    pub fn serve_founding(&self, founder: Arc<dyn Founder>) {
        let _ = self.founder.set(founder);
    }

    /// Ask `workspace`'s governance action `action` with `arguments` of its holder, as this
    /// device ([`Founder::ask`]). A workspace this device is founding now is asked once the
    /// founding ended: until then it has no holder.
    #[must_use]
    pub fn ask_holder(
        self: &Arc<Self>,
        workspace: WorkspaceId,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<ServiceAnswer> {
        let mut ended = self.founded.subscribe();
        if !self.founding.lock_recover().contains(&workspace) {
            return self.founder.get()?.ask(workspace, action, arguments);
        }
        let (engine, action) = (Arc::clone(self), action.to_owned());
        Some(Box::pin(async move {
            while engine.founding.lock_recover().contains(&workspace) {
                if ended.changed().await.is_err() {
                    break;
                }
            }
            match engine
                .founder
                .get()
                .and_then(|founder| founder.ask(workspace, &action, arguments))
            {
                Some(asked) => asked.await,
                None => Err("this device reaches no holder of the workspace".to_owned()),
            }
        }))
    }

    /// Ask this device's own host its own action `action` with `arguments`, in process, as the
    /// person at this device ([`Founder::ask_host`]): what a window of theirs asks through the
    /// app broker, such as the machines their Servers workspace holds.
    pub fn ask_host(
        &self,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<ServiceAnswer> {
        self.founder.get()?.ask_host(action, arguments)
    }

    /// Whether this device holds a link into a workspace that still admits someone: a person who
    /// redeems it dials this device, which stays dialable until the link is taken back or lapses.
    #[must_use]
    pub fn invitations_open(&self, now: SystemTime) -> bool {
        let now = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        self.open_links
            .lock_recover()
            .values()
            .any(|lapses| lapses.iter().any(|until| u128::from(*until) > now))
    }

    /// Note the links into `workspace` its holder's list names, each open until it lapses: in
    /// place of what was noted before.
    pub fn note_invitations(&self, workspace: WorkspaceId, listed: &serde_json::Value) {
        if !self.dials_for(workspace) {
            return;
        }
        let lapses = listed
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|link| link["expires_at_ns"].as_u64())
            .collect();
        self.open_links.lock_recover().insert(workspace, lapses);
    }

    /// Note the link into `workspace` that is open until `until`: one more beside the ones its
    /// holder listed.
    pub fn note_link(&self, workspace: WorkspaceId, until: u64) {
        if !self.dials_for(workspace) {
            return;
        }
        self.open_links
            .lock_recover()
            .entry(workspace)
            .or_default()
            .push(until);
    }

    /// Whether a person redeeming a link into `workspace` dials this device: it holds the
    /// workspace as its holder. One held elsewhere is answered by that device.
    fn dials_for(&self, workspace: WorkspaceId) -> bool {
        self.holder_of(workspace) == Some(self.identity.device_cert.device_public_key)
    }

    /// Put the workspace a join is installing into on this device's layout: an enrollment only
    /// installs into the workspace made for it, and says so before the host is asked.
    pub fn bind(&self, workspace: WorkspaceId) {
        self.shown.0.lock_recover().insert(workspace);
    }

    /// Let go of a workspace a join did not install into, so this device keeps nothing it does not
    /// hold.
    pub fn unbind(&self, workspace: WorkspaceId) {
        self.shown.0.lock_recover().remove(&workspace);
    }
}

/// What the broker's owner commits ask of this device's workspaces: the engine's owner is the
/// canonical copy of every workspace it holds, and what it commits leaves on the peer network's session
/// with the workspace's host.
impl tilo_broker_core::owner_service::OwnerService for Engine {
    /// A held workspace whose host takes part in no channels: its records are documents the owner
    /// commits, never sealed channels.
    fn commits_documents(&self, workspace_id: WorkspaceId) -> bool {
        self.holds(workspace_id)
            && self
                .owner
                .with_current(workspace_id, |view| {
                    Ok(!view.workspace_host().capabilities.holds_workspace_keys())
                })
                .unwrap_or(false)
    }

    fn projects_workspace(&self, workspace_id: WorkspaceId) -> bool {
        self.holds(workspace_id)
    }

    /// The engine opened its owner before the session started.
    fn opened_owner(&self, _unavailable: &str) -> Result<NativeWorkspaceOwner, String> {
        Ok(self.owner.clone())
    }

    fn resolve_owner(&self) -> Result<NativeWorkspaceOwner, String> {
        Ok(self.owner.clone())
    }

    fn check_ownership(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace_id: WorkspaceId,
    ) -> Result<(), String> {
        self.ownership.check(owner, workspace_id, &self.identity)
    }

    fn signer(
        &self,
        owner: &NativeWorkspaceOwner,
        workspace_id: WorkspaceId,
    ) -> Result<Arc<dyn collab_types::signer::DeviceSigner>, String> {
        self.check_ownership(owner, workspace_id)?;
        Ok(Arc::clone(&self.identity.signer))
    }

    /// Send the owner's outgoing ownership of `workspace` to its peers soon: what was just committed
    /// leaves this device. Queued, never awaited.
    fn schedule_publication(&self, workspace_id: WorkspaceId) {
        if let Some(link) = self.link() {
            link.publish_ownership(workspace_id);
        }
    }
}

/// A workspace's revocations, as the threads beside the session's loop ask for them: the broker
/// decides on its own thread and the automation host arms shortcuts on the loop, and both refuse a
/// device the workspace revoked.
pub type Revocations = Arc<dyn RevocationsOf>;

/// What a workspace of this device's engine revokes, asked from a thread of the session's own or
/// from the session's loop: `Send + Sync` where the host has threads.
pub trait RevocationsOf: collab_types::signer::MaybeSendSync {
    fn of(&self, workspace: &str) -> Option<RevocationList>;
}

/// What a workspace of this device's engine revokes, for the callers that ask from another thread.
/// A session that holds no workspace knows of none.
#[cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::arc_with_non_send_sync,
        reason = "a page has one thread, and the engine it shares is `MaybeSendSync`, never `Send` there"
    )
)]
pub fn revocations_of(engine: Option<&Arc<Engine>>) -> Revocations {
    struct Held(Option<Arc<Engine>>);

    impl RevocationsOf for Held {
        fn of(&self, workspace: &str) -> Option<RevocationList> {
            self.0
                .as_ref()
                .and_then(|engine| engine.revocations(workspace))
        }
    }

    Arc::new(Held(engine.cloned()))
}

/// The file this device's workspaces are kept in under `state`, its directory made: what a session
/// started on `state` opens, and where a tool prepares the workspaces that session finds.
///
/// # Errors
/// The directory cannot be made.
#[cfg(not(target_arch = "wasm32"))]
pub fn owner_path(state: &Path) -> Result<PathBuf, String> {
    let path = state.join("tilo").join(FILE);
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    }
    Ok(owner_file_of(&path))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion, not a hazard"
    )]

    tilo_automerge_isolate::test_worker!();

    /// A record opened under keys a rotation replaced, or under keys handed over that changed, is
    /// opened whole again under the keys held now; the same keys read on what was kept.
    #[test]
    fn a_record_kept_under_keys_a_rotation_replaced_is_forgotten() {
        let channel = collab_types::WorkspaceId::from_u128(7);
        let held: super::KeysUsed = [None, Some((channel, 1))];
        let rotated: super::KeysUsed = [None, Some((channel, 2))];
        let handed: super::KeysUsed = [Some((channel, 1)), Some((channel, 1))];
        // Whatever a read kept stands in for the opened record.
        let opened = || (held, Some("opened"));
        assert_eq!(super::kept_under(Some(opened()), &held), Some("opened"));
        assert_eq!(super::kept_under(Some(opened()), &rotated), None);
        assert_eq!(super::kept_under(Some(opened()), &handed), None);
        assert_eq!(super::kept_under::<Option<&str>>(None, &held), None);
    }

    use super::*;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert};

    /// A device of a person, certified for their root, with `capabilities`,
    /// and the person's root key.
    fn identity(capabilities: DeviceCapabilities) -> (DeviceIdentity, [u8; 32]) {
        device_of(tilo_test_env::node_key(), capabilities)
    }

    /// The person's device whose key is `device`.
    fn device_of(device: [u8; 32], capabilities: DeviceCapabilities) -> (DeviceIdentity, [u8; 32]) {
        let person = ed25519_dalek::SigningKey::from_bytes(&[0x21; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&device);
        let root = person.verifying_key().to_bytes();
        let mut cert = DeviceCert {
            user_public_key: root,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Laptop".to_owned(),
            capabilities,
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
        let identity = DeviceIdentity::kept(cert, device);
        (identity, root)
    }

    /// A host whose own proof of ownership this device's engine asks: these tests read what it asks
    /// with, not the proof a desktop makes over the peer network.
    struct Kept;

    impl MembershipCheck for Kept {
        fn check(
            &self,
            _owner: &NativeWorkspaceOwner,
            _workspace: WorkspaceId,
            _identity: &DeviceIdentity,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    /// This device's engine, kept in `state` .
    fn engine(state: &Path, identity: DeviceIdentity) -> Engine {
        Engine::open(identity, Some(state), Vec::new(), Arc::new(Kept)).expect("an engine")
    }

    /// A workspace held on `engine` as a member, the device's person its owner: what founding it on
    /// this device's own workspace service leaves the engine holding.
    fn founded(engine: &Engine) -> WorkspaceId {
        let certificate = engine.identity().device_cert.clone();
        let key = ed25519_dalek::SigningKey::from_bytes(
            &engine
                .identity()
                .signer
                .endpoint_secret()
                .expect("a kept key"),
        );
        let workspace =
            tilo_peer_state::owner::testing::founded(engine.owner(), &certificate, &key)
                .expect("the workspace is held here");
        engine.bind(workspace);
        workspace
    }

    tilo_test_env::scenario! {
        fn a_workspace_is_deleted_only_by_the_device_that_alone_holds_it;

        phase a_workspace_made_here_and_held_nowhere_else_may_be_deleted;
        {
            let state = tempfile::tempdir().expect("a state directory");
            let (identity, _) = identity(DeviceCapabilities::member());
            let engine = engine(state.path(), identity);
            let workspace = founded(&engine);

            assert!(
                engine.may_delete(workspace),
                "its owner, in it alone, on its only device, may delete it"
            );
            assert_eq!(
                engine.delete(workspace),
                Ok(true),
                "the record it was held in is taken out"
            );
            assert!(!engine.holds(workspace));
            assert!(!engine.may_delete(workspace));
            assert_eq!(
                engine.delete(workspace),
                Ok(false),
                "a record holding nothing of it has nothing to take out"
            );
        }

        phase a_workspace_this_device_holds_no_record_of_may_not_be_deleted;
        {
            let state = tempfile::tempdir().expect("a state directory");
            let (identity, _) = identity(DeviceCapabilities::member());
            let engine = engine(state.path(), identity);

            assert!(!engine.may_delete(WorkspaceId::new()));
        }
    }

    /// A link out of a workspace this device holds keeps it dialable for whoever redeems it, and
    /// only until the link lapses; a link into a workspace another device holds moves nothing.
    #[test]
    fn a_link_out_of_a_held_workspace_keeps_this_device_dialable_until_it_lapses() {
        let state = tempfile::tempdir().expect("a state directory");
        let (identity, _) = identity(DeviceCapabilities::member());
        let now = tilo_clock::wall();
        let lapses = tilo_clock::unix_now_ns().saturating_add(60_000_000_000);
        let workspace = {
            let engine = engine(state.path(), identity);
            let workspace = founded(&engine);
            assert!(!engine.invitations_open(now), "no link is out");
            engine.note_link(workspace, lapses);
            assert!(engine.invitations_open(now), "one is");
            assert!(
                !engine.invitations_open(now + tilo_clock::time::Duration::from_secs(120)),
                "it lapsed"
            );
            let listed = serde_json::json!([{ "expires_at_ns": lapses }]);
            engine.note_invitations(workspace, &listed);
            assert!(engine.invitations_open(now), "its holder lists it too");
            engine.note_invitations(workspace, &serde_json::json!([]));
            assert!(!engine.invitations_open(now), "the holder lists none");
            workspace
        };

        let (copy, _) = device_of([0x44; 32], DeviceCapabilities::member());
        let copy = engine(state.path(), copy);
        copy.note_link(workspace, lapses);
        assert!(
            !copy.invitations_open(now),
            "a joiner dials the holder, not this copy"
        );
    }

    /// A resource written through the store is this device's signed write of the document the store
    /// placed it in, read back from the store at once, and by the next session from what the
    /// replica kept.
    #[test]
    fn a_resource_written_through_the_store_is_kept_and_read_back() {
        use tilo_resource_store::{Cell, Query, Resource, Stamp};
        let state = tempfile::tempdir().expect("a state directory");
        let (laptop, _) = identity(DeviceCapabilities::member());
        let workspace = {
            let engine = engine(state.path(), laptop.clone());
            founded(&engine)
        };
        let keys = key_group::KeyGroup::create(
            collab_types::WorkspaceId::parse(&workspace.to_string()).expect("a workspace id"),
            key_group::MemberKey::generate("laptop").expect("a member key"),
        )
        .and_then(|group| group.content_keys())
        .expect("the group's keys");
        let stamp = Stamp {
            verb: "add".to_owned(),
            invoker: "ada".to_owned(),
            invocation: "i1".to_owned(),
            membership: Vec::new(),
        };
        let issue = |title: &str, readers: &str| Resource {
            readers: readers.to_owned(),
            parent: Some("board".to_owned()),
            order: Some("a0".to_owned()),
            fields: [("title".to_owned(), Cell::One(title.into()))].into(),
        };
        let titles = |engine: &Engine| {
            engine.read_resources(workspace, |store, may_read| {
                store
                    .query(&Query::of("tasks.issue", 10), may_read)
                    .resources
                    .into_iter()
                    .map(|(_, held)| held.scalar("title"))
                    .collect::<Vec<_>>()
            })
        };
        {
            let engine = engine(state.path(), laptop.clone());
            engine.hand_keys(workspace, keys.clone());
            for title in ["First", "Renamed"] {
                pollster::block_on(engine.put_resource(
                    workspace,
                    "so.tilo.tasks",
                    ("tasks.issue", "i-1"),
                    issue(title, "*"),
                    &stamp,
                ))
                .expect("the laptop writes the issue");
            }
            assert_eq!(titles(&engine), ["Renamed".into()]);
            let ((), writes) = pollster::block_on(engine.edit_resource_content(
                workspace,
                "so.tilo.tasks",
                ("tasks.issue", "i-1"),
                &stamp,
                |tx, root| {
                    automerge::transaction::Transactable::put(tx, root, "body", "Steps")?;
                    Ok(())
                },
            ))
            .expect("the laptop writes the issue's body");
            assert_eq!(writes.len(), 1, "one content document written");
            let private = pollster::block_on(engine.put_resource(
                workspace,
                "so.tilo.tasks",
                ("tasks.issue", "i-2"),
                issue("Mine", "someone"),
                &stamp,
            ));
            assert!(
                matches!(
                    private,
                    Err(tilo_peer_state::owner::WriteRefusal::Refused(_))
                ),
                "a reader set whose keys are not here is written nowhere"
            );
        }

        let engine = engine(state.path(), laptop);
        engine.hand_keys(workspace, keys);
        assert_eq!(
            titles(&engine),
            ["Renamed".into()],
            "the next session reads it back"
        );
    }

    /// A machine copied from a device, before the workspace admits it, reads what the copy holds,
    /// nothing it does not hold, and writes nothing.
    #[test]
    fn a_copy_awaiting_its_host_reads_what_it_holds_and_writes_nothing() {
        let state = tempfile::tempdir().expect("a state directory");
        let (laptop, _) = identity(DeviceCapabilities::member());
        let workspace = {
            let engine = engine(state.path(), laptop.clone());
            founded(&engine)
        };
        let mut note = automerge::AutoCommit::new();
        automerge::transaction::Transactable::put(&mut note, automerge::ROOT, "title", "Kept")
            .expect("a note");
        let written = note.save();
        let keys = key_group::KeyGroup::create(
            collab_types::WorkspaceId::parse(&workspace.to_string()).expect("a workspace id"),
            key_group::MemberKey::generate("laptop").expect("a member key"),
        )
        .and_then(|group| group.content_keys())
        .expect("the group's keys");
        {
            let engine = engine(state.path(), laptop);
            engine.hand_keys(workspace, keys.clone());
            pollster::block_on(engine.write_resource(
                workspace,
                ("so.tilo.notes", "note"),
                &written,
                true,
                Some(&keys),
            ))
            .expect("the laptop writes its note");
        }

        let (copy, _) = device_of([0x44; 32], DeviceCapabilities::member());
        let copy = engine(state.path(), copy);
        // The keys the copy holds of the workspace.
        copy.hand_keys(workspace, keys.clone());
        let id = workspace.to_string();
        assert!(
            copy.awaited_host(workspace).is_some(),
            "the copy waits for the workspace to admit it"
        );
        let read = copy
            .record(&id, "so.tilo.notes", "note")
            .expect("the copy reads the note it holds")
            .expect("the note is here");
        let read = automerge::AutoCommit::load(&read).expect("a note");
        assert_eq!(
            automerge::ReadDoc::get(&read, automerge::ROOT, "title")
                .expect("a title")
                .map(|(value, _)| value.to_string()),
            Some("\"Kept\"".to_owned())
        );
        assert!(
            !matches!(copy.record(&id, "so.tilo.notes", "other"), Ok(Some(_))),
            "what the copy does not hold reads as nothing"
        );
        automerge::transaction::Transactable::put(&mut note, automerge::ROOT, "title", "Changed")
            .expect("a change");
        let refused = pollster::block_on(copy.write_resource(
            workspace,
            ("so.tilo.notes", "note"),
            &note.save_incremental(),
            false,
            Some(&keys),
        ));
        assert!(
            matches!(
                refused,
                Err(tilo_peer_state::owner::WriteRefusal::Refused(ref reason))
                    if reason.contains("not admitted this device")
            ),
            "the copy writes nothing before it is admitted"
        );
    }
}
