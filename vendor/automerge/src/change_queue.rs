use std::collections::HashSet;

use crate::{Change, ChangeHash};

/// An indexed queue of unapplied changes that are not yet causally ready.
///
/// Maintains a hash index so that lookups are O(1) instead of linear scans.
#[derive(Debug, Clone)]
pub(crate) struct ChangeQueue {
    changes: Vec<Change>,
    /// Set of hashes of all changes in the queue — O(1) contains check.
    hashes: HashSet<ChangeHash>,
}

impl ChangeQueue {
    pub(crate) fn new() -> Self {
        Self {
            changes: Vec::new(),
            hashes: HashSet::new(),
        }
    }

    pub(crate) fn push(&mut self, c: Change) {
        self.hashes.insert(c.hash());
        self.changes.push(c);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// O(1) check whether a change with this hash is in the queue.
    pub(crate) fn has_hash(&self, hash: &ChangeHash) -> bool {
        self.hashes.contains(hash)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Change> + Clone {
        self.changes.iter()
    }

    /// Take all changes out, leaving the queue empty.
    pub(crate) fn take(&mut self) -> Vec<Change> {
        self.hashes.clear();
        std::mem::take(&mut self.changes)
    }
}

// Native allocation traversal is read-only and does not populate caches.
use crate::resource_limits::retained::{NativeVisit, Visitor};
impl NativeVisit for ChangeQueue {
    fn native_visit(
        &self,
        visitor: &mut Visitor<'_>,
    ) -> Result<(), crate::resource_limits::NativeResourceError> {
        let Self { changes, hashes } = self;
        changes.native_visit(visitor)?;
        visitor.hash_set(hashes, hashes.capacity())
    }
}
