//! `fuser::Filesystem` implementation that serves a qOS package
//! payload from a content-addressed [`BlobStore`] through an
//! [`PageCache`].
//!
//! The Collection (a flat `path -> blob_hash` table) is expanded into
//! an inode tree at mount time: inode 1 ([`FUSE_ROOT_ID`]) is the
//! root, directories and files get inodes on first visit / at build
//! time. Allocation is a deterministic depth-first walk over the
//! already-sorted path list — stable for the mount's lifetime, but not
//! meaningful across mounts.
//!
//! Inodes are a flat `Vec<Entry>` rather than recursive structs:
//! `lookup` is hot (called per path component by the kernel) and an
//! array-indexed lookup beats `HashMap` indirection; the mount is also
//! read-only after construction, so a `Vec` with stable indices needs
//! no insert/delete support.
//!
//! [`PageCache`] absorbs duplicate mmap-fault page reads; the kernel's
//! own `getattr`/`lookup` cache uses [`TTL`], which is safe here since
//! nothing cached can go stale on a read-only payload.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    KernelConfig, LockOwner, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    ReplyOpen, Request,
};

use crate::cache::PageCache;
use crate::collection::Collection;
use crate::store::{BlobError, BlobStore};

/// The root inode number — FUSE hard-codes this to 1; `fuser` 0.17
/// doesn't re-export the ABI constant, so mirror it inline.
pub const FUSE_ROOT_ID: u64 = 1;

/// Attribute/entry kernel TTL — generous, since a qOS payload is
/// immutable for the mount's lifetime (content-addressed; a new
/// package id publishes rather than mutates).
pub const TTL: Duration = Duration::from_secs(3600);

/// Default kernel max-readahead (1 MiB, matching fs/'s workspace
/// mount) — large enough that one mmap fault pulls a meaningful prefix into cache.
pub const MAX_READAHEAD: u32 = 1024 * 1024;

/// Default page-cache capacity: 4096 pages * 4 KiB = 16 MiB per
/// mount; tunable via [`QosAppFs::with_cache_pages`].
pub const DEFAULT_CACHE_PAGES: usize = 4096;

/// Convert a 1-based inode number (the numbering FUSE and
/// [`Entry::Dir`]'s `children` map use) to its 0-based index into
/// `entries`. `None` for inode 0 — never issued by FUSE — or an inode
/// number that doesn't fit `usize` (only reachable on a 32-bit target
/// with an implausibly large inode table).
fn index_of(ino: u64) -> Option<usize> {
    usize::try_from(ino.checked_sub(1)?).ok()
}

/// The Filesystem implementation.
pub struct QosAppFs<S: BlobStore + 'static> {
    /// Indexed by inode-1 (`FUSE_ROOT_ID` is inode 1, stored at index 0).
    entries: Vec<Entry>,
    /// Cache fronting `store`, wrapped in `Arc` so concurrent FUSE workers share state cheaply.
    cache: Arc<PageCache<S>>,
    /// Owner uid/gid stamped on every getattr reply; defaults to the mounting process.
    uid: u32,
    gid: u32,
}

/// One entry in the inode table.
enum Entry {
    /// Directory with a `name -> child-inode` table.
    Dir {
        /// Sorted child table for deterministic `readdir`.
        children: BTreeMap<String, u64>,
    },
    /// File backed by a blob.
    Blob {
        hash: [u8; 32],
        /// Computed once at mount time so `getattr` never calls into the store for size.
        size: u64,
    },
}

impl<S: BlobStore + 'static> QosAppFs<S> {
    /// Build a filesystem rooted at `collection`'s payload tree, reading
    /// blobs from `store`, with the default page cache size.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] if `collection` contains an invalid path,
    /// two entries overlap (one path is a directory-prefix of
    /// another's), or `store` can't report a referenced blob's size.
    pub fn new(collection: &Collection, store: S) -> Result<Self, BuildError> {
        Self::with_cache_pages(collection, store, DEFAULT_CACHE_PAGES)
    }

    /// Same as [`Self::new`] but with a caller-chosen cache capacity in pages (4 KiB each).
    ///
    /// # Errors
    ///
    /// Same conditions as [`Self::new`].
    pub fn with_cache_pages(
        collection: &Collection,
        store: S,
        cache_pages: usize,
    ) -> Result<Self, BuildError> {
        // Inode table: index 0 is the root, allocated up front.
        let mut entries: Vec<Entry> = vec![Entry::Dir {
            children: BTreeMap::new(),
        }];

        // Build directory tree.
        for (path, hash) in &collection.entries {
            if path.is_empty() || path.contains("//") {
                return Err(BuildError::BadPath(path.clone()));
            }
            let segments: Vec<&str> = path.split('/').collect();
            if segments
                .iter()
                .any(|s| s.is_empty() || *s == "." || *s == "..")
            {
                return Err(BuildError::BadPath(path.clone()));
            }

            // Walk down, allocating directory inodes as needed.
            let mut current: u64 = FUSE_ROOT_ID;
            let last_segment = segments.len().saturating_sub(1);
            for (i, seg) in segments.iter().enumerate() {
                let is_last = i == last_segment;
                let current_idx = index_of(current).ok_or_else(|| {
                    BuildError::Internal(format!("bad inode {current} while building {path:?}"))
                })?;
                let existing = match entries.get(current_idx) {
                    Some(Entry::Dir { children }) => children.get(*seg).copied(),
                    Some(Entry::Blob { .. }) => {
                        return Err(BuildError::PathOverlap(path.clone()));
                    }
                    None => {
                        return Err(BuildError::Internal(format!(
                            "inode {current} missing while building {path:?}"
                        )));
                    }
                };
                if let Some(child_ino) = existing {
                    if is_last {
                        // Two collection entries at the same path — the Collection decoder
                        // already rejects this, but belt-and-braces.
                        return Err(BuildError::PathOverlap(path.clone()));
                    }
                    let child_idx = index_of(child_ino).ok_or_else(|| {
                        BuildError::Internal(format!(
                            "bad child inode {child_ino} while building {path:?}"
                        ))
                    })?;
                    if !matches!(entries.get(child_idx), Some(Entry::Dir { .. })) {
                        return Err(BuildError::PathOverlap(path.clone()));
                    }
                    current = child_ino;
                    continue;
                }

                // Allocate.
                if is_last {
                    let size = store
                        .size(hash)
                        .map_err(|e| BuildError::Store(path.clone(), e))?;
                    entries.push(Entry::Blob { hash: *hash, size });
                } else {
                    entries.push(Entry::Dir {
                        children: BTreeMap::new(),
                    });
                }
                let new_ino = entries.len() as u64; // 1-based.
                let Some(Entry::Dir { children }) = entries.get_mut(current_idx) else {
                    return Err(BuildError::Internal(format!(
                        "parent inode {current} stopped being a directory while building {path:?}"
                    )));
                };
                children.insert((*seg).to_string(), new_ino);
                current = new_ino;
            }
        }

        let cache = Arc::new(PageCache::new(store, cache_pages));
        // SAFETY: getuid/getgid never fail.
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        Ok(Self {
            entries,
            cache,
            uid,
            gid,
        })
    }

    /// Override the owner uid/gid stamped on every reply (default:
    /// mounting process); the launcher may remap it to the sandboxed user.
    #[must_use]
    pub fn with_owner(mut self, uid: u32, gid: u32) -> Self {
        self.uid = uid;
        self.gid = gid;
        self
    }

    /// Borrow the underlying page cache — lets tests assert hit counters
    /// and `qos-app-host` plumb introspection.
    pub fn cache(&self) -> Arc<PageCache<S>> {
        Arc::clone(&self.cache)
    }

    /// Number of inodes (1 + `file_count` + `dir_count`) — for introspection/tests.
    pub fn inode_count(&self) -> usize {
        self.entries.len()
    }

    /// Resolve `path` (payload-relative, `/`-separated, no leading `/`) to
    /// its inode; `None` if a segment is missing or crosses a
    /// non-directory. Test/introspection only — the FUSE kernel path uses `lookup`.
    pub fn ino_of(&self, path: &str) -> Option<u64> {
        let mut current: u64 = FUSE_ROOT_ID;
        if path.is_empty() {
            return Some(current);
        }
        for seg in path.split('/') {
            let Entry::Dir { children } = index_of(current).and_then(|i| self.entries.get(i))?
            else {
                return None;
            };
            current = *children.get(seg)?;
        }
        Some(current)
    }

    /// The (hash, size) for a file inode; `None` for directories or
    /// unknown inodes. Test-only — exercises the same cache path as the
    /// FUSE `read` handler without a real kernel mount.
    pub fn blob_at(&self, ino: u64) -> Option<([u8; 32], u64)> {
        match self.entries.get(index_of(ino)?)? {
            Entry::Blob { hash, size } => Some((*hash, *size)),
            Entry::Dir { .. } => None,
        }
    }

    /// Build the `FileAttr` for `ino`; caller hands it to the correct `Reply…`.
    fn attr(&self, ino: u64) -> Option<FileAttr> {
        let entry = self.entries.get(index_of(ino)?)?;
        let (kind, size, perm, nlink) = match entry {
            Entry::Dir { children } => {
                // 2 + child-dir count is the historical `.`/`..` convention; a
                // read-only mount mostly has the kernel ignore this, but use it anyway.
                let sub_dir_count = children
                    .values()
                    .filter(|c| {
                        matches!(
                            index_of(**c).and_then(|i| self.entries.get(i)),
                            Some(Entry::Dir { .. })
                        )
                    })
                    .count();
                let sub_dirs = u32::try_from(sub_dir_count).unwrap_or(u32::MAX);
                let nlink = 2u32.saturating_add(sub_dirs);
                (FileType::Directory, 0u64, 0o555u16, nlink)
            }
            Entry::Blob { size, .. } => (FileType::RegularFile, *size, 0o444u16, 1u32),
        };
        let now = SystemTime::now();
        Some(FileAttr {
            ino: INodeNo(ino),
            size,
            blocks: size.div_ceil(512),
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind,
            perm,
            nlink,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        })
    }
}

/// Errors from [`QosAppFs::new`] / [`QosAppFs::with_cache_pages`].
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// A Collection entry's path is empty, has an empty segment, `.`, `..`, or `//`.
    #[error("invalid path in collection: {0:?}")]
    BadPath(String),
    /// Two entries collide — one declares a path the other treats as a directory prefix.
    #[error("path overlap in collection: {0:?}")]
    PathOverlap(String),
    /// The store couldn't report the size of a blob the Collection points at.
    #[error("blob size lookup failed for {0:?}: {1}")]
    Store(String, BlobError),
    /// An internal invariant was violated while building the inode
    /// table — always a bug in the builder, never a bad [`Collection`].
    /// Surfaced as an error rather than a panic so a broken build fails
    /// the mount cleanly instead of crashing the process.
    #[error("internal error building inode table: {0}")]
    Internal(String),
}

impl<S: BlobStore + 'static> Filesystem for QosAppFs<S> {
    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> Result<(), std::io::Error> {
        let _ = config.set_max_readahead(MAX_READAHEAD);
        Ok(())
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(name) = name.to_str() else {
            reply.error(Errno::EINVAL);
            return;
        };
        let Some(parent_entry) = index_of(parent.0).and_then(|i| self.entries.get(i)) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let Entry::Dir { children } = parent_entry else {
            reply.error(Errno::ENOTDIR);
            return;
        };
        let Some(&child_ino) = children.get(name) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let Some(attr) = self.attr(child_ino) else {
            reply.error(Errno::EIO);
            return;
        };
        reply.entry(&TTL, &attr, Generation(0));
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.attr(ino.0) {
            Some(attr) => reply.attr(&TTL, &attr),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn open(&self, _req: &Request, _ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        // Keep the kernel page cache: a re-read of the same page is served by
        // the kernel without coming back to us — exactly what we want for a
        // read-only payload.
        reply.opened(FileHandle(0), FopenFlags::FOPEN_KEEP_CACHE);
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let Some(entry) = index_of(ino.0).and_then(|i| self.entries.get(i)) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let (hash, blob_size) = match entry {
            Entry::Blob { hash, size } => (*hash, *size),
            Entry::Dir { .. } => {
                reply.error(Errno::EINVAL);
                return;
            }
        };
        // Clamp to EOF: `offset >= size` is a legitimate "no more data" reply, not an error.
        if offset >= blob_size {
            reply.data(&[]);
            return;
        }
        match self.cache.read(&hash, blob_size, offset, size) {
            Ok(bytes) => reply.data(&bytes),
            Err(e) => {
                tracing::warn!(?e, "blob read failed; replying EIO");
                reply.error(Errno::EIO);
            }
        }
    }

    fn opendir(&self, _req: &Request, _ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        reply.opened(FileHandle(0), FopenFlags::empty());
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let Some(Entry::Dir { children }) = index_of(ino.0).and_then(|i| self.entries.get(i))
        else {
            reply.error(Errno::ENOENT);
            return;
        };
        // `.` and `..` first, then sorted children. parent_ino is synthesized
        // as `ino` itself (root convention; non-root parents aren't tracked) —
        // the kernel never resolves `..`'s destination on a read-only mount,
        // but passing `ino` instead of 0 avoids surprising strict callers.
        let mut idx = 0u64;
        let entries_iter = [
            (ino.0, FileType::Directory, "."),
            (ino.0, FileType::Directory, ".."),
        ]
        .into_iter()
        .map(|(ino, kind, name)| (ino, kind, name.to_string()))
        .chain(children.iter().map(|(name, child)| {
            let kind = match index_of(*child).and_then(|i| self.entries.get(i)) {
                Some(Entry::Dir { .. }) => FileType::Directory,
                Some(Entry::Blob { .. }) | None => FileType::RegularFile,
            };
            (*child, kind, name.clone())
        }));

        for (child_ino, kind, name) in entries_iter {
            if idx < offset {
                idx = idx.saturating_add(1);
                continue;
            }
            // The `offset` handed back is the *next* offset the kernel should ask for, not this one — see fuser docs.
            if reply.add(INodeNo(child_ino), idx.saturating_add(1), kind, name) {
                // Buffer full — stop and let the kernel ask for more.
                break;
            }
            idx = idx.saturating_add(1);
        }
        reply.ok();
    }

    // Write paths are explicitly NOT implemented: the default
    // `Filesystem` impls return `ENOSYS`, a clear kernel-surfaced failure
    // — better than silently accepting a write that vanishes on unmount.
}
