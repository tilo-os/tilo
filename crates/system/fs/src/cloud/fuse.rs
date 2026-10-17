//! The kernel face of a [`CloudVolume`]: a FUSE filesystem at
//! `~/Remote/<account>`.
//!
//! Every callback is a thin translation to the volume's inode operations;
//! the volume owns all state. Attribute and entry TTLs are short so remote
//! changes applied by the sync worker show up without explicit
//! invalidation.

use std::ffi::OsStr;
use std::io;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, InitFlags,
    KernelConfig, LockOwner, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, TimeOrNow,
    WriteFlags,
};

use super::volume::{Attr, CloudVolume, Kind};

const TTL: Duration = Duration::from_secs(1);

pub struct CloudFs {
    volume: Arc<CloudVolume>,
    uid: u32,
    gid: u32,
}

impl CloudFs {
    pub fn new(volume: Arc<CloudVolume>) -> Self {
        let (uid, gid) = (
            rustix::process::getuid().as_raw(),
            rustix::process::getgid().as_raw(),
        );
        Self { volume, uid, gid }
    }

    fn attr(&self, attr: &Attr) -> FileAttr {
        FileAttr {
            ino: INodeNo(attr.ino),
            size: attr.size,
            blocks: attr.size.div_ceil(512),
            atime: attr.mtime,
            mtime: attr.mtime,
            ctime: attr.mtime,
            crtime: attr.mtime,
            kind: match attr.kind {
                Kind::Dir => FileType::Directory,
                Kind::File => FileType::RegularFile,
            },
            perm: attr.perm,
            nlink: match attr.kind {
                Kind::Dir => 2,
                Kind::File => 1,
            },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }
}

fn errno(code: i32) -> Errno {
    io::Error::from_raw_os_error(code).into()
}

fn name(os: &OsStr) -> Result<&str, Errno> {
    os.to_str().ok_or_else(|| errno(libc::EINVAL))
}

impl Filesystem for CloudFs {
    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> io::Result<()> {
        let _ = config.set_max_readahead(1024 * 1024);
        // O_TRUNC arrives with open, so truncating a non-resident file never
        // downloads it first.
        let _ = config.add_capabilities(InitFlags::FUSE_ATOMIC_O_TRUNC);
        Ok(())
    }

    fn destroy(&mut self) {
        self.volume.shutdown();
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name_os: &OsStr, reply: ReplyEntry) {
        let result = name(name_os).and_then(|n| self.volume.lookup(parent.0, n).map_err(errno));
        match result {
            Ok(attr) => reply.entry(&TTL, &self.attr(&attr), Generation(0)),
            Err(e) => reply.error(e),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.volume.getattr(ino.0) {
            Ok(attr) => reply.attr(&TTL, &self.attr(&attr)),
            Err(e) => reply.error(errno(e)),
        }
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
        _flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        // Modes, owners and times are the server's; only the size is ours
        // to change.
        let result = match size {
            Some(size) => self.volume.truncate(ino.0, size),
            None => self.volume.getattr(ino.0),
        };
        match result {
            Ok(attr) => reply.attr(&TTL, &self.attr(&attr)),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name_os: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let result = name(name_os).and_then(|n| self.volume.mkdir(parent.0, n).map_err(errno));
        match result {
            Ok(attr) => reply.entry(&TTL, &self.attr(&attr), Generation(0)),
            Err(e) => reply.error(e),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name_os: &OsStr, reply: ReplyEmpty) {
        match name(name_os).and_then(|n| self.volume.unlink(parent.0, n).map_err(errno)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name_os: &OsStr, reply: ReplyEmpty) {
        match name(name_os).and_then(|n| self.volume.rmdir(parent.0, n).map_err(errno)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name_os: &OsStr,
        newparent: INodeNo,
        newname_os: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        if !flags.is_empty() {
            // RENAME_EXCHANGE/NOREPLACE have no remote equivalent.
            reply.error(errno(libc::EINVAL));
            return;
        }
        let result = name(name_os).and_then(|from| {
            name(newname_os).and_then(|to| {
                self.volume
                    .rename(parent.0, from, newparent.0, to)
                    .map_err(errno)
            })
        });
        match result {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let access = flags.0 & libc::O_ACCMODE;
        let write = access != libc::O_RDONLY;
        let truncate = flags.0 & libc::O_TRUNC != 0;
        match self.volume.open_file(ino.0, write, truncate) {
            Ok(fh) => reply.opened(FileHandle(fh), FopenFlags::empty()),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn read(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        match self.volume.read(fh.0, offset, size) {
            Ok(bytes) => reply.data(&bytes),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        match self.volume.write(fh.0, offset, data) {
            Ok(n) => reply.written(u32::try_from(n).unwrap_or(u32::MAX)),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn flush(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        match self.volume.release(fh.0) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn fsync(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        // Staged bytes are on local disk; remote durability is the upload's.
        reply.ok();
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let entries = match self.volume.readdir(ino.0) {
            Ok(entries) => entries,
            Err(e) => {
                reply.error(errno(e));
                return;
            }
        };
        let skip = usize::try_from(offset).unwrap_or(usize::MAX);
        for (i, (entry_name, attr)) in entries.into_iter().enumerate().skip(skip) {
            let kind = match attr.kind {
                Kind::Dir => FileType::Directory,
                Kind::File => FileType::RegularFile,
            };
            let next = u64::try_from(i).unwrap_or(u64::MAX).saturating_add(1);
            if reply.add(INodeNo(attr.ino), next, kind, OsStr::new(&entry_name)) {
                break;
            }
        }
        reply.ok();
    }

    // READDIRPLUS is deliberately not advertised: enumeration must not fetch
    // exports or publish placeholder sizes as authoritative inode attributes.

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        match self.volume.quota() {
            Some(quota) => {
                let blocks = quota.total.unwrap_or(u64::MAX / 2).div_ceil(4096);
                let used = quota.used.div_ceil(4096);
                let free = blocks.saturating_sub(used);
                reply.statfs(blocks, free, free, 0, 0, 4096, 255, 4096);
            }
            None => reply.statfs(0, 0, 0, 0, 0, 4096, 255, 4096),
        }
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name_os: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        match name(name_os).and_then(|n| self.volume.create(parent.0, n).map_err(errno)) {
            Ok((attr, fh)) => reply.created(
                &TTL,
                &self.attr(&attr),
                Generation(0),
                FileHandle(fh),
                FopenFlags::empty(),
            ),
            Err(e) => reply.error(e),
        }
    }
}
