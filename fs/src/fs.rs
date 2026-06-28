use fuser::*;
use libc::{EINVAL, EIO, EISDIR, ENOENT};
use std::ffi::OsStr;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::hydration::Hydrator;
use crate::namespace::Namespace;
use crate::smart::Heuristics;

const TTL: Duration = Duration::from_secs(10);

pub struct QuickFS {
    pub namespace: Arc<Mutex<Namespace>>,
    pub hydrator: Arc<Hydrator>,
    pub heuristics: Arc<Heuristics>,
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
        }
    }
}

fn to_errno(e: i32) -> Errno {
    io::Error::from_raw_os_error(e).into()
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
        // Resolve the inode to a content hash, rejecting dirs / missing inodes.
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
        // A file with no content hash is empty.
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
}
