//! What a role reads of the workspaces' records, and what changed in them.
//!
//! The workspaces' records are written by their apps, by the person's other devices and by this
//! session's own services. Whoever writes one notes it in the one [`Changes`] stream of this host,
//! and the roles that follow records — the activity and the person's mentions — read that stream
//! and keep no watcher of their own. On a Linux desktop the store every in-process write lands in
//! notes what it writes, and the record index notes what the broker receives from the apps and
//! what a peer's change brought; on the web the page's replication notes what it takes in.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};

use automerge::{AutoCommit, LoadOptions, OnPartialLoad};
use durable_store::{
    Durability, DurableStore, Lease, ReadyWake, Revision, SettleWatch, SharedLease, StorageKey,
    StoreError, Stored, WriteOp,
};
use ui::record::RecordCheckout;

use super::Store;

/// Ceiling on an index document a role reads.
const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;

/// A record a change reached: whose app keeps it, in which workspace, and its route.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Changed {
    pub workspace: String,
    pub app: String,
    /// The record's route; empty when what moved is the workspace's list of an app's records.
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
    /// The workers reading a workspace's records, told which workspace moved when one does.
    readers: Mutex<Vec<mpsc::Sender<String>>>,
}

impl Changes {
    /// Note that `record` of `app` changed in `workspace` (an empty record: the workspace's list
    /// of `app`'s records moved), as `replayed` where it is what this device already held, and
    /// wake what follows it.
    pub fn note(&self, workspace: &str, app: &str, record: &str, replayed: bool) {
        if app.is_empty() {
            return;
        }
        let route = (workspace.to_owned(), app.to_owned(), record.to_owned());
        {
            let mut reached = self
                .0
                .reached
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            // A record noted twice before it is read is read once, and a live change of it is
            // news even where the replay reached it first.
            let held = reached.entry(route).or_insert(replayed);
            *held &= replayed;
        }
        {
            let mut moved_in = self
                .0
                .moved_in
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let moved = moved_in.entry(workspace.to_owned()).or_default();
            *moved = moved.saturating_add(1);
        }
        self.0.moved.fetch_add(1, Ordering::AcqRel);
        self.0
            .readers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|reader| reader.send(workspace.to_owned()).is_ok());
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

    /// A receiver told which workspace's records moved, each time they do: what a worker reading
    /// a workspace's records waits on.
    #[cfg(target_os = "linux")]
    pub fn reader(&self) -> mpsc::Receiver<String> {
        let (told, reader) = mpsc::channel();
        self.0
            .readers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(told);
        reader
    }
}

/// What a host reads of its records.
pub trait Records {
    /// The records a change reached since this was last taken.
    fn take_changed(&self) -> Vec<Changed>;
    /// The document of `app`'s `record` in `workspace`, as this host holds it now.
    fn document(&self, app: &str, workspace: &str, record: &str) -> Option<AutoCommit>;
    /// The kind of the records `app` keeps in `workspace` (`notes.notebook`), empty when unknown.
    fn kind(&self, app: &str, workspace: &str) -> String {
        let _ = (app, workspace);
        String::new()
    }
}

/// The checkouts the index of `app`'s records in `workspace` names, as its app wrote it.
#[must_use]
pub fn checkouts(
    store: &dyn DurableStore,
    app: &str,
    workspace: &str,
) -> Option<Vec<RecordCheckout>> {
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
    AutoCommit::load_with_options(
        &stored.bytes,
        LoadOptions::new().on_partial_load(OnPartialLoad::Ignore),
    )
    .inspect_err(
        |error| tracing::warn!(%error, %app, %workspace, "a workspace's index does not load"),
    )
    .ok()
    .map(|index| ui::record::index::discoverable_references(&index))
}

/// The document `app` keeps for `record` in `workspace`, as its app last committed it.
#[must_use]
pub fn document(
    store: &dyn DurableStore,
    app: &str,
    workspace: &str,
    record: &str,
) -> Option<AutoCommit> {
    let stored = match store.read(&StorageKey::record(app, workspace, record)) {
        Ok(stored) => stored?,
        Err(error) => {
            tracing::warn!(%error, %app, %record, "a record is not read");
            return None;
        }
    };
    if stored.bytes.is_empty() {
        return None;
    }
    AutoCommit::load_with_options(
        &stored.bytes,
        LoadOptions::new().on_partial_load(OnPartialLoad::Ignore),
    )
    .inspect_err(|error| tracing::warn!(%error, %app, %record, "a record does not load"))
    .ok()
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

/// A store whose writes of the workspaces' records are noted in the change stream.
struct Noted {
    store: Store,
    changes: Changes,
}

impl Noted {
    fn wrote(&self, key: &StorageKey) {
        match key {
            StorageKey::Record {
                package,
                workspace,
                record,
            } => self.changes.note(workspace, package, record, false),
            // The workspace's list of an app's records moved: a record appeared in it or left.
            StorageKey::WorkspaceIndex { package, workspace } => {
                self.changes.note(workspace, package, "", false)
            }
            _ => {}
        }
    }
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
        self.wrote(key);
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

    fn records(&self, package: &str, workspace: &str) -> Result<Vec<String>, StoreError> {
        self.store.as_ref().records(package, workspace)
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

#[cfg(target_os = "linux")]
mod native {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use automerge::AutoCommit;
    use ui::record::RecordCheckout;

    use super::{Changes, Records};
    use crate::platform::Store;

    /// The checkouts each workspace index named, by the app and workspace it belongs to.
    type IndexCheckouts = BTreeMap<(String, String), Arc<Vec<RecordCheckout>>>;

    /// This device's records, read through its store and the changes they were noted with.
    pub struct Linked {
        store: Store,
        changes: Changes,
        /// The checkouts each workspace index named when this session last read it: the kind of
        /// record an app keeps, which the index is the workspace's own word for.
        indexes: std::cell::RefCell<IndexCheckouts>,
    }

    impl Linked {
        /// This device's records in `store`, with `changes` saying what changed in them.
        #[must_use]
        pub fn over(store: Store, changes: Changes) -> Self {
            Self {
                store,
                changes,
                indexes: std::cell::RefCell::default(),
            }
        }

        /// The checkouts of `app`'s index in `workspace`, read once per session.
        fn checkouts(&self, app: &str, workspace: &str) -> Arc<Vec<RecordCheckout>> {
            let key = (app.to_owned(), workspace.to_owned());
            if let Some(known) = self.indexes.borrow().get(&key) {
                return Arc::clone(known);
            }
            let named =
                Arc::new(super::checkouts(self.store.as_ref(), app, workspace).unwrap_or_default());
            self.indexes.borrow_mut().insert(key, Arc::clone(&named));
            named
        }
    }

    impl Records for Linked {
        fn take_changed(&self) -> Vec<super::Changed> {
            self.changes.take()
        }

        fn document(&self, app: &str, workspace: &str, record: &str) -> Option<AutoCommit> {
            super::document(self.store.as_ref(), app, workspace, record)
        }

        fn kind(&self, app: &str, workspace: &str) -> String {
            self.checkouts(app, workspace)
                .first()
                .map(|checkout| checkout.kind.as_str().to_owned())
                .unwrap_or_default()
        }
    }
}

#[cfg(target_os = "linux")]
pub use self::native::Linked;
