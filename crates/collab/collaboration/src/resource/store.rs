//! The live resource store: the documents an app process has open, where each is kept on this
//! device, the history each keeps, the actions in flight on them, and what the workspace refused
//! or was told of them. The toolkit shows them; a server, a worker or another process keeps them the same
//! way through this store.

use std::collections::{BTreeMap, HashMap, HashSet};
use tilo_sort::CompactSort as _;

use crate::portable_storage::PortableStorage;
mod portable;
use std::path::PathBuf;
use std::rc::Rc;
use tilo_automerge::ApplyIncoming;

use durable_store::{DurableStore, StorageKey};
use tilo_app_id::AppId;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc};

mod history_access;
mod replication;
mod retained;
pub use history_access::HistoryUnavailable;
pub mod sync;
pub use sync::{DocumentSync, Outbound};
mod actions;
pub mod bootstrap;
pub mod index;
#[cfg(test)]
mod transfer_tests;
pub use actions::{ResourceActions, ToolkitActions};

#[cfg(test)]
use super::{
    BlobRef, MAX_FORMER_IDS, add_blob_reference, blob_references, children_resource_id,
    former_identities, init,
};
use super::{
    DocumentCheckout, DocumentId, DocumentRef, INDEX, ResourceOpenError, ResourceType,
    add_reference, adopt_former, commit_genesis, genesis, genesis_actor, home, identity,
    init_scoped, references, str_at, transfer, validate_identity, workspace_scope,
};
pub use crate::document::ResourceSaveStatus;
use crate::document::fields::{META, ObjectAt, StringAt, object_at, string_at};

/// The scope of the person's own resources, `(app, personal)`: theirs and not
/// a workspace's, so it is carried to their devices by their personal store
/// and never by a workspace's route.
pub const PERSONAL_SCOPE: &str = "personal";

/// Upper bound for the records an app learns from one workspace index at once: the index is
/// replicated input, not a request to allocate one local document per entry.
pub const MAX_DISCOVERED_RESOURCES: usize = 64;
const MAX_INDEX_KEYS_SCANNED: usize = MAX_DISCOVERED_RESOURCES * 4;
/// The longest workspace index this store reads: automerge reads a document whole, and an index is
/// a list of names rather than the records' own content.
const MAX_INDEX_BYTES: usize = 16 * 1024 * 1024;

/// What a toolkit learns of each read of an open resource, so a pass that read it is laid out again once
/// the resource moved.
pub type ReadObserver = fn(&ResourceStore, &DocumentId, &AutoCommit);

static READ_OBSERVER: std::sync::OnceLock<ReadObserver> = std::sync::OnceLock::new();

/// Tell `observer` of every read of an open resource from now on. The first observer installed in a
/// process stays; a process with no toolkit reads unobserved.
pub fn observe_reads(observer: ReadObserver) {
    let _ = READ_OBSERVER.set(observer);
}

fn observed_read(store: &ResourceStore, id: &DocumentId, document: &AutoCommit) {
    if let Some(observer) = READ_OBSERVER.get() {
        observer(store, id, document);
    }
}

/// One record this process has open but is not currently showing.
struct Parked {
    /// `None` exactly while checked out — the live document is the
    /// toolkit's, not this one's.
    doc: Option<AutoCommit>,
    kind: ResourceType,
    /// Debounced writer for this record's file. `None` for an in-memory
    /// set, or when the store failed to open (reported once, not per frame).
    autosave: Option<app_state::DocWriter>,
    /// Belongs to the record: an undo entry is a pair of change hashes in *this* document's graph,
    /// so sharing a stack would revert the wrong history.
    history: crate::state::UndoLog,
    action_state: ResourceActions,
}

/// The records one app process has open, and which is live. Owns per-record persistence and
/// parking, not collaboration: routing a record's deltas is the toolkit's business, since a channel
/// belongs to the connection.
pub struct ResourceSet {
    /// Moves on every write access to the set: whatever changed a resource's
    /// document (a commit, a merge, a migration, an open or a close) took one,
    /// so a reader that saw the same number has nothing new to look at
    /// ([`ResourceStore::write_generation`]).
    generation: std::cell::Cell<u64>,
    app_id: AppId,
    /// The workspace these records are opened in, which is the cluster its
    /// route names. Stamped as a record's home when it is first created here.
    home: String,
    /// The person's own workspace, for records kept in their own scope: their route names it, so one
    /// record of an app is one record on every device they use, in their store rather than a
    /// workspace's.
    person: Option<uuid::Uuid>,
    open: HashMap<DocumentId, Parked>,
    /// Names currently granted by the workspace index. Discovery is separate from `open`: a hostile
    /// or merely large index must not create local replicas or subscribe the app to unrelated
    /// documents.
    known: BTreeMap<DocumentId, ResourceType>,
    active: Option<DocumentId>,
    /// False for [`ResourceSet::in_memory`]: no per-record files are opened, so
    /// a test drives the same checkout path without touching the disk.
    persistent: bool,
    /// Where the launch-environment route keeps its documents: shared
    /// writers on app-scoped keys. `None` for other sets.
    documents: Option<Rc<dyn DurableStore>>,
    /// The host's storage, for a set opened through [`ResourceOwners`]. `None`
    /// for the launch-environment route and for [`ResourceSet::in_memory`].
    backend: Option<Rc<dyn DurableStore>>,
    /// A hydrated, host-acknowledged browser cache (Cloud portable sessions).
    portable: Option<PortableStorage>,
    pending: HashMap<DocumentId, Reserved>,
    /// Records being opened off the frame ([`ResourceStore::begin_opening`]): not open until they
    /// are taken in.
    opening: HashMap<DocumentId, opening::Opening>,
    /// The records lent out to an action frame: their documents are with the frame, so the other
    /// records stay open to it meanwhile.
    lent: HashSet<DocumentId>,
    next_reservation: u64,
    // Retained from the private native construction scope; weakly borrows the
    // one actual worker after construction ends. Never a positive authority.
    native_initial: Option<crate::native_initial::InitialReadBridge>,
    /// How many times a record arrived in the backend without being
    /// opened here ([`ResourceStore::note_kept`]).
    kept: std::cell::Cell<u64>,
    /// Records of this store's trees peers wrote that it does not hold yet
    /// ([`ResourceStore::announce`]).
    announced: std::cell::RefCell<std::collections::BTreeSet<DocumentId>>,
    /// Records the workspace's hosts answered for, and what they answered, until something of them
    /// arrives.
    answered: std::cell::RefCell<std::collections::BTreeMap<DocumentId, WorkspaceAnswer>>,
    /// This store's records come from its workspace's hosts
    /// ([`ResourceStore::ask_workspace_for_missing`]): a record not here is
    /// asked for and new only once they answered they hold none of it.
    asks_workspace: std::cell::Cell<bool>,
    /// The records not here that were opened while [`Self::asks_workspace`]:
    /// what the workspace is asked for ([`ResourceStore::wanted_resources`]).
    wanted: std::cell::RefCell<std::collections::BTreeSet<DocumentId>>,
    /// The kind each of `wanted` was opened as, by id: what a document the view opened from
    /// its workspace is held beside it as when it arrives
    /// ([`ResourceStore::wanted_kind`]).
    wanted_kinds: std::cell::RefCell<std::collections::BTreeMap<DocumentId, ResourceType>>,
    /// The windows that read this store's records, woken when another replica's change arrives,
    /// which no input or wake of theirs announces.
    readers: std::cell::RefCell<Vec<crate::WakeHandle>>,
    // Drop after the writers and reservations above. Temporary profiles must
    // outlive asynchronous flushes and recovery handles, not just their host.
    _profile_lifetime: Option<std::rc::Rc<dyn std::any::Any>>,
}

struct Reserved {
    generation: u64,
    kind: ResourceType,
    /// The durable owner reserved for a store-backed record; `None` for a
    /// portable reservation, whose cache has no single-writer owner.
    store: Option<Rc<app_state::DocOwner>>,
    /// The persistent actor minted for a portable reservation.
    actor: Option<automerge::ActorId>,
}

impl ResourceSet {
    /// The launch-environment set: per-record documents under this app's state directory. The
    /// host's app id wins over `manifest_id`, as the workspace index does, or an app's
    /// records would split from its index; the host's workspace id scopes them.
    pub fn open(manifest_id: impl Into<String>) -> Self {
        let environment = crate::process::environment();
        let identity = &environment.identity;
        let app_id = AppId::new(
            identity
                .app_id
                .clone()
                .unwrap_or_else(|| manifest_id.into()),
        );
        let home = identity
            .workspace_id
            .clone()
            // A launcher-less process still gets an explicit, non-routable scope: it may not
            // collaborate, but it must never fall back to the historical app-global file.
            .unwrap_or_else(|| uuid::Uuid::nil().to_string());
        Self::open_in(app_state::DocStore::native_store(), app_id, home)
    }

    /// The launch-environment set over an explicit store: each record is a shared-writer document
    /// in workspace `home`, and pre-workspace records are imported into exactly one
    /// workspace on first open.
    pub fn open_in(
        store: Rc<dyn DurableStore>,
        app_id: impl Into<AppId>,
        home: impl Into<String>,
    ) -> Self {
        Self {
            generation: std::cell::Cell::new(0),
            app_id: app_id.into(),
            home: home.into(),
            person: None,
            open: HashMap::new(),
            known: BTreeMap::new(),
            active: None,
            persistent: true,
            documents: Some(store),
            backend: None,
            portable: None,
            pending: HashMap::new(),
            opening: HashMap::new(),
            lent: HashSet::new(),
            next_reservation: 0,
            native_initial: crate::native_initial::capture_bridge(),
            kept: std::cell::Cell::new(0),
            announced: std::cell::RefCell::default(),
            answered: std::cell::RefCell::default(),
            asks_workspace: std::cell::Cell::new(false),
            wanted: std::cell::RefCell::default(),
            wanted_kinds: std::cell::RefCell::default(),
            readers: std::cell::RefCell::default(),
            _profile_lifetime: None,
        }
    }

    /// The app these records are stored under.
    pub fn app_id(&self) -> &AppId {
        &self.app_id
    }

    /// A set that never touches the disk. For tests, for `--shots`, and for
    /// any run that must not leave state behind.
    pub fn in_memory() -> Self {
        Self {
            generation: std::cell::Cell::new(0),
            app_id: AppId::default(),
            home: String::new(),
            person: None,
            open: HashMap::new(),
            known: BTreeMap::new(),
            active: None,
            persistent: false,
            documents: None,
            backend: None,
            portable: None,
            pending: HashMap::new(),
            opening: HashMap::new(),
            lent: HashSet::new(),
            next_reservation: 0,
            native_initial: None,
            kept: std::cell::Cell::new(0),
            announced: std::cell::RefCell::default(),
            answered: std::cell::RefCell::default(),
            asks_workspace: std::cell::Cell::new(false),
            wanted: std::cell::RefCell::default(),
            wanted_kinds: std::cell::RefCell::default(),
            readers: std::cell::RefCell::default(),
            _profile_lifetime: None,
        }
    }

    /// The workspace these records are opened in.
    pub fn home(&self) -> &str {
        &self.home
    }

    /// Name the workspace explicitly. Used by tests and by a host that opens
    /// records on behalf of a workspace other than its own launch environment.
    #[must_use]
    pub fn with_home(mut self, home: impl Into<String>) -> Self {
        self.home = home.into();
        self
    }

    /// The record currently checked out, if any.
    pub fn active(&self) -> Option<&DocumentId> {
        self.active.as_ref()
    }

    pub fn record_active_private_action(
        &mut self,
        action: app_state::PrivateAction,
    ) -> Result<(), String> {
        let id = self.active.as_ref().ok_or("no active record")?;
        let slot = self.open.get_mut(id).ok_or("active record is not open")?;
        slot.autosave
            .as_mut()
            .ok_or("record has no private durable writer")?
            .record_private_action(action)
    }

    pub fn active_private_actions(
        &self,
        doc: &AutoCommit,
    ) -> Result<Vec<app_state::PrivateAction>, String> {
        let id = self.active.as_ref().ok_or("no active record")?;
        let slot = self.open.get(id).ok_or("active record is not open")?;
        // A session-owned record may have no process-local checkpoint store; there is then nothing
        // to recover, and writing new evidence still needs a durable writer.
        match slot.autosave.as_ref() {
            Some(writer) => writer.private_actions(doc),
            None => Ok(Vec::new()),
        }
    }

    /// Every record this process has open, checked-out one included.
    pub fn open_resources(&self) -> Vec<DocumentCheckout> {
        let mut out: Vec<DocumentCheckout> = self
            .open
            .iter()
            .map(|(id, p)| DocumentCheckout::new(id.clone(), p.kind.clone()))
            .collect();
        out.compact_sort();
        out
    }

    /// The bounded, validated names currently granted by the workspace index, without implying
    /// their documents are loaded, persisted or joined on the transport.
    pub fn known_resources(&self) -> Vec<DocumentCheckout> {
        self.known
            .iter()
            .map(|(id, kind)| DocumentCheckout::new(id.clone(), kind.clone()))
            .collect()
    }

    /// Refresh the workspace-granted names without opening any: a removed or tombstoned entry
    /// disappears from future discovery, but open data stays in memory, since revocation cannot
    /// un-share bytes already received.
    pub fn discover_workspace(&mut self, workspace: &AutoCommit) -> Vec<DocumentCheckout> {
        let found = index::discoverable_references(workspace);
        self.known = found
            .iter()
            .map(|checkout| (checkout.id.clone(), checkout.kind.clone()))
            .collect();
        found
    }

    /// [`ResourceOpenError::Busy`] while `id` is lent to an action frame.
    fn not_lent(&self, id: &DocumentId) -> Result<(), ResourceOpenError> {
        if self.lent.contains(id) {
            Err(ResourceOpenError::Busy)
        } else {
            Ok(())
        }
    }

    fn remember(&mut self, want: &DocumentCheckout) {
        if want.is_valid() {
            self.known.insert(want.id.clone(), want.kind.clone());
            for (target, _) in &want.references {
                if target.is_valid() {
                    self.known.insert(target.id.clone(), target.kind.clone());
                }
            }
        }
    }

    /// Read a parked record's document without checking it out. `None` for
    /// the record currently checked out — its document is the live one.
    pub fn peek(&self, id: &DocumentId) -> Option<&AutoCommit> {
        self.open.get(id).and_then(|p| p.doc.as_ref())
    }

    /// The cluster a record's route names: the workspace it is kept in, or
    /// the person's own workspace for a record kept in their own scope.
    fn cluster(&self) -> Result<uuid::Uuid, ResourceOpenError> {
        if let Some(person) = self.person {
            return Ok(person);
        }
        uuid::Uuid::parse_str(&self.home).map_err(|_| ResourceOpenError::IdentityMismatch)
    }

    /// Primary stays `record=None` on every host.
    fn wire_route(&self, id: &DocumentId) -> Result<crate::RouteSpec, ResourceOpenError> {
        let cluster = self.cluster()?;
        Ok(if id.is_package_primary() {
            crate::RouteSpec::app(self.app_id.as_str(), cluster)
        } else {
            crate::RouteSpec::document(self.app_id.as_str(), cluster, id.as_str())
        })
    }

    /// Native caller has already read this exact route through its admitted SDK
    /// channel. Install that graph before the legacy empty-document initializer.
    ///
    /// `merge_into_live` merges the same bytes into the live document when it already holds
    /// finished local work, and says whether it took them.
    pub fn checkout_with_initial(
        &mut self,
        state: &mut crate::state::State,
        workspace: Option<&mut AutoCommit>,
        want: &DocumentCheckout,
        mutation: &collab_types::DocumentMutation,
        merge_into_live: impl FnOnce(&mut crate::state::State) -> bool,
    ) -> bool {
        let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
            return false;
        };
        if !want.is_valid()
            || self.backend.is_some()
            || self.portable.is_some()
            || state.doc.pending_ops() != 0
        {
            return false;
        }
        let Ok(mut incoming) = AutoCommit::load(bytes) else {
            return false;
        };
        if validate_identity(&incoming, want, &self.home).is_err() {
            return false;
        }
        if self.active.is_none() && !state.doc.base_heads().is_empty() {
            // Existing, finished local work is merged, never replaced. An open
            // local transaction was refused above without forcing a commit.
            if !merge_into_live(state) {
                return false;
            }
            return self.checkout(state, workspace, want);
        }
        if let Some(slot) = self.open.get_mut(&want.id) {
            let Some(doc) = slot.doc.as_mut() else {
                return false;
            };
            if doc.pending_ops() != 0 {
                return false;
            }
            if doc.merge_incoming(&mut incoming).is_err() {
                return false;
            }
        } else {
            let Ok(autosave) = self.persist_for(want, &mut incoming) else {
                return false;
            };
            if !init_scoped(
                &mut incoming,
                &want.id,
                &want.kind,
                &self.home,
                (!self.home.is_empty()).then_some(self.home.as_str()),
            ) {
                return false;
            }
            let history = crate::state::UndoLog::new(&mut incoming);
            self.open.insert(
                want.id.clone(),
                Parked {
                    doc: Some(incoming),
                    kind: want.kind.clone(),
                    autosave,
                    history,
                    action_state: ResourceActions::default(),
                },
            );
        }
        self.checkout(state, workspace, want)
    }

    /// Switch the live document to `want`: the outgoing one is parked and flushed, the incoming
    /// loaded (or created) and referenced into the workspace's index. Returns whether anything
    /// changed, so a widget can declare its record every frame.
    pub fn checkout(
        &mut self,
        state: &mut crate::state::State,
        workspace: Option<&mut AutoCommit>,
        want: &DocumentCheckout,
    ) -> bool {
        if !want.is_valid() {
            tracing::warn!(record = %want.id, "refusing invalid record checkout");
            return false;
        }
        if self.backend.is_some() || self.portable.is_some() {
            return false;
        }
        if self.active.as_ref() == Some(&want.id) {
            return false;
        }

        // A document with content and no record *becomes* this record's rather than being replaced,
        // which is "save untitled as report.odt"; replacing it would be the data loss records exist
        // to prevent.
        if self.active.is_none() && !state.doc.get_heads().is_empty() {
            let adopted = std::mem::replace(&mut state.doc, AutoCommit::new());
            state.adopt_doc(adopted);
            let Ok(autosave) = self.persist_for(want, &mut state.doc) else {
                return false;
            };
            let history = crate::state::UndoLog::new(&mut state.doc);
            self.open.insert(
                want.id.clone(),
                Parked {
                    doc: None,
                    kind: want.kind.clone(),
                    autosave,
                    history,
                    action_state: ResourceActions::default(),
                },
            );
            return self.finish_checkout(state, workspace, want);
        }

        // A malformed incoming legacy document must fail before ownership of
        // the valid active document moves into a parked slot.
        let prepared = if self.open.contains_key(&want.id) {
            None
        } else {
            let Ok(prepared) = self.load(want) else {
                return false;
            };
            Some(prepared)
        };

        if let Some(prev) = self.active.take() {
            let outgoing = std::mem::replace(&mut state.doc, AutoCommit::new());
            if let Some(slot) = self.open.get_mut(&prev) {
                let mut outgoing = outgoing;
                if let Some(saver) = slot.autosave.as_mut() {
                    slot.action_state.commit_for_save(&mut outgoing);
                    saver.request(&outgoing);
                }
                slot.doc = Some(outgoing);
            }
        }

        let incoming = if let Some((mut doc, autosave)) = prepared {
            let history = crate::state::UndoLog::new(&mut doc);
            self.open.insert(
                want.id.clone(),
                Parked {
                    doc: None,
                    kind: want.kind.clone(),
                    autosave,
                    history,
                    action_state: ResourceActions::default(),
                },
            );
            doc
        } else {
            self.open
                .get_mut(&want.id)
                .and_then(|slot| slot.doc.take())
                .unwrap_or_default()
        };

        state.adopt_doc(incoming);
        self.finish_checkout(state, workspace, want)
    }

    /// Stamps identity, applies declared references and indexes the record and its references into
    /// the workspace; shared by both checkout routes.
    fn finish_checkout(
        &mut self,
        state: &mut crate::state::State,
        workspace: Option<&mut AutoCommit>,
        want: &DocumentCheckout,
    ) -> bool {
        // Persistent records always have an explicit workspace: failing closed keeps a
        // launcher-less process from recreating the old app-global namespace.
        if self.persistent && self.home.is_empty() {
            tracing::error!(record = %want.id, "refusing to open a persistent record without a workspace scope");
            return false;
        }
        let scope = (!self.home.is_empty()).then_some(self.home.as_str());
        if !init_scoped(&mut state.doc, &want.id, &want.kind, &self.home, scope) {
            tracing::warn!(record = %want.id, workspace = %self.home, "record identity or workspace scope did not match stored document");
            return false;
        }
        for (target, relation) in &want.references {
            add_reference(
                &mut state.doc,
                &DocumentRef::new(target.id.clone(), relation.clone()),
            );
        }
        if let Some(workspace) = workspace {
            index::reference(workspace, &want.id, &want.kind);
            // Targets too: a workspace holding the annotations but not their
            // target would hand a member a reference they can't reach.
            for (target, _) in &want.references {
                index::reference(workspace, &target.id, &target.kind);
            }
        }
        self.active = Some(want.id.clone());
        self.remember(want);
        true
    }

    /// Open a record's stored document, and the writer for it.
    fn load(
        &self,
        want: &DocumentCheckout,
    ) -> Result<(AutoCommit, Option<app_state::DocWriter>), ResourceOpenError> {
        if self.persistent && self.home.is_empty() {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        let initial = self
            .native_initial
            .as_ref()
            .map(|bridge| {
                bridge
                    .record(&self.app_id, &self.home, want)
                    .inspect_err(|error| {
                        // A record on its way fails nothing.
                        if let crate::native_initial::ResourceReadError::Initialization(error) =
                            error
                        {
                            crate::native_initial::fail(error);
                        }
                    })
            })
            .transpose()?
            .flatten();
        let mut doc = if let Some(read) = &initial {
            read.with_mutation(|mutation| {
                let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
                    return Err("native initial record snapshot missing".to_string());
                };
                let doc = AutoCommit::load(bytes).map_err(|error| error.to_string())?;
                validate_identity(&doc, want, &self.home).map_err(|error| error.to_string())?;
                Ok(doc)
            })
            .and_then(|result| result)
            .map_err(ResourceOpenError::Initialization)?
        } else {
            AutoCommit::new()
        };
        let autosave = self.persist_for(want, &mut doc)?;
        if self.persistent
            && !init_scoped(&mut doc, &want.id, &want.kind, &self.home, Some(&self.home))
        {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        if let Some(read) = initial {
            read.applied();
        }
        Ok((doc, autosave))
    }

    fn documents(&self) -> Result<&Rc<dyn DurableStore>, app_state::StoreError> {
        self.documents
            .as_ref()
            .ok_or_else(|| app_state::StoreError::Io {
                context: "record set has no document store".into(),
                source: std::io::Error::from(std::io::ErrorKind::Unsupported),
            })
    }

    /// The shared-writer document for one record in this set's workspace.
    fn scoped_document(
        &self,
        resource_key: &str,
    ) -> Result<app_state::DocStore, app_state::StoreError> {
        crate::document::save::resource_document(
            self.documents()?,
            &self.app_id,
            &self.home,
            resource_key,
        )
    }

    /// A former identity's document in this workspace, restamped for `want`
    /// (see [`adopt_former`]). Launch-environment route.
    fn adopt_former_scoped(
        &self,
        want: &DocumentCheckout,
    ) -> Result<Option<AutoCommit>, app_state::StoreError> {
        if want.formerly.is_empty() {
            return Ok(None);
        }
        let documents = Rc::clone(self.documents()?);
        adopt_former(
            &documents,
            want,
            &self.home,
            |former| StorageKey::document(&self.app_id, &self.home, former.storage_key()),
            |key| app_state::DocStore::shared(Rc::clone(&documents), key.clone())?.load_complete(),
        )
    }

    /// Attach `doc` to this record's file: merge in what is stored and hand back the writer. The
    /// writer is seeded from the **stored** heads, not the merged ones, or adopted content would
    /// read as already saved.
    fn persist_for(
        &self,
        want: &DocumentCheckout,
        doc: &mut AutoCommit,
    ) -> Result<Option<app_state::DocWriter>, ResourceOpenError> {
        let id = &want.id;
        if !self.persistent {
            return Ok(None);
        }
        if self.home.is_empty() {
            return Ok(None);
        }
        let resource_key = id.storage_key();
        let mut store = match self.scoped_document(&resource_key) {
            Ok(store) => store,
            Err(e) => {
                tracing::error!(
                    record = %id,
                    error = %e,
                    "record store unavailable; writes refused"
                );
                return Ok(Some(app_state::DocWriter::disabled(format!(
                    "record {id} store unavailable: {e}"
                ))));
            }
        };
        let stored = match store.load() {
            Ok(found) => found,
            Err(e) => {
                tracing::error!(
                    record = %id,
                    error = %e,
                    "stored record could not be read; original bytes retained and writes refused"
                );
                return Ok(Some(app_state::DocWriter::disabled(format!(
                    "stored record {id} could not be read: {e}"
                ))));
            }
        };
        // A former identity's record is a read-only source, left on disk until an explicit cleanup,
        // so a crash before the first scoped autosave cannot lose it.
        let mut adopted_elsewhere = false;
        let stored = match stored {
            Some(stored) => Some(stored),
            None => match self.adopt_former_scoped(want) {
                Ok(adopted) => {
                    adopted_elsewhere = adopted.is_some();
                    adopted
                }
                Err(e) => {
                    tracing::error!(record = %id, error = %e, "former record could not be read; writes refused");
                    return Ok(Some(app_state::DocWriter::disabled(format!(
                        "former record of {id} could not be read: {e}"
                    ))));
                }
            },
        };
        // Merge transactionally. Automerge may have applied part of a failed
        // merge; never publish or enqueue that partial result to the writer.
        let mut merged = doc.clone();
        let mut stored = stored;
        if let Some(stored) = stored.as_mut()
            && let Err(error) = merged.merge_incoming(stored)
        {
            tracing::error!(record = %id, %error, "record merge refused; original stored bytes retained");
            return Ok(Some(app_state::DocWriter::disabled(format!(
                "stored record {id} could not be merged: {error}"
            ))));
        }
        if !init_scoped(
            &mut merged,
            &want.id,
            &want.kind,
            &self.home,
            Some(&self.home),
        ) {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        // The package actor belongs to its index, and a bootstrap fork actor to pre-admission
        // edits; every later canonical operation belongs to this record's original persistent
        // writer.
        merged.set_actor(store.actor());
        *doc = merged;
        let mut autosave = store.writer();
        if let Some(stored) = stored {
            // Seed only heads already in this scoped store: an adopted document's heads are absent
            // from the new file, and seeding them would strand their dependencies after a crash.
            if !adopted_elsewhere {
                autosave.seed(&stored);
            }
        }
        Ok(Some(autosave))
    }

    // A host showing several records at once: none can be checked out, since all are on screen in
    // one frame, so these reach an open record where it lies, sharing the same opening, persistence
    // and history.

    /// Open `want` if it is not open already, loading its document from its own file and bringing
    /// it into the workspace's index; returns whether it opened it.
    pub fn open_resource(
        &mut self,
        want: &DocumentCheckout,
        workspace: Option<&mut AutoCommit>,
    ) -> bool {
        if !want.is_valid() {
            tracing::warn!(record = %want.id, "refusing invalid record open");
            return false;
        }
        if self.backend.is_some() || self.portable.is_some() {
            return matches!(
                self.try_open_resource(want),
                Ok(ResourceOpenOutcome::Loaded)
            );
        }
        let fresh = !self.open.contains_key(&want.id);
        if fresh {
            let Ok((mut doc, autosave)) = self.load(want) else {
                return false;
            };
            if self.persistent && self.home.is_empty() {
                tracing::error!(record = %want.id, "refusing to open a persistent record without a workspace scope");
                return false;
            }
            let scope = (!self.home.is_empty()).then_some(self.home.as_str());
            if !init_scoped(&mut doc, &want.id, &want.kind, &self.home, scope) {
                tracing::warn!(record = %want.id, workspace = %self.home, "record identity or workspace scope did not match stored document");
                return false;
            }
            for (target, relation) in &want.references {
                add_reference(
                    &mut doc,
                    &DocumentRef::new(target.id.clone(), relation.clone()),
                );
            }
            let history = crate::state::UndoLog::new(&mut doc);
            self.open.insert(
                want.id.clone(),
                Parked {
                    doc: Some(doc),
                    kind: want.kind.clone(),
                    autosave,
                    history,
                    action_state: ResourceActions::default(),
                },
            );
        }
        if let Some(workspace) = workspace {
            index::reference(workspace, &want.id, &want.kind);
            for (target, _) in &want.references {
                index::reference(workspace, &target.id, &target.kind);
            }
        }
        self.remember(want);
        fresh
    }

    fn canonical_key(
        &self,
        want: &DocumentCheckout,
    ) -> Result<(Rc<dyn DurableStore>, StorageKey), ResourceOpenError> {
        let backend = self
            .backend
            .as_ref()
            .ok_or(ResourceOpenError::IdentityMismatch)?;
        if want.id.is_package_primary() {
            // The package-primary document is the workspace's own document
            // (`<package>/workspaces/<workspace>`), never a minted record beneath it.
            app_state::validate_document_identity(&self.app_id, &self.home, "index")?;
            return Ok((
                Rc::clone(backend),
                StorageKey::workspace_index(&self.app_id, &self.home),
            ));
        }
        let record = want.id.storage_key();
        app_state::validate_document_identity(&self.app_id, &self.home, &record)?;
        Ok((
            Rc::clone(backend),
            StorageKey::document(&self.app_id, &self.home, record),
        ))
    }

    /// Whether `id` is a record of the workspace rather than of one window: its own document, the
    /// records its people share and the parts of a chain kept beside its own document (Chat's
    /// messages), each written by every window and host, which join as shared writers. Any other
    /// record has one owner.
    fn shared_in_workspace(&self, id: &DocumentId) -> bool {
        id.is_package_primary()
            || collab_types::workspace_document(&self.app_id, id.as_str()).is_some()
            || workspace_chain_part(id)
    }

    fn try_open_resource(
        &mut self,
        want: &DocumentCheckout,
    ) -> Result<ResourceOpenOutcome, ResourceOpenError> {
        self.try_open_record_with_recovery(want, true)
    }

    /// Whether opening `want` reads nothing its committed read still has to answer: it is
    /// open or opening here already, or no broker reads it, or the broker's read answered.
    fn read_answered(&self, want: &DocumentCheckout) -> Result<bool, ResourceOpenError> {
        if self.portable.is_some()
            || self.open.contains_key(&want.id)
            || self.opening.contains_key(&want.id)
        {
            return Ok(true);
        }
        self.native_initial.as_ref().map_or(Ok(true), |bridge| {
            bridge
                .answered(&self.app_id, &self.home, want)
                .map_err(ResourceOpenError::Initialization)
        })
    }

    fn try_open_record_with_recovery(
        &mut self,
        want: &DocumentCheckout,
        recover_partial: bool,
    ) -> Result<ResourceOpenOutcome, ResourceOpenError> {
        if !want.is_valid_local_storage_request() {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        if self.portable.is_some() {
            return self.open_portable(want);
        }
        let (backend, key) = self.canonical_key(want)?;
        if self.pending.contains_key(&want.id) {
            return Err(ResourceOpenError::Busy);
        }
        // A record opening off the frame is opened by taking it in, once it is decoded.
        if self.opening.contains_key(&want.id) {
            self.take_in_opening(&want.id, true)?;
        }
        if let Some(slot) = self.open.get(&want.id) {
            if slot.kind != want.kind {
                return Err(ResourceOpenError::IdentityMismatch);
            }
            validate_identity(
                slot.doc.as_ref().ok_or(ResourceOpenError::Busy)?,
                want,
                &self.home,
            )?;
            return Ok(ResourceOpenOutcome::AlreadyOpen);
        }
        // A record no committed read answers (the app's own channel serves
        // it) opens below, as without a bridge.
        if let Some(read) = self
            .native_initial
            .as_ref()
            .map(|bridge| bridge.record(&self.app_id, &self.home, want))
            .transpose()?
            .flatten()
        {
            let store = if self.shared_in_workspace(&want.id) {
                app_state::DocStore::shared(backend, key)?
            } else {
                app_state::DocStore::exclusive(backend, key)?
            };
            let (mut doc, autosave) = canonical_read::finish(store, read, |doc| {
                validate_identity(doc, want, &self.home).map_err(|error| error.to_string())
            })
            .map_err(ResourceOpenError::Initialization)?;
            let history = crate::state::UndoLog::new(&mut doc);
            self.open.insert(
                want.id.clone(),
                Parked {
                    doc: Some(doc),
                    kind: want.kind.clone(),
                    autosave: Some(autosave),
                    history,
                    action_state: ResourceActions::default(),
                },
            );
            self.answered.borrow_mut().remove(&want.id);
            return Ok(ResourceOpenOutcome::Loaded);
        }
        // On a thread that may not wait on the disk, a record's presence is told by a read, not by
        // a lease a new reservation would take again: each lease is opened by the store's own
        // thread and handed over once.
        if want.formerly.is_empty()
            && backend.declined_waiting_here()
            && backend.read(&key)?.is_none()
        {
            return Err(ResourceOpenError::MissingResource);
        }
        // The workspace's own documents are held by every window and host, each in its own process,
        // and a relaunch overlaps the instance still exiting: they join as shared writers (appends
        // interleave, every load merges) instead of refusing the second opener.
        let owner = if self.shared_in_workspace(&want.id) {
            app_state::DocOwner::acquire_shared(Rc::clone(&backend), key)?
        } else {
            app_state::DocOwner::acquire(Rc::clone(&backend), key)?
        };
        let loaded = if recover_partial {
            owner.load()?
        } else {
            owner.load_complete()?
        };
        let (mut doc, autosave) = if let Some(loaded) = loaded {
            validate_identity(loaded.document(), want, &self.home)?;
            let doc = owner.accept(loaded)?;
            let owner = Rc::new(owner);
            let autosave = app_state::DocWriter::try_start(&owner)?;
            (doc, autosave)
        } else {
            // Nothing under the new id: a former identity's record is written in full under the new
            // key before it is shown.
            let mut doc = adopt_former(
                &backend,
                want,
                &self.home,
                |former| StorageKey::document(&self.app_id, &self.home, former.storage_key()),
                |key| {
                    Ok(
                        app_state::DocOwner::acquire(Rc::clone(&backend), key.clone())?
                            .load_complete()?
                            .map(|loaded| loaded.document().clone()),
                    )
                },
            )?
            .ok_or(ResourceOpenError::MissingResource)?;
            doc.set_actor(owner.actor());
            if !init_scoped(&mut doc, &want.id, &want.kind, &self.home, Some(&self.home)) {
                return Err(ResourceOpenError::IdentityMismatch);
            }
            validate_identity(&doc, want, &self.home)?;
            commit_genesis(&mut doc);
            let owner = Rc::new(owner);
            let autosave = app_state::DocWriter::start_new(&owner, &doc)?;
            (doc, autosave)
        };
        let history = crate::state::UndoLog::new(&mut doc);
        self.open.insert(
            want.id.clone(),
            Parked {
                doc: Some(doc),
                kind: want.kind.clone(),
                autosave: Some(autosave),
                history,
                action_state: ResourceActions::default(),
            },
        );
        Ok(ResourceOpenOutcome::Loaded)
    }

    /// An open record's document, for a host showing several at once; `None` for a record not held
    /// here or the one checked out, whose live document is the toolkit's.
    pub fn document_mut(&mut self, id: &DocumentId) -> Option<&mut AutoCommit> {
        let parked = self.open.get_mut(id)?;
        if let Some(doc) = parked.doc.as_mut() {
            parked
                .action_state
                .finish_external(doc, &mut parked.history);
        }
        parked.action_state.generation = parked
            .action_state
            .generation
            .and_then(|generation| generation.checked_add(1));
        parked.doc.as_mut()
    }

    /// An open record's document together with its history, so a caller
    /// driving undo can do both against the same record.
    pub fn document_and_history_mut(
        &mut self,
        id: &DocumentId,
    ) -> Option<(&mut AutoCommit, &mut crate::state::UndoLog)> {
        let parked = self.open.get_mut(id)?;
        if let Some(doc) = parked.doc.as_mut() {
            parked
                .action_state
                .finish_external(doc, &mut parked.history);
        }
        parked.action_state.generation = parked
            .action_state
            .generation
            .and_then(|generation| generation.checked_add(1));
        let doc = parked.doc.as_mut()?;
        Some((doc, &mut parked.history))
    }

    /// Offer every open record's document to its own writer, the multi-record twin of
    /// [`Self::tick`]. # Errors A record's cache reports that it could not take the document.
    pub fn tick_open(&mut self) -> Result<(), String> {
        if self.portable.is_some() {
            return self.flush_portable(None, false);
        }
        for slot in self.open.values_mut() {
            if let (Some(doc), Some(saver)) = (slot.doc.as_mut(), slot.autosave.as_mut()) {
                slot.action_state.commit_for_save(doc);
                saver.request(doc);
            }
        }
        Ok(())
    }

    /// Write out every open record and wait for it to land, the multi-record twin of
    /// [`Self::flush`]. # Errors Each record's writer that reports a storage failure, by record.
    pub fn flush_open(&mut self) -> Result<(), String> {
        if self.portable.is_some() {
            return self.flush_portable(None, false);
        }
        let mut errors = Vec::new();
        for (id, slot) in &mut self.open {
            if let (Some(doc), Some(saver)) = (slot.doc.as_mut(), slot.autosave.as_mut()) {
                slot.action_state.commit_for_save(doc);
                saver.request(doc);
                if let Err(error) = saver.try_flush() {
                    errors.push(format!("{id}: {error}"));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    /// Offer the live document to the active record's writer. Cheap when
    /// nothing changed — a head comparison, no serialisation.
    pub fn tick(&mut self, live: &mut AutoCommit) {
        if let Some(slot) = self.active.as_ref().and_then(|id| self.open.get_mut(id))
            && let Some(saver) = slot.autosave.as_mut()
        {
            slot.action_state.commit_for_save(live);
            saver.request(live);
        }
    }

    /// Flush active and parked records, reporting unavailable or failed writers. # Errors Returns
    /// record-specific persistence failures without discarding live bytes.
    pub fn try_flush(&mut self, live: &mut AutoCommit) -> Result<(), String> {
        self.try_flush_documents(Some(live), Durability::Durable)
    }

    /// [`Self::try_flush`] for a caller that must know the records are *written*, not that the
    /// store committed them: a store that commits on a thread of its own always answers "not yet".
    fn try_flush_written(&mut self) -> Result<(), String> {
        self.try_flush_documents(None, Durability::Written)
    }

    fn try_flush_documents(
        &mut self,
        mut live: Option<&mut AutoCommit>,
        wanted: Durability,
    ) -> Result<(), String> {
        let mut errors = Vec::new();
        for (id, slot) in &mut self.open {
            let Some(saver) = slot.autosave.as_mut() else {
                if self.persistent {
                    errors.push(format!("{id}: record persistence unavailable"));
                }
                continue;
            };
            let doc = if self.active.as_ref() == Some(id) {
                live.as_deref_mut()
            } else {
                slot.doc.as_mut()
            };
            let Some(doc) = doc else {
                errors.push(format!("{id}: record document unavailable for flush"));
                continue;
            };
            slot.action_state.commit_for_save(doc);
            saver.save(doc);
            let flushed = match wanted {
                Durability::Durable => saver.try_flush(),
                Durability::Written => saver.try_flush_written(),
            };
            if let Err(error) = flushed {
                errors.push(format!("{id}: {error}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    /// Write out everything still pending, the live document included, and wait for it to land. #
    /// Errors Each record's writer that reports a storage failure, by record.
    pub fn flush(&mut self, live: &mut AutoCommit) -> Result<(), String> {
        self.try_flush(live)
    }
}

/// How far a flush must go: to the store's commit, or to the bytes being
/// written, where the store commits them on a thread of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Durability {
    Durable,
    Written,
}

/// A [`ResourceSet`] several widgets share via a handle, so two tabs on the same file stay one
/// document; single-threaded on purpose, since a `Mutex` here would guard against nothing.
#[derive(Clone)]
pub struct ResourceStore(std::rc::Rc<std::cell::RefCell<ResourceSet>>);

/// Whether `id` is a part of a chain that belongs to the workspace's own document
/// (`derived:part.N@derived:ROLE@` and then the workspace's default document): what an app keeps
/// beside its own document of the workspace because it grows without end.
fn workspace_chain_part(id: &DocumentId) -> bool {
    id.chain_part().is_some_and(|(chain, _)| {
        chain
            .as_str()
            .strip_prefix("derived:")
            .and_then(|derived| derived.split_once('@'))
            .is_some_and(|(_, of)| of == DocumentId::workspace_default().as_str())
    })
}

type OwnerIndex = Rc<
    std::cell::RefCell<HashMap<(String, String), std::rc::Weak<std::cell::RefCell<ResourceSet>>>>,
>;

/// Explicit profile-scoped owner registry. Clones share the original registry;
/// weak entries never keep a closed writer alive.
#[derive(Clone)]
pub struct ResourceOwners {
    store: Rc<dyn DurableStore>,
    /// A hydrated host-acknowledged browser cache; record sets use it
    /// instead of `store` when present.
    portable: Option<PortableStorage>,
    owners: OwnerIndex,
    profile_lifetime: Option<Rc<dyn std::any::Any>>,
}

impl ResourceOwners {
    /// Owners persisting under a local profile directory.
    pub fn new(profile_state_root: PathBuf) -> Self {
        Self::with_store(Rc::new(app_state::shared_store(profile_state_root)))
    }

    /// Owners persisting through a host-provided store.
    pub fn with_store(store: Rc<dyn DurableStore>) -> Self {
        Self {
            store,
            portable: None,
            owners: Rc::default(),
            profile_lifetime: None,
        }
    }

    /// Retain a host-owned profile resource until its last writer/recovery owner
    /// is gone. Call this before opening any records.
    pub fn retain_profile(&mut self, lifetime: Rc<dyn std::any::Any>) {
        self.profile_lifetime = Some(lifetime);
    }

    /// Get the unique live owner for this app and workspace. # Errors Rejects an invalid
    /// app or workspace.
    pub fn open(
        &mut self,
        app_id: &str,
        workspace_id: &str,
    ) -> Result<ResourceStore, ResourceOpenError> {
        self.open_with(app_id, workspace_id, true)
    }

    /// The owner of `app_id`'s records of the person, kept in `scope`: unlike [`Self::open`] it
    /// never joins a native component's construction, since the person's records are not the
    /// history that component was launched to read. # Errors Rejects an invalid app or scope.
    pub fn open_person(
        &mut self,
        app_id: &str,
        scope: &str,
    ) -> Result<ResourceStore, ResourceOpenError> {
        self.open_scoped(app_id, scope, false, None)
    }

    /// The owner of `app_id`'s resources in `workspace` that a window of
    /// another app opens beside its own. Like [`Self::open_person`] it
    /// never joins a native component's construction.
    ///
    /// # Errors
    /// Rejects an invalid package or workspace.
    pub fn open_beside(
        &mut self,
        app_id: &str,
        workspace: &str,
    ) -> Result<ResourceStore, ResourceOpenError> {
        self.open_with(app_id, workspace, false)
    }

    /// The owner of `app_id`'s records of one person, kept in their own scope. Those carry one
    /// route per `(app, record)` in the person's own workspace, so every device of theirs opens
    /// the same document, and that workspace carries it; one that names no person keeps them on
    /// this device.
    /// # Errors Rejects an invalid app or scope.
    pub fn open_personal(
        &mut self,
        app_id: &str,
        author: &crate::MemberId,
    ) -> Result<ResourceStore, ResourceOpenError> {
        self.open_scoped(
            app_id,
            PERSONAL_SCOPE,
            false,
            author
                .person_id()
                .and_then(collab_types::person_workspace_of),
        )
    }

    fn open_with(
        &mut self,
        app_id: &str,
        workspace_id: &str,
        joins_construction: bool,
    ) -> Result<ResourceStore, ResourceOpenError> {
        self.open_scoped(app_id, workspace_id, joins_construction, None)
    }

    fn open_scoped(
        &mut self,
        app_id: &str,
        workspace_id: &str,
        joins_construction: bool,
        person: Option<uuid::Uuid>,
    ) -> Result<ResourceStore, ResourceOpenError> {
        // A cluster of the person's own routes their own scope only: a
        // workspace's records name the workspace.
        let person = person.filter(|_| workspace_id == PERSONAL_SCOPE);
        if self.portable.is_some() {
            app_state::validate_document_namespace(app_id, workspace_id)?;
        } else {
            app_state::validate_document_identity(app_id, workspace_id, "validation")?;
        }
        if let Some(cache) = &self.portable
            && let Some(owner) =
                cache.resource_owner::<std::cell::RefCell<ResourceSet>>(app_id, workspace_id)
        {
            return Ok(ResourceStore(owner));
        }
        let key = (app_id.to_owned(), workspace_id.to_owned());
        if let Some(owner) = self
            .owners
            .borrow()
            .get(&key)
            .and_then(std::rc::Weak::upgrade)
        {
            return Ok(ResourceStore(owner));
        }
        let store = ResourceStore::new(ResourceSet {
            generation: std::cell::Cell::new(0),
            app_id: app_id.into(),
            home: workspace_id.to_owned(),
            person,
            open: HashMap::new(),
            known: BTreeMap::new(),
            active: None,
            persistent: true,
            documents: None,
            backend: self.portable.is_none().then(|| Rc::clone(&self.store)),
            portable: self.portable.clone(),
            pending: HashMap::new(),
            opening: HashMap::new(),
            lent: HashSet::new(),
            next_reservation: 0,
            native_initial: (joins_construction && self.portable.is_none())
                .then(crate::native_initial::capture_bridge)
                .flatten(),
            kept: std::cell::Cell::new(0),
            announced: std::cell::RefCell::default(),
            answered: std::cell::RefCell::default(),
            asks_workspace: std::cell::Cell::new(false),
            wanted: std::cell::RefCell::default(),
            wanted_kinds: std::cell::RefCell::default(),
            readers: std::cell::RefCell::default(),
            _profile_lifetime: self.profile_lifetime.clone(),
        });
        if let Some(cache) = &self.portable {
            cache.register_resource_owner(app_id, workspace_id, &store.0);
        }
        let entry = Rc::downgrade(&store.0);
        if let Some(index) = Rc::get_mut(&mut self.owners) {
            index.get_mut().insert(key, entry);
        } else {
            self.owners.borrow_mut().insert(key, entry);
        }
        Ok(store)
    }
}

/// The heads of what `doc` keeps without the changes `refused` names and everything built on them,
/// and a [`ResourceRefusal`] saying `reason` per action the withdrawn changes carried, in order.
fn withdrawal(
    doc: &mut AutoCommit,
    refused: &[automerge::ChangeHash],
    reason: &str,
) -> (Vec<automerge::ChangeHash>, Vec<ResourceRefusal>) {
    let changes = doc.get_changes_meta(&[]);
    let mut withdrawn: std::collections::HashSet<automerge::ChangeHash> =
        std::collections::HashSet::new();
    let mut refusals = Vec::new();
    // The history lists every change after its dependencies.
    for change in &changes {
        if !refused.contains(&change.hash) && !change.deps.iter().any(|dep| withdrawn.contains(dep))
        {
            continue;
        }
        withdrawn.insert(change.hash);
        let claimed = collab_types::ChangeClaim::of_change(
            change.hash.0,
            change.message.as_deref(),
            Vec::new(),
        );
        if let Ok(claim) = claimed {
            for (action, item) in claim.action.named() {
                let refusal = ResourceRefusal {
                    action,
                    item,
                    reason: reason.to_owned(),
                };
                if !refusals.contains(&refusal) {
                    refusals.push(refusal);
                }
            }
        }
    }
    let depended: std::collections::HashSet<automerge::ChangeHash> = changes
        .iter()
        .filter(|change| !withdrawn.contains(&change.hash))
        .flat_map(|change| change.deps.iter().copied())
        .collect();
    let kept = changes
        .iter()
        .map(|change| change.hash)
        .filter(|hash| !withdrawn.contains(hash) && !depended.contains(hash))
        .collect();
    (kept, refusals)
}

/// An action whose change the workspace refused: the record withdrew it, and the app holding the
/// record is told, as it is of an action refused on this device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceRefusal {
    /// The verb's key, or the edited field's path
    /// ([`collab_types::ClaimedAction::named`]).
    pub action: String,
    /// The item it acted on, if any.
    pub item: Option<String>,
    /// Why, for a person.
    pub reason: String,
}

/// What windows said they did to a record, held until the workspace it is shared in keeps the
/// change, so success is never said of a change the workspace then refuses. An unreplicated record
/// is decided by nobody else, so it is said at once; a refusal withdraws the changes and drops the
/// notice.
#[derive(Debug, Default)]
pub struct WorkspaceNotices {
    /// A host replicates the record to a host that decides its changes.
    replicated: bool,
    held: Vec<HeldNotice>,
    /// The heads the last publication carried.
    sent: Vec<automerge::ChangeHash>,
    /// The workspace answered every commit published so far.
    answered: bool,
    said: Vec<String>,
}

#[derive(Debug)]
struct HeldNotice {
    text: String,
    heads: Vec<automerge::ChangeHash>,
    /// A publication carried its heads.
    published: bool,
}

impl WorkspaceNotices {
    fn hold(&mut self, text: String, heads: Vec<automerge::ChangeHash>) {
        let unchanged = heads == self.sent;
        if !self.replicated || (unchanged && self.answered) {
            self.said.push(text);
            return;
        }
        self.held.push(HeldNotice {
            text,
            heads,
            published: unchanged,
        });
    }

    /// A host replicating the record asked for what to publish.
    fn replicating(&mut self) {
        self.replicated = true;
    }

    /// A publication carried the record up to `heads`.
    fn sent(&mut self, heads: &[automerge::ChangeHash]) {
        self.replicated = true;
        heads.clone_into(&mut self.sent);
        self.answered = false;
        for notice in &mut self.held {
            notice.published = true;
        }
    }

    /// The workspace's host answered every commit published so far. Whether a notice became
    /// something to say: an answer that settles nothing to say shows nothing new.
    fn settled(&mut self) -> bool {
        self.answered = true;
        let (published, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|notice| notice.published);
        self.held = waiting;
        let says = !published.is_empty();
        self.said
            .extend(published.into_iter().map(|notice| notice.text));
        says
    }

    /// Changes were withdrawn: what was said at heads `doc` no longer holds
    /// is not said.
    fn withdrawn(&mut self, doc: &mut AutoCommit) {
        self.held.retain(|notice| {
            notice
                .heads
                .iter()
                .all(|head| doc.get_change_meta_by_hash(head).is_some())
        });
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ResourceOpenOutcome {
    Loaded,
    AlreadyOpen,
}

/// What a workspace's hosts answered of a record a store asked them for and does not hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WorkspaceAnswer {
    /// They hold none of it: it is new to whoever may start it.
    HoldsNone,
    /// It is another person's own record, which they send that person's devices alone.
    NotYours,
}

/// instance views so both bind to the same explicit profile/device identity.
pub fn portable_namespace(cache: &PortableStorage) -> String {
    match cache.namespace() {
        crate::portable_storage::ProfileNamespace::Portable { profile, device } => {
            format!("portable:{profile}:{device}")
        }
        crate::portable_storage::ProfileNamespace::Native(root) => root.display().to_string(),
    }
}

#[derive(Clone)]
pub struct ResourceAdmission {
    checkout: DocumentCheckout,
    workspace: String,
}

impl ResourceAdmission {
    pub fn matches(&self, doc: &AutoCommit, checkout: &DocumentCheckout) -> bool {
        if &self.checkout != checkout {
            return false;
        }
        if checkout.id.is_package_primary() {
            // Primary documents carry no minted identity; see `admission`.
            return validate_identity(doc, checkout, &self.workspace).is_ok();
        }
        identity(doc).as_ref() == Some(checkout)
    }
}

/// The unique construction owner of a not-yet-live record. Drop cancels only this generation.
pub struct ResourceReservation {
    store: ResourceStore,
    checkout: DocumentCheckout,
    generation: u64,
}

impl ResourceReservation {
    pub fn checkout(&self) -> &DocumentCheckout {
        &self.checkout
    }

    /// Privately initialize once, then create the one history and writer and publish Ready. #
    /// Errors Retains the reservation on initializer, validation or worker-startup failure.
    pub fn initialize(
        &mut self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<(), ResourceOpenError> {
        self.initialize_seeded(initialize, |_| Ok(()))
    }

    /// [`Self::initialize`], then `seed` the record with what depends on who creates it or where;
    /// the seed follows the genesis and precedes undo history, so it is nobody's undo step. #
    /// Errors As [`Self::initialize`]; a failing seed fails the initialization.
    pub fn initialize_seeded(
        &mut self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
        seed: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<(), ResourceOpenError> {
        if self.store.0.borrow().portable.is_some() {
            return self.initialize_portable(initialize, seed);
        }
        self.initialize_with(initialize, seed, app_state::DocWriter::start_new)
    }

    fn initialize_with(
        &self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
        seed: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
        start: impl FnOnce(
            &Rc<app_state::DocOwner>,
            &AutoCommit,
        ) -> Result<app_state::DocWriter, app_state::StoreError>,
    ) -> Result<(), ResourceOpenError> {
        let (owner, workspace) = {
            let set = self.store.0.borrow();
            let pending = set
                .pending
                .get(&self.checkout.id)
                .filter(|p| p.generation == self.generation)
                .ok_or(ResourceOpenError::Busy)?;
            (
                Rc::clone(
                    pending
                        .store
                        .as_ref()
                        .ok_or(ResourceOpenError::IdentityMismatch)?,
                ),
                set.home.clone(),
            )
        };
        let mut doc = owner.new_document();
        genesis(&mut doc, &self.checkout, &workspace, initialize)?;
        seed(&mut doc).map_err(ResourceOpenError::Initialization)?;
        validate_identity(&doc, &self.checkout, &workspace)?;
        commit_genesis(&mut doc);
        let mut set = self.store.set_mut();
        if set
            .pending
            .get(&self.checkout.id)
            .is_none_or(|p| p.generation != self.generation)
        {
            return Err(ResourceOpenError::Busy);
        }
        let autosave = start(&owner, &doc)?;
        let history = crate::state::UndoLog::new(&mut doc);
        set.pending.remove(&self.checkout.id);
        set.open.insert(
            self.checkout.id.clone(),
            Parked {
                doc: Some(doc),
                kind: self.checkout.kind.clone(),
                autosave: Some(autosave),
                history,
                action_state: ResourceActions::default(),
            },
        );
        if let Some(slot) = set.open.get_mut(&self.checkout.id)
            && let (Some(doc), Some(saver)) = (&mut slot.doc, &mut slot.autosave)
        {
            saver.request(doc);
        }
        Ok(())
    }
}

/// A record lent to an action frame ([`ResourceStore::with_action_state`]),
/// returned to its set when the frame ends, however it ends.
struct ResourceLoan<'a> {
    store: &'a ResourceStore,
    id: &'a DocumentId,
    parts: Option<(AutoCommit, crate::state::UndoLog, ResourceActions)>,
}

impl Drop for ResourceLoan<'_> {
    fn drop(&mut self) {
        let Some((doc, history, action_state)) = self.parts.take() else {
            return;
        };
        let Ok(mut set) = self.store.try_set_mut() else {
            tracing::error!(record = %self.id, "a lent record could not be returned to its set");
            return;
        };
        set.lent.remove(self.id);
        if let Some(parked) = set.open.get_mut(self.id) {
            parked.doc = Some(doc);
            parked.history = history;
            parked.action_state = action_state;
        }
    }
}

impl Drop for ResourceReservation {
    fn drop(&mut self) {
        let mut set = self.store.set_mut();
        if set
            .pending
            .get(&self.checkout.id)
            .is_some_and(|p| p.generation == self.generation)
        {
            set.pending.remove(&self.checkout.id);
        }
    }
}

/// The document of `id`, a record of `kind` the workspace `home` keeps in `store`, as last made
/// durable; `None` and a log when it cannot be read here — it has not reached this device, or its
/// bytes do not read as a record.
fn read_resource(
    store: &Rc<dyn DurableStore>,
    app: &str,
    home: &str,
    id: &DocumentId,
    kind: &ResourceType,
) -> Option<AutoCommit> {
    let origin = transfer::Origin::Record(DocumentCheckout::new(id.clone(), kind.clone()));
    match transfer::read(store, app, home, &origin) {
        Ok(doc) => Some(doc),
        Err(error) => {
            tracing::warn!(record = %id, workspace = %home, %error, "a workspace record is not readable; left out");
            None
        }
    }
}

impl ResourceStore {
    /// The set, to change: the write generation moves with it.
    fn set_mut(&self) -> std::cell::RefMut<'_, ResourceSet> {
        let set = self.0.borrow_mut();
        set.generation.set(set.generation.get().wrapping_add(1));
        set
    }

    /// [`Self::set_mut`], unless the set is borrowed now.
    fn try_set_mut(&self) -> Result<std::cell::RefMut<'_, ResourceSet>, std::cell::BorrowMutError> {
        let set = self.0.try_borrow_mut()?;
        set.generation.set(set.generation.get().wrapping_add(1));
        Ok(set)
    }

    /// A number that moves whenever anything may have changed a resource of
    /// this store (a commit, a merge, a migration, an open or a close), and
    /// stays the same while nothing did: what a reader holding many resources
    /// compares before looking at each of them again. It may move without a
    /// change; it never stays put across one.
    #[must_use]
    pub fn write_generation(&self) -> u64 {
        self.0
            .try_borrow()
            .map_or(u64::MAX, |set| set.generation.get())
    }

    /// The route record `id` travels on: its workspace, the app and its wire name, unchecked
    /// against the record's document. What names a record across workspaces in one process (its
    /// content content link) keys by it.
    pub fn wire_route(&self, id: &DocumentId) -> Result<crate::RouteSpec, ResourceOpenError> {
        self.0
            .try_borrow()
            .map_err(|_| ResourceOpenError::Busy)?
            .wire_route(id)
    }

    /// Checked ownership-to-wire mapping. Primary stays `record=None` on every host.
    pub fn route(
        &self,
        checkout: &DocumentCheckout,
    ) -> Result<crate::RouteSpec, ResourceOpenError> {
        let set = self.0.try_borrow().map_err(|_| ResourceOpenError::Busy)?;
        let doc = set
            .peek(&checkout.id)
            .ok_or(ResourceOpenError::MissingResource)?;
        validate_identity(doc, checkout, &set.home)?;
        set.wire_route(&checkout.id)
    }

    /// The route native component admission joins before the factory runs: the open record's, a
    /// live New reservation's, the workspace's record while it opens off the frame, or an original
    /// creation whose genesis the factory has yet to write.
    pub fn admission_route(
        &self,
        checkout: &DocumentCheckout,
    ) -> Result<crate::RouteSpec, ResourceOpenError> {
        {
            let set = self.0.try_borrow().map_err(|_| ResourceOpenError::Busy)?;
            if let Some(reserved) = set.pending.get(&checkout.id) {
                if reserved.kind != checkout.kind {
                    return Err(ResourceOpenError::IdentityMismatch);
                }
                return set.wire_route(&checkout.id);
            }
            if let Some(route) = set.opening_route(checkout) {
                return route;
            }
            if set
                .open
                .get(&checkout.id)
                .is_some_and(|slot| slot.kind != checkout.kind)
            {
                return Err(ResourceOpenError::IdentityMismatch);
            }
            if let Some(doc) = set.peek(&checkout.id) {
                validate_identity(doc, checkout, &set.home)?;
            }
        }
        self.route(checkout)
    }

    /// Schedule this already committed History result through its existing writer, without sweeping
    /// or closing other owners' unfinished edits. # Errors The cache could not take the document,
    /// or the store is busy and took nothing.
    pub fn schedule_committed_history(&self, id: &DocumentId) -> Result<(), String> {
        let Ok(mut set) = self.try_set_mut() else {
            return Err("resource store is busy".to_string());
        };
        if set.portable.is_some() {
            if set
                .open
                .get(id)
                .and_then(|slot| slot.doc.as_ref())
                .is_some_and(|doc| doc.pending_ops() == 0)
            {
                return set.flush_portable(Some(id), false);
            }
            return Ok(());
        }
        let Some(slot) = set.open.get_mut(id) else {
            return Ok(());
        };
        if let (Some(doc), Some(saver)) = (slot.doc.as_mut(), slot.autosave.as_mut())
            && doc.pending_ops() == 0
        {
            saver.request(doc);
        }
        Ok(())
    }

    /// Cheap content observation for retained projections over open records: base heads cover committed
    /// changes, `pending_ops` the live typing transaction, and the record identity makes opening or
    /// closing a document invalidate a snapshot.
    pub fn content_revision(&self) -> String {
        let set = self.0.borrow();
        let mut records = set.open.iter().collect::<Vec<_>>();
        records.compact_sort_by_key(|(id, _)| *id);
        let mut revision = String::new();
        for (id, parked) in records {
            use std::fmt::Write as _;
            let Some(doc) = parked.doc.as_ref() else {
                continue;
            };
            let _ = write!(
                revision,
                "{}:{:?}:{}\u{1e}",
                id.as_str(),
                doc.base_heads(),
                doc.pending_ops()
            );
        }
        revision
    }

    /// Captured storage identity for History; never samples ambient launch scope.
    pub fn history_namespace(
        &self,
        id: &DocumentId,
    ) -> Result<(Option<String>, String, String), ResourceOpenError> {
        let set = self.0.try_borrow().map_err(|_| ResourceOpenError::Busy)?;
        let parked = set.open.get(id).ok_or(ResourceOpenError::MissingResource)?;
        let storage = set
            .portable
            .as_ref()
            .map(portable_namespace)
            .or_else(|| {
                set.backend
                    .as_ref()
                    .map(|store| store.namespace().to_owned())
            })
            .or_else(|| {
                parked
                    .autosave
                    .as_ref()
                    .filter(|saver| saver.is_enabled())
                    .map(|saver| saver.document_path().to_string_lossy().into_owned())
            });
        Ok((storage, set.app_id.to_string(), set.home.clone()))
    }
    /// Whether the store can be read now: no caller holds it mutably.
    pub fn readable(&self) -> bool {
        self.0.try_borrow().is_ok()
    }
    /// The package and home workspace this store's records belong to, which name their replication
    /// route, and the durable store they live in when it has one.
    pub fn backend(&self) -> Option<Rc<dyn DurableStore>> {
        let set = self.0.borrow();
        set.backend.clone().or_else(|| set.documents.clone())
    }

    pub fn app_and_home(&self) -> (String, String) {
        let set = self.0.borrow();
        (set.app_id.to_string(), set.home.clone())
    }

    /// The resources this store holds open, and the chain of each part among them: the resources a
    /// window keeps.
    #[must_use]
    pub fn held_resources(&self) -> Vec<DocumentId> {
        let Ok(set) = self.0.try_borrow() else {
            return Vec::new();
        };
        let mut held: Vec<DocumentId> = Vec::new();
        for id in set.open.keys().chain(set.lent.iter()) {
            let chain = id.chain_part().map(|(chain, _)| chain);
            for kept in std::iter::once(id.clone()).chain(chain) {
                if !held.contains(&kept) {
                    held.push(kept);
                }
            }
        }
        held
    }

    /// The numbers of the parts of the chain `base` this store holds, in order: those open here,
    /// and those kept on this device unopened. Listing reads the store's index, so a frame loop
    /// asks on [`Self::kept_revision`] instead of every frame.
    #[must_use]
    pub fn held_parts(&self, base: &DocumentId) -> Vec<u64> {
        let Ok(set) = self.0.try_borrow() else {
            return Vec::new();
        };
        let mut held: std::collections::BTreeSet<u64> = set
            .open
            .keys()
            .chain(set.lent.iter())
            .filter_map(DocumentId::chain_part)
            .filter(|(chain, _)| chain == base)
            .map(|(_, index)| index)
            .collect();
        if !set.home.is_empty() {
            let kept = match (
                &set.portable,
                set.backend.as_ref().or(set.documents.as_ref()),
            ) {
                (Some(cache), _) => cache.records(&set.app_id, &set.home),
                (None, Some(store)) => store.documents(&set.app_id, &set.home).unwrap_or_default(),
                (None, None) => Vec::new(),
            };
            // A portable cache keys a record by its id, a store by its
            // storage key.
            held.extend(kept.iter().filter_map(|key| {
                base.part_of_storage_key(key).or_else(|| {
                    DocumentId::parse(key)?
                        .chain_part()
                        .filter(|(chain, _)| chain == base)
                        .map(|(_, index)| index)
                })
            }));
        }
        held.into_iter().collect()
    }

    /// Tell the store that a record it does not have open was kept in its backend by someone else
    /// (a workspace's replication, delivering a part another device started), so what follows the
    /// records it keeps looks again. Callable while a frame reads the store.
    pub fn note_kept(&self) {
        if let Ok(set) = self.0.try_borrow() {
            set.kept.set(set.kept.get().wrapping_add(1));
        }
        self.wake_readers();
    }

    /// Peers wrote `records` of this store's trees, which it does not hold: whatever follows those
    /// trees takes them up at once, as after [`Self::note_kept`]. Callable while a frame reads the
    /// store.
    pub fn announce(&self, records: impl IntoIterator<Item = DocumentId>) {
        if let Ok(set) = self.0.try_borrow() {
            set.announced
                .borrow_mut()
                .extend(records.into_iter().filter(|record| record.tree().is_some()));
            set.kept.set(set.kept.get().wrapping_add(1));
        }
        self.wake_readers();
    }

    /// The numbers of the parts of the chain `base` peers wrote
    /// ([`Self::announce`]), in order.
    #[must_use]
    pub fn announced_parts(&self, base: &DocumentId) -> Vec<u64> {
        let Ok(set) = self.0.try_borrow() else {
            return Vec::new();
        };
        let announced = set.announced.borrow();
        announced
            .iter()
            .filter_map(DocumentId::chain_part)
            .filter(|(chain, _)| chain == base)
            .map(|(_, index)| index)
            .collect()
    }

    /// Moves whenever the store is told something arrived for what follows
    /// it: [`Self::note_kept`], [`Self::announce`], [`Self::note_workspace_answered`].
    #[must_use]
    pub fn kept_revision(&self) -> u64 {
        self.0.try_borrow().map_or(0, |set| set.kept.get())
    }

    /// The numbers of the parts of the chain `base` open here and ready to read, in order. Reads no
    /// index, so it is cheap enough for every frame.
    #[must_use]
    pub fn open_parts(&self, base: &DocumentId) -> Vec<u64> {
        let Ok(set) = self.0.try_borrow() else {
            return Vec::new();
        };
        let mut open: Vec<u64> = set
            .open
            .iter()
            .filter(|(_, parked)| parked.doc.is_some())
            .filter_map(|(id, _)| id.chain_part())
            .filter(|(chain, _)| chain == base)
            .map(|(_, index)| index)
            .collect();
        open.compact_sort();
        open
    }

    /// Every window default record of `kind` this package keeps in this workspace, sorted by id, as
    /// last made durable: what an app's windows started, so a closed window's document is reached
    /// again. Read without taking a writer; a record that cannot be read, is retired or is not of
    /// `kind` here is left out and logged.
    pub fn window_resources(&self, kind: &ResourceType) -> Vec<(DocumentId, AutoCommit)> {
        let Some(store) = self.backend() else {
            return Vec::new();
        };
        let (package, home) = self.app_and_home();
        if home.is_empty() {
            return Vec::new();
        }
        let keys = match store.documents(package.as_str(), &home) {
            Ok(keys) => keys,
            Err(error) => {
                tracing::warn!(%package, workspace = %home, %error, "listing the workspace's records failed");
                return Vec::new();
            }
        };
        let mut found = keys
            .iter()
            .filter_map(|key| DocumentId::window_default_of_storage_key(key))
            .filter_map(|id| {
                Some((
                    id.clone(),
                    read_resource(&store, &package, &home, &id, kind)?,
                ))
            })
            .collect::<Vec<_>>();
        found.compact_sort_by(|a, b| a.0.cmp(&b.0));
        found
    }

    /// Every record of `kind` this workspace holds, sorted by id, as last made durable, each with
    /// the name the workspace's index gives it: the window default records this device's windows
    /// started ([`Self::window_resources`]) and the records the index names, so a document a window
    /// on another device made, or an agent made in a hosted workspace, is listed too. Read without
    /// taking a writer; a record whose document is not here is left out and logged, and one the
    /// index does not name has no name.
    pub fn workspace_resources(
        &self,
        kind: &ResourceType,
    ) -> Vec<(DocumentId, AutoCommit, Option<String>)> {
        let Some(store) = self.backend() else {
            return Vec::new();
        };
        let (package, home) = self.app_and_home();
        if home.is_empty() {
            return Vec::new();
        }
        let mut found: Vec<(DocumentId, AutoCommit, Option<String>)> = self
            .window_resources(kind)
            .into_iter()
            .map(|(id, doc)| (id, doc, None))
            .collect();
        for (id, name) in self.indexed_resources(kind) {
            match found.binary_search_by(|(held, _, _)| held.cmp(&id)) {
                // A window here started it and the index names it: the name is the index's.
                Ok(at) => found[at].2 = name,
                Err(at) => {
                    if let Some(doc) = read_resource(&store, &package, &home, &id, kind) {
                        found.insert(at, (id, doc, name));
                    }
                }
            }
        }
        found
    }

    /// The records of `kind` the workspace's index names, each with the name it gives it, sorted by
    /// id: what an app's windows and a hosted workspace's agent published here, whether or not a
    /// window of this device started them.
    fn indexed_resources(&self, kind: &ResourceType) -> Vec<(DocumentId, Option<String>)> {
        let Some(index) = self.workspace_index() else {
            return Vec::new();
        };
        let mut named: Vec<(DocumentId, Option<String>)> = index::discoverable_references(&index)
            .into_iter()
            .filter(|checkout| &checkout.kind == kind)
            .map(|checkout| {
                let name = index::name_of(&index, &checkout.id);
                (checkout.id, name)
            })
            .collect();
        named.compact_sort_by(|a, b| a.0.cmp(&b.0));
        named
    }

    /// The workspace's index document — the names of the records this package keeps here — as this
    /// device last wrote it, read without taking a writer since an app of the workspace owns it.
    fn workspace_index(&self) -> Option<AutoCommit> {
        let store = self.backend()?;
        let (package, home) = self.app_and_home();
        if home.is_empty() {
            return None;
        }
        let stored = match store.read(&StorageKey::workspace_index(package.as_str(), &home)) {
            Ok(stored) => stored?,
            Err(error) => {
                tracing::warn!(%package, workspace = %home, %error, "a workspace's index is not read");
                return None;
            }
        };
        // Automerge reads a document whole, and an index is a list of names rather than the
        // records' own content: a file past this is not one a workspace wrote.
        if stored.bytes.is_empty() || stored.bytes.len() > MAX_INDEX_BYTES {
            return None;
        }
        // An app keeping the index appends to it, and this read may land on a tail it has not
        // finished writing: the changes before it are the index as of the last write that completed.
        AutoCommit::load_with_options(
            &stored.bytes,
            automerge::LoadOptions::new()
                .on_partial_load(automerge::OnPartialLoad::Ignore),
        )
        .inspect_err(|error| {
            tracing::warn!(%error, %package, workspace = %home, "a workspace's index does not load");
        })
        .ok()
    }
    /// Explicit immutable identity only: the store namespace, package and
    /// workspace. Legacy and memory sets have no canonical namespace.
    pub fn canonical_namespace(&self) -> Option<(String, String, String)> {
        let set = self.0.borrow();
        Some((
            match (&set.portable, &set.backend) {
                (Some(cache), _) => portable_namespace(cache),
                (None, backend) => backend.as_ref()?.namespace().to_owned(),
            },
            set.app_id.to_string(),
            set.home.clone(),
        ))
    }

    /// Open Existing only; a missing document is never implicitly initialized. In a store whose
    /// records come from its workspace's hosts ([`Self::ask_workspace_for_missing`]) a record not
    /// here is [`ResourceOpenError::Arriving`] until they answered they hold none of it: only then is
    /// it [`ResourceOpenError::MissingResource`], which a caller may take as new. # Errors Reports
    /// ownership contention, absence, identity or schema refusal and storage failures.
    pub fn try_open_resource(
        &self,
        want: &DocumentCheckout,
    ) -> Result<ResourceOpenOutcome, ResourceOpenError> {
        let opened = self
            .try_set_mut()
            .map_err(|_| ResourceOpenError::Busy)?
            .try_open_resource(want);
        match opened {
            Err(ResourceOpenError::MissingResource)
                if self.workspace_answer(&want.id) == Some(WorkspaceAnswer::NotYours) =>
            {
                Err(ResourceOpenError::NotYours)
            }
            Err(ResourceOpenError::MissingResource) if self.awaits_workspace(want) => {
                Err(ResourceOpenError::Arriving)
            }
            Ok(outcome) => {
                if let Ok(set) = self.0.try_borrow() {
                    set.wanted.borrow_mut().remove(&want.id);
                }
                Ok(outcome)
            }
            other => other,
        }
    }

    /// [`Self::try_open_resource`] for a record a window holds beside the one it shows: one
    /// whose committed history has not arrived is [`ResourceOpenError::Arriving`] rather than
    /// waited for, and the store's readers are woken when it arrives. The window builds
    /// without it and keeps waiting for it however long its host takes.
    ///
    /// # Errors
    /// As [`Self::try_open_resource`].
    pub fn try_open_beside(
        &self,
        want: &DocumentCheckout,
    ) -> Result<ResourceOpenOutcome, ResourceOpenError> {
        let answered = {
            let set = self.0.try_borrow().map_err(|_| ResourceOpenError::Busy)?;
            set.read_answered(want)?
        };
        if !answered {
            return Err(ResourceOpenError::Arriving);
        }
        self.try_open_resource(want)
    }

    /// Open `want` if this store holds it, without asking the workspace for
    /// one it does not hold: what a list checks for each record it could
    /// name. [`ResourceOpenError::MissingResource`] says only that it is not here.
    ///
    /// # Errors
    /// As [`Self::try_open_resource`], a record not here always missing.
    pub fn open_if_held(
        &self,
        want: &DocumentCheckout,
    ) -> Result<ResourceOpenOutcome, ResourceOpenError> {
        self.try_set_mut()
            .map_err(|_| ResourceOpenError::Busy)?
            .try_open_resource(want)
    }

    /// Take this store's records to come from its workspace's hosts, which
    /// may send a device a record only once it asks for it (a public
    /// workspace sends a newcomer its governance and shared shell, and the
    /// rest when opened): a record not here is not new until they said they
    /// hold none of it ([`Self::note_workspace_answered`]). Opening one asks
    /// for it ([`Self::wanted_resources`]) and is
    /// [`ResourceOpenError::Arriving`] meanwhile; it opens once it arrives.
    /// Without this a record not here after the workspace's replay was
    /// taken as new, and the fresh document a window founded overwrote or
    /// forked the workspace's own.
    pub fn ask_workspace_for_missing(&self) {
        if let Ok(set) = self.0.try_borrow() {
            set.asks_workspace.set(true);
        }
    }

    /// The records opened here that are not here yet and that the
    /// workspace's hosts have not answered: what to ask them for.
    #[must_use]
    pub fn wanted_resources(&self) -> Vec<DocumentId> {
        self.0
            .try_borrow()
            .map(|set| set.wanted.borrow().iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Reserve a New identity with its check and no document/history/writer yet:
    /// the workspace's own records are reserved as shared writers, any other as
    /// its only writer.
    ///
    /// # Errors
    /// Refuses collisions, malformed existing data and unavailable storage.
    pub fn reserve_resource(
        &self,
        want: &DocumentCheckout,
    ) -> Result<ResourceReservation, ResourceOpenError> {
        let mut set = self.try_set_mut().map_err(|_| ResourceOpenError::Busy)?;
        if !want.is_valid_local_storage_request() {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        if set.portable.is_some() {
            return self.reserve_portable(&mut set, want);
        }
        // A record the app's own channel answers (one a sealed workspace does not
        // hold yet) is new, and the app makes it: reserved as without a
        // bridge. Every other new record waits for its creation admission.
        if set
            .native_initial
            .as_ref()
            .is_some_and(|bridge| !bridge.answered_by_own_channel(want))
        {
            return Err(ResourceOpenError::Initialization(
                "native new document requires governor creation admission before canonical reservation".into(),
            ));
        }
        let (backend, key) = set.canonical_key(want)?;
        if set.pending.contains_key(&want.id) || set.open.contains_key(&want.id) {
            return Err(ResourceOpenError::Busy);
        }
        // The reservation becomes the live writer, so the workspace's own documents
        // are joined as shared writers here too; see `try_open_resource`.
        let owner = if set.shared_in_workspace(&want.id) {
            app_state::DocOwner::acquire_shared(backend, key)?
        } else {
            app_state::DocOwner::acquire(backend, key)?
        };
        if owner.load()?.is_some() {
            return Err(ResourceOpenError::Busy);
        }
        let generation = set
            .next_reservation
            .checked_add(1)
            .ok_or(ResourceOpenError::Busy)?;
        set.next_reservation = generation;
        set.pending.insert(
            want.id.clone(),
            Reserved {
                generation,
                kind: want.kind.clone(),
                store: Some(Rc::new(owner)),
                actor: None,
            },
        );
        Ok(ResourceReservation {
            store: self.clone(),
            checkout: want.clone(),
            generation,
        })
    }

    pub fn is_ready(&self, id: &DocumentId) -> bool {
        self.0.try_borrow().is_ok_and(|set| set.peek(id).is_some())
    }

    /// Inspect durability without closing a typing transaction or scheduling work.
    ///
    /// # Errors
    /// Reports missing/reserved records.
    pub fn save_status(&self, id: &DocumentId) -> Result<ResourceSaveStatus, ResourceOpenError> {
        let set = self.0.try_borrow().map_err(|_| ResourceOpenError::Busy)?;
        if set.pending.contains_key(id) {
            return Err(ResourceOpenError::Busy);
        }
        let slot = set.open.get(id).ok_or(ResourceOpenError::MissingResource)?;
        let doc = slot.doc.as_ref().ok_or(ResourceOpenError::Busy)?;
        let writers = slot.action_state.writers.borrow();
        if let Some(cache) = &set.portable {
            return Ok(portable::save_status(
                cache,
                &set.app_id,
                &set.home,
                id,
                doc,
                Some(&writers),
            ));
        }
        if let Some(saver) = &slot.autosave {
            return Ok(ResourceSaveStatus::of_writer(doc, saver, Some(&writers)));
        }
        Ok(if set.persistent {
            ResourceSaveStatus::Failed("record persistence unavailable".to_owned())
        } else {
            ResourceSaveStatus::MemoryOnly
        })
    }

    /// Have `wake` called whenever [`Self::save_status`] of `id` may change
    /// without input: its store resolving its writer's writes, or a host
    /// acknowledging a portable cache. A memory-only record never changes.
    /// Sample again once armed: a write that landed before is not heard of.
    pub fn wake_on_save_progress(&self, id: &DocumentId, wake: durable_store::ReadyWake) {
        let Ok(set) = self.0.try_borrow() else {
            return;
        };
        if let Some(cache) = &set.portable {
            cache.watch_durability(&portable::key(&set.app_id, &set.home, id), wake);
            return;
        }
        if let Some(saver) = set.open.get(id).and_then(|slot| slot.autosave.as_ref()) {
            saver.wake_on_settle(wake);
        }
    }

    /// Block until the store has committed or refused every write `id`'s
    /// writer already submitted, without submitting its pending edits: the
    /// acknowledgement [`Self::save_status`] reports, awaited instead of
    /// polled.
    ///
    /// # Errors
    /// Reports missing/busy records, an unpersisted record, and the writer's
    /// outstanding failure.
    pub fn await_submitted(&self, id: &DocumentId) -> Result<(), String> {
        let set = self
            .0
            .try_borrow()
            .map_err(|_| "resource store is busy".to_string())?;
        let slot = set
            .open
            .get(id)
            .ok_or_else(|| format!("{id}: record is not open"))?;
        let saver = slot
            .autosave
            .as_ref()
            .ok_or_else(|| format!("{id}: record is not persisted"))?;
        saver.try_flush().map_err(|error| format!("{id}: {error}"))
    }

    pub fn durability_error(&self, id: &DocumentId) -> Option<String> {
        if self.0.borrow().portable.is_some() {
            return match self.save_status(id) {
                Ok(ResourceSaveStatus::Failed(error)) => Some(error),
                _ => None,
            };
        }
        self.0
            .borrow()
            .open
            .get(id)
            .and_then(|p| p.autosave.as_ref())
            .and_then(app_state::DocWriter::last_error)
    }

    /// Stage private evidence on the exact record owner, never its CRDT.
    pub fn record_private_action(
        &self,
        id: &DocumentId,
        action: app_state::PrivateAction,
    ) -> Result<(), String> {
        let mut set = self.try_set_mut().map_err(|_| "resource store is busy")?;
        let slot = set.open.get_mut(id).ok_or("record is not open")?;
        slot.autosave
            .as_mut()
            .ok_or("record has no private durable writer")?
            .record_private_action(action)
    }

    /// Read bounded evidence against this owner's live history, without cloning
    /// the document or opening a second owner.
    pub fn private_actions(
        &self,
        id: &DocumentId,
    ) -> Result<Vec<app_state::PrivateAction>, String> {
        let set = self.0.try_borrow().map_err(|_| "resource store is busy")?;
        let slot = set.open.get(id).ok_or("record is not open")?;
        let doc = slot.doc.as_ref().ok_or("record document is lent")?;
        // Absence of a local checkpoint store is not failed recovery. In
        // particular, session-owned records need not have a local writer.
        // record_private_action continues to refuse writes without one.
        match slot.autosave.as_ref() {
            Some(writer) => writer.private_actions(doc),
            None => Ok(Vec::new()),
        }
    }

    /// Offer one serviced canonical record without awaiting host ACK or
    /// touching another record. Accepted bytes remain Saving until durable.
    pub fn offer_resource(&self, id: &DocumentId) -> Result<(), String> {
        let mut set = self
            .try_set_mut()
            .map_err(|_| "resource store is busy".to_owned())?;
        if set.portable.is_some() {
            return set.offer_portable_resource(id);
        }
        let slot = set
            .open
            .get_mut(id)
            .ok_or_else(|| format!("{id}: record is not open"))?;
        let doc = slot
            .doc
            .as_mut()
            .ok_or_else(|| format!("{id}: record is busy"))?;
        let Some(saver) = &mut slot.autosave else {
            return Ok(());
        };
        slot.action_state.commit_for_save(doc);
        saver.offer(doc)
    }

    /// Close/retry one actual owner without settling another record's edit.
    pub fn try_flush_resource(&self, id: &DocumentId) -> Result<(), String> {
        let mut set = self
            .try_set_mut()
            .map_err(|_| "resource store is busy".to_string())?;
        if set.portable.is_some() {
            return set.flush_portable(Some(id), false);
        }
        let slot = set
            .open
            .get_mut(id)
            .ok_or_else(|| format!("{id}: record is not open"))?;
        let doc = slot
            .doc
            .as_mut()
            .ok_or_else(|| format!("{id}: record is busy"))?;
        let Some(saver) = &mut slot.autosave else {
            return Ok(());
        };
        slot.action_state.commit_for_save(doc);
        saver.save(doc);
        saver.try_flush().map_err(|error| format!("{id}: {error}"))
    }

    /// Request current changes once and await actual acknowledgements for every record.
    ///
    /// # Errors
    /// Returns record-specific storage failures; a subsequent explicit retry sends a full checkpoint.
    pub fn try_flush(&self) -> Result<(), String> {
        let mut set = self.set_mut();
        if set.portable.is_some() {
            return set.flush_portable(None, false);
        }
        set.try_flush_documents(None, Durability::Durable)
    }

    /// Write out what is not stored yet, and report only what failed: the
    /// records are the store's from then on, and the store's own thread
    /// commits them ([`ResourceSet::try_flush_written`]).
    pub fn try_flush_written(&self) -> Result<(), String> {
        let mut set = self.set_mut();
        if set.portable.is_some() {
            return set.flush_portable(None, true);
        }
        set.try_flush_written()
    }

    /// Share an existing set.
    pub fn new(set: ResourceSet) -> Self {
        Self(std::rc::Rc::new(std::cell::RefCell::new(set)))
    }

    /// The production store: see [`ResourceSet::open`].
    pub fn open(manifest_id: impl Into<String>) -> Self {
        Self::new(ResourceSet::open(manifest_id))
    }

    /// A store that never touches disk: see [`ResourceSet::in_memory`].
    pub fn in_memory() -> Self {
        Self::new(ResourceSet::in_memory())
    }

    /// Open `want` if it is not open already. Returns whether this opened
    /// it.
    pub fn open_resource(&self, want: &DocumentCheckout) -> bool {
        self.set_mut().open_resource(want, None)
    }

    /// Create exactly the new resource named by its owning action. Reopening
    /// existing tabs must use existing-only admission instead.
    ///
    /// # Errors
    /// Refuses existing identities, unavailable persistence, and invalid scope.
    pub fn create_resource(&self, want: &DocumentCheckout) -> Result<(), ResourceOpenError> {
        if !want.is_valid_local_storage_request() {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        let canonical = {
            let set = self.try_set_mut().map_err(|_| ResourceOpenError::Busy)?;
            set.backend.is_some() || set.portable.is_some()
        };
        if canonical {
            return self
                .reserve_resource(want)
                .and_then(|mut reservation| reservation.initialize(|_| Ok(())))
                .map_err(|error| match error {
                    ResourceOpenError::Busy => ResourceOpenError::AlreadyExists,
                    other => other,
                });
        }
        let mut set = self.try_set_mut().map_err(|_| ResourceOpenError::Busy)?;
        if set.open.contains_key(&want.id) {
            return Err(ResourceOpenError::AlreadyExists);
        }
        if want.kind.as_str().is_empty() || (set.persistent && set.home.is_empty()) {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        let owner = if set.persistent {
            let key = want.id.storage_key();
            let mut owner = set.scoped_document(&key)?;
            // An empty/corrupt document is existing evidence too, never
            // permission to replace it with a freshly initialized one.
            if owner.load_complete()?.is_some() || owner.has_stored()? {
                return Err(ResourceOpenError::AlreadyExists);
            }
            Some(owner)
        } else {
            None
        };
        let mut owner = owner;
        let mut doc = owner
            .as_mut()
            .map_or_else(AutoCommit::new, app_state::DocStore::new_document);
        let scope = (!set.home.is_empty()).then_some(set.home.as_str());
        if !init_scoped(&mut doc, &want.id, &want.kind, &set.home, scope) {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        for (target, relation) in &want.references {
            add_reference(
                &mut doc,
                &DocumentRef::new(target.id.clone(), relation.clone()),
            );
        }
        commit_genesis(&mut doc);
        let mut autosave = owner.map(|owner| owner.writer());
        if let Some(error) = autosave
            .as_ref()
            .and_then(|saver| saver.durability_snapshot().error)
        {
            return Err(ResourceOpenError::Initialization(error));
        }
        let history = crate::state::UndoLog::new(&mut doc);
        if let Some(saver) = &mut autosave {
            saver.request(&doc);
        }
        set.open.insert(
            want.id.clone(),
            Parked {
                doc: Some(doc),
                kind: want.kind.clone(),
                autosave,
                history,
                action_state: ResourceActions::default(),
            },
        );
        Ok(())
    }

    /// Existing-only admission for presentation owners. Loads a saved graph
    /// without seeding missing content. Package-global records retain the
    /// existing explicit one-workspace migration claim and their original history.
    pub fn open_existing_resource_checked(
        &self,
        want: &DocumentCheckout,
    ) -> Result<(), ResourceOpenError> {
        let mut set = self.try_set_mut().map_err(|_| ResourceOpenError::Busy)?;
        if set.backend.is_some() || set.portable.is_some() {
            return set.try_open_record_with_recovery(want, false).map(|_| ());
        }
        if !set.open.contains_key(&want.id) && set.native_initial.is_some() {
            let (mut doc, mut autosave) = set.load(want)?;
            validate_identity(&doc, want, &set.home)?;
            if let Some(saver) = autosave.as_mut() {
                saver.request(&doc);
            }
            let history = crate::state::UndoLog::new(&mut doc);
            set.open.insert(
                want.id.clone(),
                Parked {
                    doc: Some(doc),
                    kind: want.kind.clone(),
                    autosave,
                    history,
                    action_state: ResourceActions::default(),
                },
            );
        }
        if !set.open.contains_key(&want.id) {
            if !set.persistent {
                return Err(ResourceOpenError::MissingResource);
            }
            if set.home.is_empty() {
                return Err(ResourceOpenError::IdentityMismatch);
            }
            let key = want.id.storage_key();
            let mut owner = set.scoped_document(&key)?;
            let (mut doc, adopted) = if let Some(doc) = owner.load_complete()? {
                (doc, false)
            } else {
                if owner.has_stored()? {
                    return Err(app_state::StoreError::EmptyExistingDocument {
                        path: owner.document_path().to_path_buf(),
                    }
                    .into());
                }
                let Some(mut doc) = set.adopt_former_scoped(want)? else {
                    return Err(ResourceOpenError::MissingResource);
                };
                if !init_scoped(&mut doc, &want.id, &want.kind, &set.home, Some(&set.home)) {
                    return Err(ResourceOpenError::IdentityMismatch);
                }
                (doc, true)
            };
            validate_identity(&doc, want, &set.home)?;
            if home(&doc).as_deref() != Some(set.home.as_str()) {
                return Err(ResourceOpenError::IdentityMismatch);
            }
            let mut autosave = owner.writer();
            if let Some(error) = autosave.durability_snapshot().error {
                return Err(ResourceOpenError::Initialization(error));
            }
            if !adopted {
                autosave.seed(&doc);
            }
            let history = crate::state::UndoLog::new(&mut doc);
            if adopted {
                autosave.request(&doc);
            }
            set.open.insert(
                want.id.clone(),
                Parked {
                    doc: Some(doc),
                    kind: want.kind.clone(),
                    autosave: Some(autosave),
                    history,
                    action_state: ResourceActions::default(),
                },
            );
        }
        let slot = set
            .open
            .get(&want.id)
            .ok_or(ResourceOpenError::MissingResource)?;
        let doc = slot.doc.as_ref().ok_or(ResourceOpenError::Busy)?;
        // Legacy in-memory owners legitimately have no workspace. Match that
        // captured namespace exactly; do not manufacture a canonical scope.
        let expected_scope = (!set.home.is_empty()).then_some(set.home.as_str());
        if identity(doc).is_none_or(|found| found.id != want.id || found.kind != want.kind)
            || want.kind.as_str().is_empty()
            || home(doc).as_deref() != Some(set.home.as_str())
            || workspace_scope(doc).as_deref() != expected_scope
        {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        Ok(())
    }

    /// Exact runtime owner identity; namespace equality is insufficient for reentry.
    pub fn same_owner(&self, other: &Self) -> bool {
        std::rc::Rc::ptr_eq(&self.0, &other.0)
    }

    pub fn admission(
        &self,
        checkout: &DocumentCheckout,
    ) -> Result<ResourceAdmission, ResourceOpenError> {
        let set = self.0.try_borrow().map_err(|_| ResourceOpenError::Busy)?;
        let slot = set
            .open
            .get(&checkout.id)
            .ok_or(ResourceOpenError::MissingResource)?;
        let doc = slot.doc.as_ref().ok_or(ResourceOpenError::Busy)?;
        if checkout.id.is_package_primary() {
            // A package-primary document carries no minted `id`/`kind` in its
            // metadata (see `primary_document`), so `identity` is always
            // `None` for it; admit it by the same rule that opened it.
            validate_identity(doc, checkout, &set.home)?;
        } else if identity(doc).as_ref() != Some(checkout) {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        Ok(ResourceAdmission {
            checkout: checkout.clone(),
            workspace: set.home.clone(),
        })
    }

    pub fn with_checked<R>(
        &self,
        id: &DocumentId,
        f: impl FnOnce(&mut AutoCommit) -> R,
    ) -> Result<R, ResourceOpenError> {
        // A checked mutable loan is external to any action frame, exactly as
        // `document_mut`: close the open frame and advance its generation so
        // an operation resolved before this loan is invalidated.
        self.with_action_state(id, |document, history, actions| {
            actions.finish_external(document, history);
            actions.generation = actions
                .generation
                .and_then(|generation| generation.checked_add(1));
            f(document)
        })
    }

    pub fn read_checked<R>(
        &self,
        id: &DocumentId,
        read: impl FnOnce(&AutoCommit) -> R,
    ) -> Result<R, ResourceOpenError> {
        let mut read = Some(read);
        let mut out = None;
        self.read_checked_with(id, &mut |document| {
            if let Some(read) = read.take() {
                out = Some(read(document));
            }
        })?;
        out.ok_or(ResourceOpenError::MissingResource)
    }

    /// [`Self::read_checked`] with the reading erased: one body for every
    /// reader.
    #[inline(never)]
    fn read_checked_with(
        &self,
        id: &DocumentId,
        read: &mut dyn FnMut(&AutoCommit),
    ) -> Result<(), ResourceOpenError> {
        let set = self.0.try_borrow().map_err(|_| ResourceOpenError::Busy)?;
        set.not_lent(id)?;
        let document = set
            .open
            .get(id)
            .and_then(|parked| parked.doc.as_ref())
            .ok_or(ResourceOpenError::MissingResource)?;
        // A component's pass that reads it reads it again once it moved.
        observed_read(self, id, document);
        read(document);
        Ok(())
    }

    /// What tells this store from another holding the same records.
    pub fn key(&self) -> usize {
        std::rc::Rc::as_ptr(&self.0).cast::<()>() as usize
    }

    /// One exclusive record admission for the document and owned action frame.
    ///
    /// The record's document, history and action state are lent out of the
    /// set for the frame, so what the frame does may open, read and change
    /// the set's other records (the parts of a chain beside the record it
    /// shows); the record itself answers [`ResourceOpenError::Busy`] to
    /// anyone else until the frame returns it.
    pub fn with_action_state<R>(
        &self,
        id: &DocumentId,
        callback: impl FnOnce(&mut AutoCommit, &mut crate::state::UndoLog, &mut ResourceActions) -> R,
    ) -> Result<R, ResourceOpenError> {
        let mut loan = {
            let mut set = self.try_set_mut().map_err(|_| ResourceOpenError::Busy)?;
            set.not_lent(id)?;
            let parked = set
                .open
                .get_mut(id)
                .ok_or(ResourceOpenError::MissingResource)?;
            let doc = parked
                .doc
                .take()
                .ok_or(ResourceOpenError::MissingResource)?;
            let lent = (
                doc,
                std::mem::take(&mut parked.history),
                std::mem::take(&mut parked.action_state),
            );
            set.lent.insert(id.clone());
            ResourceLoan {
                store: self,
                id,
                parts: Some(lent),
            }
        };
        let (doc, history, state) = loan
            .parts
            .as_mut()
            .ok_or(ResourceOpenError::MissingResource)?;
        Ok(with_resource_loan(self, id, doc, |doc| {
            callback(doc, history, state)
        }))
    }

    /// Change what the store keeps with the open resource `id` beside its document, without lending
    /// the document: what a toolkit reads of its own part between frames.
    ///
    /// # Errors
    /// `Busy` while the store or the resource is lent; `MissingResource` when it is not open.
    pub fn with_resource_actions<R>(
        &self,
        id: &DocumentId,
        f: impl FnOnce(&mut ResourceActions) -> R,
    ) -> Result<R, ResourceOpenError> {
        let mut set = self.try_set_mut().map_err(|_| ResourceOpenError::Busy)?;
        set.not_lent(id)?;
        let parked = set
            .open
            .get_mut(id)
            .ok_or(ResourceOpenError::MissingResource)?;
        Ok(f(&mut parked.action_state))
    }

    /// The writers this replica has written the open record `id` as, for a
    /// route that publishes this replica's own changes and nobody else's
    /// ([`crate::state::DocumentFrontier::delta`]).
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub fn writing_actors(
        &self,
        id: &DocumentId,
    ) -> Result<crate::state::WritingActors, ResourceOpenError> {
        let set = self.0.try_borrow().map_err(|_| ResourceOpenError::Busy)?;
        set.not_lent(id)?;
        let parked = set.open.get(id).ok_or(ResourceOpenError::MissingResource)?;
        Ok(parked.action_state.writers.borrow().clone())
    }

    /// Generation of the record-owned action frame, for retained callers that
    /// need to invalidate a previously resolved operation.
    ///
    /// # Errors
    /// Returns `Busy` if the record set is borrowed, or `MissingResource` if the record is not open.
    pub fn action_generation(&self, id: &DocumentId) -> Result<Option<u64>, ResourceOpenError> {
        let set = self.0.try_borrow().map_err(|_| ResourceOpenError::Busy)?;
        set.not_lent(id)?;
        let parked = set.open.get(id).ok_or(ResourceOpenError::MissingResource)?;
        Ok(parked.action_state.generation)
    }

    /// Read the open resource `id`'s document with the revision of its history, without opening an
    /// exclusive action loan: what a stamp of its canonical edit state reads.
    ///
    /// # Errors
    /// `Busy` while the resource is lent or its action generation or history revision is spent;
    /// `MissingResource` when it is not open.
    pub fn read_edit_state<R>(
        &self,
        id: &DocumentId,
        read: impl FnOnce(&AutoCommit, u64) -> R,
    ) -> Result<R, ResourceOpenError> {
        let set = self.0.try_borrow().map_err(|_| ResourceOpenError::Busy)?;
        set.not_lent(id)?;
        let parked = set.open.get(id).ok_or(ResourceOpenError::MissingResource)?;
        let document = parked.doc.as_ref().ok_or(ResourceOpenError::Busy)?;
        if parked
            .action_state
            .generation
            .is_none_or(|value| value == u64::MAX)
            || parked.history.revision() == u64::MAX
        {
            return Err(ResourceOpenError::Busy);
        }
        Ok(read(document, parked.history.revision()))
    }

    /// Capture the exact operational identity before admitting remote bytes.
    pub fn replica_identity_stamp(
        &self,
        checkout: &DocumentCheckout,
    ) -> Result<ReplicaIdentityStamp, String> {
        let (_, _, workspace) = self
            .history_namespace(&checkout.id)
            .map_err(|error| error.to_string())?;
        self.read_checked(&checkout.id, |doc| {
            ReplicaIdentityStamp::capture(doc, checkout, &workspace)
        })
        .map_err(|error| error.to_string())?
    }

    /// Withdraw from the open record `id` the changes `refused` names, which
    /// the workspace refused, with every local change made on top of them, and
    /// note one [`ResourceRefusal`] per action they carried, saying `reason`
    /// (for a person), for the app to take ([`Self::take_refusals`]).
    ///
    /// A refused change is shared state nowhere but here, and a peer holds
    /// back every change that depends on one it never receives: kept, it
    /// would show as sent forever and silence everything written after it.
    /// The document becomes what it was without them, saved whole; local
    /// undo starts over, since its steps may name withdrawn changes. A
    /// replication frontier past them must be [`rebased`] onto the heads
    /// this returns. Returns `None` when nothing was withdrawn: the record
    /// holds none of them, or they are its creation (still noted).
    ///
    /// [`rebased`]: crate::state::DocumentFrontier::rebase
    ///
    /// # Errors
    /// The record is not open here or its owner is busy, or the document
    /// does not rebuild.
    pub fn withdraw_refused(
        &self,
        id: &DocumentId,
        refused: &[automerge::ChangeHash],
        reason: &str,
    ) -> Result<Option<Vec<automerge::ChangeHash>>, String> {
        let mut set = self
            .try_set_mut()
            .map_err(|_| ResourceOpenError::Busy.to_string())?;
        let set = &mut *set;
        let portable = set
            .portable
            .as_ref()
            .map(|cache| (cache, portable::key(&set.app_id, &set.home, id)));
        let parked = set
            .open
            .get_mut(id)
            .ok_or_else(|| ResourceOpenError::MissingResource.to_string())?;
        let Parked {
            doc,
            autosave,
            history,
            action_state,
            ..
        } = parked;
        let doc = doc
            .as_mut()
            .ok_or_else(|| ResourceOpenError::MissingResource.to_string())?;
        let withdrawn = with_resource_loan(
            self,
            id,
            doc,
            |doc| -> Result<Option<Vec<automerge::ChangeHash>>, String> {
                if !refused
                    .iter()
                    .any(|hash| doc.get_change_meta_by_hash(hash).is_some())
                {
                    return Ok(None);
                }
                // What was typed so far closes as its own step first.
                action_state.finish_external(doc, history);
                let (kept_heads, refusals) = withdrawal(doc, refused, reason);
                action_state.refusals.extend(refusals);
                if kept_heads.is_empty() {
                    // The record's own creation was refused: it stays as this
                    // device made it, a record nobody else holds, rather than
                    // one with no identity. Nothing said of it holds.
                    action_state.notices.held.clear();
                    return Ok(None);
                }
                // The withdrawn changes left this copy but not the workspace:
                // its relay holds them under this copy's writer and the
                // writers derived from it (`bound_dispatch::document_writer`,
                // `author_writer`), numbered. Written on under those writers,
                // the rebuilt document would number its next changes the
                // same again, and a record holding two changes under one
                // writer and sequence reads whole nowhere. So the copy
                // writes under a writer of its own from here; what it wrote
                // before and still holds stays its own to publish.
                let before = doc.get_actor().clone();
                let device = crate::history_source::authors::DeviceActors::of(&before);
                {
                    let mut writers = action_state.writers.borrow_mut();
                    writers.note(before.clone());
                    for change in doc.get_changes_meta(&[]) {
                        if device.wrote(&change.actor.to_hex_string()) {
                            writers.note(change.actor.into_owned());
                        }
                    }
                }
                let actor = automerge::ActorId::random();
                if let Some((cache, key)) = &portable {
                    cache.claim(key, actor.to_bytes().to_vec())?;
                } else if let Some(writer) = autosave.as_mut() {
                    // A device keeps the writer it saved under: restored on
                    // the next open, it would number that open's first
                    // changes the withdrawn ones' sequences again. Taking a
                    // change back replaces the saved writer, as a page's
                    // claim replaces its saved row's.
                    writer.replace_writer(&actor)?;
                }
                let mut rebuilt = doc
                    .fork_at(&kept_heads)
                    .map_err(|error| error.to_string())?;
                rebuilt.set_actor(actor);
                history.restart(&mut rebuilt);
                action_state.generation = action_state
                    .generation
                    .and_then(|value| value.checked_add(1));
                *doc = rebuilt;
                action_state.notices.withdrawn(doc);
                Ok(Some(doc.get_heads()))
            },
        )?;
        if withdrawn.is_some()
            && let Some(writer) = autosave.as_mut()
        {
            // What was saved holds the withdrawn changes: only a whole save
            // replaces it.
            writer.request_checkpoint(doc);
        }
        Ok(withdrawn)
    }

    /// Say `text` of what was just done to the open record `id` once the
    /// workspace it is shared in keeps it (`WorkspaceNotices`): at once when no
    /// host replicates it, never when the workspace refuses it. The pending
    /// action is committed first, so the change it names is among the heads
    /// held.
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub fn hold_notice(&self, id: &DocumentId, text: String) -> Result<(), ResourceOpenError> {
        self.with_action_state(id, |doc, _, actions| {
            actions.commit_for_save(doc);
            let heads = doc.get_heads();
            actions.notices.hold(text, heads);
        })
    }

    /// The workspace's host answered every commit of record `id` it published:
    /// what was held for them is said ([`Self::take_said`]). Whether there is now something to
    /// say: an answer that settled nothing to say gives its windows nothing new to draw, and that
    /// is every keystroke's answer while a person types.
    pub fn workspace_settled(&self, id: &DocumentId) -> bool {
        if let Ok(mut set) = self.try_set_mut()
            && let Some(parked) = set.open.get_mut(id)
        {
            return parked.action_state.notices.settled();
        }
        false
    }

    /// What to say now of record `id` ([`Self::hold_notice`]), each once.
    pub fn take_said(&self, id: &DocumentId) -> Vec<String> {
        let Ok(mut set) = self.try_set_mut() else {
            return Vec::new();
        };
        set.open
            .get_mut(id)
            .map(|parked| std::mem::take(&mut parked.action_state.notices.said))
            .unwrap_or_default()
    }

    /// The refusals of record `id` noted since the last call
    /// ([`Self::withdraw_refused`]).
    pub fn take_refusals(&self, id: &DocumentId) -> Vec<ResourceRefusal> {
        let Ok(mut set) = self.try_set_mut() else {
            return Vec::new();
        };
        set.open
            .get_mut(id)
            .map(|parked| std::mem::take(&mut parked.action_state.refusals))
            .unwrap_or_default()
    }

    /// Do something with a record's document. `None` if the record is not open here.
    pub fn with<R>(&self, id: &DocumentId, f: impl FnOnce(&mut AutoCommit) -> R) -> Option<R> {
        let mut set = self.set_mut();
        set.document_mut(id)
            .map(|document| with_resource_loan(self, id, document, f))
    }

    /// Whether the store takes a write now: not while something reads or
    /// changes it (a frame's read of a record holds it).
    #[must_use]
    pub fn takes_writes(&self) -> bool {
        self.0.try_borrow_mut().is_ok()
    }

    pub fn read<R>(&self, id: &DocumentId, f: impl FnOnce(&AutoCommit) -> R) -> Option<R> {
        self.read_checked(id, f).ok()
    }

    /// Do something with a record's document *and* its history — commit an
    /// action, undo one.
    pub fn with_history<R>(
        &self,
        id: &DocumentId,
        f: impl FnOnce(&mut AutoCommit, &mut crate::state::UndoLog) -> R,
    ) -> Option<R> {
        let mut set = self.try_set_mut().ok()?;
        let (doc, history) = set.document_and_history_mut(id)?;
        Some(f(doc, history))
    }

    /// Inspect committed history without turning a pending burst into a boundary.
    pub fn read_history<R>(
        &self,
        id: &DocumentId,
        read: impl FnOnce(&AutoCommit, &crate::state::UndoLog) -> R,
    ) -> Option<R> {
        let set = self.0.try_borrow().ok()?;
        set.not_lent(id).ok()?;
        let parked = set.open.get(id)?;
        let document = parked.doc.as_ref()?;
        observed_read(self, id, document);
        Some(read(document, &parked.history))
    }

    /// Every record this store holds.
    pub fn open_resources(&self) -> Vec<DocumentCheckout> {
        self.0.borrow().open_resources()
    }

    /// The open records that belong to another's tree ([`DocumentId::tree`]),
    /// ready to read; none while a frame holds the store.
    #[must_use]
    pub fn open_tree_records(&self) -> Vec<DocumentCheckout> {
        let Ok(set) = self.0.try_borrow() else {
            return Vec::new();
        };
        set.open
            .iter()
            .filter(|(id, parked)| parked.doc.is_some() && id.tree().is_some())
            .map(|(id, parked)| DocumentCheckout::new(id.clone(), parked.kind.clone()))
            .collect()
    }

    /// Offer every open record to its writer. Once per idle tick.
    ///
    /// # Errors
    /// A record's cache reports that it could not take the document.
    pub fn tick(&self) -> Result<(), String> {
        self.set_mut().tick_open()
    }

    /// Write out every open record and wait for it to land.
    ///
    /// # Errors
    /// Each record's writer that reports a storage failure, by record.
    pub fn flush(&self) -> Result<(), String> {
        self.set_mut().flush_open()
    }
}

impl std::fmt::Debug for ResourceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceStore")
            .field("open", &self.0.borrow().open.len())
            .finish()
    }
}

struct LoanIdentity {
    store: ResourceStore,
    record: DocumentId,
    document: *const AutoCommit,
}
thread_local! {
    static RESOURCE_LOANS: std::cell::RefCell<Vec<LoanIdentity>> = const { std::cell::RefCell::new(Vec::new()) };
}
struct LoanGuard;
impl Drop for LoanGuard {
    fn drop(&mut self) {
        RESOURCE_LOANS.with_borrow_mut(|loans| {
            loans.pop();
        });
    }
}
/// Call `callback` on `document`, resource `resource` of `store`, lent to it: what it checks with
/// [`resource_loan_matches`] is this loan.
fn with_resource_loan<R>(
    store: &ResourceStore,
    record: &DocumentId,
    document: &mut AutoCommit,
    callback: impl FnOnce(&mut AutoCommit) -> R,
) -> R {
    RESOURCE_LOANS.with_borrow_mut(|loans| {
        loans.push(LoanIdentity {
            store: store.clone(),
            record: record.clone(),
            document,
        })
    });
    let _loan = LoanGuard;
    callback(document)
}
/// Whether the innermost loan open on this thread is `document`, resource `checkout` of `store`:
/// what an invocation that names the resource checks before it writes.
pub fn resource_loan_matches(
    store: &ResourceStore,
    checkout: &DocumentCheckout,
    document: &AutoCommit,
) -> bool {
    RESOURCE_LOANS.with_borrow(|loans| {
        loans.last().is_some_and(|loan| {
            loan.store.same_owner(store)
                && loan.record == checkout.id
                && std::ptr::eq(loan.document, document)
        })
    })
}

mod identity_stamp;
pub use identity_stamp::ReplicaIdentityStamp;
#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod identity_tests;

mod arrival;
mod canonical_read;
mod mutation_merge;
mod opening;
mod stored;
pub use opening::ResourceOpening;
#[cfg(test)]
#[cfg(feature = "broker-transport")]
mod native_head_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod portable_tests;

#[cfg(test)]
mod native_writer_tests;
