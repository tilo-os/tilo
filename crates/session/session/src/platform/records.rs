//! What a role reads of the workspaces' records, and what changed in them.
//!
//! A role that follows the records — a mention of the person in one of them — asks this seam which
//! records a change reached since it last looked, and reads those records' documents. The event
//! is the host's own: on a Linux desktop it is the change the session's record index and its
//! broker's commit path already hear of, queued for whoever reads them next; on the web it is what
//! the page's replication took in from the workspace. Neither role watches a directory itself.

use automerge::{AutoCommit, LoadOptions, OnPartialLoad};
use durable_store::{DurableStore, StorageKey};
use ui::record::RecordCheckout;

/// A record a change reached: whose app keeps it, in which workspace, and its route.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Changed {
    pub workspace: String,
    pub app: String,
    pub record: String,
}

/// What a host reads of its records.
pub trait Records {
    /// The records a change reached since this was last taken, and whether the host had replayed
    /// what its workspaces held by then: a mention that predates this device following a record is
    /// no news.
    fn take_changed(&self) -> (bool, Vec<Changed>);
    /// The document of `app`'s `record` in `workspace`, as this host holds it now.
    fn document(&self, app: &str, workspace: &str, record: &str) -> Option<AutoCommit>;
    /// The kind of the records `app` keeps in `workspace` (`notes.notebook`), empty when this host
    /// does not know it.
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
    if stored.bytes.is_empty() {
        return None;
    }
    // An app keeping the index appends to it, and this read may land on a tail it has not
    // finished writing: the changes before it are the index as of the last write that completed.
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

    /// The records a change reached, as this session already learns of them: the search index and
    /// the broker's commit path note every record they took in, and what reaches them next is read
    /// by the roles that follow records.
    #[derive(Clone, Default)]
    pub struct Touched(Arc<Inner>);

    #[derive(Default)]
    struct Inner {
        reached: Mutex<Vec<Changed>>,
        /// What wakes the session's loop when a record changed: the pass that reads the queue runs
        /// then, not at the next frame something else asked for.
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

    /// This device's records, read through its store and the changes the session heard of.
    pub struct Linked {
        store: Store,
        touched: Touched,
        /// Whether the first read of the changes happened: what the device already held was noted
        /// before any role looked, so the first look is the workspace's replay of it.
        read: std::cell::Cell<bool>,
        /// The checkouts each workspace index named when this session last read it: the kind of
        /// record an app keeps, which the index is the workspace's own word for.
        indexes: std::cell::RefCell<BTreeMap<(String, String), Arc<Vec<RecordCheckout>>>>,
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

        /// The checkouts the index of `app`'s records in `workspace` names, as this session last
        /// read it: an index that did not change since is read once.
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
