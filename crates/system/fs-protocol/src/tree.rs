//! `files.tree` — a workspace's files as the records that carry them: the tree's own
//! record and its chunks ([`CHUNK_NODES`]).

use std::collections::{BTreeMap, BTreeSet};

use automerge::{ObjId, ObjType, ROOT, ReadDoc};
use tilo_automerge::read::ReadTyped as _;
use tilo_resource::{Delta, Doc, ModelError, NoAction, Resource, SharedResource, Touch};

use crate::files::{FIRST_TREE_CHUNK, FileDocumentId, FileNameV1, FileNodeKind, TreeChunk};

pub const TREE_KIND: &str = "files.tree";

/// Nodes a chunk of the tree holds before new nodes start the next one.
///
/// A workspace's tree is kept in chunks, each a record of its own
/// ([`chunk_document_id`]), so that writing a file costs what its chunk costs and not what the
/// folder holds: a write is one change of one chunk, and a holder that keeps a record compacts,
/// seals, stores and sends one chunk. A node lives in the chunk its record id names
/// ([`FileDocumentId::chunk`]), chosen when it is made: the newest chunk while it has room, so
/// the files of one checkout fill one chunk after another, and an edit, rename or delete of a
/// file is a change of the chunk it was made in. A node never moves to another chunk.
///
/// A thousand nodes with a few versions each is about a megabyte of record: small enough that
/// compacting or sending a chunk whole costs milliseconds, and few enough records that a
/// folder of fifty thousand files is fifty of them.
pub const CHUNK_NODES: usize = 1024;

/// The map of the tree's own record naming each chunk that holds nodes, by its number in
/// decimal: what a device that holds the tree's record opens next.
pub const CHUNKS_KEY: &str = "chunks";

/// The record chunk `chunk` of the tree is, as every device derives it: the tree's own
/// record for chunk 0, which holds the root's identity and the list of chunks; for every
/// other, the record `derived:chunk.<n>@<tree record>`, as the platform names the chunks of
/// any record.
#[must_use]
pub fn chunk_document_id(chunk: TreeChunk) -> String {
    if chunk == 0 {
        return document_id();
    }
    format!("derived:chunk.{chunk}@{}", document_id())
}

/// The chunk of the tree `record` is, or `None` for a record that is none of them.
#[must_use]
pub fn chunk_of_record(record: &str) -> Option<TreeChunk> {
    let tree = document_id();
    if record == tree {
        return Some(0);
    }
    let number = record
        .strip_prefix("derived:chunk.")?
        .strip_suffix(tree.as_str())?
        .strip_suffix('@')?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) || number.starts_with('0') {
        return None;
    }
    number.parse().ok()
}

/// The chunks the tree's own record `doc` lists, in order.
#[must_use]
pub fn listed_chunks<D: ReadDoc>(doc: &D) -> Vec<TreeChunk> {
    let Some(chunks) = doc.obj_at(&ROOT, CHUNKS_KEY, ObjType::Map) else {
        return Vec::new();
    };
    let mut listed: Vec<TreeChunk> = doc
        .keys(&chunks)
        .filter_map(|key| key.parse().ok())
        .filter(|chunk| *chunk >= FIRST_TREE_CHUNK)
        .collect();
    listed.sort_unstable();
    listed
}

/// List `chunk` in the tree's own record `doc`; whether it was not listed yet.
///
/// # Errors
/// The document refused the change.
pub fn list_chunk(doc: &mut Doc, chunk: TreeChunk) -> Result<bool, automerge::AutomergeError> {
    use automerge::transaction::Transactable as _;
    let chunks = root_map(doc, CHUNKS_KEY)?;
    let key = chunk.to_string();
    if doc.get(&chunks, key.as_str())?.is_some() {
        return Ok(false);
    }
    doc.put(&chunks, key, true)?;
    Ok(true)
}

/// The chunk a new node goes to, given the newest chunk (`None` when there is none yet) and
/// how many nodes it holds: the newest while it has room, else the one after it.
#[must_use]
pub const fn chunk_for_new(newest: Option<TreeChunk>, nodes: usize) -> TreeChunk {
    match newest {
        None => FIRST_TREE_CHUNK,
        Some(newest) if nodes >= CHUNK_NODES => newest.saturating_add(1),
        Some(newest) => newest,
    }
}

/// The record a workspace's tree travels as, under the Files app: every device
/// derives it from the kind alone.
#[must_use]
pub fn document_id() -> String {
    collab_types::person_document_id(TREE_KIND)
}

/// The app whose record a workspace's tree is: it declares the kind, and a
/// host that keeps the tree reads it as that app's record.
pub const FILES_APP: &str = <FilesTree as SharedResource>::OWNER;

/// The verbs of Files that act on a workspace's own folder by a path in the workspace
/// (`Tasks/12 Plan/notes.md`), whoever holds its tree: a workspace's server while every device
/// of its people is off, or the device that keeps the workspace. The same verbs naming a
/// `device` and absolute paths change that device's own disk, and only that device carries
/// them out.
pub const TREE_VERBS: [&str; 6] = [
    "new-folder",
    "rename",
    "move",
    "copy-into",
    "trash",
    "restore",
];

/// Whether `action` is one of [`TREE_VERBS`].
#[must_use]
pub fn is_tree_verb(action: &str) -> bool {
    TREE_VERBS.contains(&action)
}

/// The read of Files that answers what a folder of the workspace holds, by a path in the
/// workspace: names, kinds, sizes and changed times, a page at a time; or what its trash holds.
pub const LIST: &str = "list";

/// The read of Files that answers what a file of the workspace says, by a path in the
/// workspace, a page at a time.
pub const READ_TEXT: &str = "read-text";

/// Whether `action` is one of the reads of a workspace's own folder by a path in the workspace
/// ([`LIST`], [`READ_TEXT`]), which whoever holds its tree answers as it carries out
/// [`TREE_VERBS`]: with no window, and with every device of its people off.
#[must_use]
pub fn is_tree_read(action: &str) -> bool {
    matches!(action, LIST | READ_TEXT)
}

/// The map of node objects, at the document's root.
pub const NODES_KEY: &str = "nodes";

/// The tree's map of node objects in `doc`, made when the document has none
/// ([`root_map`]).
///
/// # Errors
/// The document refused the change, or holds something else under the key.
pub fn tree_nodes(doc: &mut Doc) -> Result<ObjId, automerge::AutomergeError> {
    root_map(doc, NODES_KEY)
}

/// The map at `key` of `doc`'s root, made when the document has none.
///
/// Every replica makes it with the same change, byte for byte: one writer
/// named for the key, no dependencies, the time zero. A computer that wrote
/// files before it ever reached the workspace, a page that put the first file
/// in and a host that put one for an agent each hold that one change, so their
/// trees merge into one map. A map each made on its own would be the same key
/// written twice, and the merge would show one of them: every file of the
/// other gone.
///
/// # Errors
/// The document refused the change, or holds something else under the key.
pub fn root_map(doc: &mut Doc, key: &str) -> Result<ObjId, automerge::AutomergeError> {
    use automerge::transaction::Transactable as _;
    use tilo_automerge::ApplyIncoming as _;
    if let Some(map) = doc.obj_at(&ROOT, key, ObjType::Map) {
        return Ok(map);
    }
    let writer = format!("tilo.files.tree.v1/{key}");
    let mut made = Doc::new().with_actor(automerge::ActorId::from(writer.as_bytes()));
    made.put_object(&ROOT, key, ObjType::Map)?;
    made.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Structure.change_message())
            .with_time(0),
    );
    doc.merge_incoming(&mut made)?;
    doc.obj_at(&ROOT, key, ObjType::Map)
        .ok_or_else(|| automerge::AutomergeError::InvalidObjId(key.to_owned()))
}
pub const ROOT_ID: &str = "ROOT";
/// The parent a deleted node is moved under, so that a concurrent child of it
/// is trashed with it rather than left dangling.
pub const TRASH_ID: &str = "TRASH";
pub const ROOT_DOCUMENT_ID_KEY: &str = "root_document_id";
pub const DOCUMENT_ID_KEY: &str = "document_id";
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
/// The format of a text document's file (`md`, `odt`): a file whose content
/// the tree holds itself, as a rich-text document under [`DOCUMENT_KEY`], and
/// names by no hash.
pub const CODEC_KEY: &str = "codec";
/// The document a text document's node holds.
pub const DOCUMENT_KEY: &str = "doc";
/// The signed content revision the node's bytes come from.
pub const CONTENT_REVISION_KEY: &str = "content_revision_v1";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    /// The stable identity of the workspace root: the same on every device
    /// that holds this tree.
    pub root_document_id: Option<FileDocumentId>,
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
    pub fn of_document(record: &[u8]) -> Result<Self, ModelError> {
        let document = automerge::AutoCommit::load(record)?;
        FilesTree::read(&document)
    }

    /// The tree the tree's own record `index` and the chunks `chunks` hold, each as its saved
    /// bytes ([`TreeChunks`]).
    ///
    /// # Errors
    ///
    /// A record is not a saved document, or the nodes it holds are malformed.
    pub fn of_records<'a>(
        index: Option<&[u8]>,
        chunks: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<Self, ModelError> {
        let mut tree = match index {
            Some(index) => Self::of_document(index)?,
            None => Self::default(),
        };
        for chunk in chunks {
            let held = Self::of_document(chunk)?;
            tree.nodes.extend(held.nodes);
        }
        Ok(tree)
    }

    /// The chunk a node made in this tree now goes to ([`chunk_for_new`]).
    #[must_use]
    pub fn chunk_for_new(&self) -> TreeChunk {
        let mut counts = BTreeMap::<TreeChunk, usize>::new();
        for node in self.nodes.values() {
            let chunk = node.document_id.map_or(FIRST_TREE_CHUNK, |id| id.chunk());
            let count = counts.entry(chunk).or_default();
            *count = count.saturating_add(1);
        }
        let newest = counts.last_key_value();
        chunk_for_new(
            newest.map(|(chunk, _)| *chunk),
            newest.map_or(0, |(_, n)| *n),
        )
    }
}

/// A workspace's tree as the records that hold it: its own record (the index) and its
/// chunks, each read as it changes, so a change of one chunk costs that chunk.
#[derive(Clone, Debug, Default)]
pub struct TreeChunks {
    root_document_id: Option<FileDocumentId>,
    listed: BTreeSet<TreeChunk>,
    chunks: BTreeMap<TreeChunk, BTreeMap<String, TreeEntry>>,
}

impl TreeChunks {
    /// Take in the tree's own record as it reads now.
    pub fn read_index<D: ReadDoc>(&mut self, doc: &D) {
        self.root_document_id =
            resource_of(doc, &ROOT, ROOT_DOCUMENT_ID_KEY).or(self.root_document_id);
        self.listed.extend(listed_chunks(doc));
    }

    /// Take in chunk `chunk` as its record reads now, in place of what it read before.
    pub fn read_chunk<D: ReadDoc>(&mut self, chunk: TreeChunk, doc: &D) {
        let mut nodes = BTreeMap::new();
        if let Some(map) = doc.obj_at(&ROOT, NODES_KEY, ObjType::Map) {
            for id in doc.keys(&map) {
                if let Some(node) = doc.obj_at(&map, &id, ObjType::Map) {
                    nodes.insert(id, entry_of(doc, &node));
                }
            }
        }
        self.listed.insert(chunk);
        self.chunks.insert(chunk, nodes);
    }

    /// Take in the record `record` of the tree, saved as `bytes`: its own record or a chunk.
    /// `false` for a record that is neither.
    ///
    /// # Errors
    /// `bytes` is not a saved document.
    pub fn read_resource(&mut self, record: &str, bytes: &[u8]) -> Result<bool, ModelError> {
        let Some(chunk) = chunk_of_record(record) else {
            return Ok(false);
        };
        let doc = automerge::AutoCommit::load(bytes)?;
        if chunk == 0 {
            self.read_index(&doc);
        } else {
            self.read_chunk(chunk, &doc);
        }
        Ok(true)
    }

    /// The chunks the index lists or that were read, in order: every record a holder of the
    /// tree opens.
    #[must_use]
    pub fn listed(&self) -> Vec<TreeChunk> {
        self.listed.iter().copied().collect()
    }

    /// Whether chunk `chunk` was read.
    #[must_use]
    pub fn holds(&self, chunk: TreeChunk) -> bool {
        self.chunks.contains_key(&chunk)
    }

    /// The nodes chunk `chunk` holds, as it read last.
    #[must_use]
    pub fn chunk(&self, chunk: TreeChunk) -> Option<&BTreeMap<String, TreeEntry>> {
        self.chunks.get(&chunk)
    }

    /// The whole tree, every chunk read so far.
    #[must_use]
    pub fn tree(&self) -> Tree {
        Tree {
            root_document_id: self.root_document_id,
            nodes: self
                .chunks
                .values()
                .flat_map(|nodes| nodes.iter().map(|(id, node)| (id.clone(), node.clone())))
                .collect(),
        }
    }

    /// The chunk a node made in this tree now goes to ([`chunk_for_new`]).
    #[must_use]
    pub fn chunk_for_new(&self) -> TreeChunk {
        let newest = self.listed.iter().next_back().copied();
        let nodes = newest
            .and_then(|chunk| self.chunks.get(&chunk))
            .map_or(0, BTreeMap::len);
        chunk_for_new(newest, nodes)
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
    /// For a text document, the format its file is in: its content is the
    /// tree's own ([`document_file`] reads it), and it has no hash.
    pub codec: Option<String>,
    /// The content's length in bytes.
    pub size: u64,
    /// When its content last changed, in milliseconds since the epoch.
    pub mtime: u64,
    /// Who last changed it, as the device that wrote the node named them: a
    /// tree nobody has written yet names nobody.
    pub author: Option<String>,
    pub document_id: Option<FileDocumentId>,
}

/// A workspace's files, published as a record.
///
/// The app that keeps the record declares it with its
/// `ResourceApp::PERSON_RESOURCES` (`PersonKind::shared`), and the daemon that
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
            root_document_id: resource_of(doc, &ROOT, ROOT_DOCUMENT_ID_KEY),
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
        if delta.key(&ROOT, ROOT_DOCUMENT_ID_KEY) == Touch::Written {
            state.root_document_id = resource_of(doc, &ROOT, ROOT_DOCUMENT_ID_KEY);
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

impl SharedResource for FilesTree {
    const OWNER: &'static str = tilo_app_id::apps::FILE_EXPLORER;
}

/// The file the text document `id` of the tree `doc` is: its document written
/// in its format, the binary content it embeds read from `content`. `None` for
/// a node that is no text document, or one in a format nothing here writes.
#[cfg(feature = "documents")]
#[must_use]
pub fn document_file(
    doc: &Doc,
    id: &str,
    content: &dyn tilo_content::ContentSource,
) -> Option<Vec<u8>> {
    let node = nodes_at(doc, &nodes_of(doc)?, id)?;
    let codec = doc.str_at(&node, CODEC_KEY)?;
    let provider = tilo_doc_codecs::provider_for_ext(&codec)?;
    let root = doc.obj_at(&node, DOCUMENT_KEY, ObjType::Map)?;
    let containers = tilo_doc::model::open(doc, &root).ok()?;
    provider
        .export(
            tilo_doc::DocRef {
                doc,
                c: &containers,
            },
            content,
        )
        .ok()
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
fn name_of<D: ReadDoc>(doc: &D, node: &ObjId, key: &str) -> Option<FileNameV1> {
    doc.bytes_at(node, key)
        .map(FileNameV1)
        .or_else(|| doc.str_at(node, key).map(FileNameV1::from))
}

fn resource_of<D: ReadDoc>(doc: &D, obj: &ObjId, key: &str) -> Option<FileDocumentId> {
    doc.str_at(obj, key)
        .and_then(|value| uuid::Uuid::parse_str(&value).ok())
        .map(FileDocumentId)
}

fn entry_of<D: ReadDoc>(doc: &D, node: &ObjId) -> TreeEntry {
    TreeEntry {
        parent: doc.str_at(node, PARENT_KEY),
        name: name_of(doc, node, NAME_KEY).unwrap_or_default(),
        kind: match doc.str_at(node, KIND_KEY).as_deref() {
            Some("dir") => FileNodeKind::Directory,
            _ => FileNodeKind::File,
        },
        hash: doc.str_at(node, HASH_KEY),
        codec: doc
            .str_at(node, CODEC_KEY)
            .filter(|codec| !codec.is_empty()),
        size: doc.uint_at(node, SIZE_KEY).unwrap_or_default(),
        mtime: doc.uint_at(node, MTIME_KEY).unwrap_or_default(),
        author: doc.str_at(node, AUTHOR_KEY).filter(|name| !name.is_empty()),
        document_id: resource_of(doc, node, DOCUMENT_ID_KEY),
    }
}

/// Prefix of a node's key derived from its stable record id, so every device
/// that writes or rebuilds the same record makes the same node object.
pub const RECORD_NODE_ID_PREFIX: &str = "record:";

/// The node key every device derives for the record `document_id`.
#[must_use]
pub fn document_node_id(document_id: FileDocumentId) -> String {
    format!("{RECORD_NODE_ID_PREFIX}{}", document_id.0)
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

/// Where `id` is in `tree`, as `docs/plan.md`: what a history row names a file
/// by. The root itself reads as the empty path.
#[must_use]
pub fn path_of(tree: &Tree, id: &str) -> String {
    let mut names: Vec<String> = Vec::new();
    let mut at = id.to_owned();
    while at != ROOT_ID {
        let Some(node) = tree.node(&at) else {
            break;
        };
        names.push(String::from_utf8_lossy(node.name.as_bytes()).into_owned());
        match node.parent.clone() {
            Some(parent) if parent != at => at = parent,
            _ => break,
        }
    }
    names.reverse();
    names.join("/")
}

/// Whether `id` hangs under the tree's root: a node moved to the trash, or
/// under a folder that was, is no file of the workspace.
#[must_use]
pub fn reaches_root(tree: &Tree, id: &str) -> bool {
    let mut at = id;
    for _ in 0..tree.nodes.len().saturating_add(1) {
        if at == ROOT_ID {
            return true;
        }
        match tree.node(at).and_then(|node| node.parent.as_deref()) {
            Some(parent) if parent != at => at = parent,
            _ => return false,
        }
    }
    false
}

/// One node of a workspace's tree that the workspace still holds, and where
/// it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveFile<'a> {
    /// The node's key in the tree: what a row stands for.
    pub id: &'a str,
    /// The node itself: its name's bytes, kind, size and content hash.
    pub node: &'a TreeEntry,
    /// Its name, as a person reads it.
    pub name: String,
    /// Where it is in the workspace, as `docs/plan.md`.
    pub path: String,
}

/// Every file and folder `tree` holds that the workspace still has — not in
/// the trash, and hanging under the root — each with where it is, sorted by
/// path. What a search of a workspace's files reads, on a page that has only
/// the record as on a host that keeps the tree.
#[must_use]
pub fn live_files(tree: &Tree) -> Vec<LiveFile<'_>> {
    let mut files: Vec<LiveFile<'_>> = tree
        .nodes
        .iter()
        .filter(|(id, _)| !tree.trashed(id) && reaches_root(tree, id))
        .map(|(id, node)| LiveFile {
            id,
            node,
            name: String::from_utf8_lossy(node.name.as_bytes()).into_owned(),
            path: path_of(tree, id),
        })
        .collect();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files
}

/// The live folder named `name` in `parent`, when `tree` holds one.
#[must_use]
pub fn folder_named(tree: &Tree, parent: &str, name: &str) -> Option<String> {
    tree.children(parent)
        .into_iter()
        .find(|(_, node)| node.kind == FileNodeKind::Directory && node.name == name)
        .map(|(id, _)| id.to_owned())
}

/// Put `placing` named `name` into the folder `parent` of the tree, written
/// as the filesystem writes a node it creates, so a mounted copy of the tree
/// shows it as a file or folder of its own. `doc` is the record of the chunk
/// `chunk` the node goes to ([`Tree::chunk_for_new`]); `tree` is the whole
/// tree and takes the node too, so the next placement sees it. A name the
/// folder already has is kept beside it ([`free_name`]).
///
/// # Errors
///
/// A name no file can have (empty, `.`, `..`, or with a `/`), a parent the
/// tree does not hold, or a document that refused the write.
pub fn place(
    (doc, chunk): (&mut Doc, TreeChunk),
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
    let nodes = tree_nodes(doc)?;
    let document_id = FileDocumentId::in_chunk(chunk);
    let id = document_node_id(document_id);
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
    doc.put(&node, DOCUMENT_ID_KEY, document_id.0.to_string())?;
    tree.nodes.insert(
        id.clone(),
        TreeEntry {
            parent: Some(parent.to_owned()),
            name: FileNameV1::from(name.as_str()),
            kind,
            hash: hash.map(str::to_owned),
            codec: None,
            size,
            mtime,
            document_id: Some(document_id),
            author: Some(author.to_owned()),
        },
    );
    Ok(Placed { id, name })
}

#[cfg(test)]
mod live_tests {
    use super::*;

    fn node(parent: &str, name: &str, kind: FileNodeKind, hash: Option<&str>) -> TreeEntry {
        TreeEntry {
            parent: Some(parent.to_owned()),
            name: FileNameV1::from(name),
            kind,
            hash: hash.map(str::to_owned),
            codec: None,
            size: 3,
            mtime: 0,
            author: None,
            document_id: None,
        }
    }

    fn paths(files: Vec<LiveFile<'_>>) -> Vec<String> {
        files.into_iter().map(|file| file.path).collect()
    }

    /// A workspace's files are the ones it still has, wherever they are in it,
    /// in the order of their paths: what a search of them reads.
    #[test]
    fn the_files_a_workspace_has_are_the_ones_out_of_the_trash_with_where_they_are() {
        let mut tree = Tree::default();
        tree.nodes.insert(
            "docs".into(),
            node(ROOT_ID, "docs", FileNodeKind::Directory, None),
        );
        tree.nodes.insert(
            "plan".into(),
            node("docs", "plan.md", FileNodeKind::File, Some("ab")),
        );
        tree.nodes.insert(
            "loose".into(),
            node(ROOT_ID, "notes.txt", FileNodeKind::File, Some("ef")),
        );
        // A file someone deleted, and one that went with the folder it was in.
        tree.nodes.insert(
            "old".into(),
            node(TRASH_ID, "old.txt", FileNodeKind::File, Some("ff")),
        );
        tree.nodes.insert(
            "binned".into(),
            node(TRASH_ID, "binned", FileNodeKind::Directory, None),
        );
        tree.nodes.insert(
            "inside".into(),
            node("binned", "inside.md", FileNodeKind::File, Some("12")),
        );
        assert_eq!(
            paths(live_files(&tree)),
            ["docs", "docs/plan.md", "notes.txt"]
        );
        assert!(!reaches_root(&tree, "old"));
        assert!(
            !reaches_root(&tree, "inside"),
            "a file of a folder in the trash is in the trash too"
        );
        assert!(reaches_root(&tree, "plan"));
        assert!(reaches_root(&tree, ROOT_ID));
    }

    /// A name is bytes: one that is not text is still the name its file is
    /// found by.
    #[test]
    fn a_name_that_is_not_text_is_read_as_it_is() {
        let mut tree = Tree::default();
        tree.nodes.insert(
            "odd".into(),
            TreeEntry {
                name: FileNameV1(vec![b'c', b'a', b'f', 0xff]),
                ..node(ROOT_ID, "unused", FileNodeKind::File, Some("34"))
            },
        );
        let live = live_files(&tree);
        assert_eq!(live.len(), 1);
        assert_eq!(
            live[0].name,
            String::from_utf8_lossy(&[b'c', b'a', b'f', 0xff])
        );
        assert_eq!(live[0].id, "odd");
        assert_eq!(live[0].node.hash.as_deref(), Some("34"));
    }
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
            (&mut doc, 1),
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
        let plan = place(
            (&mut doc, 1),
            &mut tree,
            &docs.id,
            "plan.md",
            file,
            (6, "page"),
        )
        .unwrap();
        let again = place(
            (&mut doc, 1),
            &mut tree,
            &docs.id,
            "plan.md",
            file,
            (7, "page"),
        )
        .unwrap();
        let hidden = place(
            (&mut doc, 1),
            &mut tree,
            &docs.id,
            ".env",
            file,
            (7, "page"),
        )
        .unwrap();
        let hidden_again = place(
            (&mut doc, 1),
            &mut tree,
            &docs.id,
            ".env",
            file,
            (7, "page"),
        )
        .unwrap();
        assert_eq!(again.name, "plan (2).md");
        assert_eq!(hidden_again.name, ".env (2)");
        assert_eq!(read(&doc), tree, "what was written is what the tree holds");
        let node = tree.node(&plan.id).unwrap();
        assert_eq!(node.kind, FileNodeKind::File);
        assert_eq!(node.hash.as_deref(), Some(hash.as_str()));
        assert_eq!(node.size, 12);
        assert_eq!(plan.id, document_node_id(node.document_id.unwrap()));
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
                    (&mut doc, 1),
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
                (&mut doc, 1),
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
            (&mut mount, 1),
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
            (&mut page, 1),
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

#[cfg(test)]
mod chunk_tests {
    use super::*;

    #[test]
    fn a_chunk_is_named_by_its_record_and_a_node_by_its_document_id() {
        assert_eq!(chunk_document_id(0), document_id());
        for chunk in [1, 2, 77, u32::MAX] {
            let record = chunk_document_id(chunk);
            assert_eq!(chunk_of_record(&record), Some(chunk), "{record}");
            assert_eq!(FileDocumentId::in_chunk(chunk).chunk(), chunk);
        }
        assert_eq!(chunk_of_record(&document_id()), Some(0));
        assert_eq!(chunk_of_record("derived:chunk.01@x"), None);
        assert_eq!(
            chunk_of_record(&format!("derived:chunk.01@{}", document_id())),
            None
        );
        assert_eq!(chunk_of_record("note-1"), None);
        // An id made without a chunk is in the first.
        assert_eq!(FileDocumentId::new().chunk(), FIRST_TREE_CHUNK);
        assert_ne!(FileDocumentId::in_chunk(3), FileDocumentId::in_chunk(3));
    }

    #[test]
    fn new_nodes_fill_the_newest_chunk_then_start_the_next() {
        assert_eq!(chunk_for_new(None, 0), 1);
        assert_eq!(chunk_for_new(Some(1), CHUNK_NODES - 1), 1);
        assert_eq!(chunk_for_new(Some(1), CHUNK_NODES), 2);
    }

    #[test]
    fn a_tree_reads_whole_from_its_index_and_chunks() {
        let mut index = Doc::new();
        let mut first = Doc::new();
        let mut second = Doc::new();
        let mut tree = Tree::default();
        let docs = place(
            (&mut first, 1),
            &mut tree,
            ROOT_ID,
            "docs",
            Placing::Folder,
            (1, "a"),
        )
        .unwrap();
        let file = Placing::File {
            hash: "00",
            size: 1,
        };
        place(
            (&mut second, 2),
            &mut tree,
            &docs.id,
            "plan.md",
            file,
            (2, "a"),
        )
        .unwrap();
        list_chunk(&mut index, 1).unwrap();
        assert!(list_chunk(&mut index, 2).unwrap());
        assert!(!list_chunk(&mut index, 2).unwrap());
        assert_eq!(listed_chunks(&index), [1, 2]);

        let read = Tree::of_records(
            Some(&index.save()),
            [first.save().as_slice(), second.save().as_slice()],
        )
        .unwrap();
        assert_eq!(read, tree);

        let mut chunks = TreeChunks::default();
        chunks.read_resource(&document_id(), &index.save()).unwrap();
        assert_eq!(chunks.listed(), [1, 2]);
        chunks
            .read_resource(&chunk_document_id(1), &first.save())
            .unwrap();
        chunks
            .read_resource(&chunk_document_id(2), &second.save())
            .unwrap();
        assert_eq!(chunks.tree(), tree);
        assert_eq!(chunks.chunk_for_new(), 2);
    }
}
