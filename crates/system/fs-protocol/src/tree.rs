//! `files.tree` — a workspace's files as the record that carries them.

use std::collections::BTreeMap;

use automerge::{ObjId, ObjType, ROOT, ReadDoc as _};
use tilo_automerge::read::ReadTyped as _;
use tilo_resource::{Delta, Doc, ModelError, NoAction, Resource, SharedRecord, Touch};

use crate::files::{FileNameV1, FileNodeKind, FileRecordId};

pub const TREE_KIND: &str = "files.tree";

/// The app whose record a workspace's tree is: it declares the kind, and a
/// host that keeps the tree reads it as that app's record.
pub const FILES_APP: &str = <FilesTree as SharedRecord>::OWNER;

/// The map of node objects, at the document's root.
pub const NODES_KEY: &str = "nodes";
pub const ROOT_ID: &str = "ROOT";
/// The parent a deleted node is moved under, so that a concurrent child of it
/// is trashed with it rather than left dangling.
pub const TRASH_ID: &str = "TRASH";
pub const ROOT_RECORD_ID_KEY: &str = "root_record_id";
pub const RECORD_ID_KEY: &str = "record_id";
pub const PARENT_KEY: &str = "parent";
pub const PREV_PARENT_KEY: &str = "prev_parent";
pub const NAME_KEY: &str = "name";
pub const PLACEMENT_NAME_KEY: &str = "placement_name";
pub const KIND_KEY: &str = "kind";
pub const HASH_KEY: &str = "hash";
/// The content's length in bytes.
pub const SIZE_KEY: &str = "size";
/// When the content last changed, in milliseconds since the epoch.
pub const MTIME_KEY: &str = "mtime";
pub const AUTHOR_KEY: &str = "author";
/// The signed content revision the node's bytes come from.
pub const CONTENT_REVISION_KEY: &str = "content_revision_v1";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    /// The stable identity of the workspace root: the same on every device
    /// that holds this tree.
    pub root_record_id: Option<FileRecordId>,
    pub nodes: BTreeMap<String, TreeEntry>,
}

impl Tree {
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&TreeEntry> {
        self.nodes.get(id)
    }

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    pub parent: Option<String>,
    /// Its name, exact bytes: a name is not necessarily UTF-8.
    pub name: FileNameV1,
    pub kind: FileNodeKind,
    /// The content blob its bytes come from, as the document's hex digest.
    pub hash: Option<String>,
    /// The content's length in bytes.
    pub size: u64,
    /// When its content last changed, in milliseconds since the epoch.
    pub mtime: u64,
    /// Who last changed it, as the device that wrote the node named them: a
    /// tree nobody has written yet names nobody.
    pub author: Option<String>,
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

fn record_of(doc: &Doc, obj: &ObjId, key: &str) -> Option<FileRecordId> {
    doc.str_at(obj, key)
        .and_then(|value| uuid::Uuid::parse_str(&value).ok())
        .map(FileRecordId)
}

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
        author: doc.str_at(node, AUTHOR_KEY).filter(|name| !name.is_empty()),
        record_id: record_of(doc, node, RECORD_ID_KEY),
    }
}

/// Prefix of a node's key derived from its stable record id, so every device
/// that writes or rebuilds the same record makes the same node object.
pub const RECORD_NODE_ID_PREFIX: &str = "record:";

/// The node key every device derives for the record `record_id`.
#[must_use]
pub fn record_node_id(record_id: FileRecordId) -> String {
    format!("{RECORD_NODE_ID_PREFIX}{}", record_id.0)
}

/// What a device puts into a tree where the files are not on a disk of its
/// own (a browser's page): a folder, or a file whose content the device keeps
/// under `hash`, the hex digest of its content manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placing<'a> {
    Folder,
    File { hash: &'a str, size: u64 },
}

/// One node placed into a tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    /// Its node key.
    pub id: String,
    /// The name it took: the one asked for, or the first free one beside it
    /// ([`free_name`]).
    pub name: String,
}

/// The name `wanted` takes in the folder `parent` of `tree`: itself when no
/// live node there has it, else the first of `name (2).ext`, `name (3).ext`…
/// that is free, as a file manager keeps both when a copy meets a name.
#[must_use]
pub fn free_name(tree: &Tree, parent: &str, wanted: &str) -> String {
    let taken = |name: &str| {
        tree.nodes
            .values()
            .any(|node| node.parent.as_deref() == Some(parent) && node.name == name)
    };
    if !taken(wanted) {
        return wanted.to_owned();
    }
    // A leading dot names a hidden file, not an extension.
    let (stem, extension) = match wanted.rfind('.') {
        Some(dot) if dot > 0 => (
            wanted.get(..dot).unwrap_or(wanted),
            wanted.get(dot..).unwrap_or(""),
        ),
        _ => (wanted, ""),
    };
    // A folder holds fewer names than this, so one of them is free.
    (2..=u64::try_from(tree.nodes.len())
        .unwrap_or(u64::MAX)
        .saturating_add(2))
        .map(|n| format!("{stem} ({n}){extension}"))
        .find(|name| !taken(name))
        .unwrap_or_else(|| wanted.to_owned())
}

/// The live folder named `name` in `parent`, when `tree` holds one.
#[must_use]
pub fn folder_named(tree: &Tree, parent: &str, name: &str) -> Option<String> {
    tree.children(parent)
        .into_iter()
        .find(|(_, node)| node.kind == FileNodeKind::Directory && node.name == name)
        .map(|(id, _)| id.to_owned())
}

/// Put `placing` named `name` into the folder `parent` of the tree `doc`
/// holds, written as the filesystem writes a node it creates, so a mounted
/// copy of the tree shows it as a file or folder of its own; `tree` is what
/// `doc` holds and takes the node too, so the next placement sees it. A name
/// the folder already has is kept beside it ([`free_name`]).
///
/// # Errors
///
/// A name no file can have (empty, `.`, `..`, or with a `/`), a parent the
/// tree does not hold, or a document that refused the write.
pub fn place(
    doc: &mut Doc,
    tree: &mut Tree,
    parent: &str,
    name: &str,
    placing: Placing<'_>,
    (mtime, author): (u64, &str),
) -> Result<Placed, ModelError> {
    use automerge::transaction::Transactable as _;
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(ModelError::invalid(format!("{name:?} is no file's name")));
    }
    if parent != ROOT_ID && (tree.node(parent).is_none() || tree.trashed(parent)) {
        return Err(ModelError::invalid("the folder to put it in is gone"));
    }
    let name = free_name(tree, parent, name);
    let nodes = match nodes_of(doc) {
        Some(nodes) => nodes,
        None => doc.put_object(&ROOT, NODES_KEY, ObjType::Map)?,
    };
    let record_id = FileRecordId::new();
    let id = record_node_id(record_id);
    let node = doc.put_object(&nodes, &id, ObjType::Map)?;
    doc.put(&node, PARENT_KEY, parent)?;
    doc.put(&node, PREV_PARENT_KEY, parent)?;
    doc.put(&node, NAME_KEY, name.as_bytes().to_vec())?;
    doc.put(&node, PLACEMENT_NAME_KEY, name.as_bytes().to_vec())?;
    let (kind, hash, size) = match placing {
        Placing::Folder => (FileNodeKind::Directory, None, 0),
        Placing::File { hash, size } => (FileNodeKind::File, Some(hash), size),
    };
    doc.put(
        &node,
        KIND_KEY,
        if kind == FileNodeKind::Directory {
            "dir"
        } else {
            "file"
        },
    )?;
    if let Some(hash) = hash {
        doc.put(&node, HASH_KEY, hash)?;
    }
    doc.put(&node, SIZE_KEY, size)?;
    doc.put(&node, MTIME_KEY, mtime)?;
    doc.put(&node, AUTHOR_KEY, author)?;
    doc.put(&node, RECORD_ID_KEY, record_id.0.to_string())?;
    tree.nodes.insert(
        id.clone(),
        TreeEntry {
            parent: Some(parent.to_owned()),
            name: FileNameV1::from(name.as_str()),
            kind,
            hash: hash.map(str::to_owned),
            size,
            mtime,
            record_id: Some(record_id),
            author: Some(author.to_owned()),
        },
    );
    Ok(Placed { id, name })
}

#[cfg(test)]
mod placing_tests {
    use super::*;

    fn read(doc: &Doc) -> Tree {
        FilesTree::read(doc).unwrap()
    }

    #[test]
    fn a_placed_folder_and_file_read_back_as_the_filesystem_writes_them_and_names_keep_both() {
        let mut doc = Doc::new();
        let mut tree = Tree::default();
        let docs = place(
            &mut doc,
            &mut tree,
            ROOT_ID,
            "docs",
            Placing::Folder,
            (5, "page"),
        )
        .unwrap();
        let hash = "ab".repeat(32);
        let file = Placing::File {
            hash: &hash,
            size: 12,
        };
        let plan = place(&mut doc, &mut tree, &docs.id, "plan.md", file, (6, "page")).unwrap();
        let again = place(&mut doc, &mut tree, &docs.id, "plan.md", file, (7, "page")).unwrap();
        let hidden = place(&mut doc, &mut tree, &docs.id, ".env", file, (7, "page")).unwrap();
        let hidden_again = place(&mut doc, &mut tree, &docs.id, ".env", file, (7, "page")).unwrap();
        assert_eq!(again.name, "plan (2).md");
        assert_eq!(hidden_again.name, ".env (2)");
        assert_eq!(read(&doc), tree, "what was written is what the tree holds");
        let node = tree.node(&plan.id).unwrap();
        assert_eq!(node.kind, FileNodeKind::File);
        assert_eq!(node.hash.as_deref(), Some(hash.as_str()));
        assert_eq!(node.size, 12);
        assert_eq!(plan.id, record_node_id(node.record_id.unwrap()));
        assert_eq!(tree.node(&docs.id).unwrap().kind, FileNodeKind::Directory);
        assert_eq!(folder_named(&tree, ROOT_ID, "docs"), Some(docs.id.clone()));
        assert_eq!(folder_named(&tree, ROOT_ID, "plan.md"), None);
        assert_eq!(tree.children(&docs.id).len(), 4);
        assert_eq!(hidden.name, ".env");
    }

    #[test]
    fn a_name_no_file_has_or_a_folder_that_is_gone_is_refused() {
        let mut doc = Doc::new();
        let mut tree = Tree::default();
        for name in ["", ".", "..", "a/b"] {
            assert!(
                place(
                    &mut doc,
                    &mut tree,
                    ROOT_ID,
                    name,
                    Placing::Folder,
                    (0, "p")
                )
                .is_err()
            );
        }
        assert!(
            place(
                &mut doc,
                &mut tree,
                "record:gone",
                "a",
                Placing::Folder,
                (0, "p")
            )
            .is_err()
        );
    }

    #[test]
    fn a_page_placing_into_a_tree_a_mount_wrote_keeps_the_mounts_nodes() {
        // The mount's tree, with its nodes map and a file in place.
        let mut mount = Doc::new();
        let mut mount_tree = Tree::default();
        place(
            &mut mount,
            &mut mount_tree,
            ROOT_ID,
            "kept.txt",
            Placing::File {
                hash: "00",
                size: 1,
            },
            (1, "fs"),
        )
        .unwrap();
        let mut page = mount.fork();
        let mut page_tree = read(&page);
        place(
            &mut page,
            &mut page_tree,
            ROOT_ID,
            "kept.txt",
            Placing::File {
                hash: "11",
                size: 2,
            },
            (2, "page"),
        )
        .unwrap();
        tilo_automerge::ApplyIncoming::merge_incoming(&mut mount, &mut page).unwrap();
        let names: Vec<String> = read(&mount)
            .children(ROOT_ID)
            .iter()
            .map(|(_, node)| String::from_utf8_lossy(node.name.as_bytes()).into_owned())
            .collect();
        assert_eq!(names, ["kept (2).txt", "kept.txt"]);
    }
}
