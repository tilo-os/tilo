//! Importing a manifest into one collaborative workspace.
//!
//! The workspace is the production plane, not a model of it: a `Namespace`
//! (the Automerge document the mount serves paths from), a `ContentStore`
//! (content addressed, chunked and deduplicating exactly as `tilo-fs` does),
//! and a durable `FileHistoryStore` holding one chain per record. What is
//! measured on it is therefore what a person gets, because the code path is
//! the mount's.
//!
//! Two properties decide whether the import is a faithful checkpoint:
//!
//! - **Content is shared, history is not.** Two paths with identical bytes
//!   cost one blob and get two history chains, because they are two things a
//!   person can edit independently. A comparison that ignored this would
//!   overstate one plane or the other.
//! - **A checkpoint is a set of versions, not a set of files.** History is
//!   append-only and immutable, keyed by `FileRecordId`, so materialising "the
//!   tree as of checkpoint N" is a query over each chain's head — never a
//!   re-checkout.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use collab_types::Ed25519PublicKey;
use fs::{ContentStore, FileHistoryStore, Namespace, ROOT_INO};
use fs_protocol::{
    ContentHash, FileActor, FileProvenance, FileRecordId, FileVersionId, FileVersionV1,
};
use iroh_blobs::Hash;

use crate::codebase::{Kind, ManifestEntry, SourceTree};
use crate::measurements::{Timed, millis};

/// One fixed timestamp for a whole import: a checkpoint is identified by its
/// content, and a clock read per file would make two imports of one tree
/// disagree about which is newer.
const IMPORT_MTIME_MS: u64 = 1_700_000_000;

/// Where an imported workspace was written, and what it took to write it.
pub struct Imported {
    /// The saved Automerge namespace document.
    pub namespace_path: PathBuf,
    /// The durable file-history document.
    pub history_path: PathBuf,
    /// The store the import wrote its content into, kept open so the second
    /// checkpoint writes through the store a running session already has
    /// rather than reopening it. Reopening is not what an edit costs.
    pub store: std::sync::Arc<ContentStore>,
    /// `path` → the `ino` the namespace gave it, for every imported file.
    pub inodes: BTreeMap<String, u64>,
    /// The same for every directory the import created, keyed by path. A
    /// caller resolving a path needs its parent's inode, and a parent is
    /// usually a directory the import made rather than a named entry.
    pub directories: BTreeMap<String, u64>,
    /// `path` → the record whose history chain it owns.
    pub records: BTreeMap<String, FileRecordId>,
    /// `path` → the content id of what it holds, where it holds content.
    pub contents: BTreeMap<String, Hash>,
    pub imported: Timed,
    /// Entries the import could not carry, with why. An import that silently
    /// skipped a path would not be a checkpoint of anything, so a caller can
    /// see and refuse this.
    pub failed: Vec<String>,
}

impl Imported {
    /// Whether every entry of the manifest reached the namespace with its
    /// own history record and nothing was refused.
    ///
    /// `expected` is how many manifest entries the import was given. There are
    /// more records than that, because every directory holds one too — the
    /// shape of the tree is part of the checkpoint.
    #[must_use]
    pub fn complete(&self, expected: usize) -> bool {
        self.failed.is_empty()
            && self.inodes.len() == expected
            && self
                .inodes
                .keys()
                .all(|path| self.records.contains_key(path))
    }

    /// Every distinct content the import wrote.
    #[must_use]
    pub fn distinct_contents(&self) -> usize {
        let set: std::collections::BTreeSet<&Hash> = self.contents.values().collect();
        set.len()
    }
}

/// What the import builds before it is written to disk.
struct ImportedTree {
    namespace: Namespace,
    versions: Vec<FileVersionV1>,
    inodes: BTreeMap<String, u64>,
    directories: BTreeMap<String, u64>,
    records: BTreeMap<String, FileRecordId>,
    contents: BTreeMap<String, Hash>,
    failed: Vec<String>,
}

/// Import `source` into a workspace under `root`.
///
/// The returned [`Imported`] names where the three durable files went, so a
/// caller can reopen the workspace and measure the cost of doing exactly that
/// rather than of a warm in-memory copy.
///
/// # Errors
///
/// Returns the error when the directory cannot be made or a durable file
/// cannot be written. A single unimportable entry is not an error: it lands in
/// [`Imported::failed`], because an import that stops at the first odd path in
/// a real tree never finishes.
pub async fn import(source: &SourceTree, root: &Path) -> anyhow::Result<Imported> {
    let started = std::time::Instant::now();
    std::fs::create_dir_all(root)?;
    let content_dir = root.join("blobs");
    let namespace_path = root.join("namespace.bin");
    let history_path = root.join("history");
    let store = std::sync::Arc::new(ContentStore::open(&content_dir).await?);
    let entries = &source.manifest().entries;
    let mut tree = populate(source, &store, entries).await?;
    let saved = tree.namespace.save();
    let history = FileHistoryStore::open(&history_path)?;
    history.append_batch(&tree.versions)?;
    history.flush()?;
    std::fs::write(&namespace_path, &saved)?;
    let failed = std::mem::take(&mut tree.failed);
    let ImportedTree {
        inodes,
        directories,
        records,
        contents,
        ..
    } = tree;
    Ok(Imported {
        namespace_path,
        history_path,
        store,
        inodes,
        directories,
        records,
        contents,
        imported: Timed {
            millis: millis(started.elapsed()),
            correct: failed.is_empty(),
        },
        failed,
    })
}

/// Write every entry's content and create its namespace node.
async fn populate(
    source: &SourceTree,
    store: &ContentStore,
    entries: &[ManifestEntry],
) -> std::io::Result<ImportedTree> {
    let mut tree = ImportedTree {
        namespace: Namespace::new("qualification-importer")
            .map_err(|error| std::io::Error::other(error.to_string()))?,
        versions: Vec::new(),
        inodes: BTreeMap::new(),
        directories: BTreeMap::new(),
        records: BTreeMap::new(),
        contents: BTreeMap::new(),
        failed: Vec::new(),
    };
    let mut directories: BTreeMap<String, u64> = BTreeMap::new();
    directories.insert(String::new(), ROOT_INO);
    tree.directories.insert(String::new(), ROOT_INO);
    // One blob write per distinct content: a tree of 21 000 paths names far
    // fewer blobs, and the store deduplicates by content regardless.
    let mut written: BTreeMap<&str, Option<Hash>> = BTreeMap::new();
    for entry in entries {
        let Some(parent) = directory_for(&mut tree, &mut directories, &entry.path) else {
            tree.failed
                .push(format!("{} (no parent directory)", entry.path));
            continue;
        };
        let name = entry
            .path
            .rsplit_once('/')
            .map_or(entry.path.as_str(), |(_, name)| name);
        let cached = written.get(entry.hash.as_str()).copied();
        let hash = if let Some(hash) = cached {
            hash
        } else {
            {
                // A submodule names a commit and holds no bytes of its own;
                // any other entry whose content is missing is a hole in the
                // import, which the caller must see rather than paper over
                // with an empty file.
                let bytes = if entry.kind == Kind::Submodule {
                    &[][..]
                } else {
                    let Some(bytes) = source.blob(&entry.hash) else {
                        tree.failed
                            .push(format!("{} (no content for {})", entry.path, entry.hash));
                        continue;
                    };
                    bytes
                };
                let hash = if entry.kind == Kind::Submodule || bytes.is_empty() {
                    None
                } else {
                    match store.ingest(bytes.to_vec()).await {
                        Ok(hash) => Some(hash),
                        Err(error) => {
                            tree.failed.push(format!("{} ({error})", entry.path));
                            continue;
                        }
                    }
                };
                written.insert(entry.hash.as_str(), hash);
                hash
            }
        };
        let ino = {
            let namespace = &mut tree.namespace;
            match entry.kind {
                Kind::Directory => None,
                Kind::File { .. } | Kind::Symlink => {
                    namespace.create_file(parent, name, hash.as_ref(), entry.bytes, IMPORT_MTIME_MS)
                }
                // A submodule is a directory holding no bytes of its own, so
                // its commit id lives in its history and nowhere else.
                Kind::Submodule => namespace.create_dir(parent, name, IMPORT_MTIME_MS),
            }
        };
        let Some(ino) = ino else {
            tree.failed
                .push(format!("{} (namespace refused the name)", entry.path));
            continue;
        };
        if let Some(hash) = hash {
            tree.contents.insert(entry.path.clone(), hash);
        }
        let record_id = tree.namespace.record_id_for(ino).unwrap_or_default();
        tree.records.insert(entry.path.clone(), record_id);
        let kind = match entry.kind {
            Kind::Submodule => Kind::Directory,
            kind => kind,
        };
        tree.versions
            .push(version(record_id, hash, entry.bytes, kind));
        tree.inodes.insert(entry.path.clone(), ino);
    }
    Ok(tree)
}

/// The history version that says "this record was created with this content".
fn version(record_id: FileRecordId, hash: Option<Hash>, bytes: u64, kind: Kind) -> FileVersionV1 {
    version_after(record_id, &[], hash, bytes, kind, FileProvenance::Create)
}

/// A version of `record_id` that supersedes `parents`.
fn version_after(
    record_id: FileRecordId,
    parents: &[FileVersionId],
    hash: Option<Hash>,
    bytes: u64,
    kind: Kind,
    provenance: FileProvenance,
) -> FileVersionV1 {
    FileVersionV1 {
        id: FileVersionId::new(),
        record_id,
        parents: parents.to_vec(),
        // The importer is one device doing every create, not a simulation of
        // a crowd: a checkpoint's history should attribute to whoever
        // imported it.
        actor: FileActor {
            person: Ed25519PublicKey([0x71; 32]),
            device: Ed25519PublicKey([0x72; 32]),
        },
        timestamp_ms: IMPORT_MTIME_MS,
        content_hash: hash.map(|hash| ContentHash(*hash.as_bytes())),
        // What the path *is*, apart from its bytes: a peer comparing two
        // checkpoints has to see a mode change and a truncation as changes
        // even where the content id did not move.
        metadata_hash: ContentHash(*blake3::hash(&metadata_bytes(bytes, kind)).as_bytes()),
        provenance,
    }
}

/// A second checkpoint on top of [`import`]: `edits` names paths whose
/// content changes between the two, and every one of them gets a version
/// whose parents are the heads the first checkpoint left.
///
/// This is what makes a workspace a history rather than a snapshot, and what a
/// person asks after a reboot: what changed since I was last here. The answer
/// is the set of records whose heads moved, which is comparable to
/// `git diff --name-only <first> <second>` without reading a byte of content.
///
/// # Errors
///
/// Returns the error when the workspace or its history cannot be reopened, or
/// a history append is refused.
/// The inode `path` names in `namespace`, walking it from the root.
fn resolve(namespace: &mut Namespace, path: &str) -> Option<u64> {
    let mut ino = fs::ROOT_INO;
    for segment in path.split('/') {
        ino = namespace.lookup(ino, segment)?.ino.0;
    }
    Some(ino)
}

pub async fn advance(imported: &Imported, edits: &[(String, Vec<u8>)]) -> anyhow::Result<Advanced> {
    let started = std::time::Instant::now();
    let mut namespace = Namespace::from_bytes(
        "qualification-advance",
        &std::fs::read(&imported.namespace_path)?,
    )?;
    let history = FileHistoryStore::open(&imported.history_path)?;
    let store = std::sync::Arc::clone(&imported.store);
    let mut versions = Vec::with_capacity(edits.len());
    let mut changed = 0_usize;
    for (path, bytes) in edits {
        let Some(record_id) = imported.records.get(path).copied() else {
            continue;
        };
        // Inodes are a per-instance allocation: the reloaded namespace hands
        // out its own, so the path is resolved again rather than carried over
        // from the checkpoint that wrote it.
        let Some(ino) = resolve(&mut namespace, path) else {
            continue;
        };
        let hash = store.ingest(bytes.clone()).await?;
        let size = bytes.len() as u64;
        if !namespace.set_content(ino, Some(&hash), size, IMPORT_MTIME_MS) {
            continue;
        }
        // The version a person sees after this edit supersedes whatever heads
        // the first checkpoint left, which is what makes the pair a history
        // rather than two unrelated snapshots.
        let heads = history
            .snapshot(record_id)
            .map(|snapshot| snapshot.heads)
            .unwrap_or_default();
        versions.push(version_after(
            record_id,
            &heads,
            Some(hash),
            size,
            Kind::File { executable: false },
            FileProvenance::Edit,
        ));
        changed = changed.saturating_add(1);
    }
    history.append_batch(&versions)?;
    history.flush()?;
    std::fs::write(&imported.namespace_path, namespace.save())?;
    Ok(Advanced {
        changed,
        committed: versions.len(),
        millis: millis(started.elapsed()),
    })
}

/// What [`advance`] did.
pub struct Advanced {
    /// Paths whose namespace entry was updated.
    pub changed: usize,
    /// Versions appended across every record.
    pub committed: usize,
    /// How long the whole edit took.
    pub millis: u64,
}

/// Mode and length, wide enough for both: four bytes of mode, eight of length.
fn metadata_bytes(bytes: u64, kind: Kind) -> [u8; 12] {
    let mode = match kind {
        Kind::Directory | Kind::Submodule | Kind::File { executable: true } => 0o755_u32,
        Kind::Symlink => 0o777,
        Kind::File { executable: false } => 0o644,
    };
    let mut buffer = [0_u8; 12];
    buffer[..4].copy_from_slice(&mode.to_be_bytes());
    buffer[4..].copy_from_slice(&bytes.to_be_bytes());
    buffer
}

/// Create `path`'s parent directories on demand, returning the parent inode.
///
/// Every directory is a record too, with its own chain: a checkpoint that
/// could not reconstruct the shape of the tree would not be one, and a tree
/// that had a directory removed and re-added is a different tree from one that
/// never did.
fn directory_for(
    tree: &mut ImportedTree,
    directories: &mut BTreeMap<String, u64>,
    path: &str,
) -> Option<u64> {
    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
    if let Some(ino) = directories.get(parent) {
        return Some(*ino);
    }
    let grandparent = directory_for(tree, directories, parent)?;
    let name = parent.rsplit_once('/').map_or(parent, |(_, name)| name);
    let ino = tree
        .namespace
        .create_dir(grandparent, name, IMPORT_MTIME_MS)?;
    let record_id = tree.namespace.record_id_for(ino).unwrap_or_default();
    tree.records.insert(parent.to_owned(), record_id);
    tree.versions
        .push(version(record_id, None, 0, Kind::Directory));
    directories.insert(parent.to_owned(), ino);
    tree.directories.insert(parent.to_owned(), ino);
    Some(ino)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_separates_executability_from_length() {
        let plain = metadata_bytes(10, Kind::File { executable: false });
        let executable = metadata_bytes(10, Kind::File { executable: true });
        let longer = metadata_bytes(11, Kind::File { executable: false });
        assert_ne!(plain, executable);
        assert_ne!(plain, longer);
        assert_eq!(plain, metadata_bytes(10, Kind::File { executable: false }));
    }

    #[test]
    fn a_symlink_is_a_different_kind_than_a_file_of_equal_bytes() {
        let link = Kind::Symlink;
        let file = Kind::File { executable: false };
        assert_ne!(link, file);
        assert_ne!(metadata_bytes(11, link), metadata_bytes(11, file));
    }
}
