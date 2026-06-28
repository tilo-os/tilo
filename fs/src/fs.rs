use fuser::*;
use libc::{EINVAL, EIO, EISDIR, ENOENT};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
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
    /// Broadcasts local CRDT deltas to peers (None when running standalone).
    sync: Option<Arc<SyncHandle>>,
    /// Per-inode in-flight write buffers (content-addressed on flush/release).
    write_buffers: Mutex<HashMap<u64, Vec<u8>>>,
}

impl QuickFS {
    pub fn new(
        namespace: Arc<Mutex<Namespace>>,
        hydrator: Arc<Hydrator>,
        heuristics: Arc<Heuristics>,
    ) -> Self {
        Self {
            namespace,
            hydrator,
            heuristics,
            sync: None,
            write_buffers: Mutex::new(HashMap::new()),
        }
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

    /// Synchronously ingest bytes into the content store (None for empty).
    fn ingest_blocking(&self, data: &[u8]) -> Option<Hash> {
        if data.is_empty() {
            return None;
        }
        let store = self.hydrator.store.clone();
        let rt = self.hydrator.runtime_handle.clone();
        let bytes = Bytes::copy_from_slice(data);
        tokio::task::block_in_place(|| rt.block_on(async move { store.ingest(bytes).await })).ok()
    }

    /// Current full content of an inode (in-flight buffer if any, else hydrated).
    fn read_full_blocking(&self, ino: u64) -> Vec<u8> {
        if let Some(buf) = self.write_buffers.lock().unwrap().get(&ino) {
            return buf.clone();
        }
        let hash = self.namespace.lock().unwrap().hash_for(ino);
        let Some(hash) = hash else {
            return Vec::new();
        };
        let hydrator = self.hydrator.clone();
        let rt = hydrator.runtime_handle.clone();
        tokio::task::block_in_place(|| rt.block_on(async move { hydrator.read(hash, 0, u64::MAX).await }))
            .unwrap_or_default()
    }

    /// Ingest `data` as the new content of `ino` and broadcast the change.
    fn commit_content(&self, ino: u64, data: &[u8]) {
        let hash = self.ingest_blocking(data);
        let delta = {
            let mut ns = self.namespace.lock().unwrap();
            ns.set_content(ino, hash.as_ref(), data.len() as u64, now_ms());
            ns.save_incremental()
        };
        self.publish(delta);
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
            Some(attr) => reply.entry(&TTL, &attr, Generation(0)),
            None => reply.error(to_errno(ENOENT)),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let ns = self.namespace.lock().unwrap();
        match ns.getattr(ino.0) {
            Some(attr) => reply.attr(&TTL, &attr),
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
            reply.error(to_errno(ENOENT));
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
        let (attr, delta) = {
            let mut ns = self.namespace.lock().unwrap();
            let Some(ino) = ns.create_file(parent.0, name, None, 0, now_ms()) else {
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
        reply.created(&TTL, &attr, Generation(0), FileHandle(0), FopenFlags::empty());
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
        // Lazily seed the buffer with the file's current content.
        let need_init = self.write_buffers.lock().unwrap().get(&ino.0).is_none();
        if need_init {
            let current = self.read_full_blocking(ino.0);
            self.write_buffers
                .lock()
                .unwrap()
                .entry(ino.0)
                .or_insert(current);
        }

        {
            let mut bufs = self.write_buffers.lock().unwrap();
            let buf = bufs.get_mut(&ino.0).unwrap();
            let start = offset as usize;
            let end = start + data.len();
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
            self.commit_content(ino.0, &data);
        }
        reply.ok();
    }

    fn fsync(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, _datasync: bool, reply: ReplyEmpty) {
        let buffered = self.write_buffers.lock().unwrap().get(&ino.0).cloned();
        if let Some(data) = buffered {
            self.commit_content(ino.0, &data);
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
            self.commit_content(ino.0, &data);
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
        // Handle truncation/extension; ignore perm/owner/time changes (the OS
        // controls those, and we don't persist them).
        if let Some(new_size) = size {
            let mut data = self.read_full_blocking(ino.0);
            data.resize(new_size as usize, 0);
            // Update the in-flight buffer if open, else commit straight away.
            let open = self.write_buffers.lock().unwrap().contains_key(&ino.0);
            if open {
                self.write_buffers.lock().unwrap().insert(ino.0, data);
            } else {
                self.commit_content(ino.0, &data);
            }
        }
        match self.namespace.lock().unwrap().getattr(ino.0) {
            Some(attr) => reply.attr(&TTL, &attr),
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
        self.remove_entry(parent, name, reply);
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        self.remove_entry(parent, name, reply);
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

impl QuickFS {
    fn remove_entry(&self, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
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
