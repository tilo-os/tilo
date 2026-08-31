//! The FUSE operations — and the filtered view they serve.
//!
//! # A denial is presented as an absence
//!
//! The access decision lives in [`crate::acl`]; this module decides how
//! a denial *looks*. It looks like nothing: a path outside the view is
//! `ENOENT` with no `readdir` entry; a path in the view but not writable
//! is `EROFS`; a path in the view and readable is the real thing.
//!
//! `EACCES` is never returned for a policy decision — it's the errno
//! most software handles by aborting, while "no such file" and
//! "read-only filesystem" are states programs already handle correctly.
//! `readdir` omits hidden entries rather than listing-then-failing them;
//! `lookup` does the *same work* (namespace consult + view check)
//! whether or not a name exists, so there's no fast path whose absence
//! is a signal; a directory's `nlink` is computed from the filtered
//! listing, never the real tree, so it can't announce hidden subdirectories.
//!
//! ## Where the illusion has an edge, and why it is not reachable
//!
//! Creating a name that already exists but is outside the view answers
//! `EROFS` rather than creating it — the alternative is writing over
//! state the app can't see. The capability model can't produce that
//! situation: a `Restrict` is a ceiling and every verb implies `Read`,
//! so no capability can hide a name inside a directory it left writable.
//! Reaching the edge needs a direct [`crate::acl::AclEntry`] with
//! `write` but not `read`, which no production caller writes. The same
//! reasoning covers `rmdir` on a directory whose only remaining children
//! are hidden: `EROFS`, not `ENOTEMPTY`.
use fuser::{
    BsdFileFlags, Errno, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    KernelConfig, LockOwner, OpenAccMode, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate,
    ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, ReplyXattr, Request,
    TimeOrNow, WriteFlags,
};
use libc::{EINVAL, EIO, EISDIR, ENOENT, ENOTDIR, ENOTEMPTY, EROFS};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iroh_blobs::Hash;

use crate::acl::{Access, AclHandle};
use crate::hydration::Hydrator;
use crate::namespace::Namespace;
use crate::smart::Heuristics;
use crate::sync::SyncHandle;

const TTL: Duration = Duration::from_secs(10);

/// A document inode's cached export, keyed by the heads it was exported at.
type DocExports = HashMap<u64, (Vec<automerge::ChangeHash>, Vec<u8>)>;

pub struct QuickFS {
    pub namespace: Arc<Mutex<Namespace>>,
    pub hydrator: Arc<Hydrator>,
    pub heuristics: Arc<Heuristics>,
    /// Codecs for recognized rich-text formats (odt/docx/md).
    registry: Arc<qdoc::Registry>,
    /// Broadcasts local CRDT deltas to peers (None when running standalone).
    sync: Option<Arc<SyncHandle>>,
    /// Per-inode in-flight write buffers (content-addressed on flush/release).
    write_buffers: Arc<Mutex<HashMap<u64, Vec<u8>>>>,
    /// Cached serialized form of each document inode, keyed by the doc's heads,
    /// so a multi-chunk read exports once (and getattr's size stays current).
    doc_exports: Mutex<DocExports>,
    /// The workspace ACL and the identity it is evaluated as — the real
    /// access-control check. `None` keeps Unix-veneer-only behaviour;
    /// `Some` checks every open/read/write against the ACL as that identity.
    /// One field so an ACL can never be attached without the identity it
    /// answers for. A *handle*, not the ACL itself, so the policy can be
    /// replaced while the mount is up — see [`crate::CapabilityFile`].
    enforcement: Option<(AclHandle, crate::acl::IdentityKey)>,
    /// Host-side thumbnail generation + cache. `Some` when the mount has a
    /// persistent cache directory (serves previews via `user.qos.thumb` and
    /// gossips the hash); `None` leaves the mount thumbnail-less.
    thumbs: Option<crate::thumbs::ThumbProvider>,
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
            write_buffers: Arc::new(Mutex::new(HashMap::new())),
            doc_exports: Mutex::new(HashMap::new()),
            enforcement: None,
            thumbs: None,
        }
    }

    /// Shared dirty-inode view for the storage authority.  The authority only
    /// observes membership; the FUSE implementation remains the sole writer.
    pub fn write_buffers_handle(&self) -> Arc<Mutex<HashMap<u64, Vec<u8>>>> {
        self.write_buffers.clone()
    }

    /// Lock the namespace, recovering from poisoning rather than propagating
    /// a panic through the FUSE dispatch loop — a panicked op must not take
    /// the whole mount down for every other in-flight request.
    fn ns(&self) -> std::sync::MutexGuard<'_, Namespace> {
        self.namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Lock the in-flight write buffers, recovering from poisoning (see [`Self::ns`]).
    fn buffers(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Vec<u8>>> {
        self.write_buffers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Lock the document export cache, recovering from poisoning (see [`Self::ns`]).
    fn doc_export_cache(&self) -> std::sync::MutexGuard<'_, DocExports> {
        self.doc_exports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The codec key for `name`'s extension if it's a recognized document.
    fn document_codec_for(&self, name: &str) -> Option<String> {
        let ext = name.rsplit('.').next().filter(|e| *e != name)?;
        self.registry
            .provider_for_ext(ext)
            .map(|_| ext.to_lowercase())
    }

    /// Whether `ino` is a CRDT-backed document (vs. opaque content).
    fn is_document(&self, ino: u64) -> bool {
        self.ns().document_codec(ino).is_some()
    }

    /// The hydration state of `ino`, served for the `user.qos.sync` xattr.
    /// `None` when the inode doesn't exist. Directories, CRDT documents and
    /// empty files are always [`SyncState::Local`]; a regular file's state
    /// comes from the hydrator (is every chunk in the cache?).
    fn sync_state_for(&self, ino: u64) -> Option<crate::syncstate::SyncState> {
        use crate::syncstate::SyncState;
        let (exists, is_dir, is_doc, hash) = {
            let ns = self.ns();
            let attr = ns.getattr(ino);
            let exists = attr.is_some();
            let is_dir = matches!(attr, Some(a) if a.kind == FileType::Directory);
            let is_doc = ns.document_codec(ino).is_some();
            let hash = ns.hash_for(ino);
            drop(ns);
            (exists, is_dir, is_doc, hash)
        };
        if !exists {
            return None;
        }
        // A file open for writing has its bytes staged locally in the buffer.
        if self.buffers().contains_key(&ino) {
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

    /// Attribute/entry TTL: zero for documents (size is computed live from
    /// the CRDT and changes as peers edit, so the kernel must not cache a
    /// stale `i_size`), the normal TTL otherwise.
    fn ttl_for(is_doc: bool) -> Duration {
        if is_doc { Duration::ZERO } else { TTL }
    }

    /// Commit an in-flight write buffer: import it as a document, or ingest it
    /// as opaque chunked content.
    fn commit_buffer(&self, ino: u64, data: &[u8]) {
        let is_doc = self.is_document(ino);
        if is_doc {
            // import_document validates before replacing; an invalid input
            // leaves the previous content intact and returns false.
            let delta = {
                let mut ns = self.ns();
                if ns.import_document(ino, &self.registry, data, now_ms()) {
                    ns.save_incremental()
                } else {
                    Vec::new()
                }
            };
            self.doc_export_cache().remove(&ino);
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
        let heads = self.ns().heads();
        if let Some((h, bytes)) = self.doc_export_cache().get(&ino)
            && *h == heads
        {
            return Some(bytes.clone());
        }
        let (heads, bytes) = {
            let mut ns = self.ns();
            let bytes = ns.export_document(ino, &self.registry)?;
            (ns.heads(), bytes)
        };
        self.doc_export_cache().insert(ino, (heads, bytes.clone()));
        Some(bytes)
    }

    /// Attach a sync handle so write ops propagate to peers.
    #[must_use]
    pub fn with_sync(mut self, sync: Arc<SyncHandle>) -> Self {
        self.sync = Some(sync);
        self
    }

    /// Attach a host-side thumbnail provider (cache under `dir`). The mount then
    /// answers the `user.qos.thumb` xattr with a generated preview and records
    /// each thumbnail's hash on the namespace node so it projects to peers.
    #[must_use]
    pub fn with_thumbs(mut self, provider: crate::thumbs::ThumbProvider) -> Self {
        self.thumbs = Some(provider);
        self
    }

    /// The thumbnail (PNG bytes) for a file, generated as metadata ahead
    /// of/independent of content, or `None` when unavailable. Resolution
    /// order: (1) content-addressed cache hit, (2) a peer-gossiped
    /// thumbnail blob hash, hydrated (tiny, ahead of the file's content) and
    /// adopted, (3) host generation from fully-local content, cached and
    /// ingested so the hash gossips to peers. Never hydrates the file's
    /// *content* from the network to make a preview — only the host, which
    /// already has the bytes, generates.
    pub fn thumbnail_for(&self, ino: u64) -> Option<Vec<u8>> {
        let provider = self.thumbs.as_ref()?;
        let (name, content_hash, thumb_hash) = {
            let ns = self.ns();
            let name = ns
                .path_for(ino)
                .and_then(|p| p.rsplit('/').next().map(std::string::ToString::to_string))?;
            let result = (name, ns.hash_for(ino), ns.thumb_hash(ino));
            drop(ns);
            result
        };
        let category = thumbnail::category_for_path(std::path::Path::new(&name));
        if !category.has_preview() {
            return None;
        }
        // No content hash → an empty (or CRDT-document) file → nothing to preview.
        let content_hash = content_hash?;
        let ch = *content_hash.as_bytes();

        if let Some(png) = provider.cached(ch) {
            return Some(png);
        }

        // A peer gossiped a thumbnail blob — hydrate the tiny blob (ahead of
        // the content) and adopt it.
        if let Some(tbh) = thumb_hash
            && let Some(png) = self.read_blob_blocking(tbh)
            && let Some(out) = provider.adopt(ch, png)
        {
            return Some(out);
        }

        let bytes = self.local_content_blocking(content_hash)?;
        let png = provider.generate(ch, category, &bytes)?;

        // Record the hash on the node so the pointer projects to peers, who
        // then hydrate the blob (not the whole file) to show a preview.
        if let Some(tbh) = self.ingest_blob_blocking(&png) {
            let delta = {
                let mut ns = self.ns();
                ns.set_thumb(ino, &tbh);
                ns.save_incremental()
            };
            if !delta.is_empty() {
                self.publish(delta);
            }
        }
        Some(png)
    }

    /// Full content of `hash` **only if entirely local** (every chunk in the
    /// cache); `None` otherwise, so making a preview never pulls content
    /// from the network.
    fn local_content_blocking(&self, hash: Hash) -> Option<Vec<u8>> {
        use crate::syncstate::SyncState;
        let hydrator = self.hydrator.clone();
        let rt = hydrator.runtime_handle.clone();
        tokio::task::block_in_place(|| {
            rt.block_on(async move {
                if hydrator.sync_state(hash).await != SyncState::Local {
                    return None;
                }
                hydrator.read(hash, 0, u64::MAX).await.ok()
            })
        })
    }

    /// Read a raw blob (the thumbnail), serving it locally or hydrating it from
    /// a peer — the small transfer that lets a preview arrive before content.
    fn read_blob_blocking(&self, hash: Hash) -> Option<Vec<u8>> {
        let hydrator = self.hydrator.clone();
        let rt = hydrator.runtime_handle.clone();
        tokio::task::block_in_place(|| {
            rt.block_on(async move { hydrator.read_blob(hash, 0, u64::MAX).await.ok() })
        })
    }

    /// Ingest the thumbnail PNG as a content-addressed blob, returning its hash.
    fn ingest_blob_blocking(&self, png: &[u8]) -> Option<Hash> {
        let store = self.hydrator.store.clone();
        let rt = self.hydrator.runtime_handle.clone();
        let data = png.to_vec();
        tokio::task::block_in_place(|| rt.block_on(async move { store.ingest(data).await.ok() }))
    }

    /// Bind this mount to a workspace ACL, enforcing per-identity access for
    /// `identity`. Every subsequent open/read/write is checked against `acl`
    /// as `identity`; without this the mount keeps Unix-veneer-only
    /// behaviour. Takes anything that becomes an [`AclHandle`]: a
    /// [`WorkspaceAcl`] for a policy that won't change, or a handle held by
    /// a [`crate::CapabilityFile`] for one that reloads under the live mount.
    #[must_use]
    pub fn with_acl(
        mut self,
        acl: impl Into<AclHandle>,
        identity: crate::acl::IdentityKey,
    ) -> Self {
        self.enforcement = Some((acl.into(), identity));
        self
    }

    /// The identity ACL check every open/read/write routes through. `Ok(())`
    /// when the mount has no ACL, or the bound identity holds `access`.
    /// Never `EACCES`: outside the view is `ENOENT`; inside but not
    /// writable is `EROFS`. The single production decision point.
    ///
    /// # Errors
    ///
    /// Returns `Err(ENOENT)` when `ino` doesn't resolve to a path or the identity can't see it,
    /// or `Err(EROFS)` when the identity can see it but lacks `access`.
    pub fn check_access(&self, ino: u64, access: Access) -> Result<(), i32> {
        if self.enforcement.is_none() {
            return Ok(());
        }
        let path = self.ns().path_for(ino).ok_or(ENOENT)?;
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

    /// Is `path` part of the view this mount serves? `true` for every path
    /// on a mount with no ACL attached — the filtered view is the ACL's, so
    /// without one there is nothing to filter.
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
        let path = self.ns().path_for(ino);
        match path {
            Some(path) => self.in_view(&path),
            None => false,
        }
    }

    /// The entries of `ino` that belong in the view, as `readdir` would
    /// report them. `None` when `ino` is not a directory in the view. `.`
    /// and `..` always survive — both are reached through this directory.
    fn visible_entries(&self, ino: u64) -> Option<Vec<(u64, FileType, String)>> {
        let (entries, dir_path) = {
            let mut ns = self.ns();
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
                    name == "."
                        || name == ".."
                        || acl.visible(identity, &child_path(&dir_path, name))
                })
                .collect(),
        )
    }

    /// The link count to report for a directory: `2` plus one per
    /// subdirectory **in the view**, taken from the filtered listing rather
    /// than the namespace so `nlink` and `readdir` can't disagree — a count
    /// from the real tree would announce hidden subdirectories.
    fn visible_dir_nlink(&self, ino: u64) -> u32 {
        let subdirs = self.visible_entries(ino).map_or(0, |entries| {
            entries
                .iter()
                .filter(|(_, kind, name)| {
                    *kind == FileType::Directory && name != "." && name != ".."
                })
                .count()
        });
        2u32.saturating_add(u32::try_from(subdirs).unwrap_or(u32::MAX))
    }

    /// The check every "make a new name in this directory" operation runs
    /// (`create`, `mkdir`, rename's destination). A parent outside the view
    /// is `ENOENT`. The new path must be writable, and **that question is
    /// asked before the name is looked up** — a name the app may not create
    /// gets the same answer whether or not something already sits on it
    /// (otherwise `create` becomes an existence oracle). Only once the app
    /// *may* create the name does a collision with something hidden matter,
    /// refused with `EROFS` rather than written over.
    fn check_new_name(&self, parent: u64, name: &str) -> Result<(), i32> {
        let Some((acl, identity)) = &self.enforcement else {
            return Ok(());
        };
        let parent_path = self.ns().path_for(parent).ok_or(ENOENT)?;
        if !acl.visible(identity, &parent_path) {
            return Err(ENOENT);
        }
        let child = child_path(&parent_path, name);
        self.check_path(&child, Access::Write)?;
        let taken = self.ns().lookup(parent, name).is_some();
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
        let parent_path = self.ns().path_for(parent).ok_or(ENOENT)?;
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

    /// Synchronously content-chunk + ingest bytes, returning the manifest
    /// hash. `Ok(None)` for empty data; `Err` if ingest fails, so callers
    /// never persist a non-empty file with a missing hash.
    fn write_content_blocking(&self, data: &[u8]) -> io::Result<Option<Hash>> {
        let store = self.hydrator.store.clone();
        let rt = self.hydrator.runtime_handle.clone();
        let data = data.to_vec();
        tokio::task::block_in_place(|| rt.block_on(async move { store.write_content(&data).await }))
            .map_err(|e| io::Error::other(e.to_string()))
    }

    /// Current full content of an inode: the in-flight buffer if any, else
    /// the hydrated blob. `Ok(empty)` only for a genuinely empty/hashless
    /// file; a hydration failure is `Err` so a write never seeds from empty
    /// and silently truncates a not-yet-local file.
    fn read_full_blocking(&self, ino: u64) -> io::Result<Vec<u8>> {
        if let Some(buf) = self.buffers().get(&ino) {
            return Ok(buf.clone());
        }
        // Documents seed from their current serialized form (so a partial /
        // append write modifies the real content rather than an empty buffer).
        if self.is_document(ino) {
            return self
                .document_export(ino)
                .ok_or_else(|| io::Error::other("document export failed"));
        }
        let hash = self.ns().hash_for(ino);
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
    /// Aborts WITHOUT mutating the entry if ingest fails, preserving the
    /// previously-committed content.
    fn commit_content(&self, ino: u64, data: &[u8]) -> io::Result<()> {
        let hash = self.write_content_blocking(data)?;
        let delta = {
            let mut ns = self.ns();
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
            let mut ns = self.ns();
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
                if let Some(entries) = ns.readdir(attr.ino.0)
                    && entries.len() > 2
                {
                    // Emptiness is judged on the real tree, since removing the directory
                    // takes its hidden children with it — if the app's own listing said
                    // empty, `ENOTEMPTY` would announce something it can't see, so this
                    // gets the same `EROFS` as any other refusal to write over unseen state.
                    drop(ns);
                    let visible = self.visible_entries(attr.ino.0).map_or(0, |e| e.len());
                    reply.error(to_errno(if visible > 2 { ENOTEMPTY } else { EROFS }));
                    return;
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

/// Reply to a `getxattr` following the two-call protocol: `size == 0` asks for
/// the length; a non-zero size must be big enough for the value, else `ERANGE`.
fn reply_xattr_bytes(reply: ReplyXattr, bytes: &[u8], size: u32) {
    if size == 0 {
        reply.size(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
    } else if (size as usize) < bytes.len() {
        reply.error(to_errno(libc::ERANGE));
    } else {
        reply.data(bytes);
    }
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
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// `offset`/`size` cross the FUSE boundary as untrusted request values,
/// so clamp rather than trust them to fit `usize` before bounding against `bytes.len()`.
fn slice(bytes: &[u8], offset: u64, size: u32) -> Vec<u8> {
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let end = start.saturating_add(size as usize).min(bytes.len());
    bytes
        .get(start..end)
        .map(<[u8]>::to_vec)
        .unwrap_or_default()
}

/// The ACL accesses an open of the given mode requires: read for
/// `O_RDONLY`, write for `O_WRONLY`, both for `O_RDWR`.
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

    /// Resolve one name in a directory. Namespace lookup and view evaluation
    /// run on every call in the same order, so a nonexistent name and a name
    /// hidden by the view cost the same work and return the same `ENOENT`.
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(name) = name.to_str() else {
            reply.error(to_errno(EINVAL));
            return;
        };
        let (found, child) = {
            let mut ns = self.ns();
            let found = ns.lookup(parent.0, name);
            let child = ns.path_for(parent.0).map(|p| child_path(&p, name));
            drop(ns);
            (found, child)
        };
        let in_view = child.is_some_and(|c| self.in_view(&c));
        match found {
            Some(attr) if in_view => {
                let is_doc = self.ns().document_codec(attr.ino.0).is_some();
                let mut attr = attr;
                if attr.kind == FileType::Directory {
                    attr.nlink = self.visible_dir_nlink(attr.ino.0);
                }
                reply.entry(&Self::ttl_for(is_doc), &attr, Generation(0));
            }
            _ => reply.error(to_errno(ENOENT)),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        // Reflect an in-flight write buffer so stat() sees read-your-writes size.
        let buf_len = self.buffers().get(&ino.0).map(|b| b.len() as u64);
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
        let found = self.ns().getattr(ino.0);
        match found {
            Some(mut attr) => {
                if let Some(len) = buf_len.or(doc_len) {
                    attr.size = len;
                    attr.blocks = len.div_ceil(512);
                }
                if attr.kind == FileType::Directory {
                    // Matches the filtered listing: an identity granted
                    // nothing sees an ordinary empty directory (nlink == 2).
                    attr.nlink = self.visible_dir_nlink(ino.0);
                }
                reply.attr(&Self::ttl_for(is_doc), &attr);
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
        // Entries outside the view simply aren't in the directory; offsets
        // below index what the app can see, so paging never has a gap.
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        let Some(entries) = self.visible_entries(ino.0) else {
            reply.error(to_errno(ENOTDIR));
            return;
        };
        // `offset` crosses the FUSE boundary as an untrusted request value.
        let skip = usize::try_from(offset).unwrap_or(usize::MAX);
        for (i, (e_ino, kind, name)) in entries.into_iter().enumerate().skip(skip) {
            let next_offset = i.saturating_add(1) as u64;
            if reply.add(INodeNo(e_ino), next_offset, kind, &name) {
                break;
            }
        }
        reply.ok();
        self.heuristics.on_readdir(ino.0);
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let exists = {
            let ns = self.ns();
            ns.getattr(ino.0).is_some()
        };
        if !exists {
            reply.error(to_errno(ENOENT));
            return;
        }
        // Requires the identity to hold the access the open mode implies.
        // Outside the view ⇒ `ENOENT`; in the view but not writable ⇒
        // `EROFS`, so opening for writing falls back to read-only.
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
        if let Some(buf) = self.buffers().get(&ino.0) {
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
            let ns = self.ns();
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
            rt.block_on(async move { hydrator.read(hash, offset, u64::from(size)).await })
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
            let mut ns = self.ns();
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
        self.buffers().insert(attr.ino.0, Vec::new());
        self.publish(delta);
        reply.created(
            &Self::ttl_for(codec.is_some()),
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
        // Seed the buffer with current content if this is the first write,
        // outside the buffer lock (may do blocking I/O) — a hydration
        // failure must fail the write, never seed-from-empty and truncate.
        let absent = self.buffers().get(&ino.0).is_none();
        let seed = if absent {
            // Seed from current content (export for documents, hydrated bytes
            // for opaque files) so a partial/append write modifies it correctly.
            if let Ok(d) = self.read_full_blocking(ino.0) {
                Some(d)
            } else {
                reply.error(to_errno(EIO));
                return;
            }
        } else {
            None
        };

        // `offset` crosses the FUSE boundary as an untrusted request value.
        let Ok(start) = usize::try_from(offset) else {
            reply.error(to_errno(EINVAL));
            return;
        };
        let Some(end) = start.checked_add(data.len()) else {
            reply.error(to_errno(EINVAL));
            return;
        };
        {
            let mut bufs = self.buffers();
            // or_insert keeps any buffer a racing op already created.
            let buf = match seed {
                Some(d) => bufs.entry(ino.0).or_insert(d),
                None => bufs.entry(ino.0).or_default(),
            };
            if end > buf.len() {
                buf.resize(end, 0);
            }
            let Some(dest) = buf.get_mut(start..end) else {
                reply.error(to_errno(EINVAL));
                return;
            };
            dest.copy_from_slice(data);
        }
        reply.written(u32::try_from(data.len()).unwrap_or(u32::MAX));
    }

    fn flush(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        let buffered = self.buffers().get(&ino.0).cloned();
        if let Some(data) = buffered {
            self.commit_buffer(ino.0, &data);
        }
        reply.ok();
    }

    fn fsync(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        let buffered = self.buffers().get(&ino.0).cloned();
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
        let buffered = self.buffers().remove(&ino.0);
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
        // Truncation is a write. Everything else here is ignored anyway, but
        // a `setattr` on something outside the view must not describe it, so
        // the visibility check runs regardless.
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        if size.is_some()
            && let Err(e) = self.check_access(ino.0, Access::Write)
        {
            reply.error(to_errno(e));
            return;
        }
        // Handle truncation/extension; ignore perm/owner/time changes.
        if let Some(new_size) = size {
            let is_doc = self.is_document(ino.0);
            let Ok(mut data) = self.read_full_blocking(ino.0) else {
                reply.error(to_errno(EIO));
                return;
            };
            // `new_size` crosses the FUSE boundary as an untrusted request value.
            data.resize(usize::try_from(new_size).unwrap_or(usize::MAX), 0);
            let open = self.buffers().contains_key(&ino.0);
            // Documents always stage in the buffer (imported as a whole on
            // release); opaque files commit immediately when not open.
            if open || is_doc {
                self.buffers().insert(ino.0, data);
            } else if let Err(_e) = self.commit_content(ino.0, &data) {
                reply.error(to_errno(EIO));
                return;
            }
        }

        let buf_len = self.buffers().get(&ino.0).map(|b| b.len() as u64);
        let found = self.ns().getattr(ino.0);
        match found {
            Some(mut attr) => {
                if let Some(len) = buf_len {
                    attr.size = len;
                    attr.blocks = len.div_ceil(512);
                }
                if attr.kind == FileType::Directory {
                    attr.nlink = self.visible_dir_nlink(ino.0);
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
            let mut ns = self.ns();
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
            let mut ns = self.ns();
            if !ns.rename(parent.0, name, newparent.0, newname, now_ms()) {
                reply.error(to_errno(ENOENT));
                return;
            }
            ns.save_incremental()
        };
        self.publish(delta);
        reply.ok();
    }

    /// Serve the `user.qos.sync` extended attribute: the entry's hydration
    /// state (`local`/`projected`/`downloading`). Every other attribute name
    /// reports absent (`ENODATA`) — the collaborative FS owns only this one.
    fn getxattr(&self, _req: &Request, ino: INodeNo, name: &OsStr, size: u32, reply: ReplyXattr) {
        // Outside the view the inode doesn't exist — checked before the
        // attribute name, so the sync state never reports on a hidden file.
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        if name.to_str() == Some(crate::syncstate::THUMB_XATTR) {
            match self.thumbnail_for(ino.0) {
                // The kernel caps a single xattr value at 64 KiB — report an
                // oversize preview as absent so the client falls back, rather than
                // failing with ERANGE.
                Some(png) if png.len() <= crate::thumbs::MAX_XATTR_THUMB => {
                    reply_xattr_bytes(reply, &png, size);
                }
                _ => reply.error(to_errno(libc::ENODATA)),
            }
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
        reply_xattr_bytes(reply, state.as_str().as_bytes(), size);
    }

    /// List the extended attributes on `ino`: just `user.qos.sync` (NUL
    /// terminated), so `getfattr`/`listxattr` discover the sync attribute.
    fn listxattr(&self, _req: &Request, ino: INodeNo, size: u32, reply: ReplyXattr) {
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        let (exists, name) = {
            let ns = self.ns();
            let exists = ns.getattr(ino.0).is_some();
            let name = ns
                .path_for(ino.0)
                .and_then(|p| p.rsplit('/').next().map(std::string::ToString::to_string));
            drop(ns);
            (exists, name)
        };
        if !exists {
            reply.error(to_errno(ENOENT));
            return;
        }
        let mut buf = Vec::with_capacity(crate::syncstate::SYNC_XATTR.len().saturating_add(1));
        buf.extend_from_slice(crate::syncstate::SYNC_XATTR.as_bytes());
        buf.push(0);
        // Cheap extension check, no generation — a `getxattr` of it may
        // still be `ENODATA` if the content isn't local yet.
        let previewable = self.thumbs.is_some()
            && name.as_deref().is_some_and(|n| {
                thumbnail::category_for_path(std::path::Path::new(n)).has_preview()
            });
        if previewable {
            buf.extend_from_slice(crate::syncstate::THUMB_XATTR.as_bytes());
            buf.push(0);
        }
        if size == 0 {
            reply.size(u32::try_from(buf.len()).unwrap_or(u32::MAX));
        } else if (size as usize) < buf.len() {
            reply.error(to_errno(libc::ERANGE));
        } else {
            reply.data(&buf);
        }
    }
}

#[cfg(test)]
mod thumb_tests {
    //! The host-side thumbnail metadata path: `QuickFS::thumbnail_for` is
    //! the exact code `getxattr(user.qos.thumb)` runs, so driving it
    //! exercises the production path end to end.
    use super::*;
    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;
    use crate::thumbs::ThumbProvider;
    use iroh::endpoint::Endpoint;
    use iroh::endpoint::presets;
    use std::io::Cursor;

    fn solid_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut buf = image::RgbaImage::new(w, h);
        for px in buf.pixels_mut() {
            *px = image::Rgba([rgb[0], rgb[1], rgb[2], 255]);
        }
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(buf)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    async fn fixture(
        thumb_dir: std::path::PathBuf,
    ) -> (Arc<Mutex<Namespace>>, Arc<ContentStore>, QuickFS) {
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store.clone()));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let provider = ThumbProvider::open(&thumb_dir).unwrap();
        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics).with_thumbs(provider);
        (namespace, store, fs)
    }

    /// The headline: the host generates a preview from a file's local
    /// content, serves it as metadata, records its blob hash on the node so
    /// it projects to peers, and a second request hits the cache.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_generates_a_thumbnail_and_records_its_hash() {
        let dir = tempfile::tempdir().unwrap();
        let (namespace, store, fs) = fixture(dir.path().join("thumbs")).await;

        // A real image file whose content is local to the host.
        let png = solid_png(600, 400, [40, 130, 220]);
        let mh = store.write_content(&png).await.unwrap().unwrap();
        let ino = {
            let mut ns = namespace.lock().unwrap();
            ns.create_file(ROOT_INO, "photo.png", Some(&mh), png.len() as u64, 1)
                .unwrap()
        };

        // No thumbnail recorded yet.
        assert!(namespace.lock().unwrap().thumb_hash(ino).is_none());

        // The metadata path returns a real, sized PNG preview.
        let thumb = fs.thumbnail_for(ino).expect("host generates a preview");
        let decoded = thumbnail::decode_png(&thumb).expect("it is a valid PNG");
        assert!(decoded.is_valid());
        assert_eq!(
            decoded.max_edge(),
            crate::thumbs::HOST_EDGE,
            "downscaled to the host preview size"
        );
        assert!(
            decoded.max_edge() < u32::try_from(png.len()).unwrap_or(u32::MAX),
            "the preview is far smaller than the source"
        );

        // The thumbnail's blob hash is now on the node (so it gossips), and the
        // blob is a real content blob in the store peers can hydrate.
        let tbh = namespace
            .lock()
            .unwrap()
            .thumb_hash(ino)
            .expect("thumbnail hash recorded on the node → projects to peers");
        assert!(
            store.has(tbh).await,
            "the thumbnail blob is stored for peers"
        );

        // A second request returns the identical bytes from the cache.
        let again = fs.thumbnail_for(ino).expect("cached");
        assert_eq!(
            again, thumb,
            "second request is a cache hit, identical bytes"
        );
    }

    /// A preview survives a remount: a fresh mount over the same cache
    /// directory serves the same thumbnail without re-reading the file's content.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thumbnail_survives_a_remount() {
        let dir = tempfile::tempdir().unwrap();
        let png = solid_png(320, 240, [200, 60, 30]);

        let thumb = {
            let (namespace, store, fs) = fixture(dir.path().join("thumbs")).await;
            let mh = store.write_content(&png).await.unwrap().unwrap();
            let ino = {
                let mut ns = namespace.lock().unwrap();
                ns.create_file(ROOT_INO, "pic.png", Some(&mh), png.len() as u64, 1)
                    .unwrap()
            };
            fs.thumbnail_for(ino).expect("generated")
        };

        // A brand-new mount (fresh store, content NOT local) over the same
        // thumb cache still serves the preview — proof it came from the
        // persistent cache, not a re-decode.
        let (namespace2, store2, fs2) = fixture(dir.path().join("thumbs")).await;
        // Same content hash → same cache key. Register the node without the bytes.
        let mh = store2.write_content(&png).await.unwrap().unwrap();
        // Remove the content so it is not local on this mount.
        let ino2 = {
            let mut ns = namespace2.lock().unwrap();
            ns.create_file(ROOT_INO, "pic.png", Some(&mh), png.len() as u64, 1)
                .unwrap()
        };
        let served = fs2
            .thumbnail_for(ino2)
            .expect("served from the on-disk cache");
        assert_eq!(
            served, thumb,
            "same content → same cached preview after remount"
        );
    }

    /// A file whose content is not local yields no thumbnail (the host never
    /// hydrates content just to preview it); a badge-only type never does either.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_local_content_and_badge_types_yield_no_thumbnail() {
        let dir = tempfile::tempdir().unwrap();
        let (namespace, _store, fs) = fixture(dir.path().join("thumbs")).await;

        // A projected image: a hash the store never ingested (not local).
        let projected = iroh_blobs::Hash::from([3u8; 32]);
        let img_ino = {
            let mut ns = namespace.lock().unwrap();
            ns.create_file(ROOT_INO, "remote.png", Some(&projected), 4096, 1)
                .unwrap()
        };
        assert!(
            fs.thumbnail_for(img_ino).is_none(),
            "no preview without local content — never hydrate the file to make one"
        );

        // A non-previewable type is never a thumbnail regardless of content.
        let zip_ino = {
            let mut ns = namespace.lock().unwrap();
            ns.create_file(ROOT_INO, "bundle.zip", Some(&projected), 4096, 1)
                .unwrap()
        };
        assert!(
            fs.thumbnail_for(zip_ino).is_none(),
            "a .zip is a badge, not a preview"
        );
    }

    /// The projected-preview-before-content path: a peer with only gossiped
    /// metadata (thumbnail blob hash + blob) shows a preview WITHOUT the
    /// file's content ever being local.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_gossiped_thumbnail_is_served_before_content() {
        let dir = tempfile::tempdir().unwrap();
        let (namespace, store, fs) = fixture(dir.path().join("thumbs")).await;

        // The real (small) thumbnail a host generated, present as a content blob
        // (as it would be after hydrating the tiny blob from the host).
        let preview = thumbnail::encode_png(
            &thumbnail::image_thumb(&solid_png(256, 256, [90, 200, 120]), 256).unwrap(),
        )
        .unwrap();
        let thumb_blob = store.ingest(preview.clone()).await.unwrap();

        // The file's content is projected (not local), but the node carries the
        // gossiped thumbnail hash.
        let projected = iroh_blobs::Hash::from([5u8; 32]);
        let ino = {
            let mut ns = namespace.lock().unwrap();
            let ino = ns
                .create_file(ROOT_INO, "big-photo.png", Some(&projected), 9_000_000, 1)
                .unwrap();
            ns.set_thumb(ino, &thumb_blob);
            ino
        };

        let served = fs
            .thumbnail_for(ino)
            .expect("a projected preview is served before content");
        assert_eq!(
            served, preview,
            "the gossiped thumbnail is served without local content"
        );
    }
}
