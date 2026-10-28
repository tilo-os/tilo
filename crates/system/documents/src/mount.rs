//! The documents table served as a filesystem, for the session shell.
//!
//! Layout under the mount point: `<room>/<app>/<token>/<name>[/…]`. The
//! launcher binds one app's `<room>/<app>` at [`crate::SANDBOX_DIR`], so an
//! app reaches only its own documents. The room and app levels answer any
//! valid name with an empty folder (the launcher binds an app's view before
//! anything is chosen) and list nothing; an app's view lists its tokens; a
//! token holds the chosen file or folder.
//!
//! Every operation looks its document up in the table again, so taking a
//! document back is immediate, and reaches the host only through
//! [`crate::host::Target`], which resolves beneath the chosen item. Changes
//! are refused unless the choice was for writing: a Save may create and
//! write its file, write its save sibling and rename that over it; a
//! writable folder may change anything beneath it.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileExt as _, FileTypeExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{
    FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, LockOwner,
    OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyWrite, Request, WriteFlags,
};
use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::io::Errno;

use crate::host::{self, Target};
use crate::{AppKey, Documents, Kind, is_app, is_room};

/// How long the kernel may keep an answer: short, so a document taken
/// back stops showing within a second even where nothing re-asks.
const TTL: Duration = Duration::from_secs(1);
const ROOT: u64 = 1;

/// The documents filesystem, mounted; unmounted when dropped.
pub struct Mounted {
    _session: fuser::BackgroundSession,
    at: PathBuf,
}

impl Mounted {
    pub fn path(&self) -> &Path {
        &self.at
    }
}

/// Mount `documents` at `at`, creating the folder (owner-only) and clearing
/// a mount a crashed shell left behind there. Only this user can enter it:
/// the kernel refuses every other user, and nothing on it can be executed
/// or be a device.
///
/// # Errors
///
/// When FUSE is unavailable or the folder cannot be prepared.
pub fn mount(documents: Documents, at: &Path) -> io::Result<Mounted> {
    clear_stale_mount(at)?;
    std::fs::create_dir_all(at)?;
    std::fs::set_permissions(at, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let mut config = fuser::Config::default();
    config.mount_options = vec![
        fuser::MountOption::FSName("tilo-documents".into()),
        fuser::MountOption::NoSuid,
        fuser::MountOption::NoDev,
        fuser::MountOption::NoExec,
    ];
    config.acl = fuser::SessionACL::Owner;
    // Every app's documents are served side by side: a read that waits on
    // a slow disk or a sleeping drive holds up only itself, never another
    // app's open or the shell's own look at the table. The handlers take
    // the table's lock only to look an entry up, never across a read. Most
    // of the time a thread waits on the disk rather than a CPU, so even one
    // CPU gets a second thread.
    config.n_threads =
        Some(std::thread::available_parallelism().map_or(2, |cpus| cpus.get().max(2)));
    let session = fuser::spawn_mount(DocumentsFs::new(documents), at, &config)?;
    Ok(Mounted {
        _session: session,
        at: at.to_path_buf(),
    })
}

/// A mount a crashed shell left behind answers `ENOTCONN`; unmount it so
/// the folder can be mounted again.
fn clear_stale_mount(at: &Path) -> io::Result<()> {
    match std::fs::metadata(at) {
        Err(error) if error.raw_os_error() == Some(Errno::NOTCONN.raw_os_error()) => {
            let status = std::process::Command::new("fusermount3")
                .args(["-u", "-z"])
                .arg(at)
                .status()?;
            if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "fusermount3 could not remove the stale mount at {}",
                    at.display()
                )))
            }
        }
        _ => Ok(()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Node {
    Root,
    Room(String),
    App(AppKey),
    Token(AppKey, String),
    /// A path in a document: its first component is the document's name.
    Entry(AppKey, String, PathBuf),
}

struct Inode {
    node: Node,
    lookups: u64,
}

struct Open {
    ino: u64,
    file: Arc<File>,
    write: bool,
}

#[derive(Default)]
struct Table {
    inodes: HashMap<u64, Inode>,
    by_node: HashMap<Node, u64>,
    next_ino: u64,
    open: HashMap<u64, Open>,
    next_handle: u64,
}

impl Table {
    fn node(&self, ino: u64) -> io::Result<Node> {
        if ino == ROOT {
            return Ok(Node::Root);
        }
        self.inodes
            .get(&ino)
            .map(|inode| inode.node.clone())
            .ok_or_else(|| errno(Errno::STALE))
    }

    /// The inode for `node`, counted as one more kernel lookup.
    fn remember(&mut self, node: Node) -> u64 {
        if node == Node::Root {
            return ROOT;
        }
        if let Some(&ino) = self.by_node.get(&node) {
            if let Some(inode) = self.inodes.get_mut(&ino) {
                inode.lookups = inode.lookups.saturating_add(1);
            }
            return ino;
        }
        self.next_ino = self.next_ino.max(ROOT).saturating_add(1);
        let ino = self.next_ino;
        self.by_node.insert(node.clone(), ino);
        self.inodes.insert(ino, Inode { node, lookups: 1 });
        ino
    }

    fn forget(&mut self, ino: u64, count: u64) {
        let gone = self.inodes.get_mut(&ino).is_some_and(|inode| {
            inode.lookups = inode.lookups.saturating_sub(count);
            inode.lookups == 0
        });
        if gone
            && !self.open.values().any(|open| open.ino == ino)
            && let Some(inode) = self.inodes.remove(&ino)
        {
            self.by_node.remove(&inode.node);
        }
    }

    /// After a rename inside a document: what was known under `from` is
    /// now under `to`.
    fn renamed(&mut self, key: &AppKey, token: &str, from: &Path, to: &Path) {
        let moved: Vec<(u64, Node)> = self
            .inodes
            .iter()
            .filter_map(|(&ino, inode)| match &inode.node {
                Node::Entry(k, t, inner) if k == key && t == token => {
                    let rest = inner.strip_prefix(from).ok()?;
                    let path = if rest.as_os_str().is_empty() {
                        to.to_path_buf()
                    } else {
                        to.join(rest)
                    };
                    Some((ino, Node::Entry(k.clone(), t.clone(), path)))
                }
                _ => None,
            })
            .collect();
        for (ino, node) in moved {
            if let Some(inode) = self.inodes.get_mut(&ino) {
                self.by_node.remove(&inode.node);
                inode.node = node.clone();
                self.by_node.insert(node, ino);
            }
        }
    }

    /// A file just created as `node`: its inode, counted as a lookup, and
    /// its open handle.
    fn created(&mut self, node: Node, file: File, write: bool) -> (u64, u64) {
        let ino = self.remember(node);
        let fh = self.handle(Open {
            ino,
            file: Arc::new(file),
            write,
        });
        (ino, fh)
    }

    fn handle(&mut self, open: Open) -> u64 {
        self.next_handle = self.next_handle.saturating_add(1);
        self.open.insert(self.next_handle, open);
        self.next_handle
    }
}

fn errno(code: Errno) -> io::Error {
    io::Error::from_raw_os_error(code.raw_os_error())
}

fn reply_error(error: &io::Error) -> fuser::Errno {
    fuser::Errno::from_i32(error.raw_os_error().unwrap_or(Errno::IO.raw_os_error()))
}

/// The filesystem: the shared table, and the kernel's inodes and handles.
pub(crate) struct DocumentsFs {
    documents: Documents,
    table: Mutex<Table>,
    uid: u32,
    gid: u32,
}

impl DocumentsFs {
    pub(crate) fn new(documents: Documents) -> Self {
        Self {
            documents,
            table: Mutex::new(Table::default()),
            uid: rustix::process::getuid().as_raw(),
            gid: rustix::process::getgid().as_raw(),
        }
    }

    fn lock(&self) -> io::Result<MutexGuard<'_, Table>> {
        self.table.lock().map_err(|_| errno(Errno::IO))
    }

    /// What an entry node names now, while its document is still given.
    fn target(&self, key: &AppKey, token: &str, inner: &Path) -> io::Result<Target> {
        host::target(&self.documents, key, token, inner).ok_or_else(|| errno(Errno::NOENT))
    }

    fn child(&self, parent: &Node, name: &OsStr) -> io::Result<Node> {
        let text = name.to_str();
        let node = match parent {
            Node::Root => Node::Room(
                text.filter(|room| is_room(room))
                    .ok_or_else(|| errno(Errno::NOENT))?
                    .to_owned(),
            ),
            Node::Room(room) => Node::App(
                text.and_then(|app| AppKey::new(room, app).filter(|_| is_app(app)))
                    .ok_or_else(|| errno(Errno::NOENT))?,
            ),
            Node::App(key) => {
                let token = text.ok_or_else(|| errno(Errno::NOENT))?;
                self.documents
                    .document(key, token)
                    .ok_or_else(|| errno(Errno::NOENT))?;
                Node::Token(key.clone(), token.to_owned())
            }
            Node::Token(key, token) => Node::Entry(key.clone(), token.clone(), PathBuf::from(name)),
            Node::Entry(key, token, inner) => {
                Node::Entry(key.clone(), token.clone(), inner.join(name))
            }
        };
        Ok(node)
    }

    fn dir_attr(&self, ino: u64, perm: u16) -> FileAttr {
        FileAttr {
            ino: INodeNo(ino),
            size: 0,
            blocks: 0,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::Directory,
            perm,
            nlink: 2,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn entry_attr(&self, ino: u64, target: &Target) -> io::Result<FileAttr> {
        let file = File::from(target.open(OFlags::PATH, Mode::empty())?);
        self.file_attr(ino, &file, target.writable())
    }

    fn file_attr(&self, ino: u64, file: &File, writable: bool) -> io::Result<FileAttr> {
        let meta = file.metadata()?;
        let kind = file_type(meta.file_type());
        let mut perm = u16::try_from(meta.mode() & 0o777).unwrap_or(0o600);
        if !writable {
            perm &= !0o222;
        }
        Ok(FileAttr {
            ino: INodeNo(ino),
            size: meta.size(),
            blocks: meta.blocks(),
            atime: time(meta.atime(), meta.atime_nsec()),
            mtime: time(meta.mtime(), meta.mtime_nsec()),
            ctime: time(meta.ctime(), meta.ctime_nsec()),
            crtime: time(meta.ctime(), meta.ctime_nsec()),
            kind,
            perm,
            nlink: if kind == FileType::Directory { 2 } else { 1 },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        })
    }

    fn attr(&self, ino: u64, node: &Node) -> io::Result<FileAttr> {
        match node {
            Node::Root | Node::Room(_) | Node::App(_) => Ok(self.dir_attr(ino, 0o500)),
            Node::Token(key, token) => {
                let document = self
                    .documents
                    .document(key, token)
                    .ok_or_else(|| errno(Errno::NOENT))?;
                // A Save creates its file and renames its sibling here.
                let perm = if document.kind == Kind::Save {
                    0o700
                } else {
                    0o500
                };
                Ok(self.dir_attr(ino, perm))
            }
            Node::Entry(key, token, inner) => {
                let target = self.target(key, token, inner)?;
                self.entry_attr(ino, &target)
            }
        }
    }

    /// The entries of a folder node, without `.` and `..`.
    fn list(&self, node: &Node) -> io::Result<Vec<(OsString, FileType)>> {
        match node {
            // Rooms and apps are entered by name, never listed: an app sees
            // only its own view, and nothing here names another.
            Node::Root | Node::Room(_) => Ok(Vec::new()),
            Node::App(key) => Ok(self
                .documents
                .tokens(key)
                .into_iter()
                .map(|token| (OsString::from(token), FileType::Directory))
                .collect()),
            Node::Token(key, token) => {
                let document = self
                    .documents
                    .document(key, token)
                    .ok_or_else(|| errno(Errno::NOENT))?;
                let names =
                    std::iter::once(document.name().to_os_string()).chain(document.save_sibling());
                Ok(names
                    .filter_map(|name| {
                        let target = self.target(key, token, Path::new(&name)).ok()?;
                        let file = File::from(target.open(OFlags::PATH, Mode::empty()).ok()?);
                        let kind = file_type(file.metadata().ok()?.file_type());
                        Some((name, kind))
                    })
                    .collect())
            }
            Node::Entry(key, token, inner) => {
                let target = self.target(key, token, inner)?;
                let dir = target.open(OFlags::DIRECTORY | OFlags::RDONLY, Mode::empty())?;
                let mut entries = Vec::new();
                for entry in rustix::fs::Dir::read_from(&dir)? {
                    let entry = entry?;
                    let name = OsStr::from_bytes(entry.file_name().to_bytes());
                    if name == "." || name == ".." {
                        continue;
                    }
                    entries.push((name.to_os_string(), rustix_type(entry.file_type())));
                }
                Ok(entries)
            }
        }
    }

    /// The writable target for changing `name` in folder node `parent`.
    fn change(&self, parent: &Node, name: &OsStr) -> io::Result<(Node, Target)> {
        let node = self.child(parent, name)?;
        let Node::Entry(key, token, inner) = &node else {
            return Err(errno(Errno::ACCESS));
        };
        let target = self
            .target(key, token, inner)
            .map_err(|_| errno(Errno::ACCESS))?;
        if !target.writable() {
            return Err(errno(Errno::ACCESS));
        }
        // The chosen folder itself is not the app's to replace or remove.
        if matches!(target.document.kind, Kind::Folder { .. }) && target.is_root() {
            return Err(errno(Errno::ACCESS));
        }
        Ok((node, target))
    }
}

fn time(seconds: i64, nanos: i64) -> SystemTime {
    let (Ok(seconds), Ok(nanos)) = (u64::try_from(seconds), u32::try_from(nanos)) else {
        return UNIX_EPOCH;
    };
    UNIX_EPOCH
        .checked_add(Duration::new(seconds, nanos.min(999_999_999)))
        .unwrap_or(UNIX_EPOCH)
}

fn file_type(kind: std::fs::FileType) -> FileType {
    if kind.is_dir() {
        FileType::Directory
    } else if kind.is_symlink() {
        FileType::Symlink
    } else if kind.is_fifo() {
        FileType::NamedPipe
    } else if kind.is_socket() {
        FileType::Socket
    } else if kind.is_char_device() {
        FileType::CharDevice
    } else if kind.is_block_device() {
        FileType::BlockDevice
    } else {
        FileType::RegularFile
    }
}

fn rustix_type(kind: rustix::fs::FileType) -> FileType {
    match kind {
        rustix::fs::FileType::Directory => FileType::Directory,
        rustix::fs::FileType::Symlink => FileType::Symlink,
        rustix::fs::FileType::Fifo => FileType::NamedPipe,
        rustix::fs::FileType::Socket => FileType::Socket,
        rustix::fs::FileType::CharacterDevice => FileType::CharDevice,
        rustix::fs::FileType::BlockDevice => FileType::BlockDevice,
        _ => FileType::RegularFile,
    }
}

/// The open flags passed on to the host: the access mode and how writes
/// land, nothing that creates, truncates or follows.
#[allow(
    clippy::cast_sign_loss,
    reason = "open flags are a bit set the kernel hands over as a C int"
)]
fn open_flags(raw: i32) -> OFlags {
    OFlags::from_bits_retain(raw as u32)
        & (OFlags::RWMODE | OFlags::APPEND | OFlags::SYNC | OFlags::DSYNC)
}

fn wants_write(flags: OFlags) -> bool {
    !(flags & OFlags::RWMODE).is_empty()
}

impl Filesystem for DocumentsFs {
    fn lookup(&self, _: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let result = (|| {
            let parent = self.lock()?.node(parent.0)?;
            let node = self.child(&parent, name)?;
            let attr = self.attr(0, &node)?;
            let ino = self.lock()?.remember(node);
            Ok::<_, io::Error>(FileAttr {
                ino: INodeNo(ino),
                ..attr
            })
        })();
        match result {
            Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
            Err(error) => reply.error(reply_error(&error)),
        }
    }

    fn forget(&self, _: &Request, ino: INodeNo, count: u64) {
        if let Ok(mut table) = self.lock() {
            table.forget(ino.0, count);
        }
    }

    fn getattr(&self, _: &Request, ino: INodeNo, fh: Option<FileHandle>, reply: ReplyAttr) {
        let result = (|| {
            let table = self.lock()?;
            if let Some(open) = fh.and_then(|fh| table.open.get(&fh.0)) {
                return self.file_attr(ino.0, &open.file, open.write);
            }
            let node = table.node(ino.0)?;
            drop(table);
            self.attr(ino.0, &node)
        })();
        match result {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(error) => reply.error(reply_error(&error)),
        }
    }

    fn setattr(
        &self,
        _: &Request,
        ino: INodeNo,
        _mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let result = (|| {
            if uid.is_some() || gid.is_some() {
                return Err(errno(Errno::PERM));
            }
            let node = self.lock()?.node(ino.0)?;
            let Node::Entry(key, token, inner) = &node else {
                return Err(errno(Errno::ACCESS));
            };
            let target = self.target(key, token, inner)?;
            let changes = size.is_some() || atime.is_some() || mtime.is_some();
            // A mode change is answered as done and changes nothing: the
            // app's copy of a person's file keeps its permissions.
            if changes {
                if !target.writable() {
                    return Err(errno(Errno::ACCESS));
                }
                let file = File::from(target.open(OFlags::WRONLY, Mode::empty())?);
                if let Some(size) = size {
                    file.set_len(size)?;
                }
                let to_time = |time: fuser::TimeOrNow| match time {
                    fuser::TimeOrNow::SpecificTime(time) => time,
                    fuser::TimeOrNow::Now => SystemTime::now(),
                };
                let mut times = std::fs::FileTimes::new();
                if let Some(atime) = atime {
                    times = times.set_accessed(to_time(atime));
                }
                if let Some(mtime) = mtime {
                    times = times.set_modified(to_time(mtime));
                }
                if atime.is_some() || mtime.is_some() {
                    file.set_times(times)?;
                }
            }
            self.entry_attr(ino.0, &target)
        })();
        match result {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(error) => reply.error(reply_error(&error)),
        }
    }

    fn open(&self, _: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let result = (|| {
            let flags = open_flags(flags.0);
            let node = self.lock()?.node(ino.0)?;
            let Node::Entry(key, token, inner) = &node else {
                return Err(errno(Errno::ISDIR));
            };
            let target = self.target(key, token, inner)?;
            let write = wants_write(flags);
            if write && !target.writable() {
                return Err(errno(Errno::ACCESS));
            }
            let file = File::from(target.open(flags, Mode::empty())?);
            Ok(self.lock()?.handle(Open {
                ino: ino.0,
                file: Arc::new(file),
                write,
            }))
        })();
        match result {
            Ok(fh) => reply.opened(FileHandle(fh), FopenFlags::empty()),
            Err(error) => reply.error(reply_error(&error)),
        }
    }

    fn read(
        &self,
        _: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _: OpenFlags,
        _: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let result = (|| {
            let file = self
                .lock()?
                .open
                .get(&fh.0)
                .map(|open| Arc::clone(&open.file))
                .ok_or_else(|| errno(Errno::BADF))?;
            let mut buffer = vec![0; usize::try_from(size).unwrap_or(usize::MAX)];
            let mut filled = 0;
            while filled < buffer.len() {
                let at = offset.saturating_add(u64::try_from(filled).unwrap_or(u64::MAX));
                let rest = buffer.get_mut(filled..).ok_or_else(|| errno(Errno::IO))?;
                match file.read_at(rest, at) {
                    Ok(0) => break,
                    Ok(count) => filled = filled.saturating_add(count),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
            buffer.truncate(filled);
            Ok(buffer)
        })();
        match result {
            Ok(bytes) => reply.data(&bytes),
            Err(error) => reply.error(reply_error(&error)),
        }
    }

    fn write(
        &self,
        _: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _: WriteFlags,
        _: OpenFlags,
        _: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        let result = (|| {
            let file = self
                .lock()?
                .open
                .get(&fh.0)
                .filter(|open| open.write)
                .map(|open| Arc::clone(&open.file))
                .ok_or_else(|| errno(Errno::BADF))?;
            file.write_all_at(data, offset)?;
            u32::try_from(data.len()).map_err(|_| errno(Errno::FBIG))
        })();
        match result {
            Ok(count) => reply.written(count),
            Err(error) => reply.error(reply_error(&error)),
        }
    }

    fn flush(&self, _: &Request, _: INodeNo, _: FileHandle, _: LockOwner, reply: ReplyEmpty) {
        reply.ok();
    }

    fn fsync(&self, _: &Request, _: INodeNo, fh: FileHandle, datasync: bool, reply: ReplyEmpty) {
        let result = (|| {
            let file = self
                .lock()?
                .open
                .get(&fh.0)
                .map(|open| Arc::clone(&open.file))
                .ok_or_else(|| errno(Errno::BADF))?;
            if datasync {
                file.sync_data()
            } else {
                file.sync_all()
            }
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(reply_error(&error)),
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
        if let Ok(mut table) = self.lock() {
            table.open.remove(&fh.0);
            table.forget(ino.0, 0);
        }
        reply.ok();
    }

    fn readdir(
        &self,
        _: &Request,
        ino: INodeNo,
        _: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let result = (|| {
            let node = self.lock()?.node(ino.0)?;
            let mut entries = vec![
                (OsString::from("."), FileType::Directory),
                (OsString::from(".."), FileType::Directory),
            ];
            entries.extend(self.list(&node)?);
            let skip = usize::try_from(offset).unwrap_or(usize::MAX);
            for (index, (name, kind)) in entries.into_iter().enumerate().skip(skip) {
                // Not an inode the kernel holds: a lookup gives the real one.
                let entry_ino = u64::try_from(index)
                    .unwrap_or(u64::MAX)
                    .saturating_add(1 << 62);
                let next = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
                if reply.add(INodeNo(entry_ino), next, kind, &name) {
                    break;
                }
            }
            Ok::<_, io::Error>(())
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(reply_error(&error)),
        }
    }

    fn create(
        &self,
        _: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let result = (|| {
            let parent = self.lock()?.node(parent.0)?;
            let (node, target) = self.change(&parent, name)?;
            #[allow(
                clippy::cast_sign_loss,
                reason = "open flags are a bit set the kernel hands over as a C int"
            )]
            let exclusive = OFlags::from_bits_retain(flags as u32) & OFlags::EXCL;
            let open = open_flags(flags) | OFlags::CREATE | exclusive;
            let file = File::from(target.open(open, Mode::from_raw_mode(mode & !umask & 0o666))?);
            let write = wants_write(open);
            let attr = self.file_attr(0, &file, true)?;
            let (ino, fh) = self.lock()?.created(node, file, write);
            Ok::<_, io::Error>((
                FileAttr {
                    ino: INodeNo(ino),
                    ..attr
                },
                fh,
            ))
        })();
        match result {
            Ok((attr, fh)) => reply.created(
                &TTL,
                &attr,
                Generation(0),
                FileHandle(fh),
                FopenFlags::empty(),
            ),
            Err(error) => reply.error(reply_error(&error)),
        }
    }

    fn mkdir(
        &self,
        _: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let result = (|| {
            let parent = self.lock()?.node(parent.0)?;
            let (node, target) = self.change(&parent, name)?;
            if !matches!(target.document.kind, Kind::Folder { write: true }) {
                return Err(errno(Errno::ACCESS));
            }
            let (dir, entry) = target.open_parent()?.ok_or_else(|| errno(Errno::ACCESS))?;
            rustix::fs::mkdirat(&dir, &entry, Mode::from_raw_mode(mode & !umask & 0o777))?;
            let attr = self.entry_attr(0, &target)?;
            let ino = self.lock()?.remember(node);
            Ok::<_, io::Error>(FileAttr {
                ino: INodeNo(ino),
                ..attr
            })
        })();
        match result {
            Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
            Err(error) => reply.error(reply_error(&error)),
        }
    }

    fn unlink(&self, _: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let result = self.remove(parent, name, AtFlags::empty());
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(reply_error(&error)),
        }
    }

    fn rmdir(&self, _: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let result = self.remove(parent, name, AtFlags::REMOVEDIR);
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(reply_error(&error)),
        }
    }

    fn rename(
        &self,
        _: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let result = (|| {
            let (from_parent, to_parent) = {
                let table = self.lock()?;
                (table.node(parent.0)?, table.node(newparent.0)?)
            };
            let (from_node, from) = self.change(&from_parent, name)?;
            let (to_node, to) = self.change(&to_parent, newname)?;
            let (Node::Entry(key, token, from_inner), Node::Entry(to_key, to_token, to_inner)) =
                (&from_node, &to_node)
            else {
                return Err(errno(Errno::ACCESS));
            };
            if key != to_key || token != to_token {
                // Nothing moves between documents: each is its own choice.
                return Err(errno(Errno::XDEV));
            }
            if from.document.kind == Kind::Save && (from.is_root() || !to.is_root()) {
                // A Save replaces its file with its sibling, and that is all.
                return Err(errno(Errno::ACCESS));
            }
            let (from_dir, from_name) = from.open_parent()?.ok_or_else(|| errno(Errno::ACCESS))?;
            let (to_dir, to_name) = to.open_parent()?.ok_or_else(|| errno(Errno::ACCESS))?;
            rustix::fs::renameat_with(
                &from_dir,
                &from_name,
                &to_dir,
                &to_name,
                rustix::fs::RenameFlags::from_bits_retain(flags.bits()),
            )?;
            self.lock()?.renamed(key, token, from_inner, to_inner);
            Ok::<_, io::Error>(())
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(reply_error(&error)),
        }
    }
}

impl DocumentsFs {
    fn remove(&self, parent: INodeNo, name: &OsStr, flags: AtFlags) -> io::Result<()> {
        let parent = self.lock()?.node(parent.0)?;
        let (_, target) = self.change(&parent, name)?;
        if target.document.kind == Kind::Save && target.is_root() {
            // A Save may write its file, not remove it; its sibling goes.
            return Err(errno(Errno::ACCESS));
        }
        let (dir, entry) = target.open_parent()?.ok_or_else(|| errno(Errno::ACCESS))?;
        rustix::fs::unlinkat(&dir, &entry, flags)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    /// The mount, when this machine can mount FUSE; a missing `/dev/fuse`
    /// skips, unless `TILO_REQUIRE_FUSE` says this lane has it.
    fn mounted(documents: &Documents) -> Option<(tempfile::TempDir, Mounted)> {
        let at = tempfile::tempdir().unwrap();
        match mount(documents.clone(), &at.path().join("documents")) {
            Ok(mounted) => Some((at, mounted)),
            Err(error) if std::env::var_os("TILO_REQUIRE_FUSE").is_none() => {
                eprintln!("skipping: FUSE is unavailable here ({error})");
                None
            }
            Err(error) => panic!("TILO_REQUIRE_FUSE is set and mounting failed: {error}"),
        }
    }

    fn in_view(mounted: &Mounted, key: &AppKey, sandbox: &Path) -> PathBuf {
        let rest = sandbox.strip_prefix(crate::SANDBOX_DIR).unwrap();
        crate::app_view(mounted.path(), key).join(rest)
    }

    #[test]
    fn choosing_a_file_in_the_picker_gives_the_app_that_file_through_the_mount() {
        let documents = Documents::new();
        let Some((_dir, mounted)) = mounted(&documents) else {
            return;
        };
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("report.pdf"), b"%PDF").unwrap();
        std::fs::write(home.path().join("secret.txt"), b"secret").unwrap();
        let key = AppKey::new("room-1", "so.tilo.pdf-viewer").unwrap();
        let view = crate::app_view(mounted.path(), &key);
        assert_eq!(
            std::fs::read_dir(&view).unwrap().count(),
            0,
            "nothing before a choice"
        );

        let given = documents
            .grant(&key, &home.path().join("report.pdf"), Kind::Read)
            .unwrap();
        let path = in_view(&mounted, &key, &given);
        assert_eq!(std::fs::read(&path).unwrap(), b"%PDF");
        assert!(
            std::fs::write(&path, b"changed").is_err(),
            "an Open is read-only"
        );
        assert!(
            std::fs::read(path.with_file_name("secret.txt")).is_err(),
            "nothing beside the chosen file"
        );
        let token_dir = path.parent().unwrap();
        let listed: Vec<_> = std::fs::read_dir(token_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(listed, vec![OsString::from("report.pdf")]);

        let other = AppKey::new("room-1", "so.tilo.files").unwrap();
        assert!(
            std::fs::read(in_view(&mounted, &other, &given)).is_err(),
            "another app's view does not have it"
        );
        documents.revoke(&key);
        assert!(std::fs::read(&path).is_err(), "taken back at once");
    }

    #[test]
    fn a_save_to_a_new_file_writes_its_sibling_and_renames_it_over() {
        let documents = Documents::new();
        let Some((_dir, mounted)) = mounted(&documents) else {
            return;
        };
        let home = tempfile::tempdir().unwrap();
        let key = AppKey::new("room-1", "so.tilo.logs").unwrap();
        let given = documents
            .grant(&key, &home.path().join("export.log"), Kind::Save)
            .unwrap();
        let path = in_view(&mounted, &key, &given);
        let sibling = path.with_file_name(".export.log.tilo-save");
        let mut file = std::fs::File::create(&sibling).unwrap();
        file.write_all(b"log lines").unwrap();
        file.sync_all().unwrap();
        drop(file);
        std::fs::rename(&sibling, &path).unwrap();
        assert_eq!(
            std::fs::read(home.path().join("export.log")).unwrap(),
            b"log lines"
        );
        assert!(!home.path().join(".export.log.tilo-save").exists());
        assert!(std::fs::write(path.with_file_name("other.log"), b"x").is_err());
        assert!(
            std::fs::remove_file(&path).is_err(),
            "a Save does not remove"
        );
    }

    #[test]
    fn the_mount_serves_on_more_than_one_thread() {
        let documents = Documents::new();
        let Some((_dir, _mounted)) = mounted(&documents) else {
            return;
        };
        // The serving threads are named `fuser-0`, `fuser-1`, …; a second
        // one is there only when the mount serves on more than one. They
        // start beside the mount, not before it returns.
        let second = || {
            std::fs::read_dir("/proc/self/task")
                .unwrap()
                .flatten()
                .filter_map(|task| std::fs::read_to_string(task.path().join("comm")).ok())
                .any(|name| name.trim_end() == "fuser-1")
        };
        let mut serving = second();
        for _ in 0..100 {
            if serving {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            serving = second();
        }
        assert!(
            serving,
            "one slow read must not hold up every app's documents"
        );
    }

    #[test]
    fn a_folder_choice_gives_its_subtree_and_nothing_above_it() {
        let documents = Documents::new();
        let Some((_dir, mounted)) = mounted(&documents) else {
            return;
        };
        let home = tempfile::tempdir().unwrap();
        let folder = home.path().join("Backups");
        std::fs::create_dir_all(folder.join("2026")).unwrap();
        std::fs::write(folder.join("2026/a.tar"), b"a").unwrap();
        std::fs::write(home.path().join("secret.txt"), b"secret").unwrap();
        let key = AppKey::new("room-1", "so.tilo.backups").unwrap();
        let given = documents
            .grant(&key, &folder, Kind::Folder { write: true })
            .unwrap();
        let path = in_view(&mounted, &key, &given);
        let mut bytes = Vec::new();
        std::fs::File::open(path.join("2026/a.tar"))
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"a");
        std::fs::write(path.join("2026/b.tar"), b"b").unwrap();
        assert_eq!(std::fs::read(folder.join("2026/b.tar")).unwrap(), b"b");
        std::fs::create_dir(path.join("2027")).unwrap();
        assert!(folder.join("2027").is_dir());
        assert!(std::fs::read(path.join("../secret.txt")).is_err());
        assert!(
            std::fs::remove_dir(&path).is_err(),
            "the chosen folder stays"
        );
    }
}
