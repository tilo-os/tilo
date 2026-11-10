//! What a role reads of the workspaces' records, and what changed in them.
//!
//! A role that follows the records asks this seam which records a change reached since it last
//! looked, and reads their documents. The event is the host's own: on a Linux desktop the change
//! the session's record index already hears of, on the web what the page's replication took in.
//! Neither role watches a directory itself.

use automerge::{AutoCommit, LoadOptions, OnPartialLoad};
use durable_store::{DurableStore, StorageKey};
use ui::record::RecordCheckout;

/// Ceiling on an index document a role reads.
const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;

/// A record a change reached: whose app keeps it, in which workspace, and its route.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Changed {
    pub workspace: String,
    pub app: String,
    pub record: String,
}

/// What a host reads of its records.
pub trait Records {
    /// The records a change reached since this was last taken, and whether what reached them is
    /// what the host already held (its workspaces replayed before any role looked): that is no
    /// news, where a live change is.
    fn take_changed(&self) -> (bool, Vec<Changed>);
    /// The document of `app`'s `record` in `workspace`, as this host holds it now.
    fn document(&self, app: &str, workspace: &str, record: &str) -> Option<AutoCommit>;
    /// The kind of the records `app` keeps in `workspace` (`notes.notebook`), empty when unknown.
    fn kind(&self, app: &str, workspace: &str) -> String {
        let _ = (app, workspace);
        String::new()
    }
}

#[cfg(target_os = "linux")]
pub use self::native::{Linked, Touched};

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

#[cfg(target_os = "linux")]
mod native {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex, PoisonError};

    use automerge::AutoCommit;
    use ui::record::RecordCheckout;

    use super::{Changed, Records};
    use crate::platform::Store;

    /// The records a change reached, as this session already learns of them: the record index and
    /// the broker's commit path note every record they take in.
    #[derive(Clone, Default)]
    pub struct Touched(Arc<Inner>);

    #[derive(Default)]
    struct Inner {
        reached: Mutex<Vec<Changed>>,
        /// What wakes the session's loop when a record changed, so the pass reads the queue then.
        wake: Mutex<Option<tilo_service::Wake>>,
    }

    impl Touched {
        /// Note that `record` of `app` changed in `workspace`, and wake the loop that reads it.
        pub fn note(&self, workspace: &str, app: &str, record: &str) {
            if app.is_empty() || record.is_empty() {
                return;
            }
            let changed = Changed {
                workspace: workspace.to_owned(),
                app: app.to_owned(),
                record: record.to_owned(),
            };
            {
                let mut reached = self
                    .0
                    .reached
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if !reached.contains(&changed) {
                    reached.push(changed);
                }
            }
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

        /// Wake the loop through `wake` whenever a record changed.
        pub fn waking(&self, wake: tilo_service::Wake) {
            *self.0.wake.lock().unwrap_or_else(PoisonError::into_inner) = Some(wake);
        }

        fn take(&self) -> Vec<Changed> {
            std::mem::take(
                &mut *self
                    .0
                    .reached
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner),
            )
        }
    }

    /// The checkouts each workspace index named, by the app and workspace it belongs to.
    type IndexCheckouts = BTreeMap<(String, String), Arc<Vec<RecordCheckout>>>;

    /// This device's records, read through its store and the changes the session heard of.
    pub struct Linked {
        store: Store,
        touched: Touched,
        /// Whether the changes were read before: what the device already held was noted before any
        /// role looked, so the first look is the workspace's replay of it.
        read: std::cell::Cell<bool>,
        /// The checkouts each workspace index named when this session last read it: the kind of
        /// record an app keeps, which the index is the workspace's own word for.
        indexes: std::cell::RefCell<IndexCheckouts>,
    }

    impl Linked {
        /// This device's records in `store`, with `touched` saying what changed in them.
        #[must_use]
        pub fn over(store: Store, touched: Touched) -> Self {
            Self {
                store,
                touched,
                read: std::cell::Cell::default(),
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
        fn take_changed(&self) -> (bool, Vec<Changed>) {
            let first = !self.read.replace(true);
            (first, self.touched.take())
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
