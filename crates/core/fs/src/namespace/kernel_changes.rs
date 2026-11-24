//! What a change to the namespace means for the kernel's view of the mount: which
//! nodes and directories changed, read from the document's own changes, so the
//! cost follows the changes and not the tree.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use super::{Namespace, ROOT_ID};
use automerge::legacy::Key;
use automerge::{AutoCommit, ChangeHash, ObjId, Prop, ReadDoc, Value};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct KernelChanges {
    pub nodes: BTreeSet<u64>,
    pub directories: BTreeSet<u64>,
}

#[derive(Clone)]
enum Owner {
    /// An operation on the `nodes` map adds or removes the node its key names.
    Nodes,
    Node(String),
}

impl Namespace {
    /// Which nodes and directories changed since `heads`, limited to those the
    /// mount has given an inode.
    pub fn kernel_changes_since(&mut self, heads: &[ChangeHash]) -> KernelChanges {
        let mut touched: BTreeSet<String> = BTreeSet::new();
        let mut before: HashMap<String, String> = HashMap::new();
        for chunk in self.doc.chunk_numbers() {
            let Some(nodes) = self.doc.chunk(chunk).map(|held| held.nodes.clone()) else {
                continue;
            };
            let Some(doc) = self.doc.doc_mut(chunk) else {
                continue;
            };
            let known: Vec<ChangeHash> = heads
                .iter()
                .copied()
                .filter(|hash| doc.has_change(hash))
                .collect();
            let changes = doc.get_changes(&known);
            let doc = &**doc;
            let mut owners: HashMap<String, Option<Owner>> = HashMap::new();
            let mut here: BTreeSet<String> = BTreeSet::new();
            for change in changes {
                for op in change.decode().operations {
                    let owner = owners
                        .entry(op.obj.to_string())
                        .or_insert_with_key(|object| owner_of(doc, &nodes, object));
                    match (owner, op.key) {
                        (Some(Owner::Nodes), Key::Map(id)) => {
                            here.insert(id.to_string());
                        }
                        (Some(Owner::Node(id)), _) => {
                            here.insert(id.clone());
                        }
                        _ => {}
                    }
                }
            }
            for id in &here {
                if let Some(parent) = parent_at(doc, &nodes, id, &known) {
                    before.insert(id.clone(), parent);
                }
            }
            touched.extend(here);
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
            for parent in now.into_iter().chain(before.get(id).cloned()) {
                if let Some(&ino) = self.id_to_ino.get(&parent) {
                    changes.directories.insert(ino);
                }
            }
        }
        changes
    }

    /// The path of `ino` below the mount, from its raw name bytes; empty for the
    /// root, `None` when unknown or not reaching the root.
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
            names.push(crate::node::host_name(name.as_bytes()));
            cur = parent;
        }
        Some(names.into_iter().rev().collect())
    }
}

fn owner_of(doc: &AutoCommit, nodes: &ObjId, object: &str) -> Option<Owner> {
    let object = doc.import_obj(object).ok()?;
    if object == *nodes {
        return Some(Owner::Nodes);
    }
    let mut parents = doc.parents(&object).ok()?;
    parents.find_map(|parent| match parent.prop {
        Prop::Map(id) if parent.obj == *nodes => Some(Owner::Node(id)),
        _ => None,
    })
}

/// The parent node `id` had at `heads` of its chunk's document.
fn parent_at(doc: &AutoCommit, nodes: &ObjId, id: &str, heads: &[ChangeHash]) -> Option<String> {
    let (Value::Object(_), node) = doc.get_at(nodes, id, heads).ok()?? else {
        return None;
    };
    let (parent, _) = doc.get_at(&node, "parent", heads).ok()??;
    parent.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::ROOT_INO;

    fn listed(ns: &mut Namespace, dir: u64) {
        let _ = ns.readdir(dir);
    }

    tilo_test_env::scenario! {
        fn kernel_changes_name_exactly_what_a_change_touched;

        phase a_merged_creation_names_the_directory_it_lands_in;
        {
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

        phase a_move_names_both_directories_and_the_node;
        {
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

        phase nothing_changed_names_nothing;
        {
            let mut ns = Namespace::new("idle").unwrap();
            ns.create_dir(ROOT_INO, "docs", 1).unwrap();
            let heads = ns.heads();
            assert_eq!(ns.kernel_changes_since(&heads), KernelChanges::default());
        }
    }
}
