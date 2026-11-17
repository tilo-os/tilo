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
    DOCUMENT_ID_KEY, DocumentIndexDigest, IndexedDocument, NODES_KEY, Namespace, NodeKind, NodeRow,
    NodeWalk, Obj, RECORD_ID_MIGRATION_DOMAIN, ROOT_DOCUMENT_ID_KEY, ROOT_ID, TreeIndex,
    escape_path_name, read_name, str_at,
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
        let mut queue = VecDeque::from([(ROOT_ID.to_string(), String::from("/"))]);
        let mut visited = HashSet::from([ROOT_ID.to_string()]);
        while let Some((parent, parent_path)) = queue.pop_front() {
            for id in children.get(&parent).into_iter().flatten() {
                if !visited.insert(id.clone()) {
                    continue;
                }
                let Some(item) = raw.get(id) else { continue };
                let path = if parent_path == "/" {
                    format!("/{}", escape_path_name(item.name.as_bytes()))
                } else {
                    format!("{parent_path}/{}", escape_path_name(item.name.as_bytes()))
                };
                // Traverse all structurally reachable nodes, even a legacy duplicate-ID node;
                // only the unique stable record is eligible for the public index.
                queue.push_back((id.clone(), path.clone()));
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
                        path,
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
        self.document_index_order = self.document_index.keys().copied().collect();
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
    /// gate bounds.
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
            // A tree whose own record carries no root identity yet: the nodes' ids are derived
            // for the whole tree, which is the load path.
            self.ensure_document_ids();
            self.rebuild_document_index();
            return;
        }
        for id in touched {
            self.ensure_document_id(id);
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

    pub(super) fn ensure_document_id(&mut self, id: &str) {
        let Some(node) = self.node(id) else { return };
        if str_at(&self.doc, &node, DOCUMENT_ID_KEY)
            .and_then(|value| Uuid::parse_str(&value).ok())
            .is_some()
        {
            return;
        }
        let document_id = Self::deterministic_document_id(id);
        let _ = self
            .doc
            .put(&node, DOCUMENT_ID_KEY, document_id.0.to_string());
        self.tree.bind_document(id, document_id);
    }

    /// Snapshot one bounded canonical page, ordered by stable record UUID, cloning at most
    /// `limit` entries.
    pub fn indexed_document_page(&self, offset: usize, limit: usize) -> Vec<IndexedDocument> {
        #[cfg(test)]
        self.document_index_page_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let end = offset
            .saturating_add(limit)
            .min(self.document_index_order.len());
        self.document_index_order
            .get(offset..end)
            .unwrap_or_default()
            .iter()
            .filter_map(|document_id| self.document_index.get(document_id))
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn indexed_document_count(&self) -> usize {
        self.document_index_order.len()
    }

    #[must_use]
    pub fn indexed_document(&self, document_id: FileDocumentId) -> Option<IndexedDocument> {
        self.document_index.get(&document_id).cloned()
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
        hasher.update(&(self.document_index_order.len() as u64).to_be_bytes());
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
            self.path_for_id(id).map(|path| IndexedDocument {
                document_id,
                node_id: id.to_string(),
                parent_id: self.parent_of(id).unwrap_or_default(),
                path,
                is_dir: self.kind_of(id) == NodeKind::Dir,
            })
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
        if let Ok(position) = self.document_index_order.binary_search(&row.document_id) {
            self.document_index_order.remove(position);
        }
    }

    pub(super) fn index_admit_row(&mut self, row: IndexedDocument) {
        self.document_index_digest.add(&row);
        let position = self
            .document_index_order
            .binary_search(&row.document_id)
            .unwrap_or_else(|position| position);
        self.document_index_order.insert(position, row.document_id);
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

    pub(super) fn deterministic_document_id(id: &str) -> FileDocumentId {
        let mut hasher = blake3::Hasher::new();
        hasher.update(RECORD_ID_MIGRATION_DOMAIN);
        // The nodes-map object id is minted once with the namespace and stays stable across
        // save/load and replica merges; including it keeps legacy ids such as `alice/0` from
        // colliding across workspaces.
        hasher.update(NODES_KEY.as_bytes());
        hasher.update(&[0]);
        hasher.update(id.as_bytes());
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
        // Mark deterministic migrations as RFC 4122, version-5-shaped UUIDs.
        bytes[6] = (bytes[6] & 0x0f) | 0x50;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        FileDocumentId(Uuid::from_bytes(bytes))
    }

    /// Populate snapshots written before record-scoped replication; peers migrating the same
    /// legacy namespace independently derive the same ids.
    pub(super) fn ensure_document_ids(&mut self) {
        if self.root_document_id().is_none() {
            let document_id = Self::deterministic_document_id(ROOT_ID);
            let _ = self.doc.put(
                &Obj::index(),
                ROOT_DOCUMENT_ID_KEY,
                document_id.0.to_string(),
            );
        }
        let ids: Vec<_> = self.doc.sorted_node_ids().into_iter().collect();
        for id in ids {
            self.ensure_document_id(&id);
        }
        self.document_ids_complete = true;
    }
}
