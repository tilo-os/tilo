//! `fuser::Filesystem` implementation that serves a qOS package
//! payload from a content-addressed [`BlobStore`] through an
//! [`PageCache`].
//!
//! ## Inode allocation
//!
//! The Collection is a flat `path -> blob_hash` table. The
//! mount expands it into a tree of inodes:
//!
//! - inode 1 (FUSE_ROOT_ID) is the payload root.
//! - Each interior directory (`bin/`, `share/icons/`, …) gets a
//!   freshly allocated inode the first time it is visited.
//! - Each leaf file gets a freshly allocated inode at build
//!   time. Its `Entry::Blob` records the blob hash + size.
//!
//! Allocation order is deterministic: a depth-first walk over
//! the Collection's already-sorted path list. The hash isn't
//! semantically meaningful — readers cannot rely on a specific
//! inode for a specific path across mounts — but it IS stable
//! across the lifetime of one mount.
//!
//! ## Why a flat `Vec<Entry>` rather than recursive structs?
//!
//! Two reasons:
//!
//! 1. `lookup` is hot. The FUSE kernel module calls it on every
//!    path component during a path walk. An array-indexed
//!    `(inode -> entry)` lookup is one memory read; chasing
//!    `HashMap` indirection per component costs more.
//! 2. The mount is read-only after construction. There are no
//!    insertions or deletions, so a `Vec` with stable indices
//!    is the cleanest representation.
//!
//! ## Caching
//!
//! - The [`PageCache`] absorbs duplicate page reads from the
//!   kernel's mmap fault path.
//! - The kernel itself caches `getattr` / `lookup` for
//!   [`Self::TTL`], which is fine for a read-only payload —
//!   nothing the kernel cached can become stale.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use fuser::{
    FileAttr, FileType, Filesystem, KernelConfig, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    Request,
};
use fuser::consts::FOPEN_KEEP_CACHE;
use fuser::ll::fuse_abi::consts::FUSE_ROOT_ID;
use fuser::ll::{FileHandle, FopenFlags, Generation, INodeNo, OpenFlags};
use fuser::{Errno, ReplyOpen};
use libc::{EINVAL, EIO, ENOENT, ENOTDIR};

use crate::cache::PageCache;
use crate::collection::Collection;
use crate::store::{BlobError, BlobStore};

/// Attribute / entry kernel TTL.
///
/// A qOS package payload is immutable for the lifetime of the
/// mount (blobs are content-addressed; you'd publish a new
/// package id rather than mutate one), so the TTL is generous.
pub const TTL: Duration = Duration::from_secs(60 * 60);

/// Default kernel max-readahead. 1 MiB matches `fs/`'s
/// workspace mount — large enough that a single mmap fault
/// pulls a meaningful prefix into the cache.
pub const MAX_READAHEAD: u32 = 1024 * 1024;

/// Default page-cache capacity (in pages). 4096 pages * 4 KiB
/// = 16 MiB of resident cache per mount. Tunable per-mount via
/// [`QosAppFs::with_cache_pages`].
pub const DEFAULT_CACHE_PAGES: usize = 4096;

/// The Filesystem implementation.
pub struct QosAppFs<S: BlobStore + 'static> {
    /// Indexed by inode-1 (FUSE_ROOT_ID is inode 1, stored at
    /// index 0).
    entries: Vec<Entry>,
    /// Cache fronting `store`. Wrapped in `Arc` so reads from
    /// concurrent FUSE workers share state cheaply.
    cache: Arc<PageCache<S>>,
    /// Owner uid/gid stamped on every getattr reply. Set from
    /// the mounting process by default.
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
        /// Computed once at mount time so `getattr` never has
        /// to call into the store for size.
        size: u64,
    },
}

impl<S: BlobStore + 'static> QosAppFs<S> {
    /// Build a filesystem rooted at `collection`'s payload tree,
    /// reading blob bytes from `store`. Uses the default page
    /// cache size.
    pub fn new(collection: &Collection, store: S) -> Result<Self, BuildError> {
        Self::with_cache_pages(collection, store, DEFAULT_CACHE_PAGES)
    }

    /// Same as [`Self::new`] but with a caller-chosen cache
    /// capacity in pages (4 KiB each).
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
            if segments.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
                return Err(BuildError::BadPath(path.clone()));
            }

            // Walk down, allocating directory inodes as needed.
            let mut current: u64 = FUSE_ROOT_ID;
            for (i, seg) in segments.iter().enumerate() {
                let is_last = i == segments.len() - 1;
                let existing = match &entries[(current - 1) as usize] {
                    Entry::Dir { children } => children.get(*seg).copied(),
                    Entry::Blob { .. } => {
                        return Err(BuildError::PathOverlap(path.clone()));
                    }
                };
                if let Some(child_ino) = existing {
                    if is_last {
                        // Two collection entries pointing at
                        // the same path — the Collection
                        // decoder already rejects this, but
                        // belt-and-braces.
                        return Err(BuildError::PathOverlap(path.clone()));
                    }
                    if !matches!(entries[(child_ino - 1) as usize], Entry::Dir { .. }) {
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
                let Entry::Dir { children } = &mut entries[(current - 1) as usize] else {
                    unreachable!("checked above");
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

    /// Override the owner uid/gid stamped on every reply. The
    /// default is the mounting process's uid/gid; the launcher
    /// may want to remap it to the sandboxed user.
    pub fn with_owner(mut self, uid: u32, gid: u32) -> Self {
        self.uid = uid;
        self.gid = gid;
        self
    }

    /// Borrow the underlying page cache. Lets tests assert hit
    /// counters and lets `qos-app-host` plumb introspection.
    pub fn cache(&self) -> Arc<PageCache<S>> {
        Arc::clone(&self.cache)
    }

    /// Number of inodes (1 + file_count + dir_count) — for
    /// introspection / tests.
    pub fn inode_count(&self) -> usize {
        self.entries.len()
    }

    /// Build the `FileAttr` for `ino`. The caller is responsible
    /// for handing it to the correct `Reply…`.
    fn attr(&self, ino: u64) -> Option<FileAttr> {
        let entry = self.entries.get((ino - 1) as usize)?;
        let (kind, size, perm, nlink) = match entry {
            Entry::Dir { children } => {
                // 2 + child-dir count is the historical convention
                // (`.`, `..`, plus one for each sub-directory),
                // but in a read-only mount the kernel mostly
                // ignores this. Use 2 + dir-child-count.
                let sub_dirs = children
                    .values()
                    .filter(|c| matches!(self.entries.get((**c - 1) as usize), Some(Entry::Dir { .. })))
                    .count() as u32;
                (FileType::Directory, 0u64, 0o555, 2 + sub_dirs)
            }
            Entry::Blob { size, .. } => (FileType::RegularFile, *size, 0o444, 1),
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
    /// A Collection entry's path is empty, contains an empty
    /// segment, `.`, `..`, or `//`.
    #[error("invalid path in collection: {0:?}")]
    BadPath(String),
    /// Two entries collide — one declares a path the other
    /// treats as a directory prefix.
    #[error("path overlap in collection: {0:?}")]
    PathOverlap(String),
    /// The store couldn't tell us the size of a blob the
    /// Collection points at.
    #[error("blob size lookup failed for {0:?}: {1}")]
    Store(String, BlobError),
}

fn to_errno(e: i32) -> Errno {
    std::io::Error::from_raw_os_error(e).into()
}

impl<S: BlobStore + 'static> Filesystem for QosAppFs<S> {
    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> Result<(), std::io::Error> {
        let _ = config.set_max_readahead(MAX_READAHEAD);
        Ok(())
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(name) = name.to_str() else {
            reply.error(to_errno(EINVAL));
            return;
        };
        let Some(parent_entry) = self.entries.get((parent.0 - 1) as usize) else {
            reply.error(to_errno(ENOENT));
            return;
        };
        let Entry::Dir { children } = parent_entry else {
            reply.error(to_errno(ENOTDIR));
            return;
        };
        let Some(&child_ino) = children.get(name) else {
            reply.error(to_errno(ENOENT));
            return;
        };
        let Some(attr) = self.attr(child_ino) else {
            reply.error(to_errno(EIO));
            return;
        };
        reply.entry(&TTL, &attr, Generation(0));
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.attr(ino.0) {
            Some(attr) => reply.attr(&TTL, &attr),
            None => reply.error(to_errno(ENOENT)),
        }
    }

    fn open(&self, _req: &Request, _ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        // Keep the kernel page cache: a re-read of the same
        // page is served from the kernel without coming back
        // to us, which is exactly what we want for a read-only
        // payload.
        reply.opened(FileHandle(0), FopenFlags::from_bits_retain(FOPEN_KEEP_CACHE));
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::ll::LockOwner>,
        reply: ReplyData,
    ) {
        let entry = match self.entries.get((ino.0 - 1) as usize) {
            Some(e) => e,
            None => {
                reply.error(to_errno(ENOENT));
                return;
            }
        };
        let (hash, blob_size) = match entry {
            Entry::Blob { hash, size } => (*hash, *size),
            Entry::Dir { .. } => {
                reply.error(to_errno(EINVAL));
                return;
            }
        };
        // Clamp to EOF: `offset >= size` is a legitimate "no
        // more data" reply, not an error.
        if offset >= blob_size {
            reply.data(&[]);
            return;
        }
        match self.cache.read(&hash, blob_size, offset, size) {
            Ok(bytes) => reply.data(&bytes),
            Err(e) => {
                tracing::warn!(?e, "blob read failed; replying EIO");
                reply.error(to_errno(EIO));
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
        let Some(Entry::Dir { children }) = self.entries.get((ino.0 - 1) as usize) else {
            reply.error(to_errno(ENOENT));
            return;
        };
        // `.` and `..` first, then sorted children.
        let mut idx = 0u64;
        // We synthesize parent_ino as the same inode for the
        // root (matches kernel convention), but a non-root
        // directory's parent isn't tracked. Since this is a
        // read-only mount the kernel never cares about `..`'s
        // actual destination — but we pass `ino` for
        // determinism rather than 0 which would surprise
        // strict callers.
        let entries_iter = [
            (ino.0, FileType::Directory, "."),
            (ino.0, FileType::Directory, ".."),
        ]
        .into_iter()
        .map(|(ino, kind, name)| (ino, kind, name.to_string()))
        .chain(children.iter().map(|(name, child)| {
            let kind = match &self.entries[(*child - 1) as usize] {
                Entry::Dir { .. } => FileType::Directory,
                Entry::Blob { .. } => FileType::RegularFile,
            };
            (*child, kind, name.clone())
        }));

        for (child_ino, kind, name) in entries_iter {
            if idx < offset {
                idx += 1;
                continue;
            }
            // The `offset` we hand back is the *next* offset
            // the kernel should ask for, not this one — see
            // fuser docs.
            if reply.add(INodeNo(child_ino), idx + 1, kind, name) {
                // Buffer full — stop and let the kernel ask
                // for more.
                break;
            }
            idx += 1;
        }
        reply.ok();
    }

    // Write paths are explicitly NOT implemented. The default
    // `Filesystem` impls return `ENOSYS`, which the kernel
    // surfaces as a clear failure — better than silently
    // accepting a write that vanishes on unmount.
}
