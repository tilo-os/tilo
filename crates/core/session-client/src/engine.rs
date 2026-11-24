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
use tilo_peer_state::Member;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_peer_state::join::Role;
#[cfg(not(target_arch = "wasm32"))]
use tilo_peer_state::owner::Change;
use tilo_peer_state::owner::NativeWorkspaceOwner;
#[cfg(not(target_arch = "wasm32"))]
use tilo_peer_state::owner::{FileHost, open_file_with, owner_file_of};
use tilo_user_identity::RevocationList;
use tilo_workspace_keys::{KeyAccess, KeyProvider};

#[cfg(not(target_arch = "wasm32"))]
use crate::identity;
use crate::workspace_keys;
pub use tilo_peer_state::workspace_view::{Holder, Hosting, NotDeletable};

mod documents;
#[cfg(test)]
pub(crate) use documents::page_holder;

/// The engine's record, under this device's state directory: the name an earlier build kept it
/// under, so the state it wrote is still read.
const FILE: &str = "workspaces.json";

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
    /// The workspace transport of this session's peer network, once it runs: a record the
    /// workspace's hosts send only once asked is asked for here, and the peer network is gone while this
    /// is `None` .
    link: Arc<std::sync::Mutex<Option<Arc<workspace_keys::KeyLink>>>>,
    /// What lands here from the workspaces' other devices: what a read waiting for a record it
    /// holds none of wakes on.
    arrivals: Arc<crate::arrivals::Arrivals>,
    /// Keys of workspaces whose group this device takes no part in, handed to it by their host
    /// ([`Self::hand_keys`]): a sealed record they open reads with them.
    handed: Arc<std::sync::Mutex<RouteKeys>>,
    /// The keys each record's channel last gave this device: they seal and open while the channel
    /// asks for access again.
    group_keys: std::sync::Mutex<RouteKeys>,
    /// Where this device's own workspace service keeps the workspaces it holds, under the state
    /// the engine was opened on; `None` for an engine that keeps no state.
    service_root: Option<std::path::PathBuf>,
    /// When the links out of each workspace lapse, as its holder lists them: a joiner redeeming
    /// one dials this device, which stays dialable while one is open ([`Self::invitations_open`]).
    open_links: std::sync::Mutex<std::collections::BTreeMap<WorkspaceId, Vec<u64>>>,
}

/// What a workspace's holder answers an ask of its governance, once it has.
#[cfg(not(target_arch = "wasm32"))]
pub type ServiceAnswer =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send>>;

/// What a workspace's holder answers an ask of its governance, once it has.
#[cfg(target_arch = "wasm32")]
pub type ServiceAnswer =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, String>>>>;

/// Keys, by workspace.
type RouteKeys = BTreeMap<WorkspaceId, key_group::ContentKeys>;
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
    /// Where this device asks its other devices for a group's frames when a write is sealed
    /// under an epoch whose key it does not hold ([`workspace_keys::KeyLink::want_frames`]).
    link: Arc<std::sync::Mutex<Option<Arc<workspace_keys::KeyLink>>>>,
}

impl HistoryKeys {
    /// The keys of `keys`' channels alone: a device whose host hands it none.
    #[must_use]
    pub fn new(keys: KeyProvider) -> Self {
        Self {
            keys,
            handed: Arc::default(),
            link: Arc::default(),
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
        let group = write
            .encryption
            .map_or(workspace, |metadata| metadata.key_group(workspace));
        let opened = match self.handed(group) {
            Some(handed) => handed.open(write),
            None => self.service()?.open_history_write(workspace, write),
        };
        if opened.is_err()
            && let Some(link) = self.link.lock_recover().clone()
        {
            // A write sealed under an epoch whose key is not here yet: the person's other devices
            // are asked for the frames of the group it is sealed under, as they are for any
            // record's keys.
            link.want_write_frames(workspace, write);
        }
        opened
    }

    fn named_readers(&self, workspace: WorkspaceId, document: &str) -> Option<String> {
        self.service().ok()?.named_readers(workspace, document)
    }

    fn seal_checkpoint(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
        named: Option<&str>,
        snapshot: &[u8],
    ) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> {
        let group =
            collab_types::reader_group(workspace, named.unwrap_or(collab_types::EVERY_MEMBER))
                .ok_or("invalid reader set for compaction")?;
        match self.handed(group) {
            Some(handed) => handed.seal_checkpoint(workspace, (app, record), snapshot),
            None => self
                .service()?
                .seal_checkpoint(workspace, app, record, named, snapshot),
        }
    }

    fn seal_scoped_checkpoint(
        &self,
        workspace: WorkspaceId,
        document: (&str, &str),
        named: Option<&str>,
        readers: &std::collections::BTreeSet<[u8; 32]>,
        snapshot: &[u8],
    ) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> {
        self.service()?
            .seal_scoped_checkpoint(workspace, document, named, readers, snapshot)
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
    ) -> Self {
        let keys = KeyProvider::default();
        let handed = Arc::<std::sync::Mutex<RouteKeys>>::default();
        let link = Arc::<std::sync::Mutex<Option<Arc<workspace_keys::KeyLink>>>>::default();
        owner.keep_sealed_histories(Arc::new(HistoryKeys {
            keys: keys.clone(),
            handed: Arc::clone(&handed),
            link: Arc::clone(&link),
        }));
        Self {
            owner,
            shown,
            identity,
            keys,
            link,
            arrivals: Arc::new(crate::arrivals::Arrivals::default()),
            handed,
            group_keys: std::sync::Mutex::default(),
            service_root: None,
            open_links: std::sync::Mutex::default(),
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
    ) -> Result<Self, String> {
        let state = state.ok_or("this session keeps no state, so it holds no workspace")?;
        let path = owner_path(state)?;
        let shown = Arc::new(Shown(std::sync::Mutex::new(
            workspaces.into_iter().collect(),
        )));
        let owner = open_file_with(&path, Arc::clone(&shown) as Arc<dyn FileHost>)?;
        let mut engine = Self::holding(owner, shown, identity);
        engine.service_root = Some(state.join("tilo").join("workspace-service"));
        Ok(engine)
    }

    /// This device's engine, when this computer has an identity and a state directory to keep its
    /// workspaces in.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn for_this_device(
        state: Option<&Path>,
        workspaces: impl IntoIterator<Item = WorkspaceId>,
    ) -> Result<Self, String> {
        let identity = identity::device_identity()
            .ok_or("this computer has no identity yet, so it holds no workspace")?;
        Self::open(identity, state, workspaces)
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

    /// Where the device hosting `workspace` is reached, when this device is an admitted member of
    /// it and another device hosts it: what this device's workspace service holds its member copy
    /// of `workspace` by. The host its own entry names, else a device the workspace's governance
    /// names among those keeping it, as a device that entered as a further device of its person
    /// holds no entry of its own. `None` for a workspace hosted here, not admitted yet, or not held.
    #[must_use]
    pub fn member_of(
        &self,
        workspace: WorkspaceId,
    ) -> Option<tilo_peer_state::join::TransportHint> {
        let own = self.identity.device_public_key();
        self.owner
            .with_current(workspace, |view| {
                if Admission::of(view, own) != Admission::Admitted {
                    return Ok(None);
                }
                let entry = view.transport_hint().ok().cloned();
                Ok(entry
                    .into_iter()
                    .chain(view.keeping_devices())
                    .find(|hint| !hint.host.is_of_device(&own)))
            })
            .ok()
            .flatten()
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
            None => return Err("this session's key service is not running".to_owned()),
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

    /// Keys for a new write to this document. The workspace key remains valid
    /// only while every workspace key holder may read the document.
    pub fn document_seal_keys(&self, route: &DocumentRoute) -> Result<SealKeys, String> {
        self.documents_seal_keys(std::slice::from_ref(route))
            .pop()
            .unwrap_or_else(|| Err("no keys were asked".to_owned()))
    }

    /// [`Self::document_seal_keys`] of each of `routes`, in their order. Whether each needs keys
    /// of its own is read off one reading of who holds its workspace's keys
    /// ([`tilo_peer_state::DocumentKeysBasis`]), which a workspace of hundreds makes the larger part
    /// of the work.
    pub fn documents_seal_keys(&self, routes: &[DocumentRoute]) -> Vec<Result<SealKeys, String>> {
        let now = tilo_clock::wall();
        let workspaces: BTreeSet<_> = routes
            .iter()
            .filter_map(|route| route.workspace_id().ok())
            .collect();
        let mut isolated: BTreeMap<usize, Result<bool, String>> = BTreeMap::new();
        for workspace in workspaces {
            let of_workspace = || {
                routes
                    .iter()
                    .enumerate()
                    .filter(move |(_, route)| route.workspace_id().ok() == Some(workspace))
            };
            let read = self.owner.with_current(workspace, |view| {
                let basis = view.peer().document_keys_basis(now);
                Ok(of_workspace()
                    .map(|(at, route)| (at, basis.requires(&route.app_id, &route.document)))
                    .collect::<Vec<_>>())
            });
            match read {
                Ok(read) => isolated.extend(read),
                Err(error) => {
                    isolated.extend(of_workspace().map(|(at, _)| (at, Err(error.clone()))));
                }
            }
        }
        routes
            .iter()
            .enumerate()
            .map(|(at, route)| match isolated.remove(&at) {
                Some(isolated) => self.route_seal_keys(route, isolated?),
                None => Err(route
                    .workspace_id()
                    .err()
                    .map_or_else(|| "no workspace".to_owned(), |error| error.to_string())),
            })
            .collect()
    }

    /// The keys a new write to `route` seals with, `isolated` when it needs keys of its own.
    fn route_seal_keys(&self, route: &DocumentRoute, isolated: bool) -> Result<SealKeys, String> {
        let workspace = route.workspace_id().map_err(|error| error.to_string())?;
        let named = self.named_document_readers(workspace, &route.document)?;
        if !isolated {
            return match named {
                Some(readers) => self.reader_keys(workspace, &readers),
                None => self.seal_keys(workspace),
            };
        }
        self.scoped_keys(route, named.as_deref())
    }

    /// Open encrypted content referenced by a document as its current reader.
    pub fn document_content_keys(
        &self,
        route: &DocumentRoute,
        group: WorkspaceId,
    ) -> Result<SealKeys, String> {
        let workspace = route.workspace_id().map_err(|error| error.to_string())?;
        if !self.document_read_policy(workspace)(&route.app_id, &route.document) {
            return Err("this device cannot read the document's content".into());
        }
        let readers = self.named_document_readers(workspace, &route.document)?;
        let original = readers.as_deref().unwrap_or(collab_types::EVERY_MEMBER);
        if collab_types::reader_group(workspace, original) == Some(group) {
            return self.reader_keys(workspace, original);
        }
        if route
            .key_scope_for(readers.as_deref())
            .map_err(|error| error.to_string())?
            .group()
            != group
        {
            return Err("the content belongs to another document group".into());
        }
        self.scoped_keys(route, readers.as_deref())
    }

    pub fn named_document_readers(
        &self,
        workspace: WorkspaceId,
        document: &str,
    ) -> Result<Option<String>, String> {
        if !neighbour_wire::KeyScope::has_named_readers(document) {
            return Ok(None);
        }
        let readers = self
            .keys
            .get()
            .and_then(|service| service.named_readers(workspace, document))
            .ok_or("the document's named readers have not arrived")?;
        let person = hex::encode(self.identity.device_cert.user_public_key);
        if !readers.split(',').any(|reader| reader == person) {
            return Err("this device is outside the document's named reader set".into());
        }
        Ok(Some(readers))
    }

    fn scoped_keys(
        &self,
        route: &DocumentRoute,
        readers: Option<&str>,
    ) -> Result<SealKeys, String> {
        let scope = route
            .key_scope_for(readers)
            .map_err(|error| error.to_string())?;
        let service = self
            .keys
            .get()
            .ok_or("this session's key service is not running")?;
        let group = service
            .register_scope(scope)
            .ok_or("invalid document key scope")?;
        Ok(match service.access(group).0 {
            KeyAccess::Ready(keys) => SealKeys::Here(keys),
            KeyAccess::Waiting | KeyAccess::Unavailable => SealKeys::Waiting,
            KeyAccess::Removed => return Err("this device's document access was removed".into()),
            KeyAccess::NoSecretStore => SealKeys::NoSecretStore,
        })
    }

    /// The keys this device seals a document only `readers` read with, in `workspace`: the
    /// workspace's own for every member, else the group of that reader set
    /// ([`collab_types::reader_group`]), whose keys only its people's devices hold.
    /// # Errors
    /// As [`Self::seal_keys`], and when `readers` names no set.
    pub fn reader_keys(&self, workspace: WorkspaceId, readers: &str) -> Result<SealKeys, String> {
        if readers == collab_types::EVERY_MEMBER {
            return self.seal_keys(workspace);
        }
        let keys = self
            .keys
            .get()
            .ok_or("this session's key service is not running")?;
        let group = keys
            .reader_group(workspace, readers)
            .ok_or("the reader set names no one")?;
        Ok(match keys.access(group).0 {
            KeyAccess::Ready(keys) => SealKeys::Here(keys),
            KeyAccess::Waiting | KeyAccess::Unavailable => SealKeys::Waiting,
            KeyAccess::Removed => {
                return Err("the reader set's group removed this device".to_owned());
            }
            KeyAccess::NoSecretStore => SealKeys::NoSecretStore,
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
        self.members(workspace).map(|list| list.members)
    }

    /// [`Self::people`], with the devices each of them holds now: the keys a change their device
    /// signed is written under, such as the workspace's own governance, which names them.
    ///
    /// # Errors
    /// The workspace is not held here, or its governance cannot be read.
    pub fn members(&self, workspace: &str) -> Result<tilo_peer_state::MemberList, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.owner.with_current(workspace, |view| {
            view.peer()
                .governance()
                .governed_members(tilo_clock::wall())
        })
    }

    /// What the calls of `workspace` allow, by channel, and the person each of its member devices
    /// is: what a live session holds the workspace's call media to
    /// ([`tilo_live::live_session::LiveSession::set_calls`]). Nothing for a workspace this device is not in.
    #[must_use]
    pub fn call_gate(&self, workspace: &str) -> tilo_peer_state::CallGate {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return (BTreeMap::new(), BTreeMap::new());
        };
        self.owner
            .with_current(workspace, |view| {
                view.peer().governance().call_gate(tilo_clock::wall())
            })
            .unwrap_or_default()
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

    /// Whether this device reads `app`'s `document` of `workspace` now, as its governance decides
    /// it: a record the workspace lets no one here read, or one its reader set leaves this device
    /// out, is not read. Decided on the governance this device holds, without opening the record:
    /// a predicate for what lists and copies may include.
    pub fn document_read_policy(
        &self,
        workspace: WorkspaceId,
    ) -> impl Fn(&str, &str) -> bool + use<> {
        let owner = self.owner.clone();
        let own = self.identity.device_cert.device_public_key;
        move |app, document_id| {
            owner
                .with_current(workspace, |view| {
                    let reader = Admission::of(view, own).reader(own);
                    view.peer()
                        .check_document_read(reader, app, document_id, tilo_clock::wall())
                })
                .is_ok()
        }
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

    /// Whether the governance this device holds of `workspace` refuses this person inviting into
    /// it: their part there does not let them, or this device is not in it. A workspace this
    /// device has no committed view of yet — one it is still taking in — is not refused: it has no
    /// part to read here yet, and its holder judges the invitation once it is.
    #[must_use]
    pub fn invite_refused(&self, workspace: &str) -> bool {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        let key = self.identity().signer.as_ref();
        self.owner
            .with_current(workspace, |view| {
                Ok(!view.peer().governance().may_invite(key, tilo_clock::wall()))
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
                })
            })
            .collect();
        holders.sort_by(|a, b| a.label.cmp(&b.label));
        holders
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

    /// Read `workspace`'s records with `keys` from now on: the workspace's host handed them to this
    /// device, which takes no part in the workspace's group.
    pub fn hand_keys(&self, workspace: WorkspaceId, keys: key_group::ContentKeys) {
        self.handed.lock_recover().insert(workspace, keys);
    }

    /// The keys `workspace`'s host handed this device, when it handed any.
    #[must_use]
    pub fn handed_keys(&self, workspace: WorkspaceId) -> Option<key_group::ContentKeys> {
        self.handed.lock_recover().get(&workspace).cloned()
    }

    pub fn keys(&self) -> KeyProvider {
        self.keys.clone()
    }

    /// This device's collaboration identity: the key its endpoint and its channels sign with, and
    /// the certificate that says whose it is.
    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
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

/// What the broker's writes ask of this device's workspaces: the engine's owner is the
/// canonical copy of every workspace it holds, and what it commits leaves on the peer network's
/// session with the workspace's other devices.
#[cfg(not(target_arch = "wasm32"))]
impl tilo_broker_core::workspace_commits::WorkspaceCommits for Engine {
    fn projects_workspace(&self, workspace_id: WorkspaceId) -> bool {
        self.holds(workspace_id)
    }

    fn health(&self) -> Result<(), String> {
        self.owner.health()
    }

    fn effect(
        &self,
        workspace: String,
        redacted: Vec<u8>,
    ) -> tilo_broker_core::workspace_commits::EffectFuture {
        let owner = self.owner.clone();
        let caller = self.identity.device_cert.clone();
        let now = tilo_clock::wall();
        Box::pin(async move {
            let workspace_id = WorkspaceId::parse(&workspace)
                .ok_or_else(|| format!("{workspace} is not a workspace"))?;
            let receipt: tilo_desktop_model::WorkspaceEffectReceipt =
                serde_json::from_slice(&redacted).map_err(|error| error.to_string())?;
            owner.record_effect(workspace_id, &caller, &receipt, now)
        })
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
    use super::*;
    use tilo_peer_state::workspace_documents::DeviceDocuments as _;
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

    /// This device's engine, kept in `state` .
    fn engine(state: &Path, identity: DeviceIdentity) -> Engine {
        Engine::open(identity, Some(state), Vec::new()).expect("an engine")
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
        let copy = Arc::new(engine(state.path(), copy));
        copy.note_link(workspace, lapses);
        assert!(
            !copy.invitations_open(now),
            "a joiner dials the holder, not this copy"
        );
    }

    /// `written` committed as `engine`'s device's sealed note `note` of `workspace`, as a write of
    /// its session commits it.
    fn commit_note(
        engine: &Engine,
        workspace: WorkspaceId,
        written: &[u8],
        keys: &key_group::ContentKeys,
    ) {
        let (app, doc) = ("so.tilo.notes", "note");
        let device = engine.identity.device_cert.device_public_key;
        let now = tilo_clock::wall();
        pollster::block_on(engine.owner.write_local(
            workspace,
            device,
            engine.identity.signer.clone(),
            tilo_peer_state::owner::LocalWrite {
                app,
                document: doc,
                readers: None,
                changes: written,
                founding: false,
                snapshot: true,
                sealed: Some(keys),
                commit: None,
                now,
            },
        ))
        .expect("the laptop writes its note");
    }

    /// A machine copied from a device, before the workspace admits it, reads what the copy holds
    /// through its page holder, and nothing it does not hold.
    #[test]
    fn a_copy_awaiting_its_host_reads_what_it_holds() {
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
            commit_note(&engine, workspace, &written, &keys);
        }

        let (copy, _) = device_of([0x44; 32], DeviceCapabilities::member());
        let copy = Arc::new(engine(state.path(), copy));
        // The keys the copy holds of the workspace.
        copy.hand_keys(workspace, keys);
        let id = workspace.to_string();
        assert!(
            copy.awaited_host(workspace).is_some(),
            "the copy waits for the workspace to admit it"
        );
        let holder = page_holder(&copy, workspace).expect("the copy holds the workspace");
        let read = match holder
            .read(&id, "so.tilo.notes", "note", None)
            .expect("the copy reads the note it holds")
        {
            tilo_peer_state::workspace_documents::DocumentRead::Whole { bytes, .. } => bytes,
            read => panic!("the note is here, not {read:?}"),
        };
        let read = automerge::AutoCommit::load(&read).expect("a note");
        assert_eq!(
            automerge::ReadDoc::get(&read, automerge::ROOT, "title")
                .expect("a title")
                .map(|(value, _)| value.to_string()),
            Some("\"Kept\"".to_owned())
        );
        assert!(
            !matches!(
                holder.read(&id, "so.tilo.notes", "other", None),
                Ok(tilo_peer_state::workspace_documents::DocumentRead::Whole { .. })
            ),
            "what the copy does not hold reads as nothing"
        );
    }
}
