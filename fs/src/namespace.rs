//! The namespace plane: an Automerge-CRDT-backed directory tree.
//!
//! This replaces the old in-memory `MetadataStore`. The directory structure and
//! per-file metadata live in a single Automerge document that is replicated to
//! every peer, so concurrent structural edits (create / delete / rename) merge
//! deterministically with no lost operations.
//!
//! ## Schema (path-keyed flat map)
//!
//! ```text
//! ROOT.entries : Map<String /*path*/, Entry>
//! Entry        : Map { kind: "file"|"dir", hash: hex?, size: u64,
//!                      mtime: u64 /*ms*/, author: String, deleted: bool }
//! ```
//!
//! Paths are root-relative with no leading slash (`""` is the root, `"a"`,
//! `"a/b.txt"`). We key by path rather than by id+parent on purpose: an
//! id/parent tree CRDT suffers the classic *concurrent-move cycle* (peer A moves
//! `/x` into `/y` while peer B moves `/y` into `/x`); a flat path map makes that
//! representationally impossible. Deletes are tombstones (`deleted: true`) rather
//! than key removal so a concurrent delete+edit merges to "deleted" instead of
//! resurrecting the file.
//!
//! Inode numbers are a *local* concern (each peer assigns its own, stably for the
//! session) and are intentionally NOT stored in the CRDT.

use std::collections::HashMap;
use std::time::{Duration, UNIX_EPOCH};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value, ROOT};
use fuser::{FileAttr, FileType, INodeNo};
use iroh_blobs::Hash;

pub const ROOT_INO: u64 = 1;

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
        if s == "dir" { NodeKind::Dir } else { NodeKind::File }
    }
}

/// A resolved view of a single entry (read out of the CRDT for a path).
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: String,
    pub kind: NodeKind,
    pub hash: Option<Hash>,
    pub size: u64,
    pub mtime: u64,
    pub author: String,
    pub deleted: bool,
}

/// The CRDT-backed directory tree plus the local path<->inode index.
pub struct Namespace {
    doc: AutoCommit,
    entries: ObjId,
    ino_to_path: HashMap<u64, String>,
    path_to_ino: HashMap<String, u64>,
    next_ino: u64,
    author: String,
}

// ---- path helpers ----------------------------------------------------------

fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

fn basename(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn hash_to_hex(h: &Hash) -> String {
    hex::encode(h.as_bytes())
}

fn hash_from_hex(s: &str) -> Option<Hash> {
    let bytes = hex::decode(s).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(Hash::from(arr))
}

// ---- scalar readers --------------------------------------------------------

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

fn read_bool(doc: &AutoCommit, obj: &ObjId, key: &str) -> bool {
    matches!(
        doc.get(obj, key),
        Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), ScalarValue::Boolean(true))
    )
}

impl Namespace {
    pub fn new(author: impl Into<String>) -> Self {
        let mut doc = AutoCommit::new();
        let entries = doc
            .put_object(&ROOT, "entries", ObjType::Map)
            .expect("put entries map");
        let mut ino_to_path = HashMap::new();
        let mut path_to_ino = HashMap::new();
        ino_to_path.insert(ROOT_INO, String::new());
        path_to_ino.insert(String::new(), ROOT_INO);
        Self {
            doc,
            entries,
            ino_to_path,
            path_to_ino,
            next_ino: ROOT_INO + 1,
            author: author.into(),
        }
    }

    /// Reconstruct from a saved Automerge document (bootstrap / load).
    pub fn from_bytes(author: impl Into<String>, data: &[u8]) -> Result<Self, automerge::AutomergeError> {
        let mut doc = AutoCommit::load(data)?;
        let entries = match doc.get(&ROOT, "entries") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc.put_object(&ROOT, "entries", ObjType::Map)?,
        };
        let mut ino_to_path = HashMap::new();
        let mut path_to_ino = HashMap::new();
        ino_to_path.insert(ROOT_INO, String::new());
        path_to_ino.insert(String::new(), ROOT_INO);
        Ok(Self {
            doc,
            entries,
            ino_to_path,
            path_to_ino,
            next_ino: ROOT_INO + 1,
            author: author.into(),
        })
    }

    pub fn author(&self) -> &str {
        &self.author
    }

    // ---- CRDT sync surface -------------------------------------------------

    /// Full snapshot for bootstrapping a freshly-joined peer.
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Incremental changes since the last call — broadcast these over gossip.
    pub fn save_incremental(&mut self) -> Vec<u8> {
        self.doc.save_incremental()
    }

    /// Apply an incremental change blob received from a peer.
    pub fn apply_incremental(&mut self, data: &[u8]) -> Result<(), automerge::AutomergeError> {
        self.doc.load_incremental(data)?;
        Ok(())
    }

    /// Merge another full document into this one (bootstrap exchange).
    pub fn merge_doc(&mut self, other: &mut Namespace) -> Result<(), automerge::AutomergeError> {
        self.doc.merge(&mut other.doc)?;
        Ok(())
    }

    // ---- internal entry access --------------------------------------------

    fn entry_obj(&self, path: &str) -> Option<ObjId> {
        match self.doc.get(&self.entries, path) {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => None,
        }
    }

    /// Read a path's entry, or `None` if it doesn't exist or is tombstoned.
    pub fn entry_at(&self, path: &str) -> Option<Entry> {
        let obj = self.entry_obj(path)?;
        if read_bool(&self.doc, &obj, "deleted") {
            return None;
        }
        let kind = NodeKind::from_str(&read_str(&self.doc, &obj, "kind").unwrap_or_default());
        let hash = read_str(&self.doc, &obj, "hash").and_then(|h| hash_from_hex(&h));
        Some(Entry {
            path: path.to_string(),
            kind,
            hash,
            size: read_u64(&self.doc, &obj, "size"),
            mtime: read_u64(&self.doc, &obj, "mtime"),
            author: read_str(&self.doc, &obj, "author").unwrap_or_default(),
            deleted: false,
        })
    }

    /// All non-tombstoned entry paths (test/debug helper).
    pub fn live_paths(&self) -> Vec<String> {
        self.doc
            .keys(&self.entries)
            .filter(|k| self.entry_at(k).is_some())
            .collect()
    }

    // ---- inode index -------------------------------------------------------

    fn ino_for_path(&mut self, path: &str) -> u64 {
        if let Some(&ino) = self.path_to_ino.get(path) {
            return ino;
        }
        let ino = self.next_ino;
        self.next_ino += 1;
        self.path_to_ino.insert(path.to_string(), ino);
        self.ino_to_path.insert(ino, path.to_string());
        ino
    }

    pub fn path_for_ino(&self, ino: u64) -> Option<&str> {
        self.ino_to_path.get(&ino).map(|s| s.as_str())
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

    // ---- FUSE-facing reads -------------------------------------------------

    /// `getattr` — resolve an inode to its attributes.
    pub fn getattr(&self, ino: u64) -> Option<FileAttr> {
        let path = self.ino_to_path.get(&ino)?.clone();
        if path.is_empty() {
            return Some(self.attr_for(ROOT_INO, NodeKind::Dir, 0, 0));
        }
        let e = self.entry_at(&path)?;
        Some(self.attr_for(ino, e.kind, e.size, e.mtime))
    }

    /// `lookup` — resolve a (parent inode, name) to a child's attributes.
    pub fn lookup(&mut self, parent_ino: u64, name: &str) -> Option<FileAttr> {
        let parent = self.ino_to_path.get(&parent_ino)?.clone();
        let path = join(&parent, name);
        let e = self.entry_at(&path)?;
        let ino = self.ino_for_path(&path);
        Some(self.attr_for(ino, e.kind, e.size, e.mtime))
    }

    /// `readdir` — `(ino, kind, name)` for `.`, `..`, and live children.
    pub fn readdir(&mut self, ino: u64) -> Option<Vec<(u64, FileType, String)>> {
        let path = self.ino_to_path.get(&ino)?.clone();
        // Only directories (root, or an entry of kind Dir) are listable.
        if !path.is_empty() {
            match self.entry_at(&path) {
                Some(e) if e.kind == NodeKind::Dir => {}
                _ => return None,
            }
        }
        let parent_path = parent_of(&path).to_string();
        let parent_ino = self.ino_for_path(&parent_path);

        let mut out = vec![
            (ino, FileType::Directory, ".".to_string()),
            (parent_ino, FileType::Directory, "..".to_string()),
        ];

        let mut children: Vec<(String, NodeKind)> = self
            .doc
            .keys(&self.entries)
            .filter(|k| parent_of(k) == path)
            .filter_map(|k| self.entry_at(&k).map(|e| (k, e.kind)))
            .collect();
        children.sort_by(|a, b| a.0.cmp(&b.0));

        for (child_path, kind) in children {
            let child_ino = self.ino_for_path(&child_path);
            out.push((child_ino, kind.to_file_type(), basename(&child_path).to_string()));
        }
        Some(out)
    }

    /// The blake3 content hash for a file inode, if it has content.
    pub fn hash_for(&self, ino: u64) -> Option<Hash> {
        let path = self.ino_to_path.get(&ino)?;
        self.entry_at(path)?.hash
    }

    // ---- mutations (seeding + FUSE writes) ---------------------------------

    fn write_entry(
        &mut self,
        path: &str,
        kind: NodeKind,
        hash: Option<&Hash>,
        size: u64,
        mtime: u64,
    ) {
        let obj = match self.entry_obj(path) {
            Some(o) => o,
            None => self
                .doc
                .put_object(&self.entries, path, ObjType::Map)
                .expect("put entry object"),
        };
        let _ = self.doc.put(&obj, "kind", kind.as_str());
        match hash {
            Some(h) => {
                let _ = self.doc.put(&obj, "hash", hash_to_hex(h));
            }
            None => {
                let _ = self.doc.delete(&obj, "hash");
            }
        }
        let _ = self.doc.put(&obj, "size", size);
        let _ = self.doc.put(&obj, "mtime", mtime);
        let _ = self.doc.put(&obj, "author", self.author.clone());
        let _ = self.doc.put(&obj, "deleted", false);
    }

    /// Create/replace a directory entry. Returns its inode.
    pub fn create_dir(&mut self, parent_ino: u64, name: &str, mtime: u64) -> Option<u64> {
        let parent = self.ino_to_path.get(&parent_ino)?.clone();
        let path = join(&parent, name);
        self.write_entry(&path, NodeKind::Dir, None, 0, mtime);
        Some(self.ino_for_path(&path))
    }

    /// Create/replace a file entry with content. Returns its inode.
    pub fn create_file(
        &mut self,
        parent_ino: u64,
        name: &str,
        hash: Option<&Hash>,
        size: u64,
        mtime: u64,
    ) -> Option<u64> {
        let parent = self.ino_to_path.get(&parent_ino)?.clone();
        let path = join(&parent, name);
        self.write_entry(&path, NodeKind::File, hash, size, mtime);
        Some(self.ino_for_path(&path))
    }

    /// Update an existing file inode's content hash/size after a write.
    pub fn set_content(&mut self, ino: u64, hash: Option<&Hash>, size: u64, mtime: u64) -> bool {
        let Some(path) = self.ino_to_path.get(&ino).cloned() else {
            return false;
        };
        if self.entry_at(&path).is_none() {
            return false;
        }
        self.write_entry(&path, NodeKind::File, hash, size, mtime);
        true
    }

    /// Tombstone an inode (and, for directories, its whole subtree).
    pub fn remove(&mut self, ino: u64) -> bool {
        let Some(path) = self.ino_to_path.get(&ino).cloned() else {
            return false;
        };
        if self.entry_at(&path).is_none() {
            return false;
        }
        let prefix = format!("{path}/");
        let victims: Vec<String> = self
            .doc
            .keys(&self.entries)
            .filter(|k| *k == path || k.starts_with(&prefix))
            .collect();
        for v in victims {
            if let Some(obj) = self.entry_obj(&v) {
                let _ = self.doc.put(&obj, "deleted", true);
            }
        }
        true
    }

    /// Rename (move) `parent/name` to `newparent/newname`, including any subtree.
    /// Modelled as tombstone-old + insert-new (the two ops FUSE `rename` gives us).
    pub fn rename(
        &mut self,
        parent_ino: u64,
        name: &str,
        new_parent_ino: u64,
        new_name: &str,
        mtime: u64,
    ) -> bool {
        let Some(parent) = self.ino_to_path.get(&parent_ino).cloned() else {
            return false;
        };
        let Some(new_parent) = self.ino_to_path.get(&new_parent_ino).cloned() else {
            return false;
        };
        let old_path = join(&parent, name);
        let new_path = join(&new_parent, new_name);
        if old_path == new_path || self.entry_at(&old_path).is_none() {
            return false;
        }

        // Collect the old subtree (old_path + descendants), snapshot their fields.
        let old_prefix = format!("{old_path}/");
        let moving: Vec<Entry> = self
            .doc
            .keys(&self.entries)
            .filter(|k| *k == old_path || k.starts_with(&old_prefix))
            .filter_map(|k| self.entry_at(&k))
            .collect();

        for e in &moving {
            // Rewrite the path prefix old_path -> new_path.
            let suffix = &e.path[old_path.len()..]; // "" or "/sub/..."
            let dest = format!("{new_path}{suffix}");
            self.write_entry(&dest, e.kind, e.hash.as_ref(), e.size, mtime);
        }
        // Tombstone the old subtree.
        for e in &moving {
            if let Some(obj) = self.entry_obj(&e.path) {
                let _ = self.doc.put(&obj, "deleted", true);
            }
        }
        true
    }

    /// Read raw conflicting `hash` register values at a path (for conflict.rs).
    /// Returns each conflicting (opid-actor, hash-hex) pair when concurrent
    /// writers set different content. Single-value (no conflict) → one element.
    pub fn content_conflicts(&self, path: &str) -> Vec<(String, String)> {
        let Some(obj) = self.entry_obj(path) else {
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
        assert_eq!(ns.path_for_ino(ROOT_INO), Some(""));
        let attr = ns.getattr(ROOT_INO).expect("root attr");
        assert_eq!(attr.kind, FileType::Directory);
        let dir = ns.readdir(ROOT_INO).expect("root readdir");
        // just "." and ".." in an empty fs
        assert_eq!(dir.len(), 2);
    }

    #[test]
    fn create_and_lookup_file() {
        let mut ns = Namespace::new("a");
        let ino = ns.create_file(ROOT_INO, "hello.txt", Some(&h(7)), 12, 100).unwrap();
        assert_ne!(ino, ROOT_INO);

        let attr = ns.lookup(ROOT_INO, "hello.txt").expect("lookup");
        assert_eq!(attr.ino, INodeNo(ino));
        assert_eq!(attr.size, 12);
        assert_eq!(attr.kind, FileType::RegularFile);
        assert_eq!(ns.hash_for(ino), Some(h(7)));

        // inode is stable across repeated lookups
        let again = ns.lookup(ROOT_INO, "hello.txt").unwrap();
        assert_eq!(again.ino, INodeNo(ino));
    }

    #[test]
    fn nested_dirs_and_readdir() {
        let mut ns = Namespace::new("a");
        let dir = ns.create_dir(ROOT_INO, "docs", 1).unwrap();
        ns.create_file(dir, "a.txt", Some(&h(1)), 1, 1).unwrap();
        ns.create_file(dir, "b.txt", Some(&h(2)), 2, 1).unwrap();

        let names: Vec<String> = ns
            .readdir(dir)
            .unwrap()
            .into_iter()
            .map(|(_, _, n)| n)
            .collect();
        assert!(names.contains(&"a.txt".to_string()));
        assert!(names.contains(&"b.txt".to_string()));
        assert!(names.contains(&".".to_string()));
        assert!(names.contains(&"..".to_string()));

        // root only shows the dir, not the nested files
        let root_names: Vec<String> = ns
            .readdir(ROOT_INO)
            .unwrap()
            .into_iter()
            .map(|(_, _, n)| n)
            .filter(|n| n != "." && n != "..")
            .collect();
        assert_eq!(root_names, vec!["docs".to_string()]);
    }

    #[test]
    fn dotdot_points_at_parent() {
        let mut ns = Namespace::new("a");
        let dir = ns.create_dir(ROOT_INO, "sub", 1).unwrap();
        let entries = ns.readdir(dir).unwrap();
        let dotdot = entries.iter().find(|(_, _, n)| n == "..").unwrap();
        assert_eq!(dotdot.0, ROOT_INO, ".. of a child must be the root inode");
    }

    #[test]
    fn remove_tombstones_and_hides() {
        let mut ns = Namespace::new("a");
        let ino = ns.create_file(ROOT_INO, "x", Some(&h(1)), 1, 1).unwrap();
        assert!(ns.lookup(ROOT_INO, "x").is_some());
        assert!(ns.remove(ino));
        assert!(ns.lookup(ROOT_INO, "x").is_none(), "removed file must vanish");
        assert!(ns.getattr(ino).is_none());
    }

    #[test]
    fn remove_dir_takes_subtree() {
        let mut ns = Namespace::new("a");
        let dir = ns.create_dir(ROOT_INO, "d", 1).unwrap();
        let child = ns.create_file(dir, "c.txt", Some(&h(1)), 1, 1).unwrap();
        assert!(ns.remove(dir));
        assert!(ns.getattr(dir).is_none());
        assert!(ns.getattr(child).is_none(), "subtree must be removed too");
    }

    #[test]
    fn rename_moves_file() {
        let mut ns = Namespace::new("a");
        ns.create_file(ROOT_INO, "old.txt", Some(&h(9)), 4, 1).unwrap();
        assert!(ns.rename(ROOT_INO, "old.txt", ROOT_INO, "new.txt", 2));
        assert!(ns.lookup(ROOT_INO, "old.txt").is_none());
        let moved = ns.lookup(ROOT_INO, "new.txt").expect("renamed file");
        assert_eq!(ns.hash_for(moved.ino.0), Some(h(9)));
    }

    #[test]
    fn rename_moves_subtree() {
        let mut ns = Namespace::new("a");
        let d = ns.create_dir(ROOT_INO, "d1", 1).unwrap();
        ns.create_file(d, "f.txt", Some(&h(3)), 1, 1).unwrap();
        let d2 = ns.create_dir(ROOT_INO, "d2", 1).unwrap();
        assert!(ns.rename(ROOT_INO, "d1", d2, "d1moved", 2));

        assert!(ns.lookup(ROOT_INO, "d1").is_none());
        let inner = ns.lookup(d2, "d1moved").expect("moved dir");
        let f = ns.lookup(inner.ino.0, "f.txt").expect("moved file");
        assert_eq!(ns.hash_for(f.ino.0), Some(h(3)));
    }

    // ---- the CRDT property that justifies all of this: merge ----

    #[test]
    fn disjoint_inserts_merge() {
        let mut a = Namespace::new("a");
        let bytes = a.save();
        let mut b = Namespace::from_bytes("b", &bytes).unwrap();

        a.create_file(ROOT_INO, "from_a.txt", Some(&h(1)), 1, 1).unwrap();
        b.create_file(ROOT_INO, "from_b.txt", Some(&h(2)), 1, 1).unwrap();

        a.merge_doc(&mut b).unwrap();

        assert!(a.lookup(ROOT_INO, "from_a.txt").is_some());
        assert!(a.lookup(ROOT_INO, "from_b.txt").is_some(), "remote insert must appear after merge");
    }

    #[test]
    fn concurrent_delete_beats_resurrect() {
        // a creates x; both sync; a edits x while b deletes x; after merge x stays deleted.
        let mut a = Namespace::new("a");
        let ino = a.create_file(ROOT_INO, "x", Some(&h(1)), 1, 1).unwrap();
        let snap = a.save();
        let mut b = Namespace::from_bytes("b", &snap).unwrap();

        // concurrent ops
        a.set_content(ino, Some(&h(2)), 2, 5); // a edits
        let b_ino = b.lookup(ROOT_INO, "x").unwrap().ino.0;
        b.remove(b_ino); // b deletes

        a.merge_doc(&mut b).unwrap();
        assert!(
            a.lookup(ROOT_INO, "x").is_none(),
            "a tombstone must not be resurrected by a concurrent edit"
        );
    }

    #[test]
    fn incremental_changes_apply() {
        let mut a = Namespace::new("a");
        let snap = a.save();
        let mut b = Namespace::from_bytes("b", &snap).unwrap();
        // drain a's incremental baseline so the next call only has the new op
        let _ = a.save_incremental();

        a.create_file(ROOT_INO, "delta.txt", Some(&h(4)), 7, 1).unwrap();
        let delta = a.save_incremental();
        assert!(!delta.is_empty());

        b.apply_incremental(&delta).unwrap();
        let attr = b.lookup(ROOT_INO, "delta.txt").expect("delta applied on b");
        assert_eq!(attr.size, 7);
    }
}
