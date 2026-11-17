//! Files that show a record kept elsewhere: a note of the workspace's notebook as a `.md`
//! file in the folder. The node names the record and what both last read as; the record
//! stays the source of truth.

use super::{Namespace, ROOT_ID, str_at};

/// The record a node's file is the projection of (`<app>:<item>`): its content is that
/// record's, shown as a file in the folder ([`Namespace::set_projection`]).
const PROJECTS: &str = "projects";
/// What the node's file and the record it projects last both read as, as the BLAKE3 of the
/// file's bytes then: a file reading otherwise was written in the folder, a record reading
/// otherwise was changed where it is kept.
const PROJECTS_AGREED: &str = "projects_agreed";

impl Namespace {
    /// Mark node `id` as the file of the record `source` names, both reading as `agreed` now
    /// (the BLAKE3 of the file's bytes). The record stays the source of truth: the node is
    /// its projection, written from it, and a write to the file is applied to it.
    pub fn set_projection(&mut self, id: &str, source: &str, agreed: &str) {
        self.set_field(id, PROJECTS, source);
        self.set_field(id, PROJECTS_AGREED, agreed);
    }

    /// The record node `id`'s file projects, and what both last read as
    /// ([`Self::set_projection`]). `None` for a file of its own.
    pub fn projection(&self, id: &str) -> Option<(String, String)> {
        let node = self.node(id)?;
        Some((
            str_at(&self.doc, &node, PROJECTS)?,
            str_at(&self.doc, &node, PROJECTS_AGREED).unwrap_or_default(),
        ))
    }

    /// Every node, live or in the trash, whose file projects a record.
    pub fn projected_nodes(&self) -> Vec<String> {
        self.doc
            .node_ids()
            .filter(|id| {
                self.node(id)
                    .is_some_and(|node| str_at(&self.doc, &node, PROJECTS).is_some())
            })
            .cloned()
            .collect()
    }

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
        self.set_field(id, "parent", back);
        self.index_refresh_subtree(id);
        true
    }
}
