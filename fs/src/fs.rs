use fuser::*;
use libc::{EINVAL, EIO, EISDIR, ENOENT, ENOTDIR, ENOTEMPTY};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iroh_blobs::Hash;

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
                        reply.error(to_errno(ENOTEMPTY));
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

impl Filesystem for QuickFS {
    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> Result<(), io::Error> {
        let _ = config.set_max_readahead(1024 * 1024);
        Ok(())
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(name) = name.to_str() else {
            reply.error(to_errno(EINVAL));
            return;
        };
        let mut ns = self.namespace.lock().unwrap();
        match ns.lookup(parent.0, name) {
            Some(attr) => {
                let is_doc = ns.document_codec(attr.ino.0).is_some();
                reply.entry(&self.ttl_for(is_doc), &attr, Generation(0));
            }
            None => reply.error(to_errno(ENOENT)),
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
        let ns = self.namespace.lock().unwrap();
        match ns.getattr(ino.0) {
            Some(mut attr) => {
                if let Some(len) = buf_len.or(doc_len) {
                    attr.size = len;
                    attr.blocks = len.div_ceil(512);
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
        let entries = {
            let mut ns = self.namespace.lock().unwrap();
            ns.readdir(ino.0)
        };
        let Some(entries) = entries else {
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

    fn open(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let exists = {
            let ns = self.namespace.lock().unwrap();
            ns.getattr(ino.0).is_some()
        };
        if exists {
            reply.opened(FileHandle(0), FopenFlags::empty());
            self.heuristics.on_open(ino.0);
        } else {
            reply.error(to_errno(ENOENT));
        }
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
        match self.namespace.lock().unwrap().getattr(ino.0) {
            Some(mut attr) => {
                if let Some(len) = buf_len {
                    attr.size = len;
                    attr.blocks = len.div_ceil(512);
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
}
