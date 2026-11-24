//! The resources an app process has open: each one's Automerge document, the history it keeps,
//! the actions in flight on it, and what the workspace refused or was told of it.
//!
//! The process keeps no copy of its own. A resource is read from the workspace's one store when
//! it opens, through the store's [`DocumentPort`], and what changes in it goes back there: a
//! window's routes publish what its records changed, and a store that writes for itself (a host
//! with no window, the session's own records) sends each committed change through the port.
//! Without a port the store is memory alone: tests, captures, and runs that must leave nothing
//! behind.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjId, ObjType, ROOT, ReadDoc};
use tilo_app_id::AppId;
use tilo_lock::MutexRecover as _;
use tilo_sort::CompactSort as _;

use super::{
    DocumentCheckout, DocumentId, DocumentRef, INDEX, ResourceOpenError, ResourceType,
    add_reference, commit_genesis, genesis, genesis_actor, home, identity, init_scoped, references,
    str_at, validate_identity, workspace_scope,
};
use crate::document::fields::{META, ObjectAt, StringAt, object_at, string_at};

mod actions;
mod arrival;
pub mod commands;
#[cfg(feature = "broker-transport")]
mod connection_port;
pub use commands::CommandDocuments;
#[cfg(feature = "broker-transport")]
pub use connection_port::ConnectionDocuments;
mod memory_workspace;
mod opening;
pub use memory_workspace::MemoryWorkspace;
pub use opening::ResourceOpening;
pub mod bootstrap;
mod history_access;
mod identity_stamp;
pub mod index;
mod mutation_merge;
mod replication;
pub mod sync;

pub use actions::{ResourceActions, ToolkitActions};
pub use history_access::HistoryUnavailable;
pub use identity_stamp::ReplicaIdentityStamp;
pub use sync::{DocumentSync, Outbound};

/// The scope of the person's own resources, `(app, personal)`: theirs and not a workspace's, so
/// their own workspace's route carries them.
pub const PERSONAL_SCOPE: &str = "personal";

/// Upper bound for the records an app learns from one workspace index at once: the index is
/// replicated input, not a request to allocate one local document per entry.
pub const MAX_DISCOVERED_RESOURCES: usize = 64;
const MAX_INDEX_KEYS_SCANNED: usize = MAX_DISCOVERED_RESOURCES * 4;

/// How long a flush waits for the workspace to take what the store wrote.
const FLUSH_WAIT: crate::time::Duration = crate::time::Duration::from_secs(10);

/// What a toolkit learns of each read of an open resource, so a pass that read it is laid out
/// again once the resource moved.
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

/// One record a [`DocumentPort`] read asks for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DocumentAsk {
    pub id: String,
    /// The heads the reader holds, so only what changed after them is sent; `None` for the
    /// record whole.
    pub since: Option<Vec<[u8; 32]>>,
}

/// What a [`DocumentPort`] answers of one record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentRead {
    /// The record whole, at the heads and revision it stands at: a saved document or every
    /// change of it, either of which a document loads.
    Whole {
        revision: [u8; 32],
        heads: Vec<[u8; 32]>,
        bytes: Vec<u8>,
        /// The load hint for exactly `bytes` ([`tilo_automerge::load_hint`]), when the port
        /// saved them and they are large enough to need one: what their reader skips rebuilding.
        hint: Option<Vec<u8>>,
    },
    /// The changes after the heads the reader holds, which complete it to `heads`.
    Changes {
        revision: [u8; 32],
        heads: Vec<[u8; 32]>,
        bytes: Vec<u8>,
    },
    /// The record stands where the reader last read it.
    Unchanged([u8; 32]),
    /// The workspace holds nothing of it.
    Absent,
    /// It is on its way (the workspace is still opening, or its checkpoint or the keys that open
    /// it have not come), and reads once it is here.
    Pending,
    /// The workspace holds it, which is all a listing says: read it by id.
    Held,
    /// Not answered this time: ask for it again.
    Later,
}

/// Called once a write a window shows as saving was answered, from whichever thread took the answer.
pub type ReadyWake = std::sync::Arc<dyn Fn() + Send + Sync>;

/// The answer to one write through a [`DocumentPort`], asked for until it comes, waiting at most the
/// given time for it (none, to look without waiting): `None` while the workspace has not answered,
/// then whether it took the write and, if not, why.
pub type Written = Box<dyn FnMut(crate::time::Duration) -> Option<Result<(), String>>>;

/// Where a store reads its resources from and writes them back to: the workspace's one store, over
/// the app's broker connection, through the session's own reads and writes of it, or held in this
/// process for a test.
pub trait DocumentPort {
    /// The documents `reads` names of `app` in `workspace`, each whole or as the changes after the
    /// heads its reader holds; every document of the app there when `reads` names none. Never
    /// waits for the workspace: a document whose answer has not come is [`DocumentRead::Pending`],
    /// and a listing whose answer has not come answers what it last listed, until it comes
    /// ([`Self::when_answered`]).
    ///
    /// # Errors
    /// The workspace could not be asked, or refused the read.
    fn read(
        &self,
        app: &str,
        workspace: &str,
        reads: Option<Vec<DocumentAsk>>,
    ) -> Result<Vec<(String, DocumentRead)>, String>;

    /// Have `wake` called, from whichever thread takes it, once something this port answered as on
    /// its way comes in: the next read takes it. Whether anything is on its way; when nothing is,
    /// `wake` is dropped uncalled. A port that answers every read at once has nothing on its way.
    fn when_answered(&self, wake: ReadyWake) -> bool {
        drop(wake);
        false
    }

    /// Write `changes` to `app`'s document `id` in `workspace`: its founding write when
    /// `founding`, the whole document the workspace does not hold yet. `readers` names who may
    /// read it, as the workspace writes reader sets (the people's root keys in lowercase hex,
    /// sorted and joined by `,`); `None` is every member. Answered later.
    fn write(
        &self,
        app: &str,
        workspace: &str,
        id: &str,
        changes: Vec<u8>,
        founding: bool,
        readers: Option<&str>,
    ) -> Written;
}

/// Whether a record's changes are kept by its workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceSaveStatus {
    /// The workspace holds everything the record holds.
    Saved,
    /// A change is on its way to the workspace.
    Saving,
    /// The workspace did not take a change, for this reason.
    Failed(String),
    /// The record lives in this process alone.
    MemoryOnly,
}

/// One record this process has open.
struct Parked {
    /// `None` exactly while checked out or lent: the live document is the toolkit's or the
    /// frame's then.
    doc: Option<AutoCommit>,
    kind: ResourceType,
    /// Belongs to the record: an undo entry is a pair of change hashes in *this* document's graph,
    /// so sharing a stack would revert the wrong history.
    history: tilo_resource_store::history::UndoLog,
    action_state: ResourceActions,
    /// The heads the workspace holds of it, as far as this store knows: what its next write
    /// starts after. `None` while the workspace holds none of it.
    stored: Option<Vec<ChangeHash>>,
    /// The write under way, with the heads it writes up to.
    writing: Option<(Written, Vec<ChangeHash>)>,
    /// Why the workspace did not take the last write, until one is taken.
    failed: Option<String>,
    /// Told when the write under way is answered.
    wakes: Vec<ReadyWake>,
    /// The person asked to try again what the workspace refused of it, in a store whose window
    /// publishes its changes: the window sends it again ([`ResourceStore::take_resend`]).
    resend: bool,
}

impl Parked {
    fn new(mut doc: AutoCommit, kind: ResourceType, stored: Option<Vec<ChangeHash>>) -> Self {
        let history = tilo_resource_store::history::UndoLog::new(&mut doc);
        Self {
            doc: Some(doc),
            kind,
            history,
            action_state: ResourceActions::default(),
            stored,
            writing: None,
            failed: None,
            wakes: Vec::new(),
            resend: false,
        }
    }

    /// Whether the workspace holds the document at `heads`, as far as this store heard.
    fn keeps(&self, heads: &[ChangeHash]) -> bool {
        self.stored.as_ref().is_some_and(|stored| {
            stored.len() == heads.len() && heads.iter().all(|head| stored.contains(head))
        })
    }

    /// The workspace holds `heads` of the record, besides what this store wrote: what it brought
    /// is not written back. Nothing is noted while the document is checked out or a write is under
    /// way, whose answer names what the workspace holds then.
    fn workspace_holds(&mut self, heads: &[ChangeHash]) {
        let (Some(doc), None) = (self.doc.as_mut(), &self.writing) else {
            return;
        };
        let mut held: Vec<ChangeHash> =
            self.stored.iter().flatten().chain(heads).copied().collect();
        held.sort_unstable();
        held.dedup();
        held.retain(|head| doc.has_change(head));
        // The record holds nothing beyond what the workspace holds: its heads are the workspace's,
        // the form a write compares against. Otherwise the next write sends what follows `held`.
        self.stored = Some(if doc.get_changes(&held).is_empty() {
            doc.get_heads()
        } else {
            held
        });
    }

    /// Whether the workspace holds everything `doc` holds: an edit still in its open typing
    /// transaction is not the workspace's yet.
    fn keeps_all(&self, doc: &AutoCommit) -> bool {
        doc.pending_ops() == 0 && self.keeps(&doc.base_heads())
    }

    /// Take the answer to the write under way, waiting at most `wait` for it.
    fn poll_write(&mut self, wait: crate::time::Duration) {
        let Some((written, heads)) = self.writing.as_mut() else {
            return;
        };
        let Some(answer) = written(wait) else {
            return;
        };
        let heads = std::mem::take(heads);
        self.writing = None;
        for wake in self.wakes.drain(..) {
            wake();
        }
        match answer {
            Ok(()) => {
                self.stored = Some(heads);
                self.failed = None;
            }
            Err(error) => {
                tracing::warn!(%error, "the workspace did not take a record's change");
                self.failed = Some(error);
            }
        }
    }
}

/// The records one app process has open in one workspace, and which is live. Routing a record's
/// changes to its peers is the toolkit's business, since a channel belongs to the connection.
pub struct ResourceSet {
    /// Moves on every write access to the set: whatever changed a resource's document (a commit,
    /// a merge, a migration, an open or a close) took one, so a reader that saw the same number
    /// has nothing new to look at ([`ResourceStore::write_generation`]).
    generation: Cell<u64>,
    app_id: AppId,
    /// The workspace these records are opened in, which is the cluster its route names. Stamped
    /// as a record's home when it is first created here.
    home: String,
    /// The person's own workspace, for records kept in their own scope: their route names it.
    person: Option<uuid::Uuid>,
    open: HashMap<DocumentId, Parked>,
    pending: HashMap<DocumentId, Reserved>,
    /// The records lent out to an action frame: their documents are with the frame, so the other
    /// records stay open to it meanwhile.
    lent: HashSet<DocumentId>,
    next_reservation: u64,
    /// How many times something arrived for what follows this store's records
    /// ([`ResourceStore::note_kept`]).
    kept: Cell<u64>,
    /// Records of this store's trees peers wrote that it does not hold yet
    /// ([`ResourceStore::announce`]).
    announced: RefCell<BTreeSet<DocumentId>>,
    /// Records the workspace answered for, and what it answered, until something of them arrives.
    answered: RefCell<BTreeMap<DocumentId, WorkspaceAnswer>>,
    /// This store's records come from its workspace ([`ResourceStore::ask_workspace_for_missing`]):
    /// a record not here is asked for and new only once it answered it holds none of it.
    asks_workspace: Cell<bool>,
    /// The records not here that were opened while [`Self::asks_workspace`]: what the workspace is
    /// asked for ([`ResourceStore::wanted_resources`]).
    wanted: RefCell<BTreeSet<DocumentId>>,
    /// The kind each of `wanted` was opened as.
    wanted_kinds: RefCell<BTreeMap<DocumentId, ResourceType>>,
    /// The windows that read this store's records, woken when another replica's change arrives,
    /// which no input or wake of theirs announces, or what the workspace answered after a read
    /// found it on its way: from whichever thread took the answer.
    readers: Arc<Mutex<Vec<crate::WakeHandle>>>,
    /// Whether the store's readers are woken once the port's answers on their way come
    /// ([`Self::await_port`]).
    awaits_port: Arc<AtomicBool>,
    /// Records being read off the frame ([`ResourceStore::begin_opening`]): not open until they
    /// are taken in.
    opening: RefCell<HashMap<DocumentId, opening::Opening>>,
    /// Where the records are read from and written back to; `None` for a store in memory alone.
    port: Option<Rc<dyn DocumentPort>>,
    /// Whether the store writes its records' changes through `port` itself, rather than a
    /// window's routes publishing them.
    writes: bool,
    /// What this copy of each record writes as: the device's, and its own
    /// ([`tilo_resource_store::history::authors::copy_writer`]).
    writer: automerge::ActorId,
}

struct Reserved {
    generation: u64,
    kind: ResourceType,
}

/// The document a whole read answered: `None` when the workspace holds none of it.
fn decode_read(read: Option<DocumentRead>) -> Result<Option<AutoCommit>, ResourceOpenError> {
    match read {
        // A document another reader on this thread holds at these heads is copied, not decoded.
        Some(DocumentRead::Whole {
            heads, bytes, hint, ..
        }) => match tilo_automerge::shared::held_at(
            &heads.into_iter().map(ChangeHash).collect::<Vec<_>>(),
        ) {
            Some(doc) => Ok(Some(doc)),
            None => tilo_automerge::shared::load_hinted(&bytes, hint.as_deref())
                .map(Some)
                .map_err(|error| ResourceOpenError::Initialization(error.to_string())),
        },
        Some(DocumentRead::Pending | DocumentRead::Later) => Err(ResourceOpenError::Arriving),
        Some(DocumentRead::Absent) | None => Ok(None),
        Some(DocumentRead::Changes { .. } | DocumentRead::Unchanged(_) | DocumentRead::Held) => {
            Err(ResourceOpenError::Initialization(
                "the workspace answered a whole read in part".into(),
            ))
        }
    }
}

impl ResourceSet {
    fn new(
        app_id: AppId,
        home: String,
        person: Option<uuid::Uuid>,
        port: Option<Rc<dyn DocumentPort>>,
        writes: bool,
        writer: automerge::ActorId,
    ) -> Self {
        Self {
            generation: Cell::new(0),
            app_id,
            home,
            person,
            open: HashMap::new(),
            pending: HashMap::new(),
            lent: HashSet::new(),
            next_reservation: 0,
            kept: Cell::new(0),
            announced: RefCell::default(),
            answered: RefCell::default(),
            asks_workspace: Cell::new(false),
            wanted: RefCell::default(),
            wanted_kinds: RefCell::default(),
            readers: Arc::default(),
            awaits_port: Arc::default(),
            opening: RefCell::default(),
            port,
            writes,
            writer,
        }
    }

    /// A set held in memory alone. For tests, for captures, and for any run that must not leave
    /// state behind. It is a workspace of its own, which nothing else reads: the records it makes
    /// have a home like any other's, and leave with it.
    pub fn in_memory() -> Self {
        Self::new(
            AppId::default(),
            uuid::Uuid::new_v4().to_string(),
            None,
            None,
            false,
            automerge::ActorId::random(),
        )
    }

    /// The app these records belong to.
    pub fn app_id(&self) -> &AppId {
        &self.app_id
    }

    /// The workspace these records are opened in.
    pub fn home(&self) -> &str {
        &self.home
    }

    /// Name the workspace explicitly. Used by tests and by a host that opens records on behalf
    /// of a workspace other than its own launch environment.
    #[must_use]
    pub fn with_home(mut self, home: impl Into<String>) -> Self {
        self.home = home.into();
        self
    }

    /// Every record this process has open.
    pub fn open_resources(&self) -> Vec<DocumentCheckout> {
        let mut out: Vec<DocumentCheckout> = self
            .open
            .iter()
            .map(|(id, p)| DocumentCheckout::new(id.clone(), p.kind.clone()))
            .collect();
        out.compact_sort();
        out
    }

    /// [`ResourceOpenError::Busy`] while `id` is lent to an action frame.
    fn not_lent(&self, id: &DocumentId) -> Result<(), ResourceOpenError> {
        if self.lent.contains(id) {
            Err(ResourceOpenError::Busy)
        } else {
            Ok(())
        }
    }

    /// Read an open record's document without checking it out. `None` for the record currently
    /// checked out — its document is the live one.
    pub fn peek(&self, id: &DocumentId) -> Option<&AutoCommit> {
        self.open.get(id).and_then(|p| p.doc.as_ref())
    }

    /// The cluster a record's route names: the workspace it is kept in, or the person's own
    /// workspace for a record kept in their own scope.
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

    /// `want` as the workspace holds it, through the port: `None` when there is no port or the
    /// workspace holds none of it, which the store notes as the workspace's answer.
    fn read(&self, want: &DocumentCheckout) -> Result<Option<AutoCommit>, ResourceOpenError> {
        // A record opening off the frame is read by taking it in, once it is here.
        match self.opened(&want.id, true) {
            opening::Opened::Here(answer) => return answer,
            opening::Opened::OnItsWay => return Err(ResourceOpenError::Arriving),
            opening::Opened::NotOpening => {}
        }
        let Some(port) = &self.port else {
            return Ok(None);
        };
        // A record the workspace said it holds none of is not asked for again until something
        // arrives ([`ResourceStore::note_kept`]): a list of absent records costs no round trips.
        if self.answered.borrow().get(&want.id) == Some(&WorkspaceAnswer::HoldsNone) {
            return Ok(None);
        }
        let workspace = self.cluster()?.to_string();
        let ask = DocumentAsk {
            id: want.id.route().to_owned(),
            since: None,
        };
        let read = || {
            port.read(self.app_id.as_str(), &workspace, Some(vec![ask.clone()]))
                .map_err(ResourceOpenError::Unreachable)
                .map(|mut read| read.pop().map(|(_, read)| read))
        };
        let mut answer = decode_read(read()?);
        // The readers are woken once it comes; one that came since it was read is read now.
        if matches!(answer, Err(ResourceOpenError::Arriving)) && !self.await_port() {
            answer = decode_read(read()?);
        }
        self.settle(want, answer)
    }

    /// Wake the store's readers once the port's answers on their way come in, so a window that
    /// read a record or a listing as on its way reads it again: the port asked the workspace, and
    /// holds the answer for the next read. Whether they will be: not when the port has nothing on
    /// its way.
    fn await_port(&self) -> bool {
        let Some(port) = &self.port else {
            return false;
        };
        if self.awaits_port.swap(true, Ordering::AcqRel) {
            return true;
        }
        let (readers, awaits) = (Arc::clone(&self.readers), Arc::clone(&self.awaits_port));
        let wake: ReadyWake = Arc::new(move || {
            awaits.store(false, Ordering::Release);
            let readers = readers.lock_recover().clone();
            for reader in readers {
                reader.wake();
            }
        });
        let awaits = port.when_answered(wake);
        if !awaits {
            self.awaits_port.store(false, Ordering::Release);
        }
        awaits
    }

    /// Note what the workspace answered for `want`: its document, checked to be `want`'s, or that
    /// it holds none of it.
    fn settle(
        &self,
        want: &DocumentCheckout,
        answer: Result<Option<AutoCommit>, ResourceOpenError>,
    ) -> Result<Option<AutoCommit>, ResourceOpenError> {
        let Some(mut doc) = answer? else {
            self.answered
                .borrow_mut()
                .insert(want.id.clone(), WorkspaceAnswer::HoldsNone);
            return Ok(None);
        };
        validate_identity(&doc, want, &self.home)?;
        doc.set_actor(self.writer.clone());
        self.answered.borrow_mut().remove(&want.id);
        Ok(Some(doc))
    }

    /// Open `want` from the workspace when it holds it: whether it is open now.
    fn open_from_workspace(&mut self, want: &DocumentCheckout) -> Result<bool, ResourceOpenError> {
        if self.open.contains_key(&want.id) {
            return Ok(true);
        }
        let Some(mut doc) = self.read(want)? else {
            return Ok(false);
        };
        let heads = doc.get_heads();
        self.open.insert(
            want.id.clone(),
            Parked::new(doc, want.kind.clone(), Some(heads)),
        );
        Ok(true)
    }

    /// A new document for `want`, stamped with its identity: a record the workspace holds none of.
    fn founded(&self, want: &DocumentCheckout) -> Result<AutoCommit, ResourceOpenError> {
        let mut doc = AutoCommit::new().with_actor(self.writer.clone());
        // It is held from now on.
        self.answered.borrow_mut().remove(&want.id);
        let scope = (!self.home.is_empty()).then_some(self.home.as_str());
        if !init_scoped(&mut doc, &want.id, &want.kind, &self.home, scope) {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        Ok(doc)
    }

    /// Open `want` if it is not open already, reading it from the workspace or creating it;
    /// returns whether it opened it.
    pub fn open_resource(&mut self, want: &DocumentCheckout) -> bool {
        if !want.is_valid() {
            tracing::warn!(record = %want.id, "refusing invalid record open");
            return false;
        }
        let fresh = !self.open.contains_key(&want.id);
        if fresh {
            let (mut doc, stored) = match self.read(want) {
                Ok(Some(mut doc)) => {
                    let heads = doc.get_heads();
                    (doc, Some(heads))
                }
                Ok(None) => match self.founded(want) {
                    Ok(doc) => (doc, None),
                    Err(_) => return false,
                },
                Err(error) => {
                    tracing::warn!(record = %want.id, %error, "a record could not be opened");
                    return false;
                }
            };
            for (target, relation) in &want.references {
                add_reference(
                    &mut doc,
                    &DocumentRef::new(target.id.clone(), relation.clone()),
                );
            }
            self.open
                .insert(want.id.clone(), Parked::new(doc, want.kind.clone(), stored));
        }
        fresh
    }

    fn try_open_resource(
        &mut self,
        want: &DocumentCheckout,
    ) -> Result<ResourceOpenOutcome, ResourceOpenError> {
        if !want.is_valid_local_storage_request() {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        if self.pending.contains_key(&want.id) {
            return Err(ResourceOpenError::Busy);
        }
        // A record opening off the frame is opened by taking it in, once it is here.
        self.take_in_opening(&want.id, true)?;
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
        if self.open_from_workspace(want)? {
            Ok(ResourceOpenOutcome::Loaded)
        } else {
            Err(ResourceOpenError::MissingResource)
        }
    }

    /// An open record's document, for a host showing several at once; `None` for a record not
    /// held here or the one checked out, whose live document is the toolkit's.
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

    /// An open record's document together with its history, so a caller driving undo can do both
    /// against the same record.
    pub fn document_and_history_mut(
        &mut self,
        id: &DocumentId,
    ) -> Option<(&mut AutoCommit, &mut tilo_resource_store::history::UndoLog)> {
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

    /// Send what changed in each open record to the workspace, when this store writes for itself.
    /// Take every answer that came.
    pub fn tick(&mut self) {
        let (Some(port), true) = (self.port.clone(), self.writes) else {
            return;
        };
        let Ok(workspace) = self.cluster().map(|cluster| cluster.to_string()) else {
            return;
        };
        for (id, slot) in &mut self.open {
            slot.poll_write(crate::time::Duration::ZERO);
            if slot.writing.is_some() {
                continue;
            }
            let Some(doc) = slot.doc.as_mut() else {
                continue;
            };
            slot.action_state.commit_for_save(doc);
            let heads = doc.get_heads();
            if slot.stored.as_ref() == Some(&heads) || heads.is_empty() {
                continue;
            }
            let (changes, founding) = match &slot.stored {
                Some(stored) => (doc.save_after(stored), false),
                None => (doc.save(), true),
            };
            let written = port.write(
                self.app_id.as_str(),
                &workspace,
                id.route(),
                changes,
                founding,
                None,
            );
            slot.writing = Some((written, heads));
            slot.poll_write(crate::time::Duration::ZERO);
        }
    }

    /// [`Self::tick`], then wait until the workspace answered every write.
    ///
    /// # Errors
    /// Each record the workspace did not take, or did not answer for in time, by record.
    pub fn flush(&mut self) -> Result<(), String> {
        let until = crate::time::now() + FLUSH_WAIT;
        loop {
            self.tick();
            let mut errors = Vec::new();
            let mut waiting = false;
            for (id, slot) in &self.open {
                // A record whose refused write is being sent again waits for that answer: the
                // refusal before it is not the answer to this flush.
                if slot.writing.is_some() {
                    waiting = true;
                } else if let Some(error) = &slot.failed {
                    errors.push(format!("{id}: {error}"));
                }
            }
            if !errors.is_empty() {
                return Err(errors.join("; "));
            }
            if !waiting {
                return Ok(());
            }
            let now = crate::time::now();
            if now >= until {
                return Err("the workspace did not answer every write in time".to_owned());
            }
            // Wait on the first answer still to come; the next pass sends what changed meanwhile.
            // A refusal that answers it is this flush's answer, not a write to send once more.
            if let Some((id, slot)) = self
                .open
                .iter_mut()
                .find(|(_, slot)| slot.writing.is_some())
            {
                slot.poll_write(until - now);
                if let (None, Some(error)) = (&slot.writing, &slot.failed) {
                    return Err(format!("{id}: {error}"));
                }
            }
        }
    }

    /// [`Self::tick`], then whether a write is still on its way, without waiting for one: what a
    /// closing window asks each pass until the workspace answered every write.
    ///
    /// # Errors
    /// Each record the workspace did not take, by record.
    pub fn settling(&mut self) -> Result<bool, String> {
        self.tick();
        let mut errors = Vec::new();
        let mut waiting = false;
        for (id, slot) in &mut self.open {
            slot.poll_write(crate::time::Duration::ZERO);
            if let Some(error) = &slot.failed {
                errors.push(format!("{id}: {error}"));
            }
            waiting |= slot.writing.is_some();
        }
        if errors.is_empty() {
            Ok(waiting)
        } else {
            Err(errors.join("; "))
        }
    }

    /// Mark every record whose change the workspace refused to be sent again by the window
    /// that publishes it, in a store that does not write for itself. Whether this store writes
    /// for itself.
    fn ask_resend(&mut self) -> bool {
        if self.writes {
            return true;
        }
        for slot in self.open.values_mut() {
            slot.resend |= slot.failed.is_some();
        }
        false
    }
}

/// A [`ResourceSet`] several widgets share via a handle, so two tabs on the same file stay one
/// document; single-threaded on purpose, since a `Mutex` here would guard against nothing.
#[derive(Clone)]
pub struct ResourceStore(Rc<RefCell<ResourceSet>>);

type OwnerIndex = Rc<RefCell<HashMap<(String, String), std::rc::Weak<RefCell<ResourceSet>>>>>;

/// The stores of one process, one per app and workspace. Clones share them; a store nobody holds
/// any more is let go.
#[derive(Clone)]
pub struct ResourceOwners {
    port: Option<Rc<dyn DocumentPort>>,
    writes: bool,
    /// The device the records are opened on, which each copy's writer names.
    device: Option<String>,
    owners: OwnerIndex,
}

impl ResourceOwners {
    /// Stores held in memory alone.
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            port: None,
            writes: false,
            device: None,
            owners: Rc::default(),
        }
    }

    /// Stores that read their records through `port`, whose windows' routes publish what changes.
    #[must_use]
    pub fn reading(port: Rc<dyn DocumentPort>) -> Self {
        Self {
            port: Some(port),
            writes: false,
            device: None,
            owners: Rc::default(),
        }
    }

    /// Stores that read their records through `port` and write each committed change back
    /// through it themselves.
    #[must_use]
    pub fn writing(port: Rc<dyn DocumentPort>) -> Self {
        Self {
            port: Some(port),
            writes: true,
            device: None,
            owners: Rc::default(),
        }
    }

    /// These stores, on the device `device` names: each copy of a record they open writes as a
    /// writer of that device, so what the person did there before a restart stays theirs to undo.
    #[must_use]
    pub fn on_device(mut self, device: &str) -> Self {
        self.device = Some(device.to_owned());
        self
    }

    /// The unique live store for this app and workspace.
    ///
    /// # Errors
    /// Rejects an invalid app or workspace.
    pub fn open(
        &mut self,
        app_id: &str,
        workspace_id: &str,
    ) -> Result<ResourceStore, ResourceOpenError> {
        self.open_scoped(app_id, workspace_id, None)
    }

    /// The store of `app_id`'s records of the person, kept in `scope`.
    ///
    /// # Errors
    /// Rejects an invalid app or scope.
    pub fn open_person(
        &mut self,
        app_id: &str,
        scope: &str,
    ) -> Result<ResourceStore, ResourceOpenError> {
        self.open_scoped(app_id, scope, None)
    }

    /// The store of `app_id`'s resources in `workspace` that a window of another app opens beside
    /// its own.
    ///
    /// # Errors
    /// Rejects an invalid app or workspace.
    pub fn open_beside(
        &mut self,
        app_id: &str,
        workspace: &str,
    ) -> Result<ResourceStore, ResourceOpenError> {
        self.open_scoped(app_id, workspace, None)
    }

    /// The store of `app_id`'s records of one person, kept in their own scope. Those carry one
    /// route per `(app, record)` in the person's own workspace, so every device of theirs opens
    /// the same document; one that names no person keeps them here.
    ///
    /// # Errors
    /// Rejects an invalid app or scope.
    pub fn open_personal(
        &mut self,
        app_id: &str,
        author: &crate::MemberId,
    ) -> Result<ResourceStore, ResourceOpenError> {
        self.open_scoped(
            app_id,
            PERSONAL_SCOPE,
            author
                .person_id()
                .and_then(collab_types::person_workspace_of),
        )
    }

    fn open_scoped(
        &self,
        app_id: &str,
        workspace_id: &str,
        person: Option<uuid::Uuid>,
    ) -> Result<ResourceStore, ResourceOpenError> {
        // A cluster of the person's own routes their own scope only: a workspace's records name
        // the workspace.
        let person = person.filter(|_| workspace_id == PERSONAL_SCOPE);
        if !valid_name(app_id) || !valid_name(workspace_id) {
            return Err(ResourceOpenError::IdentityMismatch);
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
        let store = ResourceStore::new(ResourceSet::new(
            app_id.into(),
            workspace_id.to_owned(),
            person,
            self.port.clone(),
            self.writes,
            self.device
                .as_deref()
                .map_or_else(automerge::ActorId::random, |device| {
                    tilo_resource_store::history::authors::copy_writer(device.as_bytes())
                }),
        ));
        self.owners
            .borrow_mut()
            .insert(key, Rc::downgrade(&store.0));
        Ok(store)
    }
}

/// Whether `id` is the record a window of the app started for itself.
fn window_default(id: &DocumentId) -> bool {
    DocumentId::window_default_of_storage_key(&id.storage_key()).as_ref() == Some(id)
}

/// Whether `name` may name an app or a workspace: a non-empty name with nothing that would make
/// it another one's on the wire or in a path.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
}

/// The heads of what `doc` keeps without the changes `refused` names and everything built on them,
/// and a [`ResourceRefusal`] saying `reason` per action the withdrawn changes carried, in order.
fn withdrawal(
    doc: &mut AutoCommit,
    refused: &[ChangeHash],
    reason: &str,
) -> (Vec<ChangeHash>, Vec<ResourceRefusal>) {
    let changes = doc.get_changes_meta(&[]);
    let mut withdrawn: HashSet<ChangeHash> = HashSet::new();
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
    let depended: HashSet<ChangeHash> = changes
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
    /// The verb's key, or the edited field's path ([`collab_types::ClaimedAction::named`]).
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
    sent: Vec<ChangeHash>,
    /// The workspace answered every commit published so far.
    answered: bool,
    said: Vec<String>,
}

#[derive(Debug)]
struct HeldNotice {
    text: String,
    heads: Vec<ChangeHash>,
    /// A publication carried its heads.
    published: bool,
}

impl WorkspaceNotices {
    fn hold(&mut self, text: String, heads: Vec<ChangeHash>) {
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
    fn sent(&mut self, heads: &[ChangeHash]) {
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

    /// Changes were withdrawn: what was said at heads `doc` no longer holds is not said.
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

/// What a workspace answered of a record a store asked it for and does not hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WorkspaceAnswer {
    /// It holds none of it: it is new to whoever may start it.
    HoldsNone,
    /// It is another person's own record, which it sends that person's devices alone.
    NotYours,
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

    /// Initialize once, then make the record live with its one history.
    ///
    /// # Errors
    /// Retains the reservation on initializer or validation failure.
    pub fn initialize(
        &mut self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<(), ResourceOpenError> {
        self.initialize_seeded(initialize, |_| Ok(()))
    }

    /// [`Self::initialize`], then `seed` the record with what depends on who creates it or where;
    /// the seed follows the genesis and precedes undo history, so it is nobody's undo step.
    ///
    /// # Errors
    /// As [`Self::initialize`]; a failing seed fails the initialization.
    pub fn initialize_seeded(
        &mut self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
        seed: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<(), ResourceOpenError> {
        let (workspace, writer) = {
            let set = self.store.0.borrow();
            set.pending
                .get(&self.checkout.id)
                .filter(|p| p.generation == self.generation)
                .ok_or(ResourceOpenError::Busy)?;
            (set.home.clone(), set.writer.clone())
        };
        let mut doc = AutoCommit::new().with_actor(writer);
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
        set.pending.remove(&self.checkout.id);
        set.open.insert(
            self.checkout.id.clone(),
            Parked::new(doc, self.checkout.kind.clone(), None),
        );
        set.tick();
        Ok(())
    }
}

impl Drop for ResourceSet {
    /// What the store committed and the workspace does not hold yet goes to it as the store goes:
    /// a host that ends without closing its records still keeps them.
    fn drop(&mut self) {
        if self.writes
            && self.port.is_some()
            && let Err(error) = self.flush()
        {
            tracing::error!(app = %self.app_id.as_str(), %error, "records were not kept as their store closed");
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

/// A record lent to an action frame ([`ResourceStore::with_action_state`]), returned to its set
/// when the frame ends, however it ends.
struct ResourceLoan<'a> {
    store: &'a ResourceStore,
    id: &'a DocumentId,
    parts: Option<(
        AutoCommit,
        tilo_resource_store::history::UndoLog,
        ResourceActions,
    )>,
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

impl ResourceStore {
    /// Share an existing set.
    pub fn new(set: ResourceSet) -> Self {
        Self(Rc::new(RefCell::new(set)))
    }

    /// A store held in memory alone: see [`ResourceSet::in_memory`].
    pub fn in_memory() -> Self {
        Self::new(ResourceSet::in_memory())
    }

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

    /// A number that moves whenever anything may have changed a resource of this store (a commit,
    /// a merge, a migration, an open or a close), and stays the same while nothing did.
    #[must_use]
    pub fn write_generation(&self) -> u64 {
        self.0
            .try_borrow()
            .map_or(u64::MAX, |set| set.generation.get())
    }

    /// The route record `id` travels on: its workspace, the app and its wire name, unchecked
    /// against the record's document.
    ///
    /// # Errors
    /// The store is busy, or names no workspace.
    pub fn wire_route(&self, id: &DocumentId) -> Result<crate::RouteSpec, ResourceOpenError> {
        self.0
            .try_borrow()
            .map_err(|_| ResourceOpenError::Busy)?
            .wire_route(id)
    }

    /// Checked ownership-to-wire mapping. Primary stays `record=None` on every host.
    ///
    /// # Errors
    /// The record is not open here, or is not the one `checkout` names.
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

    /// The route component admission joins before the factory runs: the open record's, a live
    /// New reservation's, or an original creation whose genesis the factory has yet to write.
    ///
    /// # Errors
    /// As [`Self::route`].
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
            if set
                .open
                .get(&checkout.id)
                .is_some_and(|slot| slot.kind != checkout.kind)
            {
                return Err(ResourceOpenError::IdentityMismatch);
            }
            if let Some(doc) = set.peek(&checkout.id) {
                validate_identity(doc, checkout, &set.home)?;
            } else {
                return set.wire_route(&checkout.id);
            }
        }
        self.route(checkout)
    }

    /// Cheap content observation for retained projections over open records: base heads cover
    /// committed changes, `pending_ops` the live typing transaction, and the record identity makes
    /// opening or closing a document invalidate a snapshot.
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

    /// Whether the store can be read now: no caller holds it mutably.
    pub fn readable(&self) -> bool {
        self.0.try_borrow().is_ok()
    }

    /// The app and workspace this store's records belong to, which name their route.
    pub fn app_and_home(&self) -> (String, String) {
        let set = self.0.borrow();
        (set.app_id.to_string(), set.home.clone())
    }

    /// The workspace this store's records are opened in.
    pub fn home(&self) -> String {
        self.0.borrow().home.clone()
    }

    /// The resources this store holds open, and the chain of each part among them: the resources
    /// a window keeps.
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
    /// and those the workspace holds unopened. Listing asks the workspace, so a frame loop asks on
    /// [`Self::kept_revision`] instead of every frame.
    #[must_use]
    pub fn held_parts(&self, base: &DocumentId) -> Vec<u64> {
        let Ok(set) = self.0.try_borrow() else {
            return Vec::new();
        };
        let mut held: BTreeSet<u64> = set
            .open
            .keys()
            .chain(set.lent.iter())
            .filter_map(DocumentId::chain_part)
            .filter(|(chain, _)| chain == base)
            .map(|(_, index)| index)
            .collect();
        held.extend(
            set.listing()
                .iter()
                .filter_map(DocumentId::chain_part)
                .filter(|(chain, _)| chain == base)
                .map(|(_, index)| index),
        );
        held.into_iter().collect()
    }

    /// Tell the store that something arrived for what follows its records (a workspace's
    /// replication, delivering a part another device started), so what follows them looks again.
    /// Callable while a frame reads the store.
    pub fn note_kept(&self) {
        if let Ok(set) = self.0.try_borrow() {
            set.kept.set(set.kept.get().wrapping_add(1));
            set.answered.borrow_mut().clear();
        }
        self.wake_readers();
    }

    /// [`Self::merge_peer_document`] of the workspace's own copy of an open record, which holds
    /// `heads`: what it brought is the workspace's already, so the store's next write does not
    /// send it back. Whether anything changed.
    ///
    /// # Errors
    /// As [`Self::merge_peer_document`].
    pub fn merge_workspace_document(
        &self,
        checkout: &DocumentCheckout,
        document: Vec<u8>,
        heads: &[ChangeHash],
    ) -> Result<bool, String> {
        let changed = self.merge_peer_document(checkout, document)?;
        // What the workspace holds is no change of the record's: the generation stays.
        if let Ok(mut set) = self.0.try_borrow_mut()
            && let Some(slot) = set.open.get_mut(&checkout.id)
        {
            slot.workspace_holds(heads);
        }
        Ok(changed)
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
            set.answered.borrow_mut().clear();
        }
        self.wake_readers();
    }

    /// The numbers of the parts of the chain `base` peers wrote ([`Self::announce`]), in order.
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

    /// Moves whenever the store is told something arrived for what follows it:
    /// [`Self::note_kept`], [`Self::announce`], [`Self::note_workspace_answered`].
    #[must_use]
    pub fn kept_revision(&self) -> u64 {
        self.0.try_borrow().map_or(0, |set| set.kept.get())
    }

    /// The numbers of the parts of the chain `base` open here and ready to read, in order. Asks
    /// nothing, so it is cheap enough for every frame.
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

    /// Every record of `kind` this app keeps in this workspace, sorted by id, each with the name
    /// the workspace's index gives it: the window default records the app's windows started and
    /// the records the index names, so a document a window on another device made, or an agent
    /// made, is listed too. A record that does not read as one of `kind` is left out and logged.
    pub fn workspace_resources(
        &self,
        kind: &ResourceType,
    ) -> Vec<(DocumentId, AutoCommit, Option<String>)> {
        let Ok(set) = self.0.try_borrow() else {
            return Vec::new();
        };
        let held = set.whole_documents();
        let index = held
            .iter()
            .find(|(id, _)| id.is_package_primary())
            .map(|(_, doc)| doc);
        let named: BTreeMap<DocumentId, Option<String>> = index
            .map(|index| {
                index::discoverable_references(index)
                    .into_iter()
                    .filter(|checkout| &checkout.kind == kind)
                    .map(|checkout| {
                        let name = index::name_of(index, &checkout.id);
                        (checkout.id, name)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut found: Vec<(DocumentId, AutoCommit, Option<String>)> = held
            .iter()
            .filter(|(id, _)| window_default(id) || named.contains_key(id))
            .filter(|(id, doc)| {
                identity(doc).is_some_and(|found| &found.kind == kind && &found.id == id)
            })
            .map(|(id, doc)| (id.clone(), doc.clone(), named.get(id).cloned().flatten()))
            .collect();
        found.compact_sort_by(|a, b| a.0.cmp(&b.0));
        found
    }

    /// Every window default record of `kind` this app keeps in this workspace, sorted by id: what
    /// an app's windows started, so a closed window's document is reached again.
    pub fn window_resources(&self, kind: &ResourceType) -> Vec<(DocumentId, AutoCommit)> {
        self.workspace_resources(kind)
            .into_iter()
            .filter(|(id, _, _)| window_default(id))
            .map(|(id, doc, _)| (id, doc))
            .collect()
    }

    /// Open Existing only; a missing document is never implicitly initialized. In a store whose
    /// records come from its workspace ([`Self::ask_workspace_for_missing`]) a record not here is
    /// [`ResourceOpenError::Arriving`] until the workspace answered it holds none of it: only
    /// then is it [`ResourceOpenError::MissingResource`], which a caller may take as new.
    ///
    /// # Errors
    /// Reports contention, absence, identity refusal and a workspace that could not be read.
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
            // The port says the record is on its way: the workspace's hosts are asked for it
            // all the same, or nothing asks them to send it.
            Err(ResourceOpenError::Arriving) => {
                let _asked = self.awaits_workspace(want);
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

    /// Open `want` if this store holds it or its workspace does, without asking for one neither
    /// holds: what a list checks for each record it could name.
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

    /// Take this store's records to come from its workspace, which may send a device a record only
    /// once it asks for it: a record not here is not new until it said it holds none of it
    /// ([`Self::note_workspace_answered`]). Opening one asks for it ([`Self::wanted_resources`])
    /// and is [`ResourceOpenError::Arriving`] meanwhile; it opens once it arrives.
    pub fn ask_workspace_for_missing(&self) {
        if let Ok(set) = self.0.try_borrow() {
            set.asks_workspace.set(true);
        }
    }

    /// The records opened here that are not here yet and that the workspace has not answered:
    /// what to ask it for.
    #[must_use]
    pub fn wanted_resources(&self) -> Vec<DocumentId> {
        self.0
            .try_borrow()
            .map(|set| set.wanted.borrow().iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Reserve a New identity with its check and no document or history yet.
    ///
    /// # Errors
    /// Refuses a record open, reserved or held by the workspace already.
    pub fn reserve_resource(
        &self,
        want: &DocumentCheckout,
    ) -> Result<ResourceReservation, ResourceOpenError> {
        let mut set = self.try_set_mut().map_err(|_| ResourceOpenError::Busy)?;
        if !want.is_valid_local_storage_request() {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        if set.pending.contains_key(&want.id)
            || set.open.contains_key(&want.id)
            || set.read(want)?.is_some()
        {
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

    /// Whether the workspace keeps everything record `id` holds, without closing a typing
    /// transaction or sending anything.
    ///
    /// # Errors
    /// Reports missing or reserved records.
    pub fn save_status(&self, id: &DocumentId) -> Result<ResourceSaveStatus, ResourceOpenError> {
        let set = self.0.try_borrow().map_err(|_| ResourceOpenError::Busy)?;
        if set.pending.contains_key(id) {
            return Err(ResourceOpenError::Busy);
        }
        let slot = set.open.get(id).ok_or(ResourceOpenError::MissingResource)?;
        if set.port.is_none() {
            return Ok(ResourceSaveStatus::MemoryOnly);
        }
        if let Some(error) = &slot.failed {
            return Ok(ResourceSaveStatus::Failed(error.clone()));
        }
        // A window's routes publish what a store that does not write for itself changes, and
        // the window tells the store what the workspace took ([`Self::note_published`]).
        let doc = slot.doc.as_ref().ok_or(ResourceOpenError::Busy)?;
        Ok(if slot.writing.is_none() && slot.keeps_all(doc) {
            ResourceSaveStatus::Saved
        } else {
            ResourceSaveStatus::Saving
        })
    }

    /// The workspace took what a window published of record `id`, up to `heads`.
    pub fn note_published(&self, id: &DocumentId, heads: &[ChangeHash]) {
        self.settle_change(id, Ok(heads));
    }

    /// The workspace did not take what a window published of record `id`, for `reason`.
    pub fn note_unpublished(&self, id: &DocumentId, reason: String) {
        self.settle_change(id, Err(reason));
    }

    fn settle_change(&self, id: &DocumentId, answer: Result<&[ChangeHash], String>) {
        let Ok(mut set) = self.0.try_borrow_mut() else {
            return;
        };
        let Some(slot) = set.open.get_mut(id) else {
            return;
        };
        match answer {
            Ok(heads) => {
                slot.stored = Some(heads.to_vec());
                slot.failed = None;
            }
            Err(reason) => slot.failed = Some(reason),
        }
        for wake in slot.wakes.drain(..) {
            wake();
        }
    }

    /// Create exactly the new resource named by its owning action.
    ///
    /// # Errors
    /// Refuses existing identities and invalid scope.
    pub fn create_resource(&self, want: &DocumentCheckout) -> Result<(), ResourceOpenError> {
        self.reserve_resource(want)
            .and_then(|mut reservation| reservation.initialize(|_| Ok(())))
            .map_err(|error| match error {
                ResourceOpenError::Busy => ResourceOpenError::AlreadyExists,
                other => other,
            })
    }

    /// Existing-only admission for presentation owners: reads the workspace's document without
    /// seeding missing content.
    ///
    /// # Errors
    /// The record is neither open here nor held by the workspace, or is not the one `want` names.
    pub fn open_existing_resource_checked(
        &self,
        want: &DocumentCheckout,
    ) -> Result<(), ResourceOpenError> {
        let mut set = self.try_set_mut().map_err(|_| ResourceOpenError::Busy)?;
        if !set.open_from_workspace(want)? {
            return Err(ResourceOpenError::MissingResource);
        }
        let slot = set
            .open
            .get(&want.id)
            .ok_or(ResourceOpenError::MissingResource)?;
        let doc = slot.doc.as_ref().ok_or(ResourceOpenError::Busy)?;
        validate_identity(doc, want, &set.home)
    }

    /// Open `want` if it is not open already. Returns whether this opened it.
    pub fn open_resource(&self, want: &DocumentCheckout) -> bool {
        self.set_mut().open_resource(want)
    }

    /// Exact runtime owner identity; namespace equality is insufficient for reentry.
    pub fn same_owner(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    /// What admits a later write to `checkout` as the record this store opened.
    ///
    /// # Errors
    /// The record is not open here, or is not the one `checkout` names.
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
            // A package-primary document carries no minted `id`/`kind` in its metadata, so it is
            // admitted by the same rule that opened it.
            validate_identity(doc, checkout, &set.home)?;
        } else if identity(doc).as_ref() != Some(checkout) {
            return Err(ResourceOpenError::IdentityMismatch);
        }
        Ok(ResourceAdmission {
            checkout: checkout.clone(),
            workspace: set.home.clone(),
        })
    }

    /// Change the open record `id`, outside any action frame.
    ///
    /// # Errors
    /// The record is lent, busy or not open.
    pub fn with_checked<R>(
        &self,
        id: &DocumentId,
        f: impl FnOnce(&mut AutoCommit) -> R,
    ) -> Result<R, ResourceOpenError> {
        // A checked mutable loan is external to any action frame, exactly as `document_mut`:
        // close the open frame and advance its generation so an operation resolved before this
        // loan is invalidated.
        self.with_action_state(id, |document, history, actions| {
            actions.finish_external(document, history);
            actions.generation = actions
                .generation
                .and_then(|generation| generation.checked_add(1));
            f(document)
        })
    }

    /// Read the open record `id`.
    ///
    /// # Errors
    /// The record is lent, busy or not open; [`ResourceOpenError::Arriving`] while the workspace
    /// owes it.
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

    /// [`Self::read_checked`] with the reading erased: one body for every reader.
    #[inline(never)]
    fn read_checked_with(
        &self,
        id: &DocumentId,
        read: &mut dyn FnMut(&AutoCommit),
    ) -> Result<(), ResourceOpenError> {
        let set = self.0.try_borrow().map_err(|_| ResourceOpenError::Busy)?;
        set.not_lent(id)?;
        let Some(document) = set.open.get(id).and_then(|parked| parked.doc.as_ref()) else {
            // A record opening off the frame, or asked of the workspace and not answered yet, is
            // on its way, not missing.
            let owed = set.opening.borrow().contains_key(id)
                || (set.wanted.borrow().contains(id) && !set.answered.borrow().contains_key(id));
            return Err(if owed {
                ResourceOpenError::Arriving
            } else {
                ResourceOpenError::MissingResource
            });
        };
        // A component's pass that reads it reads it again once it moved.
        observed_read(self, id, document);
        read(document);
        Ok(())
    }

    /// What tells this store from another holding the same records.
    pub fn key(&self) -> usize {
        Rc::as_ptr(&self.0).cast::<()>() as usize
    }

    /// One exclusive record admission for the document and owned action frame.
    ///
    /// The record's document, history and action state are lent out of the set for the frame, so
    /// what the frame does may open, read and change the set's other records; the record itself
    /// answers [`ResourceOpenError::Busy`] to anyone else until the frame returns it.
    ///
    /// # Errors
    /// The record is lent, busy or not open.
    pub fn with_action_state<R>(
        &self,
        id: &DocumentId,
        callback: impl FnOnce(
            &mut AutoCommit,
            &mut tilo_resource_store::history::UndoLog,
            &mut ResourceActions,
        ) -> R,
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

    /// The writers this replica has written the open record `id` as, for a route that publishes
    /// this replica's own changes and nobody else's ([`crate::state::DocumentFrontier::delta`]).
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

    /// Generation of the record-owned action frame, for retained callers that need to invalidate a
    /// previously resolved operation.
    ///
    /// # Errors
    /// `Busy` if the record set is borrowed, or `MissingResource` if the record is not open.
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
    ///
    /// # Errors
    /// The record is not open here or does not read as itself.
    pub fn replica_identity_stamp(
        &self,
        checkout: &DocumentCheckout,
    ) -> Result<ReplicaIdentityStamp, String> {
        let workspace = self.home();
        self.read_checked(&checkout.id, |doc| {
            ReplicaIdentityStamp::capture(doc, checkout, &workspace)
        })
        .map_err(|error| error.to_string())?
    }

    /// Withdraw from the open record `id` the changes `refused` names, which the workspace
    /// refused, with every local change made on top of them, and note one [`ResourceRefusal`] per
    /// action they carried, saying `reason` (for a person), for the app to take
    /// ([`Self::take_refusals`]).
    ///
    /// A refused change is shared state nowhere but here, and a peer holds back every change that
    /// depends on one it never receives: kept, it would show as sent forever and silence
    /// everything written after it. The document becomes what it was without them; local undo
    /// starts over, since its steps may name withdrawn changes. A replication frontier past them
    /// must be [`rebased`] onto the heads this returns. Returns `None` when nothing was withdrawn:
    /// the record holds none of them, or they are its creation (still noted).
    ///
    /// [`rebased`]: crate::state::DocumentFrontier::rebase
    ///
    /// # Errors
    /// The record is not open here or its owner is busy, or the document does not rebuild.
    pub fn withdraw_refused(
        &self,
        id: &DocumentId,
        refused: &[ChangeHash],
        reason: &str,
    ) -> Result<Option<Vec<ChangeHash>>, String> {
        self.with_action_state(id, |doc, history, action_state| {
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
                // The record's own creation was refused: it stays as this device made it, a
                // record nobody else holds, rather than one with no identity. Nothing said of it
                // holds.
                action_state.notices.held.clear();
                return Ok(None);
            }
            // The withdrawn changes left this copy but not the workspace: its relay holds them
            // under this copy's writer and the writers derived from it, numbered. Written on under
            // those writers, the rebuilt document would number its next changes the same again,
            // and a record holding two changes under one writer and sequence reads whole nowhere.
            // So the copy writes under a writer of its own from here.
            let before = doc.get_actor().clone();
            let device = tilo_resource_store::history::authors::DeviceActors::of(&before);
            {
                let mut writers = action_state.writers.borrow_mut();
                writers.note(before.clone());
                for change in doc.get_changes_meta(&[]) {
                    if device.wrote(&change.actor.to_hex_string()) {
                        writers.note(change.actor.into_owned());
                    }
                }
            }
            let mut rebuilt = doc
                .fork_at(&kept_heads)
                .map_err(|error| error.to_string())?;
            rebuilt.set_actor(automerge::ActorId::random());
            history.restart(&mut rebuilt);
            action_state.generation = action_state
                .generation
                .and_then(|value| value.checked_add(1));
            *doc = rebuilt;
            action_state.notices.withdrawn(doc);
            Ok(Some(doc.get_heads()))
        })
        .map_err(|error| error.to_string())?
    }

    /// Say `text` of what was just done to the open record `id` once the workspace it is shared in
    /// keeps it (`WorkspaceNotices`): at once when no host replicates it, never when the workspace
    /// refuses it. The pending action is committed first, so the change it names is among the
    /// heads held.
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

    /// The workspace's host answered every commit of record `id` it published: what was held for
    /// them is said ([`Self::take_said`]). Whether there is now something to say.
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

    /// The refusals of record `id` noted since the last call ([`Self::withdraw_refused`]).
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

    /// Whether the store takes a write now: not while something reads or changes it (a frame's
    /// read of a record holds it).
    #[must_use]
    pub fn takes_writes(&self) -> bool {
        self.0.try_borrow_mut().is_ok()
    }

    pub fn read<R>(&self, id: &DocumentId, f: impl FnOnce(&AutoCommit) -> R) -> Option<R> {
        self.read_checked(id, f).ok()
    }

    /// Do something with a record's document *and* its history — commit an action, undo one.
    pub fn with_history<R>(
        &self,
        id: &DocumentId,
        f: impl FnOnce(&mut AutoCommit, &mut tilo_resource_store::history::UndoLog) -> R,
    ) -> Option<R> {
        let mut set = self.try_set_mut().ok()?;
        let (doc, history) = set.document_and_history_mut(id)?;
        Some(f(doc, history))
    }

    /// Inspect committed history without turning a pending burst into a boundary.
    pub fn read_history<R>(
        &self,
        id: &DocumentId,
        read: impl FnOnce(&AutoCommit, &tilo_resource_store::history::UndoLog) -> R,
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

    /// The open records that belong to another's tree ([`DocumentId::tree`]), ready to read; none
    /// while a frame holds the store.
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

    /// Send what changed in the open records to the workspace, when this store writes for itself.
    /// Once per idle tick; cheap when nothing changed.
    pub fn tick(&self) {
        if let Ok(mut set) = self.try_set_mut() {
            set.tick();
        }
    }

    /// Send what changed in the open records and wait until the workspace took it.
    ///
    /// # Errors
    /// Each record the workspace did not take, by record.
    pub fn flush(&self) -> Result<(), String> {
        self.set_mut().flush()
    }
}

impl ResourceStore {
    /// [`Self::flush`]: what the open records changed, sent and taken.
    ///
    /// # Errors
    /// As [`Self::flush`].
    pub fn try_flush(&self) -> Result<(), String> {
        // In a store whose window publishes its changes, trying again is the window's resend.
        if !self.set_mut().ask_resend() {
            return Ok(());
        }
        self.flush()
    }

    /// Whether the person asked to send record `id` again since this was last asked: its window
    /// publishes it whole once more, and the workspace's answer settles it
    /// ([`Self::note_published`]).
    #[must_use]
    pub fn take_resend(&self, id: &DocumentId) -> bool {
        // Asked every pass: it changes no record, so it moves no generation.
        self.0
            .try_borrow_mut()
            .ok()
            .and_then(|mut set| {
                set.open
                    .get_mut(id)
                    .map(|slot| std::mem::take(&mut slot.resend))
            })
            .unwrap_or(false)
    }

    /// [`ResourceSet::settling`] for this store's records.
    ///
    /// # Errors
    /// As [`ResourceSet::settling`].
    pub fn settling(&self) -> Result<bool, String> {
        // Lent to the frame: still settling, asked again next pass.
        self.0
            .try_borrow_mut()
            .map_or(Ok(true), |mut set| set.settling())
    }

    /// [`Self::flush`] for one record's owner; the other records are sent too, as they are cheap
    /// when unchanged.
    ///
    /// # Errors
    /// As [`Self::flush`].
    pub fn try_flush_resource(&self, _id: &DocumentId) -> Result<(), String> {
        self.try_flush()
    }

    /// Send what is not in the workspace yet without waiting for it, and report only what the
    /// workspace already refused.
    ///
    /// # Errors
    /// Each record whose last write the workspace refused.
    pub fn try_flush_written(&self) -> Result<(), String> {
        let mut set = self
            .try_set_mut()
            .map_err(|_| ResourceOpenError::Busy.to_string())?;
        set.tick();
        let refused: Vec<String> = set
            .open
            .iter()
            .filter_map(|(id, slot)| Some(format!("{id}: {}", slot.failed.as_ref()?)))
            .collect();
        if refused.is_empty() {
            Ok(())
        } else {
            Err(refused.join("; "))
        }
    }

    /// Send the result of a history step on record `id`, which is committed already.
    ///
    /// # Errors
    /// The store is busy and took nothing.
    pub fn schedule_committed_history(&self, _id: &DocumentId) -> Result<(), String> {
        self.try_set_mut()
            .map_err(|_| ResourceOpenError::Busy.to_string())?
            .tick();
        Ok(())
    }

    /// Wait until the workspace took what record `id` holds.
    ///
    /// # Errors
    /// The record is not open, or the workspace refused or did not answer it.
    pub fn await_submitted(&self, id: &DocumentId) -> Result<(), String> {
        self.save_status(id).map_err(|error| error.to_string())?;
        self.flush()
    }

    /// Why the workspace did not take record `id`'s last write, if it did not.
    #[must_use]
    pub fn durability_error(&self, id: &DocumentId) -> Option<String> {
        self.0
            .try_borrow()
            .ok()?
            .open
            .get(id)
            .and_then(|slot| slot.failed.clone())
    }

    /// [`Self::try_open_resource`] for a record a window holds beside the one it shows.
    ///
    /// # Errors
    /// As [`Self::try_open_resource`].
    pub fn try_open_beside(
        &self,
        want: &DocumentCheckout,
    ) -> Result<ResourceOpenOutcome, ResourceOpenError> {
        self.try_open_resource(want)
    }

    /// Where this store reads its records from, `None` for one in memory alone.
    #[must_use]
    pub fn backend(&self) -> Option<Rc<dyn DocumentPort>> {
        self.0.try_borrow().ok()?.port.clone()
    }

    /// Call `wake` once record `id`'s write under way is answered, or, in a store whose window
    /// publishes its records, once the workspace answered what the window published of it.
    pub fn wake_on_save_progress(&self, id: &DocumentId, wake: ReadyWake) {
        if let Ok(mut set) = self.0.try_borrow_mut() {
            let published = !set.writes;
            if let Some(slot) = set.open.get_mut(id) {
                let unanswered = slot.failed.is_none()
                    && slot.doc.as_ref().is_some_and(|doc| !slot.keeps_all(doc));
                if slot.writing.is_some() || (published && unanswered) {
                    slot.wakes.push(wake);
                } else {
                    wake();
                }
            }
        }
    }

    /// Send what record `id` changed to the workspace, where this store writes its records itself.
    ///
    /// # Errors
    /// The store is busy now.
    pub fn offer_resource(&self, _id: &DocumentId) -> Result<(), String> {
        self.try_set_mut()
            .map_err(|_| ResourceOpenError::Busy.to_string())?
            .tick();
        Ok(())
    }

    /// Where the store keeps its records, the app and the workspace; `None` for a store in memory
    /// alone, whose records belong to no workspace.
    #[must_use]
    pub fn canonical_namespace(&self) -> Option<(String, String, String)> {
        let set = self.0.try_borrow().ok()?;
        set.port.as_ref()?;
        Some((
            format!("workspace:{}", set.home),
            set.app_id.to_string(),
            set.home.clone(),
        ))
    }

    /// The identity History keys record `id`'s steps by: where the store keeps it (`None` in
    /// memory), the app and the workspace.
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub fn history_namespace(
        &self,
        id: &DocumentId,
    ) -> Result<(Option<String>, String, String), ResourceOpenError> {
        let set = self.0.try_borrow().map_err(|_| ResourceOpenError::Busy)?;
        set.open.get(id).ok_or(ResourceOpenError::MissingResource)?;
        let kept = set.port.as_ref().map(|_| format!("workspace:{}", set.home));
        Ok((kept, set.app_id.to_string(), set.home.clone()))
    }
}

impl ResourceSet {
    /// Every document of this store's app in its workspace, whole, as the workspace holds them,
    /// the open ones as they stand here.
    fn whole_documents(&self) -> Vec<(DocumentId, AutoCommit)> {
        let mut found: BTreeMap<DocumentId, AutoCommit> = self
            .open
            .iter()
            .filter_map(|(id, parked)| Some((id.clone(), parked.doc.clone()?)))
            .collect();
        let (Some(port), Ok(cluster)) = (&self.port, self.cluster()) else {
            return found.into_iter().collect();
        };
        let workspace = cluster.to_string();
        // What does not fit one answer comes as `Later`, and is asked for again.
        let mut asks: Vec<DocumentAsk> = self
            .listing()
            .into_iter()
            .filter(|id| !found.contains_key(id))
            .map(|id| DocumentAsk {
                id: id.route().to_owned(),
                since: None,
            })
            .collect();
        while !asks.is_empty() {
            let read = match port.read(self.app_id.as_str(), &workspace, Some(asks)) {
                // A record on its way is left out until it comes, and the readers are woken then.
                Ok(read) if read.iter().any(|(_, read)| *read == DocumentRead::Pending) => {
                    let _woken = self.await_port();
                    read
                }
                Ok(read) => read,
                Err(error) => {
                    tracing::warn!(app = %self.app_id, workspace = %self.home, %error, "the workspace's records were not read");
                    break;
                }
            };
            let answered = read.len();
            asks = Vec::new();
            for (route, read) in read {
                let Some(id) = DocumentId::from_route(&route) else {
                    continue;
                };
                let loaded = match read {
                    DocumentRead::Whole { bytes, hint, .. } => {
                        tilo_automerge::shared::load_hinted(&bytes, hint.as_deref())
                    }
                    DocumentRead::Later => {
                        asks.push(DocumentAsk {
                            id: route,
                            since: None,
                        });
                        continue;
                    }
                    _ => continue,
                };
                match loaded {
                    Ok(doc) => {
                        found.insert(id, doc);
                    }
                    Err(error) => {
                        tracing::warn!(record = %id, %error, "a workspace's record does not read; left out");
                    }
                }
            }
            // An answer that carried none of what it was asked cannot be asked again.
            if asks.len() == answered {
                tracing::warn!(app = %self.app_id, workspace = %self.home, "the workspace answered none of the records asked of it");
                break;
            }
        }
        found.into_iter().collect()
    }

    /// The ids of the documents of this store's app the workspace holds, and those open here.
    fn listing(&self) -> Vec<DocumentId> {
        let mut ids: BTreeSet<DocumentId> = self
            .open
            .iter()
            .filter(|(_, parked)| parked.doc.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        let (Some(port), Ok(cluster)) = (&self.port, self.cluster()) else {
            return ids.into_iter().collect();
        };
        let listed = port.read(self.app_id.as_str(), &cluster.to_string(), None);
        // A listing on its way answers what was listed last; the readers are woken once it comes.
        let _woken = self.await_port();
        match listed {
            Ok(read) => ids.extend(
                read.into_iter()
                    .filter_map(|(route, _)| DocumentId::from_route(&route)),
            ),
            Err(error) => {
                tracing::warn!(app = %self.app_id, workspace = %self.home, %error, "the workspace's records were not listed");
            }
        }
        ids.into_iter().collect()
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
    static RESOURCE_LOANS: RefCell<Vec<LoanIdentity>> = const { RefCell::new(Vec::new()) };
}
struct LoanGuard;
impl Drop for LoanGuard {
    fn drop(&mut self) {
        RESOURCE_LOANS.with_borrow_mut(|loans| {
            loans.pop();
        });
    }
}

/// Call `callback` on `document`, resource `record` of `store`, lent to it: what it checks with
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

#[cfg(test)]
mod tests;
