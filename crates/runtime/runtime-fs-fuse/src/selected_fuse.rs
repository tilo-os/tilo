//! The selected-file adapter over the existing `WorkspaceFs` protocol.
//! It creates no mount and grants no filesystem authority. A canonical signed
//! worker supplies its already admitted channel and target presentation facts.
use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, InitFlags,
    KernelConfig, LockOwner, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyWrite, Request, WriteFlags,
};
use runtime_fs::selected_client::SelectedFileConnection;
use std::{
    ffi::{OsStr, OsString},
    io,
    os::unix::ffi::OsStrExt,
    time::{Duration, UNIX_EPOCH},
};
const ROOT: INodeNo = INodeNo(1);
const FILE: INodeNo = INodeNo(2);
const TTL: Duration = Duration::ZERO;
pub struct SelectedFilesystem {
    source: SelectedFileConnection,
    name: OsString,
    uid: u32,
    gid: u32,
}
impl SelectedFilesystem {
    /// Construct a presentation adapter, never source or mount authority.
    /// The worker's native admission remains responsible for actual credentials,
    /// the FUSE channel, namespace confinement and complete mount retirement.
    /// # Errors
    /// Refuses root presentation and names the kernel cannot represent.
    pub fn new(
        source: SelectedFileConnection,
        name: OsString,
        uid: u32,
        gid: u32,
    ) -> io::Result<Self> {
        let bytes = name.as_bytes();
        if uid == 0
            || bytes.is_empty()
            || bytes.len() > 255
            || matches!(bytes, b"." | b"..")
            || bytes.contains(&b'/')
            || bytes.contains(&0)
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self {
            source,
            name,
            uid,
            gid,
        })
    }
}

fn errno(code: i32) -> Errno {
    io::Error::from_raw_os_error(code).into()
}
impl SelectedFilesystem {
    fn attr(&self, ino: INodeNo, handle: Option<FileHandle>) -> io::Result<FileAttr> {
        if ino != ROOT && ino != FILE {
            return Err(io::ErrorKind::NotFound.into());
        }
        // Even the synthetic directory is revoked when its source is revoked.
        let metadata = match handle {
            Some(handle) if ino == FILE => self.source.metadata_handle(handle.0)?,
            _ => self.source.metadata()?,
        };
        let mtime = metadata
            .modified_unix_seconds
            .and_then(|s| u64::try_from(s).ok())
            .and_then(|s| UNIX_EPOCH.checked_add(Duration::from_secs(s)))
            .unwrap_or(UNIX_EPOCH);
        let directory = ino == ROOT;
        Ok(FileAttr {
            ino,
            size: if directory { 0 } else { metadata.size },
            blocks: metadata.size.div_ceil(512),
            atime: mtime,
            mtime,
            ctime: mtime,
            crtime: mtime,
            kind: if directory {
                FileType::Directory
            } else {
                FileType::RegularFile
            },
            perm: if directory { 0o700 } else { 0o600 },
            nlink: if directory { 2 } else { 1 },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        })
    }
}
impl Filesystem for SelectedFilesystem {
    fn init(&mut self, _: &Request, config: &mut KernelConfig) -> io::Result<()> {
        config
            .add_capabilities(InitFlags::FUSE_ATOMIC_O_TRUNC)
            .map_err(|_| io::Error::other("FUSE atomic truncate is required"))?;
        Ok(())
    }
    fn lookup(&self, _: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        if parent != ROOT || name != self.name {
            reply.error(errno(libc::ENOENT));
            return;
        }
        match self.attr(FILE, None) {
            Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
            Err(error) => reply.error(error.into()),
        }
    }
    fn getattr(&self, _: &Request, ino: INodeNo, handle: Option<FileHandle>, reply: ReplyAttr) {
        match self.attr(ino, handle) {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(error) => reply.error(error.into()),
        }
    }
    fn open(&self, _: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        if ino != FILE {
            reply.error(errno(libc::EISDIR));
            return;
        }
        let mode = match flags.0 & (libc::O_ACCMODE | libc::O_TRUNC | libc::O_APPEND) {
            libc::O_RDONLY => runtime_fs::OpenMode::Read,
            value if value == (libc::O_WRONLY | libc::O_TRUNC) => runtime_fs::OpenMode::Write,
            _ => {
                reply.error(errno(libc::EOPNOTSUPP));
                return;
            }
        };
        match self.source.open(mode) {
            Ok(handle) => reply.opened(FileHandle(handle), FopenFlags::FOPEN_DIRECT_IO),
            Err(error) => reply.error(error.into()),
        }
    }
    fn read(
        &self,
        _: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _: OpenFlags,
        _: Option<LockOwner>,
        reply: ReplyData,
    ) {
        if ino != FILE {
            reply.error(errno(libc::EBADF));
            return;
        }
        match self.source.read(fh.0, offset, size) {
            Ok(bytes) => reply.data(&bytes),
            Err(error) => reply.error(error.into()),
        }
    }
    fn write(
        &self,
        _: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        bytes: &[u8],
        _: WriteFlags,
        _: OpenFlags,
        _: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        if ino != FILE {
            reply.error(errno(libc::EBADF));
            return;
        }
        match self.source.write(fh.0, offset, bytes) {
            Ok(written) => reply.written(written),
            Err(error) => reply.error(error.into()),
        }
    }
    fn flush(&self, _: &Request, ino: INodeNo, fh: FileHandle, _: LockOwner, reply: ReplyEmpty) {
        if ino != FILE {
            reply.error(errno(libc::EBADF));
            return;
        }
        match self.source.flush(fh.0) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error.into()),
        }
    }
    fn fsync(&self, _: &Request, ino: INodeNo, fh: FileHandle, _: bool, reply: ReplyEmpty) {
        if ino != FILE {
            reply.error(errno(libc::EBADF));
            return;
        }
        match self.source.flush(fh.0) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error.into()),
        }
    }
    fn release(
        &self,
        _: &Request,
        ino: INodeNo,
        fh: FileHandle,
        _: OpenFlags,
        _: Option<LockOwner>,
        _: bool,
        reply: ReplyEmpty,
    ) {
        if ino != FILE {
            reply.error(errno(libc::EBADF));
            return;
        }
        match self.source.release(fh.0) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error.into()),
        }
    }
    fn readdir(
        &self,
        _: &Request,
        ino: INodeNo,
        _: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        if ino != ROOT {
            reply.error(errno(libc::ENOTDIR));
            return;
        }
        if let Err(error) = self.source.metadata() {
            reply.error(error.into());
            return;
        }
        let rows = [
            (ROOT, FileType::Directory, OsStr::new(".")),
            (ROOT, FileType::Directory, OsStr::new("..")),
            (FILE, FileType::RegularFile, self.name.as_os_str()),
        ];
        for ((ino, kind, name), cookie) in rows
            .iter()
            .zip(1u64..)
            .skip(usize::try_from(offset).unwrap_or(usize::MAX))
        {
            if reply.add(*ino, cookie, *kind, name) {
                break;
            }
        }
        reply.ok();
    }
}
