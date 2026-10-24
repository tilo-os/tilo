//! What a change to the namespace means for the kernel's view of the mount.
//!
//! The kernel knows of a change made through the mount, and tells inotify.
//! A change it did not make (one merged from a peer, made over the control
//! socket, or a verified content revision installed beside the document)
//! reaches it only when the daemon says so ([`crate::change_signal`]). This
//! reads which nodes those were, and which directories' entries they
//! touched, from the document's own changes: the cost follows the size of
//! the changes, not of the tree.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use automerge::legacy::Key;
use automerge::{ChangeHash, Prop, ReadDoc, Value};
use fs_protocol::FileRecordId;

use super::{Namespace, ROOT_ID};

/// What changed since some heads, as the kernel sees the mount.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct KernelChanges {
    /// Inodes of the nodes that changed: their cached attributes and pages
    /// are stale.
    pub nodes: BTreeSet<u64>,
    /// Inodes of the directories whose entries changed: an entry was added,
    /// removed, renamed, moved in or out, or itself changed.
    pub directories: BTreeSet<u64>,
}

/// Where an object of the document sits in the namespace.
#[derive(Clone)]
enum Owner {
    /// The `nodes` map itself: an operation on it adds or removes the node
    /// its key names.
    Nodes,
    /// Inside the node with this id.
    Node(String),
}

impl Namespace {
    /// Which nodes and directories changed between `heads` and now,
    /// limited to those the mount has given an inode: anything else, the
    /// kernel has never been told of. Changes are read from the document's
    /// operations since `heads`, so a change to a document's content counts
    /// for the file that holds it.
    pub fn kernel_changes_since(&mut self, heads: &[ChangeHash]) -> KernelChanges {
        let mut touched: BTreeSet<String> = self
            .projected_changes
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        let mut owners: HashMap<String, Option<Owner>> = HashMap::new();
        for change in self.doc.get_changes(heads) {
            for op in change.decode().operations {
                let owner = owners
                    .entry(op.obj.to_string())
                    .or_insert_with_key(|object| self.owner_of(object));
                match (owner, op.key) {
                    (Some(Owner::Nodes), Key::Map(id)) => {
                        touched.insert(id.to_string());
                    }
                    (Some(Owner::Node(id)), _) => {
                        touched.insert(id.clone());
                    }
                    _ => {}
                }
            }
        }
        let mut changes = KernelChanges::default();
        for id in &touched {
            if let Some(&ino) = self.id_to_ino.get(id) {
                changes.nodes.insert(ino);
            }
            let now = self
                .tree
                .placement
                .get(id)
                .map(|(parent, _)| parent.clone());
            for parent in now.into_iter().chain(self.parent_at(id, heads)) {
                if let Some(&ino) = self.id_to_ino.get(&parent) {
                    changes.directories.insert(ino);
                }
            }
        }
        changes
    }

    /// The node an object of the document belongs to.
    fn owner_of(&self, object: &str) -> Option<Owner> {
        let object = self.doc.import_obj(object).ok()?;
        if object == self.nodes {
            return Some(Owner::Nodes);
        }
        let mut parents = self.doc.parents(&object).ok()?;
        parents.find_map(|parent| match parent.prop {
            Prop::Map(id) if parent.obj == self.nodes => Some(Owner::Node(id)),
            _ => None,
        })
    }

    /// The parent node `id` had at `heads`.
    fn parent_at(&self, id: &str, heads: &[ChangeHash]) -> Option<String> {
        let (Value::Object(_), node) = self.doc.get_at(&self.nodes, id, heads).ok()?? else {
            return None;
        };
        let (parent, _) = self.doc.get_at(&node, "parent", heads).ok()??;
        parent.as_str().map(str::to_owned)
    }

    /// The path of `ino` below the mount as the kernel names it, from the
    /// raw name bytes; empty for the root. `None` when it is unknown or does
    /// not reach the root.
    #[must_use]
    pub fn kernel_path_for(&self, ino: u64) -> Option<PathBuf> {
        let mut cur = self.ino_to_id.get(&ino)?.as_str();
        let mut names = Vec::new();
        let mut hops = self.tree.placement.len().saturating_add(1);
        while cur != ROOT_ID {
            if hops == 0 {
                return None;
            }
            hops = hops.saturating_sub(1);
            let (parent, name) = self.tree.placement.get(cur)?;
            names.push(OsStr::from_bytes(name.as_bytes()));
            cur = parent;
        }
        Some(names.into_iter().rev().collect())
    }

    /// From now on, note the nodes whose verified projection changes beside
    /// the document, for [`Self::kernel_changes_since`], and announce them.
    pub fn collect_projected_changes(&mut self) {
        self.projected_changes.get_or_insert_default();
    }

    /// Stop noting them, when nothing drains them any more.
    pub fn stop_collecting_projected_changes(&mut self) {
        self.projected_changes = None;
    }

    /// The verified projection of `record_id` changed without the document.
    pub(super) fn note_projected_change(&mut self, record_id: FileRecordId) {
        if self.projected_changes.is_none() {
            return;
        }
        let Some(id) = self.unique_live_node_for_record(record_id) else {
            return;
        };
        if let Some(changes) = self.projected_changes.as_mut() {
            changes.insert(id);
        }
        self.doc.announce();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::ROOT_INO;

    fn listed(ns: &mut Namespace, dir: u64) {
        let _ = ns.readdir(dir);
    }

    #[test]
    fn a_merged_creation_names_the_directory_it_lands_in() {
        let mut ours = Namespace::new("ours").unwrap();
        let docs = ours.create_dir(ROOT_INO, "docs", 1).unwrap();
        let mut theirs = Namespace::from_bytes("theirs", &ours.save()).unwrap();
        listed(&mut ours, ROOT_INO);
        listed(&mut ours, docs);
        let before = ours.heads();

        let their_docs = theirs.resolve_path_ino("/docs").unwrap();
        theirs
            .create_file(their_docs, "new.md", None, 3, 2)
            .unwrap();
        ours.merge_doc(&mut theirs).unwrap();

        let changes = ours.kernel_changes_since(&before);
        assert_eq!(changes.directories, BTreeSet::from([docs]));
        assert_eq!(ours.kernel_path_for(docs), Some(PathBuf::from("docs")));
        assert_eq!(ours.kernel_path_for(ROOT_INO), Some(PathBuf::new()));
    }

    #[test]
    fn a_move_names_both_directories_and_the_node() {
        let mut ns = Namespace::new("mover").unwrap();
        let from = ns.create_dir(ROOT_INO, "from", 1).unwrap();
        let to = ns.create_dir(ROOT_INO, "to", 1).unwrap();
        let file = ns.create_file(from, "a.md", None, 1, 1).unwrap();
        let before = ns.heads();

        assert!(ns.rename(from, "a.md", to, "b.md", 2));

        let changes = ns.kernel_changes_since(&before);
        assert_eq!(changes.directories, BTreeSet::from([from, to]));
        assert_eq!(changes.nodes, BTreeSet::from([file]));
        assert_eq!(ns.kernel_path_for(file), Some(PathBuf::from("to/b.md")));
    }

    #[test]
    fn nothing_changed_names_nothing() {
        let mut ns = Namespace::new("idle").unwrap();
        ns.create_dir(ROOT_INO, "docs", 1).unwrap();
        let heads = ns.heads();
        assert_eq!(ns.kernel_changes_since(&heads), KernelChanges::default());
    }
}
