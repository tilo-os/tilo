//! The FUSE operations — and the filtered view they serve.
//!
//! # A denial is presented as an absence
//!
//! The access decision lives in [`crate::acl`]; this module decides how a
//! denial *looks*. It looks like nothing:
//!
//! | situation | what the app sees |
//! |---|---|
//! | path outside the view | `ENOENT`, and no entry in `readdir` |
//! | path in the view, not writable | `EROFS` |
//! | path in the view, readable | the real thing |
//!
//! `EACCES` is never returned for a policy decision. It is the errno most
//! software handles by aborting or by asking the user to fix something
//! they cannot fix; "the directory contains these two files", "no such
//! file" and "read-only file system" are all states programs handle
//! correctly today, because they occur naturally.
//!
//! Consequences that are deliberate rather than incidental:
//!
//! - `readdir` omits entries outside the view. It does not list them and
//!   fail on access — they are not there.
//! - `lookup` answers `ENOENT` for a name outside the view, and does the
//!   *same work* either way: the namespace is consulted and the view is
//!   evaluated on every call, in the same order, whether or not the entry
//!   exists. There is no fast path whose absence is a signal.
//! - A directory's link count is computed from the filtered listing, so
//!   `nlink` matches what the app can see. A count taken from the real
//!   tree would report subdirectories the listing denies.
//! - Every inode number an app can name it learned from `lookup` or
//!   `readdir`, so it can only address what is already in its view.
//!
//! ## Where the illusion has an edge, and why it is not reachable
//!
//! Creating a name that already exists but is outside the view answers
//! `EROFS` rather than creating it, because the alternative is writing
//! over state the app is not allowed to see. In principle that is one bit
//! per guessed name: "this name is taken by something I cannot see".
//!
//! The capability model cannot produce that situation. A `Restrict` is
//! phrased as a ceiling and every verb implies `Read`, so no capability
//! can take reading away from a subtree it left writable — inside a
//! writable directory, nothing is hidden. `acl::tests::
//! a_restriction_narrows_writing_without_hiding_anything` pins that.
//! Reaching the edge needs a direct [`crate::acl::AclEntry`] with
//! `write` but not `read`, which no production caller writes.
//!
//! The same reasoning covers `rmdir` on a directory whose only remaining
//! children are outside the view: it answers `EROFS`, not `ENOTEMPTY`, so
//! it does not announce that something is in there.
use fuser::*;
use libc::{EINVAL, EIO, EISDIR, ENOENT, ENOTDIR, ENOTEMPTY, EROFS};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iroh_blobs::Hash;

use crate::acl::{Access, WorkspaceAcl};
use crate::hydration::Hydrator;
use crate::namespace::Namespace;
use crate::smart::Heuristics;
use crate::sync::SyncHandle;

const TTL: Duration = Duration::from_secs(10);

pub struct QuickFS {
    pub namespace: Arc<Mutex<Namespace>>,
    pub hydrator: Arc<Hydrator>,
    pub heuristics: Arc<Heuristics>,
    /// Codecs for recognized rich-text formats (odt/docx/md).
    registry: Arc<qdoc::Registry>,
    /// Broadcasts local CRDT deltas to peers (None when running standalone).
    sync: Option<Arc<SyncHandle>>,
    /// Per-inode in-flight write buffers (content-addressed on flush/release).
    write_buffers: Mutex<HashMap<u64, Vec<u8>>>,
    /// Cached serialized form of each document inode, keyed by the doc's heads,
    /// so a multi-chunk read exports once (and getattr's size stays current).
    doc_exports: Mutex<HashMap<u64, (Vec<automerge::ChangeHash>, Vec<u8>)>>,
    /// The workspace ACL and the identity it is evaluated as — the real
    /// access-control check. `None` leaves the pre-existing
    /// Unix-veneer-only behaviour intact (single-user unshared mounts).
    /// When `Some`, every open/read/write is checked against the ACL as
    /// that identity: the app runs as the OS user, the fs enforces the
    /// qOS per-identity policy on top. See [`crate::acl`].
    ///
    /// The two are one field precisely so an ACL can never be attached
    /// without the identity it answers for — there is no placeholder
    /// identity to accidentally enforce against.
    enforcement: Option<(WorkspaceAcl, crate::acl::IdentityKey)>,
}

impl QuickFS {
    pub fn new(
        namespace: Arc<Mutex<Namespace>>,
        hydrator: Arc<Hydrator>,
        heuristics: Arc<Heuristics>,
    ) -> Self {
        Self::with_registry(
            namespace,
            hydrator,
            heuristics,
            Arc::new(qdoc::builtin_registry()),
        )
    }

    pub fn with_registry(
        namespace: Arc<Mutex<Namespace>>,
        hydrator: Arc<Hydrator>,
        heuristics: Arc<Heuristics>,
        registry: Arc<qdoc::Registry>,
    ) -> Self {
        Self {
            namespace,
            hydrator,
            heuristics,
            registry,
            sync: None,
            write_buffers: Mutex::new(HashMap::new()),
            doc_exports: Mutex::new(HashMap::new()),
            enforcement: None,
        }
    }

    /// The codec key for `name`'s extension if it's a recognized document.
    fn document_codec_for(&self, name: &str) -> Option<String> {
        let ext = name.rsplit('.').next().filter(|e| *e != name)?;
        self.registry.provider_for_ext(ext).map(|_| ext.to_lowercase())
    }

    /// Whether `ino` is a CRDT-backed document (vs. opaque content).
    fn is_document(&self, ino: u64) -> bool {
        self.namespace.lock().unwrap().document_codec(ino).is_some()
    }

    /// The hydration state of `ino` (the value served for the `user.qos.sync`
    /// xattr). `None` when the inode doesn't exist. Directories, CRDT documents,
    /// and empty files are always [`SyncState::Local`]; a regular file's state
    /// comes from the hydrator (is its content — every chunk — in the cache?).
    fn sync_state_for(&self, ino: u64) -> Option<crate::syncstate::SyncState> {
        use crate::syncstate::SyncState;
        let (exists, is_dir, is_doc, hash) = {
            let ns = self.namespace.lock().unwrap();
            let attr = ns.getattr(ino);
            let exists = attr.is_some();
            let is_dir = matches!(attr, Some(a) if a.kind == FileType::Directory);
            let is_doc = ns.document_codec(ino).is_some();
            let hash = ns.hash_for(ino);
            (exists, is_dir, is_doc, hash)
        };
        if !exists {
            return None;
        }
        // A file open for writing has its bytes staged locally in the buffer.
        if self.write_buffers.lock().unwrap().contains_key(&ino) {
            return Some(SyncState::Local);
        }
        if is_dir || is_doc {
            return Some(SyncState::Local);
        }
        let Some(hash) = hash else {
            // A hashless (empty) file is trivially local.
            return Some(SyncState::Local);
        };
        let hydrator = self.hydrator.clone();
        let rt = hydrator.runtime_handle.clone();
        let state = tokio::task::block_in_place(|| {
            rt.block_on(async move { hydrator.sync_state(hash).await })
        });
        Some(state)
    }

    /// Attribute/entry TTL: zero for documents (their size is computed live from
    /// the CRDT and changes as peers edit, so the kernel must not cache a stale
    /// `i_size` and truncate reads), the normal TTL otherwise.
    fn ttl_for(&self, is_doc: bool) -> Duration {
        if is_doc {
            Duration::ZERO
        } else {
            TTL
        }
    }

    /// Commit an in-flight write buffer: import it as a document, or ingest it
    /// as opaque chunked content.
    fn commit_buffer(&self, ino: u64, data: &[u8]) {
        let is_doc = self.is_document(ino);
        if is_doc {
            // import_document validates before replacing; an invalid input
            // leaves the previous content intact and returns false.
            let delta = {
                let mut ns = self.namespace.lock().unwrap();
                if ns.import_document(ino, &self.registry, data, now_ms()) {
                    ns.save_incremental()
                } else {
                    Vec::new()
                }
            };
            self.doc_exports.lock().unwrap().remove(&ino);
            if delta.is_empty() {
                eprintln!("document import rejected (invalid format); previous content kept");
            } else {
                self.publish(delta);
            }
        } else if let Err(e) = self.commit_content(ino, data) {
            eprintln!("commit failed (content preserved): {e}");
        }
    }

    /// The serialized (on-wire) bytes of a document inode, exported once per
    /// change and cached so multi-chunk reads and `getattr` are cheap + consistent.
    fn document_export(&self, ino: u64) -> Option<Vec<u8>> {
        let heads = self.namespace.lock().unwrap().heads();
        if let Some((h, bytes)) = self.doc_exports.lock().unwrap().get(&ino) {
            if *h == heads {
                return Some(bytes.clone());
            }
        }
        let (heads, bytes) = {
            let mut ns = self.namespace.lock().unwrap();
            let bytes = ns.export_document(ino, &self.registry)?;
            (ns.heads(), bytes)
        };
        self.doc_exports
            .lock()
            .unwrap()
            .insert(ino, (heads, bytes.clone()));
        Some(bytes)
    }

    /// Attach a sync handle so write ops propagate to peers.
    pub fn with_sync(mut self, sync: Arc<SyncHandle>) -> Self {
        self.sync = Some(sync);
        self
    }

    /// Bind this mount to a workspace ACL, enforcing per-identity access
    /// for `identity`. Every subsequent open/read/write is checked
    /// against `acl` as `identity`; without this the mount keeps the
    /// Unix-veneer-only behaviour. See [`crate::acl`].
    pub fn with_acl(mut self, acl: WorkspaceAcl, identity: crate::acl::IdentityKey) -> Self {
        self.enforcement = Some((acl, identity));
        self
    }

    /// The identity ACL check every open/read/write routes through.
    ///
    /// Returns `Ok(())` when the mount has no ACL attached (the Unix
    /// veneer alone governs) or when the bound identity holds `access`
    /// on the inode's path.
    ///
    /// A denial is **never** `EACCES`:
    ///
    /// - a path outside the identity's view is `ENOENT`, the same answer
    ///   the app would get for a name that was never there;
    /// - a path inside the view that the identity may not write is
    ///   `EROFS`, which says nothing about the path and everything about
    ///   the mount — and is the failure every program already handles by
    ///   staying read-only instead of by prompting.
    ///
    /// This is the single production decision point — `open`, `read`,
    /// `write`, `setattr` and the directory mutations all reach it, so a
    /// test that drives it exercises the real access path.
    pub fn check_access(&self, ino: u64, access: Access) -> Result<(), i32> {
        if self.enforcement.is_none() {
            return Ok(());
        }
        let path = self.namespace.lock().unwrap().path_for(ino).ok_or(ENOENT)?;
        self.check_path(&path, access)
    }

    /// [`Self::check_access`] against a path rather than an inode, for the
    /// operations whose subject is a name that may not exist yet.
    fn check_path(&self, path: &str, access: Access) -> Result<(), i32> {
        let Some((acl, identity)) = &self.enforcement else {
            return Ok(());
        };
        if acl.permits(identity, path, access) {
            return Ok(());
        }
        if acl.visible(identity, path) && access == Access::Write {
            // In the view, and the view is read-only here.
            return Err(EROFS);
        }
        // Outside the view, or in it but not readable (an ancestor
        // directory the app may walk but not read as content). Either way
        // the honest answer is that there is nothing here.
        Err(ENOENT)
    }

    /// Is `path` part of the view this mount serves?
    ///
    /// `true` for every path on a mount with no ACL attached — the
    /// filtered view is the ACL's, so without one there is nothing to
    /// filter.
    fn in_view(&self, path: &str) -> bool {
        match &self.enforcement {
            Some((acl, identity)) => acl.visible(identity, path),
            None => true,
        }
    }

    /// Is `ino` part of the view? An inode with no resolvable path
    /// (trashed, cyclic) is not.
    fn ino_in_view(&self, ino: u64) -> bool {
        if self.enforcement.is_none() {
            return true;
        }
        match self.namespace.lock().unwrap().path_for(ino) {
            Some(path) => self.in_view(&path),
            None => false,
        }
    }

    /// The entries of `ino` that belong in the view, as `readdir` would
    /// report them. `None` when `ino` is not a directory in the view.
    ///
    /// `.` and `..` always survive: they are the directory itself and its
    /// parent, both of which the app reached this directory through.
    fn visible_entries(&self, ino: u64) -> Option<Vec<(u64, FileType, String)>> {
        let (entries, dir_path) = {
            let mut ns = self.namespace.lock().unwrap();
            (ns.readdir(ino), ns.path_for(ino))
        };
        let entries = entries?;
        let Some((acl, identity)) = &self.enforcement else {
            return Some(entries);
        };
        let dir_path = dir_path?;
        if !acl.visible(identity, &dir_path) {
            return None;
        }
        Some(
            entries
                .into_iter()
                .filter(|(_, _, name)| {
                    name == "." || name == ".." || acl.visible(identity, &child_path(&dir_path, name))
                })
                .collect(),
        )
    }

    /// The link count to report for a directory: `2` (itself and `..`)
    /// plus one per subdirectory **in the view**.
    ///
    /// Taken from the filtered listing rather than from the namespace so
    /// that `nlink` and `readdir` cannot disagree. A count taken from the
    /// real tree would announce the number of subdirectories the listing
    /// hides, which is the whole property this module exists to keep.
    fn visible_dir_nlink(&self, ino: u64) -> u32 {
        let subdirs = self
            .visible_entries(ino)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|(_, kind, name)| {
                        *kind == FileType::Directory && name != "." && name != ".."
                    })
                    .count()
            })
            .unwrap_or(0);
        2u32.saturating_add(subdirs as u32)
    }

    /// The check every "make a new name in this directory" operation
    /// runs: `create`, `mkdir`, and the destination half of `rename`.
    ///
    /// A parent outside the view is `ENOENT`. Otherwise the new path must
    /// be writable, and **that question is asked before the name is
    /// looked up**: a name the app may not create there gets the same
    /// answer whether or not something is already sitting on it. Asking
    /// the other way round turned `create` into an existence oracle —
    /// `EROFS` for a taken name against `ENOENT` for a free one.
    ///
    /// Only once the app *may* create the name does a collision with
    /// something outside the view matter, and then it is refused with
    /// `EROFS` rather than written over. Reaching that needs a grant that
    /// is writable but not readable, which no capability can express.
    fn check_new_name(&self, parent: u64, name: &str) -> Result<(), i32> {
        let Some((acl, identity)) = &self.enforcement else {
            return Ok(());
        };
        let parent_path = self
            .namespace
            .lock()
            .unwrap()
            .path_for(parent)
            .ok_or(ENOENT)?;
        if !acl.visible(identity, &parent_path) {
            return Err(ENOENT);
        }
        let child = child_path(&parent_path, name);
        self.check_path(&child, Access::Write)?;
        let taken = self.namespace.lock().unwrap().lookup(parent, name).is_some();
        if taken && !acl.visible(identity, &child) {
            return Err(EROFS);
        }
        Ok(())
    }

    /// The check for removing or renaming an existing name in `parent`:
    /// the entry must be in the view (else it is not there) and writable
    /// (else the view is read-only).
    fn check_existing_name(&self, parent: u64, name: &str) -> Result<(), i32> {
        if self.enforcement.is_none() {
            return Ok(());
        }
        let parent_path = self
            .namespace
            .lock()
            .unwrap()
            .path_for(parent)
            .ok_or(ENOENT)?;
        if !self.in_view(&parent_path) {
            return Err(ENOENT);
        }
        self.check_path(&child_path(&parent_path, name), Access::Write)
    }

    fn publish(&self, delta: Vec<u8>) {
        if let Some(sync) = &self.sync {
            sync.broadcast_delta(delta);
        }
    }

    /// Synchronously content-chunk + ingest bytes, returning the manifest hash.
    /// `Ok(None)` for empty data; `Err` if ingest fails (so callers never
    /// persist a non-empty file with a missing hash).
    fn write_content_blocking(&self, data: &[u8]) -> io::Result<Option<Hash>> {
        let store = self.hydrator.store.clone();
        let rt = self.hydrator.runtime_handle.clone();
        let data = data.to_vec();
        tokio::task::block_in_place(|| rt.block_on(async move { store.write_content(&data).await }))
            .map_err(|e| io::Error::other(e.to_string()))
    }

    /// Current full content of an inode: the in-flight buffer if any, else the
    /// hydrated blob. `Ok(empty)` only for a genuinely empty/hashless file;
    /// a hydration failure is `Err` (so a write never seeds from empty and
    /// silently truncates a not-yet-local file).
    fn read_full_blocking(&self, ino: u64) -> io::Result<Vec<u8>> {
        if let Some(buf) = self.write_buffers.lock().unwrap().get(&ino) {
            return Ok(buf.clone());
        }
        // Documents seed from their current serialized form (so a partial /
        // append write modifies the real content rather than an empty buffer).
        if self.is_document(ino) {
            return self
                .document_export(ino)
                .ok_or_else(|| io::Error::other("document export failed"));
        }
        let hash = self.namespace.lock().unwrap().hash_for(ino);
        let Some(hash) = hash else {
            return Ok(Vec::new());
        };
        let hydrator = self.hydrator.clone();
        let rt = hydrator.runtime_handle.clone();
        tokio::task::block_in_place(|| {
            rt.block_on(async move { hydrator.read(hash, 0, u64::MAX).await })
        })
        .map_err(|e| io::Error::other(e.to_string()))
    }

    /// Ingest `data` as the new content of `ino` and broadcast the change.
    /// Aborts (returns Err) WITHOUT mutating the entry if the ingest fails, so
    /// a failed write preserves the previously-committed content.
    fn commit_content(&self, ino: u64, data: &[u8]) -> io::Result<()> {
        let hash = self.write_content_blocking(data)?;
        let delta = {
            let mut ns = self.namespace.lock().unwrap();
            ns.set_content(ino, hash.as_ref(), data.len() as u64, now_ms());
            ns.save_incremental()
        };
        self.publish(delta);
        Ok(())
    }

    fn remove_entry(&self, parent: INodeNo, name: &OsStr, expect_dir: bool, reply: ReplyEmpty) {
        let Some(name) = name.to_str() else {
            reply.error(to_errno(EINVAL));
            return;
        };
        if let Err(e) = self.check_existing_name(parent.0, name) {
            reply.error(to_errno(e));
            return;
        }
        let delta = {
            let mut ns = self.namespace.lock().unwrap();
            let Some(attr) = ns.lookup(parent.0, name) else {
                reply.error(to_errno(ENOENT));
                return;
            };
            let is_dir = attr.kind == FileType::Directory;
            if expect_dir && !is_dir {
                reply.error(to_errno(ENOTDIR));
                return;
            }
            if !expect_dir && is_dir {
                reply.error(to_errno(EISDIR));
                return;
            }
            if expect_dir {
                // A directory must be empty to rmdir (entries beyond "." "..").
                if let Some(entries) = ns.readdir(attr.ino.0) {
                    if entries.len() > 2 {
                        // Emptiness is judged on the real tree, because
                        // removing the directory would take its hidden
                        // children with it. When the app's own listing
                        // says empty, saying `ENOTEMPTY` would announce
                        // that something it cannot see is in there — so
                        // the answer is the same `EROFS` every other
                        // refusal to write over unseen state gives.
                        drop(ns);
                        let visible = self
                            .visible_entries(attr.ino.0)
                            .map(|e| e.len())
                            .unwrap_or(0);
                        reply.error(to_errno(if visible > 2 { ENOTEMPTY } else { EROFS }));
                        return;
                    }
                }
            }
            if !ns.remove(attr.ino.0) {
                reply.error(to_errno(ENOENT));
                return;
            }
            ns.save_incremental()
        };
        self.publish(delta);
        reply.ok();
    }
}

fn to_errno(e: i32) -> Errno {
    io::Error::from_raw_os_error(e).into()
}

/// `dir` + `name`, as the ACL spells paths: absolute, no trailing slash,
/// `"/name"` directly beneath the root.
fn child_path(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn slice(bytes: &[u8], offset: u64, size: u32) -> Vec<u8> {
    let start = (offset as usize).min(bytes.len());
    let end = start.saturating_add(size as usize).min(bytes.len());
    bytes[start..end].to_vec()
}

/// The ACL accesses an open of the given mode requires: read for
/// O_RDONLY, write for O_WRONLY, both for O_RDWR.
fn required_accesses(mode: OpenAccMode) -> &'static [Access] {
    match mode {
        OpenAccMode::O_RDONLY => &[Access::Read],
        OpenAccMode::O_WRONLY => &[Access::Write],
        OpenAccMode::O_RDWR => &[Access::Read, Access::Write],
    }
}

impl Filesystem for QuickFS {
    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> Result<(), io::Error> {
        let _ = config.set_max_readahead(1024 * 1024);
        Ok(())
    }

    /// Resolve one name in a directory.
    ///
    /// Both halves — the namespace lookup and the view evaluation — run on
    /// every call, in the same order, and the answer is only assembled
    /// afterwards. A name that does not exist and a name that exists
    /// outside the view therefore cost the same two operations and return
    /// the same `ENOENT`; neither the errno nor the shape of the work
    /// distinguishes them.
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(name) = name.to_str() else {
            reply.error(to_errno(EINVAL));
            return;
        };
        let (found, child) = {
            let mut ns = self.namespace.lock().unwrap();
            let found = ns.lookup(parent.0, name);
            let child = ns.path_for(parent.0).map(|p| child_path(&p, name));
            (found, child)
        };
        let in_view = child.is_some_and(|c| self.in_view(&c));
        match found {
            Some(attr) if in_view => {
                let is_doc = self
                    .namespace
                    .lock()
                    .unwrap()
                    .document_codec(attr.ino.0)
                    .is_some();
                let mut attr = attr;
                if attr.kind == FileType::Directory {
                    attr.nlink = self.visible_dir_nlink(attr.ino.0);
                }
                reply.entry(&self.ttl_for(is_doc), &attr, Generation(0));
            }
            _ => reply.error(to_errno(ENOENT)),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        // Reflect an in-flight write buffer so stat() sees read-your-writes size.
        let buf_len = self
            .write_buffers
            .lock()
            .unwrap()
            .get(&ino.0)
            .map(|b| b.len() as u64);
        // Documents have no stored byte length — report the current export size
        // (so a collaboratively-edited document isn't read truncated).
        let is_doc = self.is_document(ino.0);
        let doc_len = if buf_len.is_none() && is_doc {
            self.document_export(ino.0).map(|b| b.len() as u64)
        } else {
            None
        };
        // Outside the view there is nothing to describe.
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        let nlink = self.visible_dir_nlink(ino.0);
        let ns = self.namespace.lock().unwrap();
        match ns.getattr(ino.0) {
            Some(mut attr) => {
                if let Some(len) = buf_len.or(doc_len) {
                    attr.size = len;
                    attr.blocks = len.div_ceil(512);
                }
                if attr.kind == FileType::Directory {
                    // Matches the filtered listing, including at the mount
                    // root: an identity granted nothing sees `nlink == 2`
                    // and `size == 0` — an ordinary empty directory.
                    attr.nlink = nlink;
                }
                reply.attr(&self.ttl_for(is_doc), &attr);
            }
            None => reply.error(to_errno(ENOENT)),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        // The filtered listing. Entries outside the view are not reported
        // as entries that error — they are simply not in the directory,
        // and the offsets below are indices into what the app can see, so
        // a paged listing never has a gap where something was removed.
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        let Some(entries) = self.visible_entries(ino.0) else {
            reply.error(to_errno(ENOTDIR));
            return;
        };
        for (i, (e_ino, kind, name)) in entries.into_iter().enumerate().skip(offset as usize) {
            if reply.add(INodeNo(e_ino), (i + 1) as u64, kind, &name) {
                break;
            }
        }
        reply.ok();
        self.heuristics.on_readdir(ino.0);
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let exists = {
            let ns = self.namespace.lock().unwrap();
            ns.getattr(ino.0).is_some()
        };
        if !exists {
            reply.error(to_errno(ENOENT));
            return;
        }
        // Per-identity ACL: an open requires the identity to hold the
        // access implied by the open mode (read for O_RDONLY, write for
        // O_WRONLY, both for O_RDWR). Outside the view ⇒ `ENOENT`; in the
        // view but not writable ⇒ `EROFS`, so an app that opens for
        // writing falls back to read-only the way it would on any
        // read-only mount.
        for access in required_accesses(flags.acc_mode()) {
            if let Err(e) = self.check_access(ino.0, *access) {
                reply.error(to_errno(e));
                return;
            }
        }
        reply.opened(FileHandle(0), FopenFlags::empty());
        self.heuristics.on_open(ino.0);
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
        // Per-identity ACL: reading requires the read cap on this path.
        if let Err(e) = self.check_access(ino.0, Access::Read) {
            reply.error(to_errno(e));
            return;
        }
        // Read-your-writes: serve from the in-flight buffer if present.
        if let Some(buf) = self.write_buffers.lock().unwrap().get(&ino.0) {
            reply.data(&slice(buf, offset, size));
            return;
        }

        // Documents: serve the cached serialization (one export per change).
        if self.is_document(ino.0) {
            match self.document_export(ino.0) {
                Some(b) => reply.data(&slice(&b, offset, size)),
                None => reply.error(to_errno(EIO)),
            }
            return;
        }

        let hash = {
            let ns = self.namespace.lock().unwrap();
            match ns.getattr(ino.0) {
                Some(attr) if attr.kind == FileType::RegularFile => ns.hash_for(ino.0),
                Some(_) => {
                    reply.error(to_errno(EISDIR));
                    return;
                }
                None => {
                    reply.error(to_errno(ENOENT));
                    return;
                }
            }
        };
        let Some(hash) = hash else {
            reply.data(&[]);
            return;
        };

        let hydrator = self.hydrator.clone();
        let rt = hydrator.runtime_handle.clone();
        let res = tokio::task::block_in_place(|| {
            rt.block_on(async move { hydrator.read(hash, offset, size as u64).await })
        });
        match res {
            Ok(data) => reply.data(&data),
            Err(_) => reply.error(to_errno(EIO)),
        }
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let Some(name) = name.to_str() else {
            reply.error(to_errno(EINVAL));
            return;
        };
        if let Err(e) = self.check_new_name(parent.0, name) {
            reply.error(to_errno(e));
            return;
        }
        let codec = self.document_codec_for(name);
        let (attr, delta) = {
            let mut ns = self.namespace.lock().unwrap();
            let ino = match &codec {
                Some(c) => ns.create_document(parent.0, name, c, now_ms()),
                None => ns.create_file(parent.0, name, None, 0, now_ms()),
            };
            let Some(ino) = ino else {
                reply.error(to_errno(ENOENT));
                return;
            };
            (ns.getattr(ino), ns.save_incremental())
        };
        let Some(attr) = attr else {
            reply.error(to_errno(EIO));
            return;
        };
        // Start a fresh empty buffer so subsequent writes accumulate.
        self.write_buffers.lock().unwrap().insert(attr.ino.0, Vec::new());
        self.publish(delta);
        reply.created(
            &self.ttl_for(codec.is_some()),
            &attr,
            Generation(0),
            FileHandle(0),
            FopenFlags::empty(),
        );
    }

    fn write(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        // Per-identity ACL: writing requires the write cap on this path.
        if let Err(e) = self.check_access(ino.0, Access::Write) {
            reply.error(to_errno(e));
            return;
        }
        // Seed the buffer with current content if this is the first write.
        // Done outside the buffer lock (it may do blocking I/O); a hydration
        // failure must fail the write, never seed-from-empty and truncate.
        let absent = self.write_buffers.lock().unwrap().get(&ino.0).is_none();
        let seed = if absent {
            // Seed from current content (export for documents, hydrated bytes
            // for opaque files) so a partial/append write modifies it correctly.
            match self.read_full_blocking(ino.0) {
                Ok(d) => Some(d),
                Err(_) => {
                    reply.error(to_errno(EIO));
                    return;
                }
            }
        } else {
            None
        };

        let start = offset as usize;
        let Some(end) = start.checked_add(data.len()) else {
            reply.error(to_errno(EINVAL));
            return;
        };
        {
            let mut bufs = self.write_buffers.lock().unwrap();
            // or_insert keeps any buffer a racing op already created.
            let buf = match seed {
                Some(d) => bufs.entry(ino.0).or_insert(d),
                None => bufs.entry(ino.0).or_default(),
            };
            if end > buf.len() {
                buf.resize(end, 0);
            }
            buf[start..end].copy_from_slice(data);
        }
        reply.written(data.len() as u32);
    }

    fn flush(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        let buffered = self.write_buffers.lock().unwrap().get(&ino.0).cloned();
        if let Some(data) = buffered {
            self.commit_buffer(ino.0, &data);
        }
        reply.ok();
    }

    fn fsync(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, _datasync: bool, reply: ReplyEmpty) {
        let buffered = self.write_buffers.lock().unwrap().get(&ino.0).cloned();
        if let Some(data) = buffered {
            self.commit_buffer(ino.0, &data);
        }
        reply.ok();
    }

    fn release(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let buffered = self.write_buffers.lock().unwrap().remove(&ino.0);
        if let Some(data) = buffered {
            self.commit_buffer(ino.0, &data);
        }
        reply.ok();
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        // Truncation is a write. Everything else here (perm/owner/time) is
        // ignored anyway, but a `setattr` on something outside the view
        // must not describe it, so the visibility check runs regardless.
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        if size.is_some() {
            if let Err(e) = self.check_access(ino.0, Access::Write) {
                reply.error(to_errno(e));
                return;
            }
        }
        // Handle truncation/extension; ignore perm/owner/time changes.
        if let Some(new_size) = size {
            let is_doc = self.is_document(ino.0);
            let mut data = match self.read_full_blocking(ino.0) {
                Ok(d) => d,
                Err(_) => {
                    reply.error(to_errno(EIO));
                    return;
                }
            };
            data.resize(new_size as usize, 0);
            let open = self.write_buffers.lock().unwrap().contains_key(&ino.0);
            // Documents always stage in the buffer (imported as a whole on
            // release); opaque files commit immediately when not open.
            if open || is_doc {
                self.write_buffers.lock().unwrap().insert(ino.0, data);
            } else if let Err(_e) = self.commit_content(ino.0, &data) {
                reply.error(to_errno(EIO));
                return;
            }
        }

        let buf_len = self
            .write_buffers
            .lock()
            .unwrap()
            .get(&ino.0)
            .map(|b| b.len() as u64);
        let nlink = self.visible_dir_nlink(ino.0);
        match self.namespace.lock().unwrap().getattr(ino.0) {
            Some(mut attr) => {
                if let Some(len) = buf_len {
                    attr.size = len;
                    attr.blocks = len.div_ceil(512);
                }
                if attr.kind == FileType::Directory {
                    attr.nlink = nlink;
                }
                reply.attr(&TTL, &attr);
            }
            None => reply.error(to_errno(ENOENT)),
        }
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let Some(name) = name.to_str() else {
            reply.error(to_errno(EINVAL));
            return;
        };
        if let Err(e) = self.check_new_name(parent.0, name) {
            reply.error(to_errno(e));
            return;
        }
        let (attr, delta) = {
            let mut ns = self.namespace.lock().unwrap();
            let Some(ino) = ns.create_dir(parent.0, name, now_ms()) else {
                reply.error(to_errno(ENOENT));
                return;
            };
            (ns.getattr(ino), ns.save_incremental())
        };
        match attr {
            Some(attr) => {
                self.publish(delta);
                reply.entry(&TTL, &attr, Generation(0));
            }
            None => reply.error(to_errno(EIO)),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        self.remove_entry(parent, name, false, reply);
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        self.remove_entry(parent, name, true, reply);
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        _flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let (Some(name), Some(newname)) = (name.to_str(), newname.to_str()) else {
            reply.error(to_errno(EINVAL));
            return;
        };
        // Both ends: the source must be a writable name in the view, the
        // destination a writable new name in a directory in the view.
        if let Err(e) = self
            .check_existing_name(parent.0, name)
            .and_then(|()| self.check_new_name(newparent.0, newname))
        {
            reply.error(to_errno(e));
            return;
        }
        let delta = {
            let mut ns = self.namespace.lock().unwrap();
            if !ns.rename(parent.0, name, newparent.0, newname, now_ms()) {
                reply.error(to_errno(ENOENT));
                return;
            }
            ns.save_incremental()
        };
        self.publish(delta);
        reply.ok();
    }

    /// Serve the `user.qos.sync` extended attribute: the entry's hydration state
    /// (`local` / `projected` / `downloading`). Every other attribute name is
    /// reported absent (`ENODATA`) — the collaborative FS owns only this one.
    fn getxattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        name: &OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        // Outside the view the inode does not exist, and that answer comes
        // before the attribute name is considered — otherwise the sync
        // state would report on a file the app cannot see.
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        if name.to_str() != Some(crate::syncstate::SYNC_XATTR) {
            reply.error(to_errno(libc::ENODATA));
            return;
        }
        let Some(state) = self.sync_state_for(ino.0) else {
            reply.error(to_errno(ENOENT));
            return;
        };
        let bytes = state.as_str().as_bytes();
        // The two-call xattr protocol: size==0 asks for the length; a non-zero
        // size must be big enough for the value, else ERANGE.
        if size == 0 {
            reply.size(bytes.len() as u32);
        } else if (size as usize) < bytes.len() {
            reply.error(to_errno(libc::ERANGE));
        } else {
            reply.data(bytes);
        }
    }

    /// List the extended attributes on `ino`: just `user.qos.sync` (NUL
    /// terminated), so `getfattr`/`listxattr` discover the sync attribute.
    fn listxattr(&self, _req: &Request, ino: INodeNo, size: u32, reply: ReplyXattr) {
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        if self.namespace.lock().unwrap().getattr(ino.0).is_none() {
            reply.error(to_errno(ENOENT));
            return;
        }
        let mut buf = Vec::with_capacity(crate::syncstate::SYNC_XATTR.len() + 1);
        buf.extend_from_slice(crate::syncstate::SYNC_XATTR.as_bytes());
        buf.push(0);
        if size == 0 {
            reply.size(buf.len() as u32);
        } else if (size as usize) < buf.len() {
            reply.error(to_errno(libc::ERANGE));
        } else {
            reply.data(&buf);
        }
    }
}
