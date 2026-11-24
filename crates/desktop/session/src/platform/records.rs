//! What a role reads of the workspaces' records, and what changed in them.
//!
//! The workspaces' records are written by their apps, by the person's other devices and by this
//! session's own services. Whoever writes one notes it in the one [`Changes`] stream of this host,
//! and the roles that follow records — the activity, the person's mentions and search — read that
//! stream and keep no watcher of their own. On a Linux desktop the records are this device's
//! workspace service's ([`Held`]), and the record index notes what the broker receives from the
//! apps and what the session writes; on the web the page's replication notes what it takes in
//! ([`Kept`]).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};

use automerge::AutoCommit;
use durable_store::{DurableStore, StorageKey};
use ui::resource::DocumentCheckout;

use super::Store;

/// Ceiling on an index document a role reads.
const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;

/// A record a change reached: whose app keeps it, in which workspace, and its route
/// ([`ui::resource::DocumentId::route`]); [`storage_key`] is where a reader finds it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Changed {
    pub workspace: String,
    pub app: String,
    /// The record's route; empty for the app's own record of the workspace, the document that
    /// also keeps its list of the app's records.
    pub record: String,
    /// What reached it is what this device already held — the workspace replaying its records
    /// before any role followed them — rather than a live change: no news.
    pub replayed: bool,
}

/// Whether this device holds a workspace, as the stream is told: a change of one it does not hold
/// is nothing any role following the stream can read.
type Holds = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// The one stream of record changes: every write that lands in the workspaces' records is noted
/// here, and every role that follows records takes it.
#[derive(Clone, Default)]
pub struct Changes(Arc<Inner>);

#[derive(Default)]
struct Inner {
    /// The records a change reached since this was last taken, each with whether what reached it
    /// is what the device already held.
    reached: Mutex<BTreeMap<(String, String, String), bool>>,
    /// How often the records of each workspace moved.
    moved_in: Mutex<BTreeMap<String, u64>>,
    /// How often any record moved.
    moved: AtomicU64,
    /// What wakes the session's loop when a record changed, so the pass reads the queue then.
    wake: Mutex<Option<tilo_service::Wake>>,
    /// The workers following the records, told each change as it is noted.
    readers: Mutex<Vec<mpsc::Sender<Changed>>>,
    /// Whether this device holds a workspace, where the session knows: what [`Changes::holding`]
    /// was told.
    holds: Mutex<Option<Holds>>,
    /// What `holds` answered for each workspace, so a change of one asks it nothing until the keys
    /// settle.
    answered: Mutex<BTreeMap<String, bool>>,
}

impl Changes {
    /// Note only what `holds` holds: a change of a workspace this device's service does not hold
    /// is no news here, and is taken when the workspace becomes held.
    pub fn holding(&self, holds: Holds) {
        *self.0.holds.lock().unwrap_or_else(PoisonError::into_inner) = Some(holds);
        self.0
            .answered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// The keys settled: a workspace this device held nothing of may be held now, so what
    /// [`Self::holding`] answered of one it does not hold is asked again.
    pub fn settling(&self) {
        self.0
            .answered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|_, held| *held);
    }

    /// Whether this device holds `workspace`, as [`Self::holding`] says: true where the session
    /// told the stream nothing, as a page's does, which notes only what it keeps.
    fn holds(&self, workspace: &str) -> bool {
        let asked = self
            .0
            .holds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(asked) = asked else {
            return true;
        };
        if let Some(held) = self
            .0
            .answered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workspace)
        {
            return *held;
        }
        let held = asked(workspace);
        self.0
            .answered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(workspace.to_owned(), held);
        held
    }

    /// Note that `record` of `app` changed in `workspace` (an empty route: the workspace's list
    /// of `app`'s moved), as `replayed` where it is what this device already held, and wake what
    /// follows it. Whether it is news here: a change of a workspace this device does not hold is
    /// not, and nothing that follows the stream is told of it.
    pub fn note(&self, workspace: &str, app: &str, record: &str, replayed: bool) -> bool {
        if app.is_empty() || !self.holds(workspace) {
            return false;
        }
        // A record noted twice before it is read is read once, and a live change of it is news even
        // where the replay reached it first.
        let route = (workspace.to_owned(), app.to_owned(), record.to_owned());
        let mut reached = self
            .0
            .reached
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let held = reached.entry(route).or_insert(replayed);
        *held &= replayed;
        drop(reached);
        let mut moved_in = self
            .0
            .moved_in
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let moved = moved_in.entry(workspace.to_owned()).or_default();
        *moved = moved.saturating_add(1);
        drop(moved_in);
        self.0.moved.fetch_add(1, Ordering::AcqRel);
        let changed = Changed {
            workspace: workspace.to_owned(),
            app: app.to_owned(),
            record: record.to_owned(),
            replayed,
        };
        self.0
            .readers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|reader| reader.send(changed.clone()).is_ok());
        if let Some(wake) = self
            .0
            .wake
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            wake();
        }
        true
    }

    /// Wake the session's loop through `wake` whenever a record changed.
    pub fn waking(&self, wake: tilo_service::Wake) {
        *self.0.wake.lock().unwrap_or_else(PoisonError::into_inner) = Some(wake);
    }

    /// What reached the records since this was last taken.
    pub fn take(&self) -> Vec<Changed> {
        std::mem::take(
            &mut *self
                .0
                .reached
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
        .into_iter()
        .map(|((workspace, app, record), replayed)| Changed {
            workspace,
            app,
            record,
            replayed,
        })
        .collect()
    }

    /// How often the records of `workspace` moved.
    pub fn moved_in(&self, workspace: &str) -> u64 {
        self.0
            .moved_in
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workspace)
            .copied()
            .unwrap_or_default()
    }

    /// How often any record moved.
    pub fn moved(&self) -> u64 {
        self.0.moved.load(Ordering::Acquire)
    }

    /// A receiver told every change noted from now on: what a worker following the records waits
    /// on.
    #[cfg(target_os = "linux")]
    pub fn reader(&self) -> mpsc::Receiver<Changed> {
        let (told, reader) = mpsc::channel();
        self.0
            .readers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(told);
        reader
    }
}

/// What a value read of the records follows: the records of one workspace, or every record.
#[derive(Clone, Debug)]
pub enum Watch {
    /// The records of this workspace, by its id.
    Workspace(String),
    /// Every record of every workspace: a value read across the device's records.
    Any,
}

impl Watch {
    /// Whether a change noted in `workspace` is one this watch follows: what the worker that reads
    /// it again listens for.
    #[cfg(target_os = "linux")]
    fn follows(&self, workspace: &str) -> bool {
        match self {
            Self::Workspace(watched) => watched == workspace,
            Self::Any => true,
        }
    }

    /// How often what this watch follows moved.
    fn moved(&self, changes: &Changes) -> u64 {
        match self {
            Self::Workspace(workspace) => changes.moved_in(workspace),
            Self::Any => changes.moved(),
        }
    }
}

/// A value read of the workspaces' records, kept current by the change stream: read when it is
/// started, and again whenever what it follows moves — on a worker of its own where this host has
/// threads, so a read never holds up a frame, and on the one thread the page has where it has one.
pub struct Reading<T> {
    #[cfg(target_os = "linux")]
    held: Arc<Mutex<(u64, Arc<T>)>>,
    #[cfg(target_os = "linux")]
    landed: Arc<AtomicU64>,
    #[cfg(not(target_os = "linux"))]
    held: std::cell::RefCell<(u64, Arc<T>)>,
    #[cfg(not(target_os = "linux"))]
    read: Box<dyn Fn() -> T>,
}

#[cfg(target_os = "linux")]
impl<T: Send + Sync + 'static> Reading<T> {
    /// Read `read` now, and again on a worker whenever `changes` says what `watch` follows moved,
    /// telling `moved` so the frame that shows it is drawn.
    pub fn start(
        name: &str,
        changes: Changes,
        watch: Watch,
        read: impl Fn() -> T + Send + Sync + 'static,
        moved: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let at = watch.moved(&changes);
        let held = Arc::new(Mutex::new((at, Arc::new(read()))));
        let landed = Arc::new(AtomicU64::new(1));
        let reader = changes.reader();
        let (shared, counted) = (Arc::clone(&held), Arc::clone(&landed));
        let worker = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                // Ends when the stream does: the session is gone.
                while let Ok(changed) = reader.recv() {
                    // A burst of changes is read once.
                    let mut mine = watch.follows(&changed.workspace);
                    while let Ok(other) = reader.try_recv() {
                        mine |= watch.follows(&other.workspace);
                    }
                    if !mine {
                        continue;
                    }
                    let at = watch.moved(&changes);
                    let fresh = Arc::new((read)());
                    *shared.lock().unwrap_or_else(PoisonError::into_inner) = (at, fresh);
                    counted.fetch_add(1, Ordering::AcqRel);
                    moved();
                }
            });
        if let Err(error) = worker {
            tracing::warn!(%error, "a value read of the records is not followed");
        }
        Self { held, landed }
    }

    /// The value as this host last read it: the worker keeps it current.
    pub fn get(&self) -> Arc<T> {
        Arc::clone(&self.held.lock().unwrap_or_else(PoisonError::into_inner).1)
    }

    /// [`Self::get`] for a caller that also reads a thread with one: the same value here.
    pub fn following(&self, _changes: &Changes, _watch: &Watch) -> Arc<T> {
        self.get()
    }

    /// How many reads landed: what a caller hashes to hear that a read changed the value.
    pub fn reads(&self) -> u64 {
        self.landed.load(Ordering::Acquire)
    }
}

#[cfg(not(target_os = "linux"))]
impl<T: 'static> Reading<T> {
    /// Read `read` now: this host has one thread, and reads the records on it.
    pub fn read(changes: &Changes, watch: &Watch, read: impl Fn() -> T + 'static) -> Self {
        Self {
            held: std::cell::RefCell::new((watch.moved(changes), Arc::new(read()))),
            read: Box::new(read),
        }
    }

    /// The value, read again where the stream says what `watch` follows moved: this host has one
    /// thread, and nothing may hold it.
    pub fn following(&self, changes: &Changes, watch: &Watch) -> Arc<T> {
        let at = watch.moved(changes);
        let mut held = self.held.borrow_mut();
        if held.0 != at {
            held.0 = at;
            held.1 = Arc::new((self.read)());
        }
        Arc::clone(&held.1)
    }

    /// The value as last read.
    pub fn get(&self) -> Arc<T> {
        Arc::clone(&self.held.borrow().1)
    }

    /// How often what `watch` follows moved when this was last read.
    pub fn reads(&self) -> u64 {
        self.held.borrow().0
    }
}

/// What changed of a record since the heads a reader holds it at, as a host reads it without the
/// record whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Since {
    /// It stands where the reader last read it.
    Unchanged,
    /// These encoded changes, after which it stands at `heads`.
    Changes {
        heads: Vec<automerge::ChangeHash>,
        bytes: Vec<u8>,
    },
    /// Only the record whole answers: the host does not hold the reader's heads, or reads no
    /// changes apart.
    Whole,
}

/// What a host reads of its records.
pub trait Records {
    /// The records a change reached since this was last taken.
    fn take_changed(&self) -> Vec<Changed>;
    /// The document of `app`'s `record` (a route) in `workspace`, as this host holds it now.
    fn document(&self, app: &str, workspace: &str, record: &str) -> Option<AutoCommit>;
    /// What changed of `app`'s `record` in `workspace` since `heads`, read without the record
    /// whole where this host can.
    fn since(
        &self,
        app: &str,
        workspace: &str,
        record: &str,
        heads: &[automerge::ChangeHash],
    ) -> Since {
        let _ = (app, workspace, record, heads);
        Since::Whole
    }
    /// The heads `app`'s `record` in `workspace` stands at here, where this host knows them
    /// without reading the record.
    fn heads(
        &self,
        app: &str,
        workspace: &str,
        record: &str,
    ) -> Option<Vec<automerge::ChangeHash>> {
        let _ = (app, workspace, record);
        None
    }
    /// The kind of what `app` keeps in `workspace` (`notes.notebook`), empty when unknown: what
    /// the workspace's index names first.
    fn kind(&self, app: &str, workspace: &str) -> String {
        self.document(app, workspace, "")
            .and_then(|index| {
                ui::resource::index::discoverable_references(&index)
                    .into_iter()
                    .next()
            })
            .map(|checkout| checkout.kind.as_str().to_owned())
            .unwrap_or_default()
    }
    /// Everything `workspace` holds here, by app and route; the empty route is the app's own
    /// document of the workspace, its index.
    fn held(&self, workspace: &str) -> Vec<(String, String)> {
        let _ = workspace;
        Vec::new()
    }
    /// The workspaces this host holds records of.
    fn workspaces(&self) -> Vec<String> {
        Vec::new()
    }
}

/// What a role that reads on a worker of its own reads the workspaces through.
#[cfg(target_os = "linux")]
pub type Source = Arc<dyn Records + Send + Sync>;
/// What a role reads the workspaces through, where the session has one thread.
#[cfg(not(target_os = "linux"))]
pub type Source = std::rc::Rc<dyn Records>;

/// What a store keeps, as a page's replication writes it, with the changes noted as they were
/// written.
pub struct Kept {
    store: Store,
    changes: Changes,
}

impl Kept {
    /// What `store` keeps, with `changes` saying what moved in it.
    #[must_use]
    pub fn over(store: Store, changes: Changes) -> Self {
        Self { store, changes }
    }
}

impl Records for Kept {
    fn take_changed(&self) -> Vec<Changed> {
        self.changes.take()
    }

    fn document(&self, app: &str, workspace: &str, route: &str) -> Option<AutoCommit> {
        document(self.store.as_ref(), app, workspace, route)
    }

    /// The apps' own documents of `workspace`, and what their indexes name.
    fn held(&self, workspace: &str) -> Vec<(String, String)> {
        let indexes = self
            .store
            .workspace_indexes()
            .inspect_err(|error| tracing::warn!(%error, "the workspace indexes are not listed"))
            .unwrap_or_default();
        indexes
            .into_iter()
            .filter(|(_, held)| held == workspace)
            .flat_map(|(app, _)| {
                let named = named_checkouts(self.store.as_ref(), &app, workspace)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(checkout, _)| checkout.id.route().to_owned());
                std::iter::once(String::new())
                    .chain(named)
                    .map(move |route| (app.clone(), route))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn workspaces(&self) -> Vec<String> {
        let mut workspaces: Vec<String> = self
            .store
            .workspace_indexes()
            .inspect_err(|error| tracing::warn!(%error, "the workspace indexes are not listed"))
            .unwrap_or_default()
            .into_iter()
            .map(|(_, workspace)| workspace)
            .collect();
        workspaces.sort();
        workspaces.dedup();
        workspaces
    }
}

/// The checkouts the index of `app`'s records in `workspace` names, as its app wrote it, each with
/// what the index says it is called.
#[must_use]
fn named_checkouts(
    store: &dyn DurableStore,
    app: &str,
    workspace: &str,
) -> Option<Vec<(DocumentCheckout, Option<String>)>> {
    let index = index_document(store, app, workspace)?;
    Some(
        ui::resource::index::discoverable_references(&index)
            .into_iter()
            .map(|checkout| {
                let name = ui::resource::index::name_of(&index, &checkout.id);
                (checkout, name)
            })
            .collect(),
    )
}

/// `app`'s workspace document in `workspace`, as its app last wrote it.
fn index_document(store: &dyn DurableStore, app: &str, workspace: &str) -> Option<AutoCommit> {
    let stored = match store.read(&StorageKey::workspace_index(app, workspace)) {
        Ok(stored) => stored?,
        Err(error) => {
            tracing::warn!(%error, %app, %workspace, "a workspace's index is not read");
            return None;
        }
    };
    // Automerge cannot read a document in part, and a workspace index is a list of names rather
    // than the records' own content: a file past this is not one a workspace wrote.
    if stored.bytes.is_empty()
        || u64::try_from(stored.bytes.len()).unwrap_or(u64::MAX) > MAX_INDEX_BYTES
    {
        return None;
    }
    // An app keeping the index appends to it, and this read may land on a tail it has not finished
    // writing: the changes before it are the index as of the last write that completed.
    loaded::document(
        store,
        &StorageKey::workspace_index(app, workspace),
        &stored.bytes,
    )
    .inspect_err(
        |error| tracing::warn!(%error, %app, %workspace, "a workspace's index does not load"),
    )
    .ok()
}

/// Where `app` keeps the record of `workspace` whose route is `record`: the workspace document
/// for the empty route, the app's own record that also keeps its index, and the document named by
/// the record id's storage key for any other. `None` for a route that names no record.
#[must_use]
pub fn storage_key(app: &str, workspace: &str, record: &str) -> Option<StorageKey> {
    let id = ui::resource::DocumentId::from_route(record)?;
    Some(if id.is_package_primary() {
        StorageKey::workspace_index(app, workspace)
    } else {
        StorageKey::document(app, workspace, id.storage_key())
    })
}

/// The document `app` keeps for the record of `workspace` whose route is `record`, as its app last
/// committed it ([`storage_key`]).
#[must_use]
pub fn document(
    store: &dyn DurableStore,
    app: &str,
    workspace: &str,
    record: &str,
) -> Option<AutoCommit> {
    let key = storage_key(app, workspace, record)?;
    if matches!(key, StorageKey::WorkspaceIndex { .. }) {
        return index_document(store, app, workspace);
    }
    stored_document(store, &key)
}

/// The record document `app` keeps in `workspace` under the storage key `stored`
/// ([`durable_store::DurableStore::documents`]), as far as its writer finished writing it:
/// a document read without its id, as a part of a chain is listed.
pub fn stored(
    store: &dyn DurableStore,
    app: &str,
    workspace: &str,
    stored: &str,
) -> Option<AutoCommit> {
    stored_document(
        store,
        &StorageKey::document(app, workspace, stored.to_owned()),
    )
}

/// The record document kept at `key`, as far as its writer finished writing it.
fn stored_document(store: &dyn DurableStore, key: &StorageKey) -> Option<AutoCommit> {
    let stored = match store.read(key) {
        Ok(stored) => stored?,
        Err(error) => {
            tracing::warn!(%error, ?key, "a record is not read");
            return None;
        }
    };
    if stored.bytes.is_empty() {
        return None;
    }
    loaded::document(store, key, &stored.bytes)
        .inspect_err(|error| tracing::warn!(%error, ?key, "a record does not load"))
        .ok()
}

/// The documents this thread read from the stores, kept loaded.
///
/// A record's writers append to what is kept of it, so a document read again is mostly the one
/// read last with a few changes after it. Loading it whole each time costs the whole record for
/// every change — a tracker of thousands of issues reloaded for one comment, by every role that
/// follows the records — so the last load is kept and brought up to date with what was appended
/// since. A document whose kept bytes are not the ones read last followed by more (rewritten
/// whole, compacted) loads whole again.
mod loaded {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use automerge::{AutoCommit, AutomergeError, LoadOptions, OnPartialLoad};
    use durable_store::{DurableStore, StorageKey};

    /// Below this a document loads faster than it is checked against the one kept.
    const KEPT_FROM_BYTES: usize = 64 * 1024;
    /// What the documents kept on one thread may come to, by their bytes: past it the one read
    /// longest ago is let go.
    const KEPT_BYTES: usize = 256 * 1024 * 1024;

    struct Kept {
        /// The bytes it was loaded from: how many, and their digest.
        len: usize,
        digest: blake3::Hash,
        doc: AutoCommit,
        /// When it was last read, in reads of this thread.
        used: u64,
    }

    #[derive(Default)]
    struct Documents {
        kept: HashMap<(usize, StorageKey), Kept>,
        reads: u64,
    }

    thread_local! {
        static DOCUMENTS: RefCell<Documents> = RefCell::default();
    }

    /// The document `bytes`, what `store` keeps at `key`, as far as its writer finished writing
    /// it: brought up from the one read last when `bytes` are its bytes followed by more.
    pub(super) fn document(
        store: &dyn DurableStore,
        key: &StorageKey,
        bytes: &[u8],
    ) -> Result<AutoCommit, AutomergeError> {
        if bytes.len() < KEPT_FROM_BYTES {
            return whole(bytes);
        }
        // The store by its address: a store dropped and another made where it was is told apart by
        // what it keeps, which is checked below.
        let at = (std::ptr::from_ref(store).cast::<()>() as usize, key.clone());
        DOCUMENTS.with(|documents| {
            let mut documents = documents.borrow_mut();
            documents.reads = documents.reads.wrapping_add(1);
            let now = documents.reads;
            if let Some(kept) = documents.kept.get_mut(&at)
                && let Some(read) = brought_up(kept, bytes)
            {
                kept.used = now;
                return Ok(read);
            }
            documents.kept.remove(&at);
            // Kept only when it loads whole: a tail still being written is left out of what is
            // read, and the next read loads it again. A reader on this thread that holds the
            // document these bytes are (the workspace's replica, a window showing the record)
            // hands over a copy of it instead: by the heads the writer kept for exactly these
            // bytes first, so a document saved another way is still the same one.
            let mut doc = if let Some(doc) = held_elsewhere(store, key, bytes) {
                doc
            } else {
                tilo_automerge::shared::note_decoded();
                let Ok(doc) = AutoCommit::load_with_options(
                    bytes,
                    LoadOptions::new().on_partial_load(OnPartialLoad::Error),
                ) else {
                    return whole(bytes);
                };
                tilo_automerge::shared::hold(bytes, &doc);
                doc
            };
            if doc.get_missing_deps(&[]).is_empty() {
                documents.kept.insert(
                    at,
                    Kept {
                        len: bytes.len(),
                        digest: blake3::hash(bytes),
                        doc: doc.clone(),
                        used: now,
                    },
                );
                documents.let_go_past(KEPT_BYTES);
            }
            Ok(doc)
        })
    }

    /// The document `bytes` are, when a reader on this thread holds it: by the heads the writer
    /// kept beside exactly these bytes, else by the bytes themselves.
    fn held_elsewhere(
        store: &dyn DurableStore,
        key: &StorageKey,
        bytes: &[u8],
    ) -> Option<AutoCommit> {
        let at = store
            .read_hint(key)
            .as_deref()
            .and_then(|hint| tilo_automerge::load_hint::heads_for(hint, bytes))
            .and_then(|heads| tilo_automerge::shared::held_at(&heads));
        at.or_else(|| tilo_automerge::shared::held(bytes))
    }

    /// `kept` brought up to `bytes`, when they are its own bytes followed by more.
    fn brought_up(kept: &mut Kept, bytes: &[u8]) -> Option<AutoCommit> {
        let before = bytes.get(..kept.len)?;
        if blake3::hash(before) != kept.digest {
            return None;
        }
        if let Some(after) = bytes.get(kept.len..).filter(|after| !after.is_empty()) {
            // A tail still being written does not load: the kept document stands as it was
            // and the next read tries again.
            // Read strictly: a load of it would leave a torn change out without saying so.
            let mut grown = kept.doc.clone();
            tilo_automerge::ApplyIncoming::load_incoming_whole(&mut grown, after).ok()?;
            if !grown.get_missing_deps(&[]).is_empty() {
                return None;
            }
            kept.doc = grown;
            kept.len = bytes.len();
            kept.digest = blake3::hash(bytes);
        }
        Some(kept.doc.clone())
    }

    impl Documents {
        /// Let the documents read longest ago go until what is kept comes to `bytes` at most.
        fn let_go_past(&mut self, bytes: usize) {
            loop {
                let total: usize = self.kept.values().map(|kept| kept.len).sum();
                if total <= bytes {
                    return;
                }
                let Some(oldest) = self
                    .kept
                    .iter()
                    .min_by_key(|(_, kept)| kept.used)
                    .map(|(at, _)| at.clone())
                else {
                    return;
                };
                self.kept.remove(&oldest);
            }
        }
    }

    /// `bytes` loaded whole, as far as their writer finished writing them.
    fn whole(bytes: &[u8]) -> Result<AutoCommit, AutomergeError> {
        AutoCommit::load_with_options(
            bytes,
            LoadOptions::new().on_partial_load(OnPartialLoad::Ignore),
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use automerge::transaction::Transactable as _;
        use automerge::{ObjType, ROOT, ReadDoc as _};
        use durable_store::WriteOp;

        fn loaded_whole() -> usize {
            tilo_automerge::shared::decoded()
        }

        fn write(store: &durable_store::MemoryStore, key: &StorageKey, op: WriteOp) {
            let lease = store.lease(key).expect("the key is this test's");
            store.write(key, op, None).expect("written");
            drop(lease);
        }

        fn read(store: &durable_store::MemoryStore, key: &StorageKey) -> AutoCommit {
            let bytes = store.read(key).unwrap().unwrap().bytes;
            document(store, key, &bytes).expect("it loads")
        }

        /// A large record its writer appends to is read as the last read and what was appended,
        /// never loaded whole again; one rewritten whole, or whose tail is still being written, is
        /// read as it is.
        #[test]
        fn an_appended_record_is_read_from_the_last_read_and_what_was_appended() {
            let store = durable_store::MemoryStore::new();
            let key = StorageKey::document("so.tilo.tasks", "workspace", String::from("record"));
            let mut doc = AutoCommit::new();
            for at in 0..4_000 {
                // Text that does not compress, as a tracker's issues' does not.
                let text = blake3::hash(format!("issue {at}").as_bytes())
                    .to_hex()
                    .to_string();
                doc.put(ROOT, format!("issue-{at}"), text).unwrap();
            }
            write(&store, &key, WriteOp::Replace(doc.save()));
            let before = loaded_whole();
            assert_eq!(read(&store, &key).length(ROOT), 4_000);
            assert_eq!(loaded_whole(), before + 1, "the first read loads it");

            for at in 0..20 {
                doc.put(ROOT, format!("comment-{at}"), "a comment").unwrap();
                write(&store, &key, WriteOp::Append(doc.save_incremental()));
                let mut read = read(&store, &key);
                assert_eq!(read.length(ROOT), 4_001 + at);
                assert_eq!(read.get_heads(), doc.get_heads());
            }
            assert_eq!(
                loaded_whole(),
                before + 1,
                "appends are read from the last read"
            );

            // A tail still being written is left out, and read once it is whole.
            doc.put(ROOT, "late", "still being written").unwrap();
            let tail = doc.save_incremental();
            let (half, rest) = tail.split_at(tail.len() / 2);
            write(&store, &key, WriteOp::Append(half.to_vec()));
            assert!(read(&store, &key).get(ROOT, "late").unwrap().is_none());
            write(&store, &key, WriteOp::Append(rest.to_vec()));
            assert!(read(&store, &key).get(ROOT, "late").unwrap().is_some());

            // Rewritten whole, as a compaction does: what it holds now, loaded whole.
            let mut other = AutoCommit::new();
            for at in 0..4_000 {
                let text = blake3::hash(format!("note {at}").as_bytes())
                    .to_hex()
                    .to_string();
                other.put(ROOT, format!("note-{at}"), text).unwrap();
            }
            write(&store, &key, WriteOp::Replace(other.save()));
            let read = read(&store, &key);
            assert!(read.get(ROOT, "issue-0").unwrap().is_none());
            assert!(read.get(ROOT, "note-0").unwrap().is_some());
        }

        /// A page opening a notebook of a hundred notes loads its document once: the workspace's
        /// replica reads it, and the window showing it and the session's own reading of it take
        /// the copy that read kept. The bytes the store holds are the same document saved whole
        /// while the replica captured another save of it, so only the heads find the copy.
        #[test]
        fn a_page_opens_a_notebook_once_and_its_readers_share_it() {
            use std::rc::Rc;
            use std::time::Instant;

            use ui::resource::{DocumentCheckout, DocumentId, ResourceOwners, ResourceType};

            let workspace = "6d1c1f36-0000-4000-8000-000000000001";
            let app = "so.tilo.notes";
            let id = DocumentId::parse("tab:00000000000000a1").expect("a document id");
            let kind = ResourceType::new("notes.notebook");

            // A hundred pages of prose, each its own change, as a notebook imported from another
            // editor's export is: the text many ops, and not compressible much.
            let mut doc = AutoCommit::new();
            assert!(ui::resource::init_scoped(
                &mut doc,
                &id,
                &kind,
                workspace,
                Some(workspace)
            ));
            for page in 0..100 {
                let body = doc
                    .put_object(ROOT, format!("page-{page}"), ObjType::Text)
                    .unwrap();
                let mut words = String::new();
                for word in 0..160 {
                    let word = blake3::hash(format!("page {page} word {word}").as_bytes())
                        .to_hex()
                        .to_string();
                    words.push_str(word.get(..8).unwrap_or_default());
                    words.push(' ');
                }
                doc.splice_text(&body, 0, 0, &words).unwrap();
                doc.commit();
            }
            // What the replica captured: the document saved whole with what was appended since as
            // change chunks, which is not the bytes a re-save of the same document makes.
            let at = doc.committed_heads();
            let mut captured = doc.save();
            let text = blake3::hash(b"the last page").to_hex().to_string();
            doc.put(ROOT, "page-last", text).unwrap();
            doc.commit();
            captured.extend_from_slice(&doc.save_after(&at));
            let stored = doc.save();
            assert_ne!(stored, captured, "the same document in another save");
            assert!(
                stored.len() >= tilo_automerge::shared::MIN_BYTES,
                "a notebook of a hundred pages is large enough to share: {} bytes",
                stored.len()
            );

            let store = Rc::new(durable_store::MemoryStore::new());
            let key = StorageKey::document(app, workspace, id.storage_key());
            write(&store, &key, WriteOp::Replace(stored.clone()));
            store
                .write_hint(
                    &key,
                    &tilo_automerge::load_hint::encode(&stored, &[], &doc.committed_heads())
                        .expect("a notebook of a hundred pages is hinted"),
                )
                .unwrap();

            // What the second read of the document cost before: these bytes loaded whole.
            tilo_automerge::shared::clear();
            let before = Instant::now();
            let whole = tilo_automerge::shared::load(&stored).expect("the stored document");
            let before_ms = before.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(whole.committed_heads(), doc.committed_heads());
            assert_eq!(loaded_whole(), 1);

            tilo_automerge::shared::clear();
            // The workspace's replica reads the document: the one load of the open.
            let read_at = Instant::now();
            let replica = tilo_automerge::shared::load(&captured).expect("the replica's read");
            let replica_ms = read_at.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(replica.committed_heads(), doc.committed_heads());
            assert_eq!(loaded_whole(), 1, "the replica's read loads it");

            // A window of Notes opens it: a copy of the document the replica holds.
            let opened_at = Instant::now();
            let memory: Rc<dyn durable_store::DurableStore> = store.clone();
            let mut owners = ResourceOwners::with_store(memory);
            let owner = owners.open(app, workspace).expect("the app's store");
            let want = DocumentCheckout::new(id.clone(), kind.clone());
            assert!(owner.open_resource(&want), "the document opens");
            let opened = owner.read(&id, AutoCommit::clone).expect("it is open");
            let opened_ms = opened_at.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(opened.committed_heads(), doc.committed_heads());
            assert!(opened.get(ROOT, "page-last").unwrap().is_some());
            assert_eq!(loaded_whole(), 1, "the window takes the copy");

            // The session reads the same document for its roles and for search: also a copy.
            let session_at = Instant::now();
            let hold: &dyn durable_store::DurableStore = store.as_ref();
            let mut read =
                super::super::document(hold, app, workspace, id.route()).expect("it loads");
            let session_ms = session_at.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(read.get_heads(), doc.committed_heads());
            assert_eq!(loaded_whole(), 1, "the session's read takes the copy");
            println!(
                "notebook of 100 pages: one whole load {before_ms:.1} ms; replica {replica_ms:.1} \
                 ms, window {opened_ms:.1} ms, session read {session_ms:.1} ms after it"
            );
        }
    }
}

/// What each workspace holds, as this device's workspace service keeps it: what its apps write,
/// what the person's other devices replicate and what search reads, with the changes noted as
/// they land.
#[cfg(target_os = "linux")]
pub struct Held {
    reads: Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
    changes: Changes,
}

#[cfg(target_os = "linux")]
impl Held {
    /// What `reads` answers, with `changes` saying what moved in it.
    #[must_use]
    pub fn over(
        reads: Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
        changes: Changes,
    ) -> Self {
        Self { reads, changes }
    }

    /// What `app`'s document at `route` in `workspace` holds since `heads`: all of it without
    /// heads. Nothing while its keys are not here yet, and nothing of a workspace this device's
    /// service does not hold: this device's own, where an app launched outside every workspace
    /// keeps what it writes, stays with that app.
    fn read(
        &self,
        (app, workspace, route): (&str, &str, &str),
        heads: Option<&[automerge::ChangeHash]>,
    ) -> Option<tilo_peer_state::workspace_documents::DocumentRead> {
        if !self.reads.workspaces().iter().any(|held| held == workspace) {
            return None;
        }
        let heads: Option<Vec<[u8; 32]>> =
            heads.map(|heads| heads.iter().map(|head| head.0).collect());
        self.reads
            .read(workspace, app, route, heads.as_deref())
            .inspect_err(
                |error| tracing::warn!(%error, %app, %workspace, %route, "a document is not read"),
            )
            .ok()
    }
}

#[cfg(target_os = "linux")]
impl Records for Held {
    fn take_changed(&self) -> Vec<Changed> {
        self.changes.take()
    }

    fn document(&self, app: &str, workspace: &str, route: &str) -> Option<AutoCommit> {
        let Some(tilo_peer_state::workspace_documents::DocumentRead::Whole { bytes, .. }) =
            self.read((app, workspace, route), None)
        else {
            return None;
        };
        AutoCommit::load(&bytes)
            .inspect_err(|error| tracing::warn!(%error, %app, %route, "a document does not load"))
            .ok()
    }

    fn since(
        &self,
        app: &str,
        workspace: &str,
        route: &str,
        heads: &[automerge::ChangeHash],
    ) -> Since {
        use tilo_peer_state::workspace_documents::DocumentRead;
        match self.read((app, workspace, route), Some(heads)) {
            Some(DocumentRead::Unchanged(_)) => Since::Unchanged,
            Some(DocumentRead::Changes { heads, bytes, .. }) => Since::Changes {
                heads: heads.into_iter().map(automerge::ChangeHash).collect(),
                bytes,
            },
            _ => Since::Whole,
        }
    }

    fn held(&self, workspace: &str) -> Vec<(String, String)> {
        self.reads
            .listing(workspace)
            .inspect_err(|error| tracing::warn!(%error, %workspace, "a workspace is not listed"))
            .unwrap_or_default()
    }

    fn workspaces(&self) -> Vec<String> {
        self.reads.workspaces()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A change of a workspace this device's service does not hold is nothing a role that
    /// follows records can read: it is not noted, moves nothing, tells no reader and wakes no
    /// loop. The workspaces the service holds are noted as before.
    #[test]
    fn a_change_of_a_workspace_this_device_does_not_hold_is_noted_to_nobody() {
        let changes = Changes::default();
        let asked = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&asked);
        changes.holding(Arc::new(move |workspace: &str| {
            flag.store(true, Ordering::SeqCst);
            workspace == "held"
        }));
        let woke = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&woke);
        changes.waking(Arc::new(move || flag.store(true, Ordering::SeqCst)));
        #[cfg(target_os = "linux")]
        let reader = changes.reader();

        assert!(
            !changes.note("unheld", "so.tilo.chat", "messages", false),
            "a change of an unheld workspace is no news here"
        );
        assert!(asked.load(Ordering::SeqCst), "the stream asks what is held");
        assert_eq!(changes.moved(), 0, "nothing moved");
        assert_eq!(changes.moved_in("unheld"), 0);
        assert!(changes.take().is_empty(), "no role has anything to take");
        assert!(
            !woke.load(Ordering::SeqCst),
            "the session's loop is not woken"
        );
        #[cfg(target_os = "linux")]
        assert!(
            reader.try_recv().is_err(),
            "no reader following the records is told"
        );

        assert!(changes.note("held", "so.tilo.chat", "messages", false));
        assert_eq!(changes.moved(), 1);
        assert_eq!(changes.moved_in("held"), 1);
        assert!(woke.load(Ordering::SeqCst), "the loop reads the change");
        #[cfg(target_os = "linux")]
        assert_eq!(
            reader
                .recv()
                .map(|changed| changed.workspace)
                .ok()
                .as_deref(),
            Some("held")
        );
    }
}
