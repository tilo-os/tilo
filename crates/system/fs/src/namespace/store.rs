//! The namespace as what the workspace's store holds: the rows a folder's page brings, applied
//! as the verbs they are, and the verbs made here, taken out to be written.
//!
//! The namespace is this device's copy of the tree for the mount; the store is the tree itself.
//! A verb a person or a program makes here is written into the copy and handed to the store as
//! the verb it is; a row the store answers with is written into the copy with collecting off, so
//! it is never handed back.

use fs_protocol::FileAction;
use fs_protocol::files::{FileNameV1, FileNodeKind};
use fs_protocol::tree::ROOT_ID;

use super::{Namespace, NodeKind};
use crate::files_sync::NodeRow;

impl Namespace {
    /// Start or stop collecting the verbs made here for the workspace's store. Off while the
    /// store's own rows are being applied.
    pub fn set_collecting(&mut self, collecting: bool) {
        self.collecting = collecting;
    }

    /// The verbs made here since the store was last told, in the order they were made.
    pub fn take_actions(&mut self) -> Vec<FileAction> {
        std::mem::take(&mut self.pending)
    }

    /// Put verbs the store did not take back at the front of what it is told next, in their
    /// order, so nothing made here is lost to a refusal.
    pub fn restore_actions(&mut self, mut actions: Vec<FileAction>) {
        actions.append(&mut self.pending);
        self.pending = actions;
    }

    /// Whether verbs made here wait to be told to the store.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Apply `action` without collecting it: what the store's own rows are applied with.
    fn apply_quietly(&mut self, action: &FileAction) -> bool {
        let collecting = self.collecting;
        self.collecting = false;
        let written = self.write_action(action);
        self.collecting = collecting;
        written
    }

    /// Write the store's row for one node into this copy: make it where the copy has none, move
    /// it where it stands elsewhere, give it what the store says it holds, and put it in the
    /// trash or back out of it. Whether the copy changed.
    pub fn apply_store_row(&mut self, row: &NodeRow) -> bool {
        if row.key == ROOT_ID {
            return false;
        }
        let name = FileNameV1(
            row.raw_name
                .clone()
                .unwrap_or_else(|| row.name.clone().into_bytes()),
        );
        let parent = if row.parent.is_empty() {
            ROOT_ID.to_owned()
        } else {
            row.parent.clone()
        };
        let kind = if row.folder {
            FileNodeKind::Directory
        } else {
            FileNodeKind::File
        };
        let Some(held) = self.entry_for_node(&row.key) else {
            let document_id = fs_protocol::files::FileDocumentId::new();
            let made = self.apply_quietly(&FileAction::Create {
                node: row.key.clone(),
                document_id,
                parent: parent.clone(),
                name: name.clone(),
                kind,
                hash: row.hash.clone(),
                size: row.size,
                mtime: row.mtime,
                author: row.author.clone().unwrap_or_default(),
            });
            if made {
                self.tree.place(&row.key, parent, name);
                self.tree.bind_document(&row.key, document_id);
                self.index_refresh_node(&row.key);
            }
            return made;
        };
        let mut changed = false;
        if (held.kind == NodeKind::Dir) != row.folder {
            // A node whose kind changed is another node: the store's is made where the old one
            // was.
            self.forget_store_node(&row.key);
            return self.apply_store_row(row);
        }
        if held.parent != parent || held.name != name {
            let moved = self.apply_quietly(&FileAction::Move {
                node: row.key.clone(),
                parent: parent.clone(),
                name: name.clone(),
                mtime: row.mtime,
            });
            if moved {
                self.tree.place(&row.key, parent, name);
                self.index_refresh_node(&row.key);
            }
            changed |= moved;
        }
        let hash = row.hash.as_deref().and_then(super::hash_from_hex);
        if held.hash != hash || held.size != row.size || held.mtime != row.mtime {
            changed |= self.apply_quietly(&FileAction::Write {
                node: row.key.clone(),
                hash: row.hash.clone(),
                size: row.size,
                mtime: row.mtime,
                author: row.author.clone().unwrap_or_default(),
            });
            self.index_refresh_node(&row.key);
        }
        changed
    }

    /// Forget a node the store no longer holds.
    pub fn forget_store_node(&mut self, key: &str) -> bool {
        if key == ROOT_ID || self.entry_for_node(key).is_none() {
            return false;
        }
        let Some(entry) = self.entry_for_node(key) else {
            return false;
        };
        if !self.doc.delete_node(key) {
            return false;
        }
        self.tree.remove(key);
        self.tree.unbind_document(key);
        self.document_index.remove(&entry.document_id);
        if let Some(ino) = self.id_to_ino.remove(key) {
            self.ino_to_id.remove(&ino);
        }
        true
    }

    /// The ids of the nodes directly under `parent` in this copy.
    #[must_use]
    pub fn child_node_ids(&self, parent: &str) -> Vec<String> {
        self.children(parent)
    }

    /// Every node id this copy holds.
    #[must_use]
    pub fn all_node_ids(&self) -> Vec<String> {
        self.tree.placement.keys().cloned().collect()
    }

    /// How many files and folders the tree holds, out of the trash, and the bytes they name.
    #[must_use]
    pub fn live_totals(&self) -> (usize, u64) {
        let mut files = 0;
        let mut bytes = 0_u64;
        for id in self.tree.placement.keys() {
            if id == ROOT_ID || self.is_directly_trashed(id) {
                continue;
            }
            let Some(meta) = self.node_meta(id) else {
                continue;
            };
            files += 1;
            if meta.kind == NodeKind::File {
                bytes = bytes.saturating_add(meta.size);
            }
        }
        (files, bytes)
    }

    /// The node a row names, as this copy holds it, when it does.
    #[must_use]
    pub fn store_row_held(&self, key: &str) -> bool {
        self.entry_for_node(key).is_some()
    }
}
