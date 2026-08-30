//! The namespace plane: an Automerge-backed move-tree CRDT (Kleppmann model).
//! Nodes are keyed by a stable `NodeId`, not path, with an explicit `parent`
//! register, so rename/move/delete are real move ops: no duplicate parents,
//! delete reparents to the `TRASH` sentinel (a concurrently-created child ends
//! up trashed, never dangling), and a concurrent cross-move cycle is
//! deterministically broken by [`Namespace::resolve`] (lowest-id node in the
//! cycle reverts to `prev_parent`/`ROOT`), so every peer converges identically.
//!
//! Inode numbers are a local `NodeId`<->`u64` index; renames keep the node id,
//! so the inode stays stable across renames.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, UNIX_EPOCH};

use automerge::sync::SyncDoc;
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use fuser::{FileAttr, FileType, INodeNo};
use iroh_blobs::Hash;

pub const ROOT_INO: u64 = 1;
/// Sentinel parent for the tree root (not materialized as a node).
pub const ROOT_ID: &str = "ROOT";
/// Sentinel parent for deleted subtrees (not materialized as a node).
const TRASH_ID: &str = "TRASH";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Dir,
}

impl NodeKind {
    pub fn to_file_type(self) -> FileType {
        match self {
            NodeKind::File => FileType::RegularFile,
            NodeKind::Dir => FileType::Directory,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            NodeKind::File => "file",
            NodeKind::Dir => "dir",
        }
    }
    fn from_str(s: &str) -> Self {
        if s == "dir" {
            NodeKind::Dir
        } else {
            NodeKind::File
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub parent: String,
    pub name: String,
    pub kind: NodeKind,
    pub hash: Option<Hash>,
    pub size: u64,
    pub mtime: u64,
    pub author: String,
}

pub struct Namespace {
    doc: AutoCommit,
    nodes: ObjId,
    ino_to_id: HashMap<u64, String>,
    id_to_ino: HashMap<String, u64>,
    next_ino: u64,
    author: String,
    next_id: u64,
}

fn hash_to_hex(h: &Hash) -> String {
    hex::encode(h.as_bytes())
}
fn hash_from_hex(s: &str) -> Option<Hash> {
    let bytes = hex::decode(s).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(Hash::from(arr))
}

fn read_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Str(x) => Some(x.to_string()),
            _ => None,
        },
        _ => None,
    }
}
fn read_u64(doc: &AutoCommit, obj: &ObjId, key: &str) -> u64 {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Uint(n) => *n,
            ScalarValue::Int(n) => *n as u64,
            _ => 0,
        },
        _ => 0,
    }
}

impl Namespace {
    pub fn new(author: impl Into<String>) -> Self {
        let mut doc = AutoCommit::new();
        let nodes = doc
            .put_object(&ROOT, "nodes", ObjType::Map)
            .expect("put nodes map");
        let mut ns = Self {
            doc,
            nodes,
            ino_to_id: HashMap::new(),
            id_to_ino: HashMap::new(),
            next_ino: ROOT_INO + 1,
            author: author.into(),
            next_id: 0,
        };
        ns.ino_to_id.insert(ROOT_INO, ROOT_ID.to_string());
        ns.id_to_ino.insert(ROOT_ID.to_string(), ROOT_INO);
        ns
    }

    pub fn from_bytes(
        author: impl Into<String>,
        data: &[u8],
    ) -> Result<Self, automerge::AutomergeError> {
        let mut doc = AutoCommit::load(data)?;
        let nodes = match doc.get(&ROOT, "nodes") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc.put_object(&ROOT, "nodes", ObjType::Map)?,
        };
        let author = author.into();
        // Resume the id counter past ids we authored before, so a restart can't reuse a node id.
        let prefix = format!("{author}/");
        let mut max_local = 0u64;
        for id in doc.keys(&nodes) {
            if let Some(n) = id.strip_prefix(&prefix)
                && let Ok(v) = n.parse::<u64>()
            {
                max_local = max_local.max(v + 1);
            }
        }
        let mut ns = Self {
            doc,
            nodes,
            ino_to_id: HashMap::new(),
            id_to_ino: HashMap::new(),
            next_ino: ROOT_INO + 1,
            author,
            next_id: max_local,
        };
        ns.ino_to_id.insert(ROOT_INO, ROOT_ID.to_string());
        ns.id_to_ino.insert(ROOT_ID.to_string(), ROOT_INO);
        Ok(ns)
    }

    pub fn author(&self) -> &str {
        &self.author
    }

    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }
    /// Changes since the last `save_incremental`/load/merge, advancing the shared
    /// automerge watermark.
    ///
    /// INVARIANT: pair every mutation with its `save_incremental` inside the same
    /// `Namespace` lock scope — the watermark is one shared cursor, so a mutation
    /// saved outside that scope can have its delta silently swallowed by a
    /// concurrent remote-apply.
    pub fn save_incremental(&mut self) -> Vec<u8> {
        self.doc.save_incremental()
    }
    pub fn apply_incremental(&mut self, data: &[u8]) -> Result<(), automerge::AutomergeError> {
        self.doc.load_incremental(data)?;
        Ok(())
    }
    pub fn prime_baseline(&mut self) {
        let _ = self.doc.save_incremental();
    }
    pub fn merge_doc(&mut self, other: &mut Namespace) -> Result<(), automerge::AutomergeError> {
        self.doc.merge(&mut other.doc)?;
        Ok(())
    }

    /// Apply a remote delta: advance the watermark, resolve, and return the resolution delta to broadcast (empty if none).
    pub fn apply_remote_and_resolve(
        &mut self,
        data: &[u8],
    ) -> Result<Vec<u8>, automerge::AutomergeError> {
        self.doc.load_incremental(data)?;
        let _ = self.doc.save_incremental();
        self.resolve();
        Ok(self.doc.save_incremental())
    }

    pub fn merge_remote_and_resolve(
        &mut self,
        data: &[u8],
    ) -> Result<Vec<u8>, automerge::AutomergeError> {
        let mut other = AutoCommit::load(data)?;
        self.doc.merge(&mut other)?;
        let _ = self.doc.save_incremental();
        self.resolve();
        Ok(self.doc.save_incremental())
    }

    pub fn sync_generate(&mut self, state: &mut automerge::sync::State) -> Option<Vec<u8>> {
        self.doc
            .sync()
            .generate_sync_message(state)
            .map(automerge::sync::Message::encode)
    }

    pub fn sync_receive(
        &mut self,
        state: &mut automerge::sync::State,
        data: &[u8],
    ) -> anyhow::Result<()> {
        let msg = automerge::sync::Message::decode(data)?;
        self.doc.sync().receive_sync_message(state, msg)?;
        Ok(())
    }

    /// Discard the pulled changes from the watermark (peers already have them), resolve, and return only the resolution delta.
    pub fn post_sync_resolve(&mut self) -> Vec<u8> {
        let _ = self.doc.save_incremental();
        self.resolve();
        self.doc.save_incremental()
    }

    fn node(&self, id: &str) -> Option<ObjId> {
        match self.doc.get(&self.nodes, id) {
            Ok(Some((Value::Object(ObjType::Map), obj))) => Some(obj),
            _ => None,
        }
    }

    fn parent_of(&self, id: &str) -> Option<String> {
        read_str(&self.doc, &self.node(id)?, "parent")
    }
    fn name_of(&self, id: &str) -> Option<String> {
        read_str(&self.doc, &self.node(id)?, "name")
    }
    fn kind_of(&self, id: &str) -> NodeKind {
        self.node(id)
            .and_then(|o| read_str(&self.doc, &o, "kind"))
            .map_or(NodeKind::File, |k| NodeKind::from_str(&k))
    }

    fn set_field(&mut self, id: &str, key: &str, val: impl Into<ScalarValue>) {
        if let Some(obj) = self.node(id) {
            let _ = self.doc.put(&obj, key, val);
        }
    }

    /// Walk the parent chain: `true` if it reaches ROOT (visible), `false` if it
    /// hits TRASH, a missing node, or a cycle.
    fn reaches_root(&self, id: &str) -> bool {
        let mut cur = id.to_string();
        let mut seen = HashSet::new();
        loop {
            if cur == ROOT_ID {
                return true;
            }
            if cur == TRASH_ID {
                return false;
            }
            if !seen.insert(cur.clone()) {
                return false;
            }
            match self.parent_of(&cur) {
                Some(p) => cur = p,
                None => return false,
            }
        }
    }

    fn entry(&self, id: &str) -> Option<Entry> {
        let obj = self.node(id)?;
        Some(Entry {
            id: id.to_string(),
            parent: read_str(&self.doc, &obj, "parent").unwrap_or_default(),
            name: read_str(&self.doc, &obj, "name").unwrap_or_default(),
            kind: NodeKind::from_str(&read_str(&self.doc, &obj, "kind").unwrap_or_default()),
            hash: read_str(&self.doc, &obj, "hash").and_then(|h| hash_from_hex(&h)),
            size: read_u64(&self.doc, &obj, "size"),
            mtime: read_u64(&self.doc, &obj, "mtime"),
            author: read_str(&self.doc, &obj, "author").unwrap_or_default(),
        })
    }

    fn children(&self, parent_id: &str) -> Vec<String> {
        self.doc
            .keys(&self.nodes)
            .filter(|id| self.parent_of(id).as_deref() == Some(parent_id))
            .collect()
    }

    /// The live child of `parent_id` named `name` — smallest id wins on a tie.
    fn child_named(&self, parent_id: &str, name: &str) -> Option<String> {
        let mut matches: Vec<String> = self
            .children(parent_id)
            .into_iter()
            .filter(|id| self.name_of(id).as_deref() == Some(name) && self.reaches_root(id))
            .collect();
        matches.sort();
        matches.into_iter().next()
    }

    fn is_ancestor(&self, maybe_ancestor: &str, id: &str) -> bool {
        let mut cur = id.to_string();
        let mut seen = HashSet::new();
        loop {
            if cur == maybe_ancestor {
                return true;
            }
            if cur == ROOT_ID || cur == TRASH_ID || !seen.insert(cur.clone()) {
                return false;
            }
            match self.parent_of(&cur) {
                Some(p) => cur = p,
                None => return false,
            }
        }
    }

    fn ino_for_id(&mut self, id: &str) -> u64 {
        if let Some(&ino) = self.id_to_ino.get(id) {
            return ino;
        }
        let ino = self.next_ino;
        self.next_ino += 1;
        self.id_to_ino.insert(id.to_string(), ino);
        self.ino_to_id.insert(ino, id.to_string());
        ino
    }

    pub fn id_for_ino(&self, ino: u64) -> Option<&str> {
        self.ino_to_id.get(&ino).map(std::string::String::as_str)
    }

    /// Absolute path of `ino` within the mount — `"/"` for the root, `"/a/b/c"`
    /// otherwise; `None` if unknown or its parent chain doesn't reach ROOT
    /// (trashed / cyclic).
    ///
    /// This is what FUSE's per-identity ACL enforcement keys on: the check is
    /// "may this identity read/write *this path*", so it needs the resolved path.
    pub fn path_for(&self, ino: u64) -> Option<String> {
        if ino == ROOT_INO {
            return Some("/".to_string());
        }
        let mut cur = self.id_for_ino(ino)?.to_string();
        let mut parts: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        loop {
            if cur == ROOT_ID {
                break;
            }
            if cur == TRASH_ID || !seen.insert(cur.clone()) {
                return None;
            }
            parts.push(self.name_of(&cur)?);
            cur = self.parent_of(&cur)?;
        }
        parts.reverse();
        Some(format!("/{}", parts.join("/")))
    }

    fn gen_id(&mut self) -> String {
        let id = format!("{}/{}", self.author, self.next_id);
        self.next_id += 1;
        id
    }

    fn attr_for(&self, ino: u64, kind: NodeKind, size: u64, mtime_ms: u64) -> FileAttr {
        let t = UNIX_EPOCH + Duration::from_millis(mtime_ms);
        FileAttr {
            ino: INodeNo(ino),
            size,
            blocks: size.div_ceil(512),
            atime: t,
            mtime: t,
            ctime: t,
            crtime: t,
            kind: kind.to_file_type(),
            perm: match kind {
                NodeKind::Dir => 0o755,
                NodeKind::File => 0o644,
            },
            nlink: match kind {
                NodeKind::Dir => 2,
                NodeKind::File => 1,
            },
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }

    pub fn getattr(&self, ino: u64) -> Option<FileAttr> {
        let id = self.ino_to_id.get(&ino)?.clone();
        if id == ROOT_ID {
            return Some(self.attr_for(ROOT_INO, NodeKind::Dir, 0, 0));
        }
        if !self.reaches_root(&id) {
            return None;
        }
        let e = self.entry(&id)?;
        Some(self.attr_for(ino, e.kind, e.size, e.mtime))
    }

    pub fn lookup(&mut self, parent_ino: u64, name: &str) -> Option<FileAttr> {
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let child = self.child_named(&parent_id, name)?;
        let e = self.entry(&child)?;
        let ino = self.ino_for_id(&child);
        Some(self.attr_for(ino, e.kind, e.size, e.mtime))
    }

    pub fn readdir(&mut self, ino: u64) -> Option<Vec<(u64, FileType, String)>> {
        let id = self.ino_to_id.get(&ino)?.clone();
        if id != ROOT_ID {
            match self.entry(&id) {
                Some(e) if e.kind == NodeKind::Dir && self.reaches_root(&id) => {}
                _ => return None,
            }
        }
        let parent_id = if id == ROOT_ID {
            ROOT_ID.to_string()
        } else {
            self.parent_of(&id).unwrap_or_else(|| ROOT_ID.to_string())
        };
        let parent_ino = self.ino_for_id(&parent_id);

        let mut out = vec![
            (ino, FileType::Directory, ".".to_string()),
            (parent_ino, FileType::Directory, "..".to_string()),
        ];

        let mut kids: Vec<(String, String, NodeKind)> = self
            .children(&id)
            .into_iter()
            .filter(|c| self.reaches_root(c))
            .filter_map(|c| {
                let name = self.name_of(&c)?;
                Some((c.clone(), name, self.kind_of(&c)))
            })
            .collect();
        kids.sort_by(|a, b| a.1.cmp(&b.1));
        for (cid, name, kind) in kids {
            let cino = self.ino_for_id(&cid);
            out.push((cino, kind.to_file_type(), name));
        }
        Some(out)
    }

    pub fn hash_for(&self, ino: u64) -> Option<Hash> {
        let id = self.ino_to_id.get(&ino)?;
        self.entry(id)?.hash
    }

    fn create_node(
        &mut self,
        parent_id: &str,
        name: &str,
        kind: NodeKind,
        hash: Option<&Hash>,
        size: u64,
        mtime: u64,
    ) -> String {
        let id = self.gen_id();
        let obj = self
            .doc
            .put_object(&self.nodes, &id, ObjType::Map)
            .expect("put node");
        let _ = self.doc.put(&obj, "parent", parent_id);
        let _ = self.doc.put(&obj, "prev_parent", parent_id);
        let _ = self.doc.put(&obj, "name", name);
        let _ = self.doc.put(&obj, "kind", kind.as_str());
        if let Some(h) = hash {
            let _ = self.doc.put(&obj, "hash", hash_to_hex(h));
        }
        let _ = self.doc.put(&obj, "size", size);
        let _ = self.doc.put(&obj, "mtime", mtime);
        let _ = self.doc.put(&obj, "author", self.author.clone());
        id
    }

    pub fn create_file(
        &mut self,
        parent_ino: u64,
        name: &str,
        hash: Option<&Hash>,
        size: u64,
        mtime: u64,
    ) -> Option<u64> {
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let id = self.create_node(&parent_id, name, NodeKind::File, hash, size, mtime);
        Some(self.ino_for_id(&id))
    }

    pub fn create_dir(&mut self, parent_ino: u64, name: &str, mtime: u64) -> Option<u64> {
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let id = self.create_node(&parent_id, name, NodeKind::Dir, None, 0, mtime);
        Some(self.ino_for_id(&id))
    }

    // Document files: content lives as a CRDT, not opaque bytes.

    /// Create a file whose content is a structured rich-text document stored as a
    /// CRDT (concurrent edits merge). `codec` is the format key (e.g. `odt`, `md`).
    pub fn create_document(
        &mut self,
        parent_ino: u64,
        name: &str,
        codec: &str,
        mtime: u64,
    ) -> Option<u64> {
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let id = self.create_node(&parent_id, name, NodeKind::File, None, 0, mtime);
        self.set_field(&id, "codec", codec);
        let node = self.node(&id)?;
        let root = self
            .doc
            .put_object(&node, "doc", ObjType::Map)
            .expect("put document root");
        qdoc::model::init(&mut self.doc, &root);
        Some(self.ino_for_id(&id))
    }

    pub fn document_codec(&self, ino: u64) -> Option<String> {
        let id = self.ino_to_id.get(&ino)?;
        read_str(&self.doc, &self.node(id)?, "codec")
    }

    fn document_root(&self, id: &str) -> Option<ObjId> {
        match self.doc.get(&self.node(id)?, "doc") {
            Ok(Some((Value::Object(ObjType::Map), root))) => Some(root),
            _ => None,
        }
    }

    pub fn export_document(&self, ino: u64, registry: &qdoc::Registry) -> Option<Vec<u8>> {
        let id = self.ino_to_id.get(&ino)?.clone();
        let codec = read_str(&self.doc, &self.node(&id)?, "codec")?;
        let provider = registry.provider_for_ext(&codec)?;
        let root = self.document_root(&id)?;
        let containers = qdoc::model::open(&self.doc, &root).ok()?;
        provider
            .export(qdoc::DocRef {
                doc: &self.doc,
                c: &containers,
            })
            .ok()
    }

    /// Replace a document's content by importing `bytes` (whole-file write).
    /// Resets the embedded CRDT; use the in-place edits below to preserve merge history.
    pub fn import_document(
        &mut self,
        ino: u64,
        registry: &qdoc::Registry,
        bytes: &[u8],
        mtime: u64,
    ) -> bool {
        let Some(id) = self.ino_to_id.get(&ino).cloned() else {
            return false;
        };
        let Some(node) = self.node(&id) else {
            return false;
        };
        let Some(codec) = read_str(&self.doc, &node, "codec") else {
            return false;
        };
        let Some(provider) = registry.provider_for_ext(&codec) else {
            return false;
        };
        // Parse into a throwaway document first — a malformed input must never destroy existing content.
        let mut probe = qdoc::Document::new();
        if provider.import(bytes, &mut probe.as_mut()).is_err() {
            return false;
        }
        let _ = self.doc.delete(&node, "doc");
        let root = self
            .doc
            .put_object(&node, "doc", ObjType::Map)
            .expect("put document root");
        qdoc::model::init(&mut self.doc, &root);
        let containers = qdoc::model::open(&self.doc, &root).expect("open");
        let mut ids = qdoc::model::seeded_id_gen(&self.doc, &containers);
        {
            let mut dmut = qdoc::DocMut {
                doc: &mut self.doc,
                c: &containers,
                ids: &mut ids,
            };
            let _ = provider.import(bytes, &mut dmut);
        }
        self.set_field(&id, "mtime", mtime);
        true
    }

    /// Current heads of the document — a cheap change token (e.g. to invalidate a derived export cache).
    pub fn heads(&mut self) -> Vec<automerge::ChangeHash> {
        self.doc.get_heads()
    }

    // Fine-grained document edits: the path that merges semantically.

    fn with_doc_mut<R>(&mut self, ino: u64, f: impl FnOnce(&mut qdoc::DocMut) -> R) -> Option<R> {
        let id = self.ino_to_id.get(&ino)?.clone();
        let root = self.document_root(&id)?;
        let containers = qdoc::model::open(&self.doc, &root).ok()?;
        let mut ids = qdoc::model::seeded_id_gen(&self.doc, &containers);
        let mut dmut = qdoc::DocMut {
            doc: &mut self.doc,
            c: &containers,
            ids: &mut ids,
        };
        Some(f(&mut dmut))
    }

    pub fn document_insert_block(
        &mut self,
        ino: u64,
        after: Option<&str>,
        kind: &str,
    ) -> Option<String> {
        self.with_doc_mut(ino, |d| d.insert_block(after, kind))
    }

    pub fn document_splice(
        &mut self,
        ino: u64,
        block_id: &str,
        idx: usize,
        del: isize,
        ins: &str,
    ) -> bool {
        self.with_doc_mut(ino, |d| d.splice_text(block_id, idx, del, ins))
            .is_some()
    }

    pub fn document_blocks(&self, ino: u64) -> Vec<String> {
        let Some(id) = self.ino_to_id.get(&ino) else {
            return Vec::new();
        };
        let Some(root) = self.document_root(id) else {
            return Vec::new();
        };
        match qdoc::model::open(&self.doc, &root) {
            Ok(c) => qdoc::model::blocks_in_order(&self.doc, &c),
            Err(_) => Vec::new(),
        }
    }

    pub fn document_block_text(&self, ino: u64, block_id: &str) -> Option<String> {
        let id = self.ino_to_id.get(&ino)?;
        let root = self.document_root(id)?;
        let containers = qdoc::model::open(&self.doc, &root).ok()?;
        qdoc::model::block_text(&self.doc, &containers, block_id)
    }

    pub fn document_remove_block(&mut self, ino: u64, block_id: &str) -> bool {
        self.with_doc_mut(ino, |d| d.remove_block(block_id))
            .is_some()
    }

    pub fn document_snapshot(&self, ino: u64) -> Option<qdoc::DocSnapshot> {
        let id = self.ino_to_id.get(&ino)?;
        let root = self.document_root(id)?;
        let containers = qdoc::model::open(&self.doc, &root).ok()?;
        Some(
            qdoc::DocRef {
                doc: &self.doc,
                c: &containers,
            }
            .materialize(),
        )
    }

    pub fn set_content(&mut self, ino: u64, hash: Option<&Hash>, size: u64, mtime: u64) -> bool {
        let Some(id) = self.ino_to_id.get(&ino).cloned() else {
            return false;
        };
        if self.node(&id).is_none() {
            return false;
        }
        match hash {
            Some(h) => self.set_field(&id, "hash", hash_to_hex(h)),
            None => {
                if let Some(obj) = self.node(&id) {
                    let _ = self.doc.delete(&obj, "hash");
                }
            }
        }
        // Content changed, so any recorded thumbnail is stale — drop the pointer (regenerates lazily).
        if let Some(obj) = self.node(&id) {
            let _ = self.doc.delete(&obj, "thumb");
        }
        self.set_field(&id, "size", size);
        self.set_field(&id, "mtime", mtime);
        let author = self.author.clone();
        self.set_field(&id, "author", author);
        true
    }

    /// Attach the content hash of a generated thumbnail blob to `ino`, so the
    /// pointer rides namespace gossip and a peer can preview before fetching the full content.
    pub fn set_thumb(&mut self, ino: u64, thumb: &Hash) -> bool {
        let Some(id) = self.ino_to_id.get(&ino).cloned() else {
            return false;
        };
        if self.node(&id).is_none() {
            return false;
        }
        self.set_field(&id, "thumb", hash_to_hex(thumb));
        true
    }

    pub fn thumb_hash(&self, ino: u64) -> Option<Hash> {
        let id = self.ino_to_id.get(&ino)?;
        read_str(&self.doc, &self.node(id)?, "thumb").and_then(|h| hash_from_hex(&h))
    }

    /// Delete = move the node (and its subtree) to TRASH.
    pub fn remove(&mut self, ino: u64) -> bool {
        let Some(id) = self.ino_to_id.get(&ino).cloned() else {
            return false;
        };
        if self.node(&id).is_none() || !self.reaches_root(&id) {
            return false;
        }
        // Record the content hash at deletion time, so a concurrent edit can be detected and resurrected.
        match self.node_hash(&id) {
            Some(h) => self.set_field(&id, "deleted_hash", hash_to_hex(&h)),
            None => {
                if let Some(obj) = self.node(&id) {
                    let _ = self.doc.delete(&obj, "deleted_hash");
                }
            }
        }
        let cur = self.parent_of(&id).unwrap_or_else(|| ROOT_ID.to_string());
        self.set_field(&id, "prev_parent", cur);
        self.set_field(&id, "parent", TRASH_ID);
        true
    }

    pub fn directly_trashed(&self) -> Vec<String> {
        self.doc
            .keys(&self.nodes)
            .filter(|id| self.parent_of(id).as_deref() == Some(TRASH_ID))
            .collect()
    }

    pub fn deleted_hash(&self, id: &str) -> Option<Hash> {
        read_str(&self.doc, &self.node(id)?, "deleted_hash").and_then(|h| hash_from_hex(&h))
    }

    pub fn prev_parent(&self, id: &str) -> Option<String> {
        read_str(&self.doc, &self.node(id)?, "prev_parent")
    }

    pub fn is_visible(&self, id: &str) -> bool {
        self.reaches_root(id)
    }

    /// Rename / move: a single reparent (node keeps its id, so inode stays stable). Rejects moving a directory into its own descendant.
    pub fn rename(
        &mut self,
        parent_ino: u64,
        name: &str,
        new_parent_ino: u64,
        new_name: &str,
        mtime: u64,
    ) -> bool {
        let Some(parent_id) = self.ino_to_id.get(&parent_ino).cloned() else {
            return false;
        };
        let Some(new_parent_id) = self.ino_to_id.get(&new_parent_ino).cloned() else {
            return false;
        };
        let Some(id) = self.child_named(&parent_id, name) else {
            return false;
        };
        if new_parent_id == id || self.is_ancestor(&id, &new_parent_id) {
            return false;
        }
        // POSIX rename replaces the target: if the destination name is occupied, trash it.
        if let Some(existing) = self.child_named(&new_parent_id, new_name)
            && existing != id
        {
            if let Some(h) = self.node_hash(&existing) {
                self.set_field(&existing, "deleted_hash", hash_to_hex(&h));
            }
            let ep = self
                .parent_of(&existing)
                .unwrap_or_else(|| ROOT_ID.to_string());
            self.set_field(&existing, "prev_parent", ep);
            self.set_field(&existing, "parent", TRASH_ID);
        }
        let cur = self.parent_of(&id).unwrap_or_else(|| ROOT_ID.to_string());
        self.set_field(&id, "prev_parent", cur);
        self.set_field(&id, "parent", &*new_parent_id);
        self.set_field(&id, "name", new_name);
        self.set_field(&id, "mtime", mtime);
        true
    }

    // Structural resolution (cycle breaking).

    /// Find one cycle in the parent graph (never reaching ROOT/TRASH), as the node ids forming the loop.
    fn find_cycle(&self) -> Option<Vec<String>> {
        let ids: Vec<String> = self.doc.keys(&self.nodes).collect();
        for start in &ids {
            let mut cur = start.clone();
            let mut seq: Vec<String> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            loop {
                if cur == ROOT_ID || cur == TRASH_ID {
                    break;
                }
                if let Some(pos) = seq.iter().position(|x| x == &cur) {
                    return Some(seq[pos..].to_vec());
                }
                if !seen.insert(cur.clone()) || self.node(&cur).is_none() {
                    break;
                }
                seq.push(cur.clone());
                match self.parent_of(&cur) {
                    Some(p) => cur = p,
                    None => break,
                }
            }
        }
        None
    }

    /// Break every move-cycle deterministically: the lowest-id node in each cycle
    /// reverts to `prev_parent` (or ROOT). Pure function of the doc, so every peer computes the same repair.
    pub fn resolve(&mut self) -> bool {
        let mut changed = false;
        let guard = self.doc.keys(&self.nodes).count() + 2;
        for _ in 0..guard {
            let Some(cycle) = self.find_cycle() else {
                break;
            };
            let loser = cycle.iter().min().cloned().unwrap();
            let pp = self
                .node(&loser)
                .and_then(|o| read_str(&self.doc, &o, "prev_parent"))
                .unwrap_or_else(|| ROOT_ID.to_string());
            let target = if cycle.contains(&pp)
                || (pp != ROOT_ID && pp != TRASH_ID && self.node(&pp).is_none())
            {
                ROOT_ID.to_string()
            } else {
                pp
            };
            self.set_field(&loser, "parent", &*target);
            changed = true;
        }
        changed
    }

    // Path + conflict helpers (used by tests and conflict.rs).

    /// Resolve a `/`-style path (root-relative, no leading slash) to a live entry.
    pub fn entry_at(&self, path: &str) -> Option<Entry> {
        let mut cur = ROOT_ID.to_string();
        if !path.is_empty() {
            for seg in path.split('/') {
                cur = self.child_named(&cur, seg)?;
            }
        }
        if cur == ROOT_ID || !self.reaches_root(&cur) {
            return None;
        }
        self.entry(&cur)
    }

    /// Resolve a `/`-style path to an inode (allocating one if needed).
    pub fn resolve_path_ino(&mut self, path: &str) -> Option<u64> {
        let mut cur = ROOT_ID.to_string();
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            cur = self.child_named(&cur, seg)?;
        }
        Some(self.ino_for_id(&cur))
    }

    pub fn live_node_ids(&self) -> Vec<String> {
        self.doc
            .keys(&self.nodes)
            .filter(|id| self.reaches_root(id))
            .collect()
    }

    /// Stable entry lookup used by the storage authority.  It intentionally
    /// exposes logical metadata, never an underlying host path.
    pub fn entry_for_node(&self, id: &str) -> Option<Entry> {
        self.entry(id).filter(|_| self.reaches_root(id))
    }

    /// Resolve the current kernel inode for a stable CRDT node identity.
    pub fn inode_for_node(&mut self, id: &str) -> Option<u64> {
        self.entry_for_node(id)?;
        Some(self.ino_for_id(id))
    }

    pub fn node_hash(&self, id: &str) -> Option<Hash> {
        self.entry(id)?.hash
    }

    pub fn node_parent_name(&self, id: &str) -> Option<(String, String)> {
        Some((self.parent_of(id)?, self.name_of(id)?))
    }

    pub fn child_id_named(&self, parent_id: &str, name: &str) -> Option<String> {
        self.child_named(parent_id, name)
    }

    pub fn content_conflicts(&self, id: &str) -> Vec<(String, String)> {
        let Some(obj) = self.node(id) else {
            return Vec::new();
        };
        match self.doc.get_all(&obj, "hash") {
            Ok(values) => values
                .into_iter()
                .filter_map(|(v, opid)| match v {
                    Value::Scalar(s) => match s.as_ref() {
                        ScalarValue::Str(x) => Some((opid.to_string(), x.to_string())),
                        _ => None,
                    },
                    _ => None,
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Create a conflict-copy sibling node under `parent_id`, unless one named `name` already exists.
    pub fn create_conflict_copy(
        &mut self,
        parent_id: &str,
        name: &str,
        hash: &Hash,
        size: u64,
        mtime: u64,
    ) -> bool {
        if self.child_named(parent_id, name).is_some() {
            return false;
        }
        self.create_node(parent_id, name, NodeKind::File, Some(hash), size, mtime);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(b: u8) -> Hash {
        Hash::from([b; 32])
    }

    #[test]
    fn root_is_inode_one_and_listable() {
        let mut ns = Namespace::new("a");
        assert_eq!(ns.id_for_ino(ROOT_INO), Some(ROOT_ID));
        assert_eq!(ns.getattr(ROOT_INO).unwrap().kind, FileType::Directory);
        assert_eq!(ns.readdir(ROOT_INO).unwrap().len(), 2); // "." ".."
    }

    #[test]
    fn create_lookup_and_nested_readdir() {
        let mut ns = Namespace::new("a");
        let ino = ns
            .create_file(ROOT_INO, "hello.txt", Some(&h(7)), 12, 1)
            .unwrap();
        let attr = ns.lookup(ROOT_INO, "hello.txt").unwrap();
        assert_eq!(attr.ino, INodeNo(ino));
        assert_eq!(attr.size, 12);
        assert_eq!(ns.hash_for(ino), Some(h(7)));

        let dir = ns.create_dir(ROOT_INO, "docs", 1).unwrap();
        ns.create_file(dir, "a.txt", Some(&h(1)), 1, 1).unwrap();
        let names: Vec<String> = ns
            .readdir(dir)
            .unwrap()
            .into_iter()
            .map(|(_, _, n)| n)
            .collect();
        assert!(names.contains(&"a.txt".to_string()));
        let dotdot = ns
            .readdir(dir)
            .unwrap()
            .into_iter()
            .find(|(_, _, n)| n == "..")
            .unwrap();
        assert_eq!(dotdot.0, ROOT_INO);
    }

    #[test]
    fn remove_moves_subtree_to_trash() {
        let mut ns = Namespace::new("a");
        let dir = ns.create_dir(ROOT_INO, "d", 1).unwrap();
        let child = ns.create_file(dir, "c.txt", Some(&h(1)), 1, 1).unwrap();
        assert!(ns.remove(dir));
        assert!(ns.getattr(dir).is_none(), "removed dir not visible");
        assert!(
            ns.getattr(child).is_none(),
            "child follows parent into trash"
        );
        assert!(ns.lookup(ROOT_INO, "d").is_none());
    }

    #[test]
    fn rename_is_a_move_that_preserves_inode() {
        let mut ns = Namespace::new("a");
        let ino = ns.create_file(ROOT_INO, "old", Some(&h(9)), 4, 1).unwrap();
        let dir = ns.create_dir(ROOT_INO, "sub", 1).unwrap();
        assert!(ns.rename(ROOT_INO, "old", dir, "new", 2));
        assert!(ns.lookup(ROOT_INO, "old").is_none());
        let moved = ns.lookup(dir, "new").unwrap();
        assert_eq!(moved.ino, INodeNo(ino), "inode preserved across move");
        assert_eq!(ns.hash_for(ino), Some(h(9)));
    }

    #[test]
    fn rename_rejects_move_into_own_descendant() {
        let mut ns = Namespace::new("a");
        let d1 = ns.create_dir(ROOT_INO, "d1", 1).unwrap();
        let d2 = ns.create_dir(d1, "d2", 1).unwrap();
        assert!(
            !ns.rename(ROOT_INO, "d1", d2, "d1", 2),
            "cannot move d1 under d1/d2"
        );
        assert!(ns.lookup(ROOT_INO, "d1").is_some());
    }

    #[test]
    fn disjoint_inserts_merge() {
        let mut a = Namespace::new("a");
        let bytes = a.save();
        let mut b = Namespace::from_bytes("b", &bytes).unwrap();
        a.create_file(ROOT_INO, "from_a", Some(&h(1)), 1, 1)
            .unwrap();
        b.create_file(ROOT_INO, "from_b", Some(&h(2)), 1, 1)
            .unwrap();
        a.merge_doc(&mut b).unwrap();
        assert!(a.lookup(ROOT_INO, "from_a").is_some());
        assert!(a.lookup(ROOT_INO, "from_b").is_some());
    }

    #[test]
    fn concurrent_cross_move_resolves_acyclically_and_converges() {
        let mut a = Namespace::new("a");
        let xino = a.create_dir(ROOT_INO, "X", 1).unwrap();
        let yino = a.create_dir(ROOT_INO, "Y", 1).unwrap();
        let snap = a.save();
        let mut b = Namespace::from_bytes("b", &snap).unwrap();

        // A moves X under Y; B moves Y under X (concurrently).
        let a_y = a.lookup(ROOT_INO, "Y").unwrap().ino.0;
        a.rename(ROOT_INO, "X", a_y, "X", 5);
        let b_x = b.lookup(ROOT_INO, "X").unwrap().ino.0;
        b.rename(ROOT_INO, "Y", b_x, "Y", 5);
        let _ = (xino, yino);

        let mut a2 = Namespace::from_bytes("a", &a.save()).unwrap();
        let mut b2 = Namespace::from_bytes("b", &b.save()).unwrap();
        let _ = a2.merge_doc(&mut b2.clone_doc());
        let _ = b2.merge_doc(&mut a.clone_doc());
        a2.resolve();
        b2.resolve();

        assert!(
            a2.find_cycle().is_none(),
            "a side must be acyclic after resolve"
        );
        assert!(
            b2.find_cycle().is_none(),
            "b side must be acyclic after resolve"
        );

        let xid = a2
            .live_node_ids()
            .into_iter()
            .find(|i| a2.name_of(i).as_deref() == Some("X"));
        let yid = a2
            .live_node_ids()
            .into_iter()
            .find(|i| a2.name_of(i).as_deref() == Some("Y"));
        assert!(
            xid.is_some() && yid.is_some(),
            "both dirs survive resolution"
        );

        for id in a2.live_node_ids() {
            assert_eq!(
                a2.parent_of(&id),
                b2.parent_of(&id),
                "peers must converge on parent of {id}"
            );
        }
    }

    #[test]
    fn delete_dir_vs_create_child_converges_to_deleted() {
        let mut a = Namespace::new("a");
        let dino = a.create_dir(ROOT_INO, "d", 1).unwrap();
        let snap = a.save();
        let mut b = Namespace::from_bytes("b", &snap).unwrap();

        a.remove(dino);
        let b_d = b.lookup(ROOT_INO, "d").unwrap().ino.0;
        b.create_file(b_d, "new.txt", Some(&h(3)), 3, 1).unwrap();

        let _ = a.merge_doc(&mut b);
        a.resolve();

        assert!(a.lookup(ROOT_INO, "d").is_none(), "d stays deleted");
        let new_id = a
            .doc
            .keys(&a.nodes)
            .find(|i| a.name_of(i).as_deref() == Some("new.txt"));
        assert!(
            new_id.is_some(),
            "the node still exists in the doc (not lost)"
        );
        assert!(
            !a.reaches_root(&new_id.unwrap()),
            "but it is not reachable from root (consistently trashed, not orphaned)"
        );
    }

    #[test]
    fn tree_survives_save_and_reload() {
        let mut a = Namespace::new("a");
        let dir = a.create_dir(ROOT_INO, "docs", 1).unwrap();
        a.create_file(dir, "f.txt", Some(&h(5)), 5, 1).unwrap();
        let bytes = a.save();

        let mut b = Namespace::from_bytes("a", &bytes).unwrap();
        let d = b.lookup(ROOT_INO, "docs").expect("dir restored");
        let f = b.lookup(d.ino.0, "f.txt").expect("nested file restored");
        assert_eq!(b.hash_for(f.ino.0), Some(h(5)));
    }

    #[test]
    fn incremental_apply_and_resolve() {
        let mut a = Namespace::new("a");
        let snap = a.save();
        let mut b = Namespace::from_bytes("b", &snap).unwrap();
        let _ = a.save_incremental();
        a.create_file(ROOT_INO, "delta.txt", Some(&h(4)), 7, 1)
            .unwrap();
        let delta = a.save_incremental();
        b.apply_remote_and_resolve(&delta).unwrap();
        assert_eq!(b.lookup(ROOT_INO, "delta.txt").unwrap().size, 7);
    }

    #[test]
    fn sync_protocol_converges_two_namespaces() {
        // Diverge from a shared base doc; the incremental sync ping-pong must converge them.
        let mut a = Namespace::new("a");
        a.create_file(ROOT_INO, "base", Some(&h(1)), 1, 1).unwrap();
        let mut b = Namespace::from_bytes("b", &a.save()).unwrap();

        a.create_file(ROOT_INO, "from_a", Some(&h(2)), 1, 1)
            .unwrap();
        b.create_file(ROOT_INO, "from_b", Some(&h(3)), 1, 1)
            .unwrap();

        let mut sa = automerge::sync::State::new();
        let mut sb = automerge::sync::State::new();
        for _ in 0..20 {
            let ma = a.sync_generate(&mut sa);
            if let Some(m) = &ma {
                b.sync_receive(&mut sb, m).unwrap();
            }
            let mb = b.sync_generate(&mut sb);
            if let Some(m) = &mb {
                a.sync_receive(&mut sa, m).unwrap();
            }
            if ma.is_none() && mb.is_none() {
                break;
            }
        }
        assert!(a.lookup(ROOT_INO, "from_b").is_some(), "a got b's change");
        assert!(b.lookup(ROOT_INO, "from_a").is_some(), "b got a's change");
    }

    #[test]
    fn document_import_export_roundtrips() {
        let reg = qdoc::builtin_registry();
        let mut ns = Namespace::new("a");
        let ino = ns.create_document(ROOT_INO, "note.md", "md", 1).unwrap();
        assert_eq!(ns.document_codec(ino).as_deref(), Some("md"));

        let md = b"# Title\n\nHello world\n";
        assert!(ns.import_document(ino, &reg, md, 2));
        let out = ns.export_document(ino, &reg).expect("export");
        let s = String::from_utf8_lossy(&out);
        assert!(
            s.contains("Title") && s.contains("Hello world"),
            "round-trip lost content: {s}"
        );
    }

    #[test]
    fn failed_import_preserves_existing_document() {
        let reg = qdoc::builtin_registry();
        let mut ns = Namespace::new("a");
        let ino = ns.create_document(ROOT_INO, "doc.md", "md", 1).unwrap();
        assert!(ns.import_document(ino, &reg, b"# Keep\n\nimportant\n", 2));
        let before = ns.export_document(ino, &reg).unwrap();

        // invalid UTF-8 is rejected by the markdown codec
        assert!(
            !ns.import_document(ino, &reg, &[0xff, 0xfe, 0x00], 3),
            "malformed import must be rejected"
        );
        let after = ns.export_document(ino, &reg).unwrap();
        assert_eq!(
            before, after,
            "existing document must survive a rejected import"
        );
    }

    #[test]
    fn document_concurrent_edits_merge_semantically() {
        // The point of model B: concurrent edits to the same document merge at the text level, not a conflict copy.
        let mut a = Namespace::new("a");
        let ino = a.create_document(ROOT_INO, "doc.md", "md", 1).unwrap();
        let bid = a.document_insert_block(ino, None, "paragraph").unwrap();
        a.document_splice(ino, &bid, 0, 0, "Hello world");

        let snap = a.save();
        let mut b = Namespace::from_bytes("b", &snap).unwrap();
        let bino = b.lookup(ROOT_INO, "doc.md").unwrap().ino.0;

        // concurrent in-place edits to the same paragraph
        a.document_splice(ino, &bid, 5, 0, " dear");
        b.document_splice(bino, &bid, 11, 0, " (B)");

        a.merge_doc(&mut b).unwrap();
        let text = a.document_block_text(ino, &bid).unwrap();
        assert!(text.contains("dear"), "A's edit must survive: {text}");
        assert!(
            text.contains("(B)"),
            "B's edit must survive (text-level merge): {text}"
        );
    }

    // test-only helper to merge using a fresh loaded copy
    impl Namespace {
        fn clone_doc(&mut self) -> Namespace {
            Namespace::from_bytes(self.author.clone(), &self.save()).unwrap()
        }
    }
}
