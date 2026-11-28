//! This device's workspaces: the engine that holds the state of every workspace the
//! person joined, reads its people and what they may do there, and seals and opens
//! its records' writes with the keys its channels give it.
//! A workspace's state — its people, what its apps recorded, its history — is held
//! by the peer-state engine ([`DeviceWorkspaces`]), which a session wires up here
//! with this device's identity.

// A page's session holds the same engine on one thread, where a few of its methods have no
// caller: the ones only a desktop's build, which keeps its own state, reaches.
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "a page's session calls fewer of the engine's methods than a desktop's"
    )
)]
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tilo_action_policy::WorkspacePolicy;
use tilo_desktop_model::WorkspaceId;
use tilo_identity::identity_store::DeviceIdentity;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::Member;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_peer_state::owner::DeviceWorkspaces;
use tilo_user_identity::RevocationList;
use tilo_workspace_keys::{KeyAccess, KeyProvider};

mod documents;

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

/// This device's workspaces, and the identity they are held with.
pub struct Engine {
    owner: DeviceWorkspaces,
    identity: DeviceIdentity,
    /// Where the workspaces' channels find this device's keys, once the channel service is running:
    /// the session opens the workspaces before it opens the service, and every reader of a sealed
    /// record goes through this.
    keys: KeyProvider,
    /// What lands here from the workspaces' other devices: what a read waiting for a record it
    /// holds none of wakes on.
    arrivals: Arc<crate::arrivals::Arrivals>,
    /// Keys of workspaces whose group this device takes no part in, handed to it by their host
    /// ([`Self::hand_keys`]): a sealed record they open reads with them.
    handed: Arc<std::sync::Mutex<RouteKeys>>,
    /// The keys each record's channel last gave this device: they seal and open while the channel
    /// asks for access again.
    group_keys: std::sync::Mutex<RouteKeys>,
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
/// ([`DeviceWorkspaces::keep_sealed_histories`]): those the workspace's host handed it
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
        let group = write
            .encryption
            .map_or(workspace, |metadata| metadata.key_group(workspace));
        match self.handed(group) {
            Some(handed) => handed.open(write),
            None => self.service()?.open_history_write(workspace, write),
        }
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
    pub fn holding(owner: DeviceWorkspaces, identity: DeviceIdentity) -> Self {
        let keys = KeyProvider::default();
        let handed = Arc::<std::sync::Mutex<RouteKeys>>::default();
        owner.keep_sealed_histories(Arc::new(HistoryKeys {
            keys: keys.clone(),
            handed: Arc::clone(&handed),
        }));
        Self {
            owner,
            identity,
            keys,
            arrivals: Arc::new(crate::arrivals::Arrivals::default()),
            handed,
            group_keys: std::sync::Mutex::default(),
        }
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

    /// What lands here from the workspaces' other devices: the event a read waiting for one waits
    /// on.
    #[must_use]
    pub fn arrivals(&self) -> Arc<crate::arrivals::Arrivals> {
        Arc::clone(&self.arrivals)
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

    /// Whether this device reads `app`'s `document` of `workspace` now, as its governance decides
    /// it: a record the workspace lets no one here read, or one its reader set leaves this device
    /// out, is not read. Decided on the governance this device holds, without opening the record.
    fn document_read_policy(&self, workspace: WorkspaceId) -> impl Fn(&str, &str) -> bool + use<> {
        let owner = self.owner.clone();
        move |app, document_id| {
            owner
                .with_current(workspace, |view| {
                    let reader = view.device().device_public_key;
                    view.peer()
                        .check_document_read(reader, app, document_id, tilo_clock::wall())
                })
                .is_ok()
        }
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
            view.peer().governance().member_list(tilo_clock::wall())
        })
    }

    /// The people of `workspace` and the grants it makes, as its governance records
    /// them: the one source of who shares it, in what part, and what each may do.
    /// # Errors
    /// The workspace is not held here, or its governance cannot be read.
    pub fn policy(&self, workspace: &str) -> Result<WorkspacePolicy, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        self.owner
            .with_current(workspace, |view| view.peer().governance().member_policy())
    }

    /// Take `workspace` out of this device's record, with everything of it this device
    /// holds. Answers whether the record held it.
    /// # Errors
    /// The record cannot be published.
    pub fn delete(&self, workspace: WorkspaceId) -> Result<bool, String> {
        self.owner.release_workspace(workspace)
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

    /// The engine's owner, for what holds the workspace's session.
    pub fn owner(&self) -> &DeviceWorkspaces {
        &self.owner
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
