//! A test's handle on a workspace's immutable history.
//!
//! History lives in the namespace document, so a test reads and writes it
//! there. The fixture keeps the calls tests are written in terms of — append
//! a version, record a conflict, read what a record holds — so an assertion
//! states what it means without reaching for a lock.

use std::sync::{Arc, Mutex};

use fs_protocol::{FileConflictV1, FileRecordId, FileVersionV1, HistoryProjectionV1};
use tilo_lock::MutexRecover as _;

use crate::Namespace;
use crate::namespace::history::{HistoryError, RecordHistorySnapshot};

pub(crate) struct History(Arc<Mutex<Namespace>>);

impl History {
    pub(crate) fn new(namespace: Arc<Mutex<Namespace>>) -> Self {
        Self(namespace)
    }

    pub(crate) fn snapshot(&self, record_id: FileRecordId) -> Option<RecordHistorySnapshot> {
        self.0.lock_recover().record_history(record_id)
    }

    pub(crate) fn projection(&self, record_id: FileRecordId) -> Option<HistoryProjectionV1> {
        self.0.lock_recover().record_history_projection(record_id)
    }

    pub(crate) fn append(&self, version: FileVersionV1) -> Result<bool, HistoryError> {
        self.0.lock_recover().append_version(version)
    }

    pub(crate) fn append_batch(
        &self,
        versions: &[FileVersionV1],
    ) -> Result<Vec<bool>, HistoryError> {
        self.0.lock_recover().append_versions(versions)
    }

    pub(crate) fn record_conflict(&self, conflict: &FileConflictV1) -> Result<bool, HistoryError> {
        self.0.lock_recover().record_history_conflict(conflict)
    }
}
