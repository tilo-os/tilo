//! The namespace's record index: the catalogue of stable records the mount serves, and the
//! structural index it is kept from.
//!
//! Every node carries a stable record id, and the catalogue names each record's node, parent and
//! path. It is rebuilt once when a document is opened and then kept in step node by node, so a
//! page request is a slice of it and a placement costs the nodes it moved rather than a walk over
//! the tree.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use fs_protocol::FileDocumentId;
use uuid::Uuid;

use super::{
    DOCUMENT_ID_KEY, DocumentIndexDigest, IndexedDocument, Namespace, NodeKind, NodeRow, NodeWalk,
    Obj, ROOT_DOCUMENT_ID_KEY, ROOT_ID, TreeIndex, read_name, str_at,
};

impl Namespace {
    /// Reconstruct the index once while loading or after an opaque remote merge. Serving a
    /// record-index page never calls this: page work is a slice of `record_index`. Every
    /// mutation this replica makes on its own refreshes only the nodes it changed
    /// ([`Self::index_refresh_node`]).
    pub(super) fn rebuild_document_index(&mut self) {
        let NodeWalk {
            tree,
            rows: raw,
            occurrences,
        } = self.read_nodes_once();
        self.tree = tree;
        let mut children = HashMap::<String, BTreeSet<String>>::new();
        for (id, item) in &raw {
            children
                .entry(item.parent_id.clone())
                .or_default()
                .insert(id.clone());
        }
        let mut record_index = BTreeMap::new();
        let mut queue = VecDeque::from([ROOT_ID.to_string()]);
        let mut visited = HashSet::from([ROOT_ID.to_string()]);
        while let Some(parent) = queue.pop_front() {
            for id in children.get(&parent).into_iter().flatten() {
                if !visited.insert(id.clone()) {
                    continue;
                }
                let Some(item) = raw.get(id) else { continue };
                // Traverse all structurally reachable nodes, even a legacy duplicate-ID node;
                // only the unique stable record is eligible for the public index. A node's
                // path is derived when a page asks for it, never built per file here.
                queue.push_back(id.clone());
                let Some(document_id) = item.document_id else {
                    continue;
                };
                if occurrences.get(&document_id) != Some(&1) {
                    continue;
                }
                record_index.insert(
                    document_id,
                    IndexedDocument {
                        document_id,
                        node_id: id.clone(),
                        parent_id: item.parent_id.clone(),
                        name: item.name.clone(),
                        is_dir: item.is_dir,
                    },
                );
            }
        }
        self.document_index_digest = DocumentIndexDigest::default();
        for record in record_index.values() {
            self.document_index_digest.add(record);
        }
        self.document_index = record_index;
        self.document_index_ambiguous = occurrences
            .into_iter()
            .filter_map(|(document_id, count)| (count > 1).then_some(document_id))
            .collect();
        self.update_document_index_revision();
        self.cycle_scan_pending = self.find_cycle(None).is_some();
        #[cfg(test)]
        {
            self.document_index_rebuilds = self.document_index_rebuilds.saturating_add(1);
        }
    }

    /// The structural index of every node, read from the document once per node. Opening a
    /// namespace and every received peer delta rebuild through here, so this is the
    /// per-node cost of both, and `Self::node_lookup_count` is the work the performance
    /// check bounds.
    pub(super) fn read_nodes_once(&self) -> NodeWalk {
        let parse_document_id = |value: String| Uuid::parse_str(&value).ok().map(FileDocumentId);
        let mut rows = HashMap::new();
        let mut occurrences = HashMap::<FileDocumentId, usize>::new();
        let mut tree = TreeIndex::default();
        for id in self.doc.sorted_node_ids() {
            let Some(node) = self.node(&id) else {
                continue;
            };
            let node_document_id =
                str_at(&self.doc, &node, DOCUMENT_ID_KEY).and_then(parse_document_id);
            // `document_id_for_id` answers the reserved root id from the document root and every
            // other node from its own object; the structural index bound the node's own key.
            let document_id = if id == ROOT_ID {
                str_at(&self.doc, &Obj::index(), ROOT_DOCUMENT_ID_KEY).and_then(parse_document_id)
            } else {
                node_document_id
            };
            let parent = str_at(&self.doc, &node, "parent");
            let name = read_name(&self.doc, &node, "name").unwrap_or_default();
            let is_dir = NodeKind::from_str(&str_at(&self.doc, &node, "kind").unwrap_or_default())
                == NodeKind::Dir;
            if let Some(node_document_id) = node_document_id {
                tree.bind_document(&id, node_document_id);
            }
            if let Some(document_id) = document_id {
                let count = occurrences
                    .get(&document_id)
                    .copied()
                    .unwrap_or_default()
                    .saturating_add(1);
                occurrences.insert(document_id, count);
            }
            if let Some(parent) = &parent {
                tree.place(&id, parent.clone(), name.clone());
            }
            rows.insert(
                id,
                NodeRow {
                    document_id,
                    parent_id: parent.unwrap_or_default(),
                    name,
                    is_dir,
                },
            );
        }
        NodeWalk {
            tree,
            rows,
            occurrences,
        }
    }

    /// Fold what an apply brought into the structural index and the catalogue: the nodes the
    /// patches touched, and the subtrees a placement change moved, so receiving one of a
    /// peer's edits costs that edit rather than the workspace. Only opening a document reads
    /// every node ([`Self::rebuild_document_index`]).
    ///
    /// Takes the patches the caller armed before the apply
    /// ([`ChangeTrackedDoc::arm_patches`]) and disarms.
    pub(super) fn index_after_apply(&mut self, touched: &HashSet<String>) {
        if self.root_document_id().is_none() {
            // A tree whose own record carries no root identity yet: the load path.
            self.rebuild_document_index();
            return;
        }
        self.index_nodes(touched);
    }

    pub(super) fn index_nodes(&mut self, touched: &HashSet<String>) {
        let mut records = HashSet::new();
        let mut moved = Vec::new();
        if touched.contains(ROOT_ID) {
            // The reserved id answers to the document root's record, so its row and the root's
            // ambiguity move with it.
            records.extend(self.root_document_id());
        }
        for id in touched {
            records.extend(self.tree.resource_of.get(id).copied());
            if self.node(id).is_none() {
                self.tree.remove(id);
                self.tree.unbind_document(id);
                continue;
            }
            let placement = self.tree.placement.get(id).cloned();
            match self.parent_of(id) {
                Some(parent) => self
                    .tree
                    .place(id, parent, self.name_of(id).unwrap_or_default()),
                None => self.tree.remove(id),
            }
            if placement.as_ref() != self.tree.placement.get(id) {
                moved.push(id.clone());
            }
            match self.node_document_id(id) {
                Some(record) => self.tree.bind_document(id, record),
                None => self.tree.unbind_document(id),
            }
            records.extend(self.tree.resource_of.get(id).copied());
        }
        for record in &records {
            if let Some(row) = self.document_index.get(record).cloned()
                && self.tree.resource_of.get(&row.node_id) != Some(record)
            {
                self.index_drop_row(&row);
            }
            if self.document_occurrences(*record) == 1 {
                self.document_index_ambiguous.remove(record);
            } else {
                self.document_index_ambiguous.insert(*record);
            }
            self.index_refresh_bound_nodes(*record);
            self.update_document_index_revision();
        }
        for id in &moved {
            self.index_refresh_subtree(id);
        }
        self.placed_since_resolve.extend(moved);
    }

    /// Bring the rows of every node bound to `record` in line with the document: a record that
    /// changed hands leaves its old node to the new one, and a duplicate is withheld from both.
    pub(super) fn index_refresh_bound_nodes(&mut self, record: FileDocumentId) {
        let bound = self.tree.records.get(&record).cloned().unwrap_or_default();
        for id in &bound {
            self.index_refresh_node(id);
        }
    }

    pub(super) fn document_occurrences(&self, document_id: FileDocumentId) -> usize {
        let bound = self
            .tree
            .nodes_of(document_id)
            .filter(|id| id.as_str() != ROOT_ID)
            .count();
        let root_key_node =
            self.root_document_id() == Some(document_id) && self.node(ROOT_ID).is_some();
        bound.saturating_add(usize::from(root_key_node))
    }

    pub(super) fn root_document_id(&self) -> Option<FileDocumentId> {
        str_at(&self.doc, &Obj::index(), ROOT_DOCUMENT_ID_KEY)
            .and_then(|value| Uuid::parse_str(&value).ok())
            .map(FileDocumentId)
    }

    /// The record id stored on a node's own map, which is its identity everywhere but the
    /// document root ([`Self::document_id_for_id`]).
    pub(super) fn node_document_id(&self, id: &str) -> Option<FileDocumentId> {
        str_at(&self.doc, &self.node(id)?, DOCUMENT_ID_KEY)
            .and_then(|value| Uuid::parse_str(&value).ok())
            .map(FileDocumentId)
    }

    /// Snapshot one bounded canonical page, ordered by stable row id, cloning at most `limit`
    /// entries. A page walks the ids before its offset, which is what a reader asks for rarely;
    /// admitting one row costs a lookup, so a checkout stays flat however full the catalogue is.
    pub fn indexed_document_page(&self, offset: usize, limit: usize) -> Vec<IndexedDocument> {
        #[cfg(test)]
        self.document_index_page_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.document_index
            .values()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn indexed_document_count(&self) -> usize {
        self.document_index.len()
    }

    #[must_use]
    pub fn indexed_document(&self, document_id: FileDocumentId) -> Option<IndexedDocument> {
        self.document_index.get(&document_id).cloned()
    }

    /// The names of the folders in the folder `ino` that a listing of it shows, from the
    /// structural index and the catalogue: no child's node is read, so counting a folder's
    /// subfolders does not cost every file beside them. A child of a folder that reaches the
    /// root reaches it too. `None` when `ino` is no folder in the tree.
    #[must_use]
    pub fn subfolder_names(&self, ino: u64) -> Option<Vec<fs_protocol::FileNameV1>> {
        let id = self.ino_to_id.get(&ino)?;
        if id != ROOT_ID && (self.kind_of(id) != NodeKind::Dir || !self.reaches_root(id)) {
            return None;
        }
        let is_dir = |child: &str| {
            self.tree
                .resource_of
                .get(child)
                .and_then(|document_id| self.document_index.get(document_id))
                .filter(|row| row.node_id == child)
                .map_or_else(|| self.kind_of(child) == NodeKind::Dir, |row| row.is_dir)
        };
        Some(
            self.tree
                .children
                .get(id)
                .into_iter()
                .flatten()
                // One per node, as a listing names each: two folders a conflict left under
                // one name are two.
                .flat_map(|(name, ids)| ids.iter().filter(|child| is_dir(child)).map(move |_| name))
                .cloned()
                .collect(),
        )
    }

    /// The path a catalogue row's node sits at, derived from where the node is now: computed
    /// for the rows a page shows, never stored per file.
    #[must_use]
    pub fn path_for_document(&self, entry: &IndexedDocument) -> Option<String> {
        self.path_for_id(&entry.node_id)
    }

    /// Opaque generation captured when the index changed. Reading it is O(1): serving a
    /// manifest or page must never enumerate Automerge heads to validate a cursor.
    #[must_use]
    pub fn indexed_document_revision(&self) -> &str {
        &self.document_index_revision
    }

    /// O(1): derived from the row count and the incrementally maintained row digest, so a
    /// create does not rehash the whole catalogue.
    pub(super) fn update_document_index_revision(&mut self) {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo.fs.namespace-record-index.v2\0");
        hasher.update(&(self.document_index.len() as u64).to_be_bytes());
        for limb in self.document_index_digest.0 {
            hasher.update(&limb.to_le_bytes());
        }
        self.document_index_revision = hasher.finalize().to_hex().to_string();
    }

    /// Bring one node's catalogue row in line with the document: a create, a rename, a move and
    /// a trash all end here, so each costs the one node instead of a walk over the namespace.
    pub(super) fn index_refresh_node(&mut self, id: &str) {
        if id == ROOT_ID {
            return;
        }
        let Some(document_id) = self.document_id_for_id(id) else {
            return;
        };
        self.index_put_row(id, document_id, |namespace| {
            namespace.reaches_root(id).then(|| IndexedDocument {
                document_id,
                node_id: id.to_string(),
                parent_id: namespace.parent_of(id).unwrap_or_default(),
                name: namespace.name_of(id).unwrap_or_default(),
                is_dir: namespace.kind_of(id) == NodeKind::Dir,
            })
        });
    }

    /// Bring node `id`'s catalogue row, of record `document_id`, in line with what `row` reads
    /// of it: the node's row where it reaches the root, else none. A node just made hands over
    /// the row it wrote rather than reading each field of it back from the document.
    pub(super) fn index_put_row(
        &mut self,
        id: &str,
        document_id: FileDocumentId,
        row: impl FnOnce(&Self) -> Option<IndexedDocument>,
    ) {
        let existing = self.document_index.get(&document_id).cloned();
        if let Some(row) = existing.as_ref().filter(|row| row.node_id != id) {
            // A duplicate durable identity is withheld until a full reconciliation can represent
            // the ambiguity fail-closed.
            let row = row.clone();
            self.index_drop_row(&row);
            self.document_index_ambiguous.insert(document_id);
            self.update_document_index_revision();
            return;
        }
        let desired = if self.document_index_ambiguous.contains(&document_id) {
            None
        } else {
            row(self)
        };
        if existing == desired {
            return;
        }
        if let Some(row) = existing {
            self.index_drop_row(&row);
        }
        if let Some(row) = desired {
            self.index_admit_row(row);
        }
        self.update_document_index_revision();
    }

    /// Re-path `root` and every node below it after a placement change, so a moved subtree
    /// costs its own size and nothing else in the catalogue is touched.
    pub(super) fn index_refresh_subtree(&mut self, root: &str) {
        let mut visited = HashSet::from([root.to_string()]);
        let mut queue = VecDeque::from([root.to_string()]);
        while let Some(id) = queue.pop_front() {
            queue.extend(
                self.tree
                    .children
                    .get(&id)
                    .into_iter()
                    .flat_map(BTreeMap::values)
                    .flat_map(BTreeSet::iter)
                    .filter(|child| visited.insert((*child).clone()))
                    .cloned(),
            );
            self.index_refresh_node(&id);
        }
    }

    pub(super) fn index_drop_row(&mut self, row: &IndexedDocument) {
        self.document_index.remove(&row.document_id);
        self.document_index_digest.remove(row);
    }

    pub(super) fn index_admit_row(&mut self, row: IndexedDocument) {
        self.document_index_digest.add(&row);
        self.document_index.insert(row.document_id, row);
    }

    #[cfg(test)]
    #[must_use]
    pub fn document_index_rebuild_count(&self) -> u64 {
        self.document_index_rebuilds
    }

    #[cfg(test)]
    #[must_use]
    pub fn document_index_page_read_count(&self) -> u64 {
        self.document_index_page_reads
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    #[must_use]
    pub fn node_entry_read_count(&self) -> u64 {
        self.node_entry_reads
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}
