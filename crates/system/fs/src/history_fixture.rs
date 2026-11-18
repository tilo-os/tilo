//! A test's handle on the content versions of a workspace's files.
//!
//! The versions live in the namespace document, so a test reads them there.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use fs_protocol::{FileDocumentId, FileVersionId, FileVersionV1};
use tilo_lock::MutexRecover as _;

use crate::Namespace;

pub(crate) struct History(Arc<Mutex<Namespace>>);

/// A file's versions, each after the ones it descends from, and its current ones.
pub(crate) struct Snapshot {
    pub(crate) versions: Vec<FileVersionV1>,
    pub(crate) heads: Vec<FileVersionId>,
}

impl History {
    pub(crate) fn new(namespace: Arc<Mutex<Namespace>>) -> Self {
        Self(namespace)
    }

    /// The file's versions, or `None` when it has none.
    pub(crate) fn snapshot(&self, document_id: FileDocumentId) -> Option<Snapshot> {
        let namespace = self.0.lock_recover();
        let versions = namespace.document_versions(document_id);
        if versions.is_empty() {
            return None;
        }
        Some(Snapshot {
            versions: ordered(&versions),
            heads: namespace.document_history_heads(document_id),
        })
    }
}

/// `versions`, each after its parents.
fn ordered(versions: &BTreeMap<FileVersionId, FileVersionV1>) -> Vec<FileVersionV1> {
    fn depth(
        id: FileVersionId,
        versions: &BTreeMap<FileVersionId, FileVersionV1>,
        known: &mut BTreeMap<FileVersionId, usize>,
    ) -> usize {
        if let Some(depth) = known.get(&id) {
            return *depth;
        }
        let parents = versions
            .get(&id)
            .map(|v| v.parents.clone())
            .unwrap_or_default();
        let depth = parents
            .into_iter()
            .map(|parent| depth(parent, versions, known).saturating_add(1))
            .max()
            .unwrap_or(0);
        known.insert(id, depth);
        depth
    }
    let mut known = BTreeMap::new();
    let mut ordered: Vec<_> = versions.values().cloned().collect();
    ordered.sort_by_key(|version| {
        (
            depth(version.id, versions, &mut known),
            version.timestamp_ms,
            version.id,
        )
    });
    ordered
}
