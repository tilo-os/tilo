//! What a role reads of the workspaces' records, and what changed in them.
//!
//! The workspaces' records are written by their apps, by the person's other devices and by this
//! session's own services. Whoever writes one notes it in the one [`Changes`] stream of this host,
//! and the roles that follow records — the activity and the person's mentions — read that stream
//! and keep no watcher of their own. On a Linux desktop the store every in-process write lands in
//! notes what it writes, the store's files are watched for the writes their apps make from their
//! own processes ([`follow`]), and the record index notes what the broker receives from the apps;
//! on the web the page's replication notes what it takes in.

use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};

use automerge::AutoCommit;
use durable_store::{
    Durability, DurableStore, Lease, ReadyWake, Revision, SettleWatch, SharedLease, StorageKey,
    StoreError, Stored, WriteOp,
};
use ui::record::DocumentCheckout;

use super::Store;

/// Ceiling on an index document a role reads.
const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;

/// A record a change reached: whose app keeps it, in which workspace, and its route
/// ([`ui::record::DocumentId::route`]); [`storage_key`] is where a reader finds it.
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
    /// The route of each record document a write was noted at, by its storage key: the key is a
    /// hash of the record's id, so the route is read once from the document's own identity.
    routes: Mutex<BTreeMap<StorageKey, String>>,
}

impl Changes {
    /// Note that `record` of `app` changed in `workspace` (an empty record: the workspace's list
    /// of `app`'s records moved), as `replayed` where it is what this device already held, and
    /// wake what follows it.
    pub fn note(&self, workspace: &str, app: &str, record: &str, replayed: bool) {
        if app.is_empty() {
            return;
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
        self.0
            .readers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|reader| {
                reader
                    .send(Changed {
                        workspace: workspace.to_owned(),
                        app: app.to_owned(),
                        record: record.to_owned(),
                        replayed,
                    })
                    .is_ok()
            });
        if let Some(wake) = self
            .0
            .wake
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            wake();
        }
    }

    /// Note the write of `key` in `store` by the route of the record kept there: the empty one for
    /// an app's workspace document, the id a record's document names itself by for any other.
    pub fn wrote(&self, store: &dyn DurableStore, key: &StorageKey) {
        match key {
            StorageKey::Document { app, workspace, .. } => {
                if let Some(route) = self.route_at(store, key) {
                    self.note(workspace, app, &route, false);
                }
            }
            StorageKey::WorkspaceIndex { app, workspace } => self.note(workspace, app, "", false),
            _ => {}
        }
    }

    /// The route of the record whose document is kept at `key`, read from the document once.
    fn route_at(&self, store: &dyn DurableStore, key: &StorageKey) -> Option<String> {
        let StorageKey::Document { document, .. } = key else {
            return None;
        };
        if let Some(known) = self
            .0
            .routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
        {
            return Some(known.clone());
        }
        // A document read while its first write is still landing names no identity yet: the next
        // write of it is read again.
        let id = ui::record::identity(&stored_document(store, key)?)?.id;
        if id.storage_key() != *document {
            return None;
        }
        let route = id.route().to_owned();
        self.0
            .routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.clone(), route.clone());
        Some(route)
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
    /// The kind of the records `app` keeps in `workspace` (`notes.notebook`), empty when unknown.
    fn kind(&self, app: &str, workspace: &str) -> String {
        let _ = (app, workspace);
        String::new()
    }
}

/// The checkouts the index of `app`'s records in `workspace` names, as its app wrote it, each with
/// what the index says it is called.
#[must_use]
pub fn named_checkouts(
    store: &dyn DurableStore,
    app: &str,
    workspace: &str,
) -> Option<Vec<(DocumentCheckout, Option<String>)>> {
    let index = index_document(store, app, workspace)?;
    Some(
        ui::record::index::discoverable_references(&index)
            .into_iter()
            .map(|checkout| {
                let name = ui::record::index::name_of(&index, &checkout.id);
                (checkout, name)
            })
            .collect(),
    )
}

/// `app`'s workspace document in `workspace` read once: the checkouts its index names, and the
/// document itself as the record `app` keeps as itself (Notes' notebook, Chat's chat) once an
/// action was done in it. Keeping the index alone is no one's work.
#[must_use]
pub fn workspace_records(
    store: &dyn DurableStore,
    app: &str,
    workspace: &str,
) -> Option<(Vec<DocumentCheckout>, Option<AutoCommit>)> {
    let document = index_document(store, app, workspace)?;
    let checkouts = ui::record::index::discoverable_references(&document);
    // Only the changes' messages are read: rebuilding every change would cost the whole log.
    let acted = document
        .committed_document()
        .get_changes_meta(&[])
        .iter()
        .any(|change| {
            change.message.as_deref().is_some_and(|message| {
                message.starts_with(tilo_actions::CHANGE_MESSAGE_PREFIX)
                    || message.starts_with(tilo_actions::CONTINUATION_PREFIX)
            })
        });
    Some((checkouts, acted.then_some(document)))
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
    let id = ui::record::DocumentId::from_route(record)?;
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

    #[cfg(test)]
    thread_local! {
        /// How many documents this thread loaded whole.
        static LOADED_WHOLE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
            // hands over a copy of it instead.
            let mut doc = if let Some(doc) = tilo_automerge::shared::held(bytes) {
                doc
            } else {
                #[cfg(test)]
                LOADED_WHOLE.with(|loaded| loaded.set(loaded.get().saturating_add(1)));
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
        use automerge::{ROOT, ReadDoc as _};
        use durable_store::WriteOp;

        fn loaded_whole() -> usize {
            LOADED_WHOLE.with(std::cell::Cell::get)
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
    }
}

/// `store` as the workspaces' records are kept in it, noting every record and index written
/// through it in `changes`: what this session's own writers land in, and what the roles that
/// follow records hear.
#[must_use]
pub fn noting(store: Store, changes: Changes) -> Store {
    #[cfg(target_os = "linux")]
    {
        Arc::new(Noted { store, changes }) as Store
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::rc::Rc::new(Noted { store, changes }) as Store
    }
}

/// Note every write to the records kept under `root` in `changes`, for as long as the watch is
/// held: the store is a directory tree the apps of the workspaces write from their own processes,
/// so its files are what tells the one change stream that a record moved when this session did not
/// write it. Only the workspaces below a package are watched; the rest of the data home is watched
/// for them appearing, and nothing it holds is a record.
#[cfg(target_os = "linux")]
#[must_use]
pub fn follow(
    root: &Path,
    store: &Store,
    changes: &Changes,
) -> Option<tilo_files::native::ChangeWatch> {
    let watched = root.to_path_buf();
    let noting = changes.clone();
    let reading = Store::clone(store);
    let top = root.to_path_buf();
    tilo_files::native::Watcher::tree(root)
        .skip(move |directory| !holds_records(directory, &watched))
        .start(move |events| {
            for event in events {
                if let Some(key) = changed_at(&event.path, &top) {
                    noting.wrote(reading.as_ref(), &key);
                }
            }
        })
        .map_err(|error| tracing::warn!(%error, "writes to the records are not followed"))
        .ok()
}

/// Whether a watch of the records' tree watches `directory`: a package of the store, or anything
/// below the workspaces of one, which is where the workspaces' indexes and records are.
#[cfg(target_os = "linux")]
fn holds_records(directory: &Path, root: &Path) -> bool {
    let Ok(below) = directory.strip_prefix(root) else {
        return false;
    };
    let mut parts = below.components();
    // A package of the store: its workspaces may appear below it at any time.
    if parts.next().is_none() {
        return false;
    }
    match parts.next() {
        None => true,
        Some(part) => part.as_os_str() == app_state::WORKSPACES_DIR,
    }
}

/// The key a write to `path` under `root` landed at: the workspace index of an app in a workspace,
/// or the document of one of its records.
#[cfg(target_os = "linux")]
fn changed_at(path: &Path, root: &Path) -> Option<StorageKey> {
    let below = path.strip_prefix(root).ok()?;
    let names: Vec<&str> = below
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect::<Option<_>>()?;
    let [app, workspaces, workspace, rest @ ..] = names.as_slice() else {
        return None;
    };
    if *workspaces != app_state::WORKSPACES_DIR {
        return None;
    }
    Some(match rest {
        [records, record, ..] if *records == app_state::DOCUMENTS_DIR => {
            StorageKey::document(*app, *workspace, *record)
        }
        _ => StorageKey::workspace_index(*app, *workspace),
    })
}

/// A store whose writes of the workspaces' records are noted in the change stream.
struct Noted {
    store: Store,
    changes: Changes,
}

impl DurableStore for Noted {
    fn namespace(&self) -> &str {
        self.store.as_ref().namespace()
    }

    fn read(&self, key: &StorageKey) -> Result<Option<Stored>, StoreError> {
        self.store.as_ref().read(key)
    }

    fn write(
        &self,
        key: &StorageKey,
        op: WriteOp,
        writer: Option<&[u8]>,
    ) -> Result<Revision, StoreError> {
        let revision = self.store.as_ref().write(key, op, writer)?;
        self.changes.wrote(self.store.as_ref(), key);
        Ok(revision)
    }

    fn durability(&self, key: &StorageKey, revision: Revision) -> Durability {
        self.store.as_ref().durability(key, revision)
    }

    fn flush(&self, key: Option<&StorageKey>) -> Result<(), StoreError> {
        self.store.as_ref().flush(key)
    }

    fn flush_without_waiting(&self, key: &StorageKey) -> Result<(), StoreError> {
        self.store.as_ref().flush_without_waiting(key)
    }

    fn flush_where_allowed(&self, key: &StorageKey) -> Result<(), StoreError> {
        self.store.as_ref().flush_where_allowed(key)
    }

    fn is_settled(&self, key: &StorageKey) -> bool {
        self.store.as_ref().is_settled(key)
    }

    fn wake_when_settled(&self, key: &StorageKey, wake: ReadyWake) -> SettleWatch {
        self.store.as_ref().wake_when_settled(key, wake)
    }

    fn lease(&self, key: &StorageKey) -> Result<Lease, StoreError> {
        self.store.as_ref().lease(key)
    }

    fn lease_shared(&self, key: &StorageKey) -> Result<SharedLease, StoreError> {
        self.store.as_ref().lease_shared(key)
    }

    fn declined_waiting_here(&self) -> bool {
        self.store.as_ref().declined_waiting_here()
    }

    fn claim(&self, key: &StorageKey, claimant: &str) -> Result<bool, StoreError> {
        self.store.as_ref().claim(key, claimant)
    }

    fn documents(&self, app: &str, workspace: &str) -> Result<Vec<String>, StoreError> {
        self.store.as_ref().documents(app, workspace)
    }

    fn workspace_indexes(&self) -> Result<Vec<(String, String)>, StoreError> {
        self.store.as_ref().workspace_indexes()
    }

    fn local_path(&self, key: &StorageKey) -> Option<std::path::PathBuf> {
        self.store.as_ref().local_path(key)
    }

    fn write_hint(&self, key: &StorageKey, hint: &[u8]) -> Result<(), StoreError> {
        self.store.as_ref().write_hint(key, hint)
    }

    fn read_hint(&self, key: &StorageKey) -> Option<Vec<u8>> {
        self.store.as_ref().read_hint(key)
    }
}

/// This device's records, read through its store and the changes they were noted with.
#[cfg(target_os = "linux")]
pub struct Linked {
    store: Store,
    changes: Changes,
}

#[cfg(target_os = "linux")]
impl Linked {
    /// This device's records in `store`, with `changes` saying what changed in them.
    #[must_use]
    pub fn over(store: Store, changes: Changes) -> Self {
        Self { store, changes }
    }
}

#[cfg(target_os = "linux")]
impl Records for Linked {
    fn take_changed(&self) -> Vec<Changed> {
        self.changes.take()
    }

    fn document(&self, app: &str, workspace: &str, record: &str) -> Option<AutoCommit> {
        document(self.store.as_ref(), app, workspace, record)
    }

    /// The kind the workspace's index names its first record as: the workspace's own word for the
    /// records its app keeps.
    fn kind(&self, app: &str, workspace: &str) -> String {
        named_checkouts(self.store.as_ref(), app, workspace)
            .and_then(|named| named.into_iter().next())
            .map(|(checkout, _)| checkout.kind.as_str().to_owned())
            .unwrap_or_default()
    }
}
