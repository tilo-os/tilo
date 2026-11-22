//! Host directories mounted over vsock, for guests without virtio-fs: each
//! share is a FUSE filesystem at the host's path whose every operation is one
//! request on the share's own connection ([`linux_vm_protocol::files`]).
//!
//! The host owns the files and decides what is allowed; this side translates
//! and shows every file as the guest user's. The kernel keeps names,
//! attributes and listings for as long as the host says nothing changed: a
//! thread asks the host what did ([`FsRequest::Changes`]) and drops exactly
//! that, so a walk over a tree nothing touched (`git status`) asks the host
//! nothing, while a member's edit shows within [`CHANGES_EVERY`].

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, LockOwner,
    Notifier, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory,
    ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, TimeOrNow, WriteFlags,
};
use linux_vm_protocol::files::{
    Attr, Change, FileKind, FileShare, FsReply, FsRequest, MAX_IO_BYTES, SetTime, Time,
};
use linux_vm_protocol::{recv, send};
use tilo_lock::MutexRecover as _;

/// How long the kernel keeps what it was told when the host reports changes:
/// until the host says otherwise, within reason.
const CACHED: Duration = Duration::from_secs(60 * 60);

/// How often the host is asked what changed: the longest a member's edit
/// takes to show here.
pub const CHANGES_EVERY: Duration = Duration::from_millis(100);

pub struct Remote<S> {
    stream: Mutex<S>,
}

impl<S: Read + Write> Remote<S> {
    pub fn open(mut stream: S, path: &str) -> io::Result<Self> {
        send(
            &mut stream,
            &FsRequest::Hello {
                path: path.to_string(),
            },
        )
        .map_err(io::Error::other)?;
        match recv(&mut stream).map_err(io::Error::other)? {
            Some(FsReply::Ok) => Ok(Self {
                stream: Mutex::new(stream),
            }),
            Some(FsReply::Error(code)) => Err(io::Error::from_raw_os_error(code)),
            _ => Err(io::Error::other(
                "the host did not answer the share's hello",
            )),
        }
    }

    pub fn ask(&self, request: &FsRequest) -> Result<FsReply, Errno> {
        let mut stream = self.stream.lock_recover();
        send(&mut *stream, request).map_err(|_| Errno::EIO)?;
        match recv::<_, FsReply>(&mut *stream) {
            Ok(Some(FsReply::Error(code))) => Err(Errno::from_i32(code)),
            Ok(Some(reply)) => Ok(reply),
            _ => Err(Errno::EIO),
        }
    }
}

pub struct RemoteFs<S> {
    remote: Arc<Remote<S>>,
    uid: u32,
    gid: u32,
    /// How long the kernel keeps names and attributes: [`CACHED`] once the
    /// host reports what changes, else nothing.
    ttl: Duration,
}

impl<S: Read + Write> RemoteFs<S> {
    /// The share `remote` serves; its kernel caches once the host has
    /// started reporting what changes ([`Self::follow_with`]).
    pub fn new(remote: Remote<S>, uid: u32, gid: u32) -> Self {
        let ttl = match remote.ask(&FsRequest::Changes) {
            Ok(FsReply::Changes { .. }) => CACHED,
            _ => Duration::ZERO,
        };
        Self {
            remote: Arc::new(remote),
            uid,
            gid,
            ttl,
        }
    }

    /// Whether the kernel keeps what it is told.
    #[must_use]
    pub fn cached(&self) -> bool {
        !self.ttl.is_zero()
    }

    /// What an opened file or directory keeps of what was read before.
    fn keep(&self) -> FopenFlags {
        if self.cached() {
            FopenFlags::FOPEN_KEEP_CACHE
        } else {
            FopenFlags::empty()
        }
    }

    /// What, given the mounted filesystem's notifier, asks the host what
    /// changed every [`CHANGES_EVERY`] and drops exactly that from the
    /// kernel, until the filesystem is gone.
    pub fn follow_with(&self) -> impl FnOnce(Notifier) -> io::Result<()> + use<S>
    where
        S: Send + 'static,
    {
        let remote = self.cached().then(|| Arc::downgrade(&self.remote));
        move |notifier| {
            let Some(remote) = remote else {
                return Ok(());
            };
            std::thread::Builder::new()
                .name("share-changes".into())
                .spawn(move || follow_changes(&remote, &notifier))
                .map(drop)
        }
    }

    fn file_attr(&self, attr: &Attr) -> FileAttr {
        let time = |time: Time| {
            u64::try_from(time.secs)
                .ok()
                .and_then(|secs| {
                    UNIX_EPOCH.checked_add(Duration::new(secs, time.nanos.min(999_999_999)))
                })
                .unwrap_or(UNIX_EPOCH)
        };
        FileAttr {
            ino: INodeNo(attr.node),
            size: attr.size,
            blocks: attr.blocks,
            atime: time(attr.atime),
            mtime: time(attr.mtime),
            ctime: time(attr.ctime),
            crtime: time(attr.ctime),
            kind: file_type(attr.kind),
            perm: attr.perm,
            nlink: attr.nlink,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn attr(&self, request: &FsRequest) -> Result<FileAttr, Errno> {
        match self.remote.ask(request)? {
            FsReply::Attr(attr) => Ok(self.file_attr(&attr)),
            _ => Err(Errno::EIO),
        }
    }

    fn ok(&self, request: &FsRequest) -> Result<(), Errno> {
        match self.remote.ask(request)? {
            FsReply::Ok => Ok(()),
            _ => Err(Errno::EIO),
        }
    }
}

fn follow_changes<S: Read + Write>(remote: &Weak<Remote<S>>, notifier: &Notifier) {
    let mut more = false;
    loop {
        if !more {
            std::thread::sleep(CHANGES_EVERY);
        }
        let Some(held) = remote.upgrade() else {
            return;
        };
        let Ok(FsReply::Changes {
            changes,
            more: rest,
        }) = held.ask(&FsRequest::Changes)
        else {
            // The share's connection is gone, and the mount with it.
            return;
        };
        drop(held);
        more = rest;
        for change in changes {
            // The kernel may no longer hold what changed: nothing to drop.
            let _ = match change {
                Change::Entry { parent, name } => {
                    notifier.inval_entry(INodeNo(parent), OsStr::from_bytes(&name))
                }
                Change::Node(node) => notifier.inval_inode(INodeNo(node), 0, 0),
            };
        }
    }
}

const fn file_type(kind: FileKind) -> FileType {
    match kind {
        FileKind::File => FileType::RegularFile,
        FileKind::Directory => FileType::Directory,
        FileKind::Symlink => FileType::Symlink,
        FileKind::Other => FileType::NamedPipe,
    }
}

fn set_time(time: Option<TimeOrNow>) -> Option<SetTime> {
    time.map(|time| match time {
        TimeOrNow::Now => SetTime::Now,
        TimeOrNow::SpecificTime(at) => {
            let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
            SetTime::At(Time {
                secs: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
                nanos: since.subsec_nanos(),
            })
        }
    })
}

fn name(name: &OsStr) -> Vec<u8> {
    name.as_bytes().to_vec()
}

/// `readdir` cookies 1 and 2 are `.` and `..`; the host's entry `i` is
/// cookie `i + 3`.
const OWN_ENTRIES: u64 = 2;

impl<S: Read + Write + Send + 'static> Filesystem for RemoteFs<S> {
    fn lookup(&self, _: &Request, parent: INodeNo, entry: &OsStr, reply: ReplyEntry) {
        match self.attr(&FsRequest::Lookup {
            parent: parent.0,
            name: name(entry),
        }) {
            Ok(attr) => reply.entry(&self.ttl, &attr, Generation(0)),
            // A name that is not there is kept as such too (node 0), until
            // the host says it came: git asks after many files no checkout
            // has.
            Err(error) if error == Errno::ENOENT && self.cached() => {
                let none = self.file_attr(&Attr {
                    node: 0,
                    kind: FileKind::File,
                    perm: 0,
                    size: 0,
                    blocks: 0,
                    nlink: 0,
                    atime: Time::default(),
                    mtime: Time::default(),
                    ctime: Time::default(),
                });
                reply.entry(&self.ttl, &none, Generation(0));
            }
            Err(error) => reply.error(error),
        }
    }

    fn forget(&self, _: &Request, ino: INodeNo, count: u64) {
        let _ = self.remote.ask(&FsRequest::Forget { node: ino.0, count });
    }

    fn getattr(&self, _: &Request, ino: INodeNo, _: Option<FileHandle>, reply: ReplyAttr) {
        match self.attr(&FsRequest::GetAttr { node: ino.0 }) {
            Ok(attr) => reply.attr(&self.ttl, &attr),
            Err(error) => reply.error(error),
        }
    }

    fn setattr(
        &self,
        _: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        _: Option<u32>,
        _: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _: Option<SystemTime>,
        _: Option<FileHandle>,
        _: Option<SystemTime>,
        _: Option<SystemTime>,
        _: Option<SystemTime>,
        _: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        match self.attr(&FsRequest::SetAttr {
            node: ino.0,
            mode,
            size,
            atime: set_time(atime),
            mtime: set_time(mtime),
        }) {
            Ok(attr) => reply.attr(&self.ttl, &attr),
            Err(error) => reply.error(error),
        }
    }

    fn readlink(&self, _: &Request, ino: INodeNo, reply: ReplyData) {
        match self.remote.ask(&FsRequest::ReadLink { node: ino.0 }) {
            Ok(FsReply::Target(target)) => reply.data(&target),
            Ok(_) => reply.error(Errno::EIO),
            Err(error) => reply.error(error),
        }
    }

    fn mkdir(
        &self,
        _: &Request,
        parent: INodeNo,
        entry: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        match self.attr(&FsRequest::MkDir {
            parent: parent.0,
            name: name(entry),
            mode: mode & !umask,
        }) {
            Ok(attr) => reply.entry(&self.ttl, &attr, Generation(0)),
            Err(error) => reply.error(error),
        }
    }

    fn unlink(&self, _: &Request, parent: INodeNo, entry: &OsStr, reply: ReplyEmpty) {
        match self.ok(&FsRequest::Unlink {
            parent: parent.0,
            name: name(entry),
        }) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn rmdir(&self, _: &Request, parent: INodeNo, entry: &OsStr, reply: ReplyEmpty) {
        match self.ok(&FsRequest::RmDir {
            parent: parent.0,
            name: name(entry),
        }) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn symlink(
        &self,
        _: &Request,
        parent: INodeNo,
        link: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        match self.attr(&FsRequest::Symlink {
            parent: parent.0,
            name: name(link),
            target: target.as_os_str().as_bytes().to_vec(),
        }) {
            Ok(attr) => reply.entry(&self.ttl, &attr, Generation(0)),
            Err(error) => reply.error(error),
        }
    }

    fn rename(
        &self,
        _: &Request,
        parent: INodeNo,
        entry: &OsStr,
        new_parent: INodeNo,
        new_name: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        if flags.contains(RenameFlags::RENAME_EXCHANGE) {
            reply.error(Errno::from_i32(libc::EINVAL));
            return;
        }
        match self.ok(&FsRequest::Rename {
            parent: parent.0,
            name: name(entry),
            new_parent: new_parent.0,
            new_name: name(new_name),
            no_replace: flags.contains(RenameFlags::RENAME_NOREPLACE),
        }) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn link(
        &self,
        _: &Request,
        ino: INodeNo,
        new_parent: INodeNo,
        new_name: &OsStr,
        reply: ReplyEntry,
    ) {
        match self.attr(&FsRequest::Link {
            node: ino.0,
            new_parent: new_parent.0,
            new_name: name(new_name),
        }) {
            Ok(attr) => reply.entry(&self.ttl, &attr, Generation(0)),
            Err(error) => reply.error(error),
        }
    }

    fn open(&self, _: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let write = flags.0 & libc::O_ACCMODE != libc::O_RDONLY;
        match self.remote.ask(&FsRequest::Open {
            node: ino.0,
            write,
            truncate: flags.0 & libc::O_TRUNC != 0,
        }) {
            // What was read stays in the kernel until the host says the
            // file changed.
            Ok(FsReply::Opened { handle }) => reply.opened(FileHandle(handle), self.keep()),
            Ok(_) => reply.error(Errno::EIO),
            Err(error) => reply.error(error),
        }
    }

    fn read(
        &self,
        _: &Request,
        _: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _: OpenFlags,
        _: Option<LockOwner>,
        reply: ReplyData,
    ) {
        match self.remote.ask(&FsRequest::Read {
            handle: fh.0,
            offset,
            size: size.min(MAX_IO_BYTES),
        }) {
            Ok(FsReply::Data(data)) => reply.data(&data),
            Ok(_) => reply.error(Errno::EIO),
            Err(error) => reply.error(error),
        }
    }

    fn write(
        &self,
        _: &Request,
        _: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _: WriteFlags,
        _: OpenFlags,
        _: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        let mut written = 0_u32;
        for (index, chunk) in data.chunks(MAX_IO_BYTES as usize).enumerate() {
            let at = offset.saturating_add((index as u64).saturating_mul(u64::from(MAX_IO_BYTES)));
            match self.remote.ask(&FsRequest::Write {
                handle: fh.0,
                offset: at,
                data: chunk.to_vec(),
            }) {
                Ok(FsReply::Written(count)) => written = written.saturating_add(count),
                Ok(_) => return reply.error(Errno::EIO),
                Err(error) => return reply.error(error),
            }
        }
        reply.written(written);
    }

    fn flush(&self, _: &Request, _: INodeNo, _: FileHandle, _: LockOwner, reply: ReplyEmpty) {
        reply.ok();
    }

    fn release(
        &self,
        _: &Request,
        _: INodeNo,
        fh: FileHandle,
        _: OpenFlags,
        _: Option<LockOwner>,
        _: bool,
        reply: ReplyEmpty,
    ) {
        let _ = self.remote.ask(&FsRequest::Release { handle: fh.0 });
        reply.ok();
    }

    fn fsync(&self, _: &Request, _: INodeNo, fh: FileHandle, _: bool, reply: ReplyEmpty) {
        match self.ok(&FsRequest::Fsync { handle: fh.0 }) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn opendir(&self, _: &Request, _: INodeNo, _: OpenFlags, reply: ReplyOpen) {
        // A listing stays in the kernel until the host says the directory
        // changed.
        let flags = if self.cached() {
            FopenFlags::FOPEN_CACHE_DIR | self.keep()
        } else {
            FopenFlags::empty()
        };
        reply.opened(FileHandle(0), flags);
    }

    fn readdir(
        &self,
        _: &Request,
        ino: INodeNo,
        _: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        if offset < 1 && reply.add(ino, 1, FileType::Directory, ".") {
            return reply.ok();
        }
        if offset < OWN_ENTRIES && reply.add(ino, OWN_ENTRIES, FileType::Directory, "..") {
            return reply.ok();
        }
        let mut next = offset.saturating_sub(OWN_ENTRIES);
        loop {
            let (entries, done) = match self.remote.ask(&FsRequest::ReadDir {
                node: ino.0,
                offset: next,
            }) {
                Ok(FsReply::Entries { entries, done }) => (entries, done),
                Ok(_) => return reply.error(Errno::EIO),
                Err(error) => return reply.error(error),
            };
            for entry in &entries {
                next = next.saturating_add(1);
                // The kernel looks every entry up before use: the number
                // here is never relied on.
                if reply.add(
                    INodeNo(u64::MAX),
                    next.saturating_add(OWN_ENTRIES),
                    file_type(entry.kind),
                    OsStr::from_bytes(&entry.name),
                ) {
                    return reply.ok();
                }
            }
            if done || entries.is_empty() {
                return reply.ok();
            }
        }
    }

    fn statfs(&self, _: &Request, _: INodeNo, reply: ReplyStatfs) {
        match self.remote.ask(&FsRequest::StatFs) {
            Ok(FsReply::StatFs(s)) => reply.statfs(
                s.blocks, s.bfree, s.bavail, s.files, s.ffree, s.bsize, s.namelen, s.frsize,
            ),
            Ok(_) => reply.error(Errno::EIO),
            Err(error) => reply.error(error),
        }
    }

    fn create(
        &self,
        _: &Request,
        parent: INodeNo,
        entry: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        match self.remote.ask(&FsRequest::Create {
            parent: parent.0,
            name: name(entry),
            mode: mode & !umask,
            exclusive: flags & libc::O_EXCL != 0,
            truncate: flags & libc::O_TRUNC != 0,
        }) {
            Ok(FsReply::Created { attr, handle }) => reply.created(
                &self.ttl,
                &self.file_attr(&attr),
                Generation(0),
                FileHandle(handle),
                FopenFlags::empty(),
            ),
            Ok(_) => reply.error(Errno::EIO),
            Err(error) => reply.error(error),
        }
    }
}

#[derive(Default)]
pub struct Mounts {
    mounted: BTreeMap<String, (FileShare, fuser::BackgroundSession)>,
}

impl Mounts {
    /// Make the mounted set `desired`: unmount what is no longer in it,
    /// mount what is new, connecting each to the host through `connect`.
    /// Returns the paths mounted, and what failed.
    pub fn apply<S, C>(
        &mut self,
        desired: &[FileShare],
        connect: C,
        uid: u32,
        gid: u32,
    ) -> (Vec<String>, Vec<String>)
    where
        S: Read + Write + Send + 'static,
        C: Fn(u32) -> io::Result<S>,
    {
        let stale: Vec<String> = self
            .mounted
            .iter()
            .filter(|(_, (share, _))| !desired.contains(share))
            .map(|(path, _)| path.clone())
            .collect();
        for path in stale {
            if let Some((_, session)) = self.mounted.remove(&path) {
                unmount(&path, session);
            }
        }
        let mut failures = Vec::new();
        for share in desired {
            if self.mounted.contains_key(&share.path) {
                continue;
            }
            match mount(share, &connect, uid, gid) {
                Ok(session) => {
                    self.mounted
                        .insert(share.path.clone(), (share.clone(), session));
                }
                Err(error) => failures.push(format!("{}: {error}", share.path)),
            }
        }
        (self.mounted.keys().cloned().collect(), failures)
    }
}

fn mount<S, C>(
    share: &FileShare,
    connect: &C,
    uid: u32,
    gid: u32,
) -> io::Result<fuser::BackgroundSession>
where
    S: Read + Write + Send + 'static,
    C: Fn(u32) -> io::Result<S>,
{
    let remote = Remote::open(connect(share.port)?, &share.path)?;
    let path = Path::new(&share.path);
    std::fs::create_dir_all(path)?;
    mount_at(remote, path, share.read_only, uid, gid)
}

/// Mount the share `remote` serves at `path`, its files the guest user's
/// (`uid`, `gid`), following what changes on the host.
pub fn mount_at<S: Read + Write + Send + 'static>(
    remote: Remote<S>,
    path: &Path,
    read_only: bool,
    uid: u32,
    gid: u32,
) -> io::Result<fuser::BackgroundSession> {
    let mut config = fuser::Config::default();
    config.mount_options = vec![
        fuser::MountOption::FSName("tilo-host".into()),
        fuser::MountOption::DefaultPermissions,
    ];
    if read_only {
        config.mount_options.push(fuser::MountOption::RO);
    }
    config.acl = fuser::SessionACL::All;
    let fs = RemoteFs::new(remote, uid, gid);
    let follow = fs.follow_with();
    let session = fuser::spawn_mount(fs, path, &config)?;
    follow(session.notifier())?;
    Ok(session)
}

/// Unmount now, even while a process still has a file open there: the
/// share is going away (a suspend, a revoked grant), and its connection
/// with it.
fn unmount(path: &str, session: fuser::BackgroundSession) {
    let _ = rustix::mount::unmount(path, rustix::mount::UnmountFlags::DETACH);
    drop(session);
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;

    use super::*;

    tilo_test_env::scenario! {
        fn the_share_connection;

        phase requests_and_errors_travel_over_the_share_connection;
        {
            let (host, guest) = UnixStream::pair().unwrap();
            let server = std::thread::spawn(move || {
                let mut host = host;
                let Some(FsRequest::Hello { path }) = recv(&mut host).unwrap() else {
                    panic!("hello first");
                };
                assert_eq!(path, "/srv/ws/files");
                send(&mut host, &FsReply::Ok).unwrap();
                let Some(FsRequest::Lookup { name, .. }) = recv(&mut host).unwrap() else {
                    panic!("lookup");
                };
                assert_eq!(name, b"missing");
                send(&mut host, &FsReply::Error(libc::ENOENT)).unwrap();
            });
            let remote = Remote::open(guest, "/srv/ws/files").unwrap();
            let answer = remote.ask(&FsRequest::Lookup {
                parent: 1,
                name: b"missing".to_vec(),
            });
            assert_eq!(answer, Err(Errno::from_i32(libc::ENOENT)));
            server.join().unwrap();
            assert_eq!(
                remote.ask(&FsRequest::StatFs),
                Err(Errno::EIO),
                "a host that went away is an I/O error"
            );
        }

        phase a_share_the_host_does_not_serve_is_not_mounted;
        {
            let (host, guest) = UnixStream::pair().unwrap();
            let server = std::thread::spawn(move || {
                let mut host = host;
                let _: Option<FsRequest> = recv(&mut host).unwrap();
                send(&mut host, &FsReply::Error(libc::EACCES)).unwrap();
            });
            let error = Remote::open(guest, "/srv/other").err().unwrap();
            assert_eq!(error.raw_os_error(), Some(libc::EACCES));
            server.join().unwrap();
        }
    }
}
