//! A test's handle on a workspace's immutable history.
//!
//! History lives in the namespace document, so a test reads it there.

use std::sync::{Arc, Mutex};

use fs_protocol::FileRecordId;
use tilo_lock::MutexRecover as _;

use crate::Namespace;
use crate::namespace::history::RecordHistorySnapshot;

pub(crate) struct History(Arc<Mutex<Namespace>>);

impl History {
    pub(crate) fn new(namespace: Arc<Mutex<Namespace>>) -> Self {
        Self(namespace)
    }

    pub(crate) fn snapshot(&self, record_id: FileRecordId) -> Option<RecordHistorySnapshot> {
        self.0.lock_recover().record_history(record_id)
    }
}
