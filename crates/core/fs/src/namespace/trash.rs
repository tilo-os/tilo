//! Taking a node back out of the trash.

use super::{Namespace, ROOT_ID};

impl Namespace {
    /// Bring node `id` back out of the trash into the folder it was deleted from, or the
    /// root when that folder is gone too. False for a node that is not in the trash.
    pub fn restore(&mut self, id: &str) -> bool {
        if !self.is_directly_trashed(id) {
            return false;
        }
        let back = self
            .prev_parent(id)
            .filter(|parent| parent == ROOT_ID || self.reaches_root(parent))
            .unwrap_or_else(|| ROOT_ID.to_string());
        if !self.write_action(&fs_protocol::FileAction::Restore {
            node: id.to_owned(),
            parent: Some(back),
        }) {
            return false;
        }
        self.reindex_node(id);
        self.index_refresh_subtree(id);
        true
    }
}
