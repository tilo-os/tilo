//! `files.tree` — a workspace's files as the record that carries them.
//!
//! The filesystem namespace is an Automerge document (`crates/system/fs`):
//! one node object per file and folder, keyed by a node id that survives a
//! rename, with the move-tree CRDT's `parent` register so that create,
//! rename, move and delete are edits peers merge rather than commands a
//! server orders. This module is that document's resource type: the same
//! bytes read as a [`Resource`], so a workspace's tree is an ordinary record
//! — carried to the person's devices and to a shared workspace's members by
//! the record engine, with history and permissions — instead of a plane of
//! its own.
//!
//! [`FilesTree::read`] is the projection (what a Files view renders). The
//! daemon writes the same document through its namespace API as mount
//! operations happen, so the record has no action vocabulary: a change to it
//! is the filesystem's own, made where the files are.

use std::collections::BTreeMap;

use automerge::{ObjId, ObjType, ROOT, ReadDoc as _};
use tilo_automerge::read::ReadTyped as _;
use tilo_resource::{Delta, Doc, ModelError, NoAction, Resource, SharedRecord, Touch};

use crate::files::{FileNameV1, FileNodeKind, FileRecordId};

/// The kind of record a workspace's tree is.
pub const TREE_KIND: &str = "files.tree";

/// The map of node objects, at the document's root.
pub const NODES_KEY: &str = "nodes";
/// The node id of the tree's root.
pub const ROOT_ID: &str = "ROOT";
/// The parent a deleted node is moved under, so that a concurrent child of it
/// is trashed with it rather than left dangling.
pub const TRASH_ID: &str = "TRASH";
/// The stable record identity of the workspace root, on the document root.
pub const ROOT_RECORD_ID_KEY: &str = "root_record_id";
/// A node's stable record identity.
pub const RECORD_ID_KEY: &str = "record_id";
/// The node it sits under.
pub const PARENT_KEY: &str = "parent";
/// The node it sat under before it moved.
pub const PREV_PARENT_KEY: &str = "prev_parent";
/// The name its node reads as now.
pub const NAME_KEY: &str = "name";
/// The name the signed binding asks for, before two records' names settle.
pub const PLACEMENT_NAME_KEY: &str = "placement_name";
/// Whether the node is a file or a folder.
pub const KIND_KEY: &str = "kind";
/// The content blob the node names.
pub const HASH_KEY: &str = "hash";
/// The content's length in bytes.
pub const SIZE_KEY: &str = "size";
/// When the content last changed, in milliseconds since the epoch.
pub const MTIME_KEY: &str = "mtime";
/// Who made the node.
pub const AUTHOR_KEY: &str = "author";
/// The signed content revision the node's bytes come from.
pub const CONTENT_REVISION_KEY: &str = "content_revision_v1";

/// A workspace's tree, as a record's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    /// The stable identity of the workspace root: the same on every device
    /// that holds this tree.
    pub root_record_id: Option<FileRecordId>,
    /// Every node, by node id, in id order.
    pub nodes: BTreeMap<String, TreeEntry>,
}

impl Tree {
    /// The node `id`, when the tree has it.
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&TreeEntry> {
        self.nodes.get(id)
    }

    /// The nodes placed under `parent`, in name order and then id order.
    #[must_use]
    pub fn children(&self, parent: &str) -> Vec<(&str, &TreeEntry)> {
        let mut children: Vec<(&str, &TreeEntry)> = self
            .nodes
            .iter()
            .filter(|(_, node)| node.parent.as_deref() == Some(parent))
            .map(|(id, node)| (id.as_str(), node))
            .collect();
        children.sort_by(|(left_id, left), (right_id, right)| {
            left.name
                .cmp(&right.name)
                .then_with(|| left_id.cmp(right_id))
        });
        children
    }

    /// Whether the node `id` is under the trash: deleted, and out of every
    /// listing.
    #[must_use]
    pub fn trashed(&self, id: &str) -> bool {
        self.node(id)
            .and_then(|node| node.parent.as_deref())
            .is_some_and(|parent| parent == TRASH_ID)
    }

    /// The tree `record` holds, as its saved bytes: what a device that reads a
    /// workspace's Files record rather than writing it sees, such as the host
    /// keeping the files' content. A record no mount has written yet holds no
    /// node.
    ///
    /// # Errors
    ///
    /// `record` is not a saved document, or the nodes it holds are malformed.
    pub fn of_record(record: &[u8]) -> Result<Self, ModelError> {
        let document = automerge::AutoCommit::load(record)?;
        FilesTree::read(&document)
    }
}

/// One file or folder of a [`Tree`], as the document holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    /// The node it sits under; `None` for one the document has not placed.
    pub parent: Option<String>,
    /// Its name, exact bytes: a name is not necessarily UTF-8.
    pub name: FileNameV1,
    /// Whether it is a file or a folder.
    pub kind: FileNodeKind,
    /// The content blob its bytes come from, as the document's hex digest.
    pub hash: Option<String>,
    /// The content's length in bytes.
    pub size: u64,
    /// When its content last changed, in milliseconds since the epoch.
    pub mtime: u64,
    /// Its stable record identity, the same on every device.
    pub record_id: Option<FileRecordId>,
}

/// A workspace's files, published as a record.
///
/// The app that keeps the record declares it with its
/// `ResourceApp::PERSON_RECORDS` (`PersonKind::shared`), and the daemon that
/// mounts the tree joins the same route, so a device's changes reach the
/// person's other devices and the members of a shared workspace.
pub struct FilesTree;

impl Resource for FilesTree {
    const KIND: &'static str = TREE_KIND;
    const LABEL: &'static str = "Files";

    type State = Tree;
    type Action = NoAction;

    fn read(doc: &Doc) -> Result<Tree, ModelError> {
        let Some(nodes) = nodes_of(doc) else {
            // A record no mount has written yet holds no node.
            return Ok(Tree::default());
        };
        let mut tree = Tree {
            root_record_id: record_of(doc, &ROOT, ROOT_RECORD_ID_KEY),
            nodes: BTreeMap::new(),
        };
        for id in doc.keys(&nodes) {
            if let Some(node) = nodes_at(doc, &nodes, &id) {
                tree.nodes.insert(id, entry_of(doc, &node));
            }
        }
        Ok(tree)
    }

    /// A change of the tree is the filesystem's: a node was created, renamed,
    /// moved, written or deleted on the device the files are on. Read back
    /// the nodes it touched, not the whole tree.
    fn refresh(state: &mut Tree, doc: &Doc, delta: &Delta) -> Result<(), ModelError> {
        if delta.is_empty() {
            return Ok(());
        }
        let Some(nodes) = nodes_of(doc) else {
            return Ok(());
        };
        if delta.key(&ROOT, NODES_KEY) == Touch::Written {
            *state = Self::read(doc)?;
            return Ok(());
        }
        if delta.key(&ROOT, ROOT_RECORD_ID_KEY) == Touch::Written {
            state.root_record_id = record_of(doc, &ROOT, ROOT_RECORD_ID_KEY);
        }
        let touched = delta.under(&nodes);
        if touched.is_all() {
            *state = Self::read(doc)?;
            return Ok(());
        }
        for id in touched.keys() {
            match nodes_at(doc, &nodes, id) {
                Some(node) => state.nodes.insert(id.to_owned(), entry_of(doc, &node)),
                None => state.nodes.remove(id),
            };
        }
        Ok(())
    }

    fn copy_state(state: &Tree) -> Option<Tree> {
        Some(state.clone())
    }

    /// Nothing: a tree's nodes are written where the files are, by the daemon
    /// holding the mount, so its genesis is the empty change every device
    /// creates identically. A device that mounts the same workspace writes
    /// the same nodes, under the same ids.
    fn initialize(_doc: &mut Doc) -> Result<(), ModelError> {
        Ok(())
    }
}

impl SharedRecord for FilesTree {
    const OWNER: &'static str = "so.tilo.file-explorer";
}

/// The map of node objects, when the document has one.
fn nodes_of(doc: &Doc) -> Option<ObjId> {
    doc.obj_at(&ROOT, NODES_KEY, ObjType::Map)
}

/// The node object `id` of the `nodes` map, when the map holds one.
fn nodes_at(doc: &Doc, nodes: &ObjId, id: &str) -> Option<ObjId> {
    doc.obj_at(nodes, id, ObjType::Map)
}

/// A name is bytes; a namespace written before names were bytes holds text.
fn name_of(doc: &Doc, node: &ObjId, key: &str) -> Option<FileNameV1> {
    doc.bytes_at(node, key)
        .map(FileNameV1)
        .or_else(|| doc.str_at(node, key).map(FileNameV1::from))
}

/// The stable record identity at `obj[key]`, when it is one.
fn record_of(doc: &Doc, obj: &ObjId, key: &str) -> Option<FileRecordId> {
    doc.str_at(obj, key)
        .and_then(|value| uuid::Uuid::parse_str(&value).ok())
        .map(FileRecordId)
}

/// What the document says of one node.
fn entry_of(doc: &Doc, node: &ObjId) -> TreeEntry {
    TreeEntry {
        parent: doc.str_at(node, PARENT_KEY),
        name: name_of(doc, node, NAME_KEY).unwrap_or_default(),
        kind: match doc.str_at(node, KIND_KEY).as_deref() {
            Some("dir") => FileNodeKind::Directory,
            _ => FileNodeKind::File,
        },
        hash: doc.str_at(node, HASH_KEY),
        size: doc.uint_at(node, SIZE_KEY).unwrap_or_default(),
        mtime: doc.uint_at(node, MTIME_KEY).unwrap_or_default(),
        record_id: record_of(doc, node, RECORD_ID_KEY),
    }
}
