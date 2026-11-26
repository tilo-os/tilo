//! The host side of a hosted VM's shared directories ([`files`]): Firecracker has
//! no virtio-fs, so the guest agent mounts each shared directory as FUSE and asks
//! this server for each operation over vsock. Every path is resolved with
//! `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)` from the share's root and
//! never follows a final symbolic link; metadata changes go through the opened
//! file, never through a path that could be swapped in between.
//!
//! Once the guest asks what changed ([`FsRequest::Changes`]), every directory
//! it was handed is watched (inotify), so its kernel can keep what it was told
//! until something here changes it: a member's edit the workspace's sync
//! writes, another program on the host, or the guest itself.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::FileExt as _;
use std::path::{Path, PathBuf};

use linux_vm_protocol::files::{
    Attr, CHANGE_OVERHEAD, Change, DirEntry, FileKind, FsReply, FsRequest, MAX_CHANGE_BYTES,
    MAX_ENTRIES, MAX_IO_BYTES, ROOT_NODE, SetTime, StatFs, Time, valid_component,
};
use linux_vm_protocol::{recv, send};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::fs::{
    AtFlags, FileType, Mode, OFlags, RenameFlags, ResolveFlags, Stat, Timespec, Timestamps,
    UTIME_NOW, UTIME_OMIT,
};

const RESOLVE: ResolveFlags = ResolveFlags::BENEATH.union(ResolveFlags::NO_MAGICLINKS);

/// The most files one connection keeps open at once.
const MAX_HANDLES: usize = 1024;

/// Nodes from here on are this server's own, for files whose inode cannot be
/// their node: one on another filesystem mounted beneath the share.
const OWN_NODES: u64 = 1 << 63;

/// What makes a watched directory's entries, or a file in it, change.
const WATCHED: WatchFlags = WatchFlags::CREATE
    .union(WatchFlags::DELETE)
    .union(WatchFlags::MOVED_FROM)
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::MODIFY)
    .union(WatchFlags::ATTRIB)
    .union(WatchFlags::CLOSE_WRITE)
    .union(WatchFlags::DELETE_SELF)
    .union(WatchFlags::MOVE_SELF)
    .union(WatchFlags::ONLYDIR);

/// One directory the server may serve.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Served {
    pub path: PathBuf,
    pub read_only: bool,
}

/// Serve one guest connection: its hello names the share, then requests
/// follow until the guest closes it.
pub fn serve<S: Read + Write>(mut stream: S, shares: &[Served]) -> io::Result<()> {
    let Some(FsRequest::Hello { path }) = recv(&mut stream).map_err(io::Error::other)? else {
        return Ok(());
    };
    let Some(share) = shares.iter().find(|share| share.path == Path::new(&path)) else {
        send(&mut stream, &FsReply::Error(libc::EACCES)).map_err(io::Error::other)?;
        return Ok(());
    };
    let mut server = match ShareServer::open(share) {
        Ok(server) => server,
        Err(error) => {
            send(&mut stream, &FsReply::Error(errno(&error))).map_err(io::Error::other)?;
            return Ok(());
        }
    };
    send(&mut stream, &FsReply::Ok).map_err(io::Error::other)?;
    while let Some(request) = recv::<_, FsRequest>(&mut stream).map_err(io::Error::other)? {
        let reply = server.handle(request);
        send(&mut stream, &reply).map_err(io::Error::other)?;
    }
    Ok(())
}

fn errno(error: &io::Error) -> i32 {
    error.raw_os_error().unwrap_or(libc::EIO)
}

fn from_rustix(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

/// One mounted share, as one connection sees it.
pub struct ShareServer {
    root: OwnedFd,
    /// The share's filesystem: a file on it has its inode as its node.
    device: u64,
    read_only: bool,
    nodes: HashMap<u64, PathBuf>,
    by_path: HashMap<PathBuf, u64>,
    /// How many lookups the kernel holds of each node, and so how many it
    /// may still drop: a path's lookup, a create, a link. The node goes
    /// when the last one is dropped, never before.
    lookups: HashMap<u64, u64>,
    next_node: u64,
    handles: HashMap<u64, File>,
    next_handle: u64,
    /// The last directory listed, so paging through it sees one order.
    listing: Option<(u64, Vec<DirEntry>)>,
    /// The watch on every directory handed out, once the guest asked what
    /// changed.
    watch: Option<Watch>,
}

struct Watch {
    inotify: OwnedFd,
    /// Each watched directory's node, by its watch.
    nodes: HashMap<i32, u64>,
    watches: HashMap<u64, i32>,
    /// A directory went unwatched (the host's watch limit): from then on
    /// every ask gives up all the guest holds.
    blind: bool,
    /// What changed and was not yet told.
    pending: BTreeSet<Change>,
}

impl ShareServer {
    pub fn open(share: &Served) -> io::Result<Self> {
        let root = rustix::fs::open(
            &share.path,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(from_rustix)?;
        let device = rustix::fs::fstat(&root).map_err(from_rustix)?.st_dev;
        let mut nodes = HashMap::new();
        nodes.insert(ROOT_NODE, PathBuf::new());
        let mut by_path = HashMap::new();
        by_path.insert(PathBuf::new(), ROOT_NODE);
        Ok(Self {
            root,
            device,
            read_only: share.read_only,
            nodes,
            by_path,
            lookups: HashMap::new(),
            next_node: OWN_NODES,
            handles: HashMap::new(),
            next_handle: 1,
            listing: None,
            watch: None,
        })
    }

    pub fn handle(&mut self, request: FsRequest) -> FsReply {
        self.dispatch(request)
            .unwrap_or_else(|error| FsReply::Error(errno(&error)))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per request of the protocol, each a few lines"
    )]
    fn dispatch(&mut self, request: FsRequest) -> io::Result<FsReply> {
        if self.read_only && changes(&request) {
            return Err(io::Error::from_raw_os_error(libc::EROFS));
        }
        Ok(match request {
            FsRequest::Hello { .. } => return Err(io::Error::from_raw_os_error(libc::EPROTO)),
            FsRequest::Lookup { parent, name } => {
                let path = self.child(parent, &name)?;
                FsReply::Attr(self.introduce(&path)?)
            }
            FsRequest::Forget { node, count } => {
                self.forget(node, count);
                FsReply::Ok
            }
            FsRequest::GetAttr { node } => {
                let path = self.path(node)?;
                FsReply::Attr(self.attr_of(&path)?)
            }
            FsRequest::SetAttr {
                node,
                mode,
                size,
                atime,
                mtime,
            } => {
                let path = self.path(node)?;
                self.set_attr(&path, mode, size, atime, mtime)?;
                FsReply::Attr(self.attr_of(&path)?)
            }
            FsRequest::ReadDir { node, offset } => self.read_dir(node, offset)?,
            FsRequest::Open {
                node,
                write,
                truncate,
            } => {
                let path = self.path(node)?;
                let mut flags = if write { OFlags::RDWR } else { OFlags::RDONLY };
                if truncate && write {
                    flags |= OFlags::TRUNC;
                }
                let file = self.open_file(&path, flags, Mode::empty())?;
                FsReply::Opened {
                    handle: self.keep(file)?,
                }
            }
            FsRequest::Read {
                handle,
                offset,
                size,
            } => {
                let file = self.handles.get(&handle).ok_or_else(bad_handle)?;
                let mut buffer = vec![0_u8; size.min(MAX_IO_BYTES) as usize];
                let read = file.read_at(&mut buffer, offset)?;
                buffer.truncate(read);
                FsReply::Data(buffer)
            }
            FsRequest::Write {
                handle,
                offset,
                data,
            } => {
                let file = self.handles.get(&handle).ok_or_else(bad_handle)?;
                file.write_all_at(&data, offset)?;
                FsReply::Written(u32::try_from(data.len()).unwrap_or(u32::MAX))
            }
            FsRequest::Release { handle } => {
                self.handles.remove(&handle);
                FsReply::Ok
            }
            FsRequest::Fsync { handle } => {
                self.handles
                    .get(&handle)
                    .ok_or_else(bad_handle)?
                    .sync_data()?;
                FsReply::Ok
            }
            FsRequest::Create {
                parent,
                name,
                mode,
                exclusive,
                truncate,
            } => {
                let path = self.child(parent, &name)?;
                let mut flags = OFlags::RDWR | OFlags::CREATE;
                if exclusive {
                    flags |= OFlags::EXCL;
                }
                if truncate {
                    flags |= OFlags::TRUNC;
                }
                let file = self.open_file(&path, flags, Mode::from_raw_mode(mode & 0o7777))?;
                let attr = self.introduce(&path)?;
                FsReply::Created {
                    attr,
                    handle: self.keep(file)?,
                }
            }
            FsRequest::MkDir { parent, name, mode } => {
                let (dir, name) = self.parent_dir(parent, &name)?;
                rustix::fs::mkdirat(&dir, name, Mode::from_raw_mode(mode & 0o7777))
                    .map_err(from_rustix)?;
                FsReply::Attr(self.introduce(&self.child(parent, name.as_bytes())?)?)
            }
            FsRequest::Unlink { parent, name } => {
                let (dir, name) = self.parent_dir(parent, &name)?;
                rustix::fs::unlinkat(&dir, name, AtFlags::empty()).map_err(from_rustix)?;
                FsReply::Ok
            }
            FsRequest::RmDir { parent, name } => {
                let (dir, name) = self.parent_dir(parent, &name)?;
                rustix::fs::unlinkat(&dir, name, AtFlags::REMOVEDIR).map_err(from_rustix)?;
                FsReply::Ok
            }
            FsRequest::Rename {
                parent,
                name,
                new_parent,
                new_name,
                no_replace,
            } => {
                let (from_dir, from) = self.parent_dir(parent, &name)?;
                let (to_dir, to) = self.parent_dir(new_parent, &new_name)?;
                let flags = if no_replace {
                    RenameFlags::NOREPLACE
                } else {
                    RenameFlags::empty()
                };
                rustix::fs::renameat_with(&from_dir, from, &to_dir, to, flags)
                    .map_err(from_rustix)?;
                self.moved(
                    &self.child(parent, &name)?,
                    &self.child(new_parent, &new_name)?,
                );
                FsReply::Ok
            }
            FsRequest::Symlink {
                parent,
                name,
                target,
            } => {
                if target.is_empty() || target.contains(&0) {
                    return Err(io::Error::from_raw_os_error(libc::EINVAL));
                }
                let (dir, link) = self.parent_dir(parent, &name)?;
                rustix::fs::symlinkat(OsStr::from_bytes(&target), &dir, link)
                    .map_err(from_rustix)?;
                FsReply::Attr(self.introduce(&self.child(parent, &name)?)?)
            }
            FsRequest::ReadLink { node } => {
                let path = self.path(node)?;
                let (dir, name) = self.split(&path)?;
                let target = rustix::fs::readlinkat(&dir, name, Vec::new()).map_err(from_rustix)?;
                FsReply::Target(target.into_bytes())
            }
            FsRequest::Link {
                node,
                new_parent,
                new_name,
            } => {
                let path = self.path(node)?;
                let (from_dir, from) = self.split(&path)?;
                let (to_dir, to) = self.parent_dir(new_parent, &new_name)?;
                rustix::fs::linkat(&from_dir, from, &to_dir, to, AtFlags::empty())
                    .map_err(from_rustix)?;
                FsReply::Attr(self.introduce(&self.child(new_parent, &new_name)?)?)
            }
            FsRequest::StatFs => {
                let vfs = rustix::fs::fstatvfs(&self.root).map_err(from_rustix)?;
                FsReply::StatFs(StatFs {
                    blocks: vfs.f_blocks,
                    bfree: vfs.f_bfree,
                    bavail: vfs.f_bavail,
                    files: vfs.f_files,
                    ffree: vfs.f_ffree,
                    bsize: u32::try_from(vfs.f_bsize).unwrap_or(4096),
                    namelen: u32::try_from(vfs.f_namemax).unwrap_or(255),
                    frsize: u32::try_from(vfs.f_frsize).unwrap_or(4096),
                })
            }
            FsRequest::Changes => {
                let (changes, more) = self.changes()?;
                FsReply::Changes { changes, more }
            }
        })
    }

    fn path(&self, node: u64) -> io::Result<PathBuf> {
        self.nodes
            .get(&node)
            .cloned()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ESTALE))
    }

    fn child(&self, parent: u64, name: &[u8]) -> io::Result<PathBuf> {
        if !valid_component(name) {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        Ok(self.path(parent)?.join(OsStr::from_bytes(name)))
    }

    /// The node naming the file at `path`: its inode, so that the guest sees
    /// one number for one file across lookups and mounts, as git's index
    /// needs to trust what it recorded. A path that names a file again
    /// (replaced, or a hard link's other name) names that file's node.
    fn intern(&mut self, path: &Path, stat: &Stat) -> u64 {
        if path.as_os_str().is_empty() {
            return ROOT_NODE;
        }
        let inode = stat.st_ino;
        let own_inode = (ROOT_NODE + 1..OWN_NODES).contains(&inode);
        let node = if stat.st_dev == self.device && own_inode {
            inode
        } else if let Some(node) = self
            .by_path
            .get(path)
            .copied()
            .filter(|node| *node >= OWN_NODES)
        {
            node
        } else {
            let node = self.next_node;
            self.next_node = self.next_node.saturating_add(1);
            node
        };
        if let Some(old) = self.nodes.insert(node, path.to_path_buf())
            && old != path
            && self.by_path.get(&old) == Some(&node)
        {
            self.by_path.remove(&old);
        }
        self.by_path.insert(path.to_path_buf(), node);
        node
    }

    /// The node for `path`, as an entry reply hands it to the kernel: the
    /// kernel now holds one lookup of it, which it drops with a
    /// [`FsRequest::Forget`].
    fn introduce(&mut self, path: &Path) -> io::Result<Attr> {
        let attr = self.attr_of(path)?;
        let lookups = self.lookups.entry(attr.node).or_insert(0);
        *lookups = lookups.saturating_add(1);
        if attr.kind == FileKind::Directory {
            self.watch_dir(attr.node, path);
        }
        Ok(attr)
    }

    /// Watch the directory `node` at `path`, once something watches.
    fn watch_dir(&mut self, node: u64, path: &Path) {
        let Some(watch) = &self.watch else {
            return;
        };
        if watch.watches.contains_key(&node) {
            return;
        }
        // Through the directory opened beneath the root, never its path:
        // nothing swapped in under the path is watched instead.
        let watched = self
            .open_beneath(path, OFlags::PATH | OFlags::DIRECTORY, Mode::empty())
            .and_then(|fd| {
                let by_fd = format!("/proc/self/fd/{}", rustix::fd::AsRawFd::as_raw_fd(&fd));
                inotify::add_watch(&watch.inotify, by_fd, WATCHED).map_err(from_rustix)
            });
        let Some(watch) = &mut self.watch else {
            return;
        };
        match watched {
            Ok(wd) => {
                watch.nodes.insert(wd, node);
                watch.watches.insert(node, wd);
            }
            // Gone already: its parent's watch says so.
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
            Err(_) => watch.blind = true,
        }
    }

    /// What changed beneath the share since the last ask, as much as one
    /// reply holds and whether more waits; the first ask starts watching
    /// every directory handed out so far.
    fn changes(&mut self) -> io::Result<(Vec<Change>, bool)> {
        let Some(watch) = &self.watch else {
            let inotify =
                inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC).map_err(from_rustix)?;
            self.watch = Some(Watch {
                inotify,
                nodes: HashMap::new(),
                watches: HashMap::new(),
                blind: false,
                pending: BTreeSet::new(),
            });
            let known: Vec<(u64, PathBuf)> = self
                .nodes
                .iter()
                .map(|(node, path)| (*node, path.clone()))
                .collect();
            for (node, path) in known {
                if node == ROOT_NODE
                    || self
                        .attr_of(&path)
                        .is_ok_and(|attr| attr.kind == FileKind::Directory)
                {
                    self.watch_dir(node, &path);
                }
            }
            return Ok((Vec::new(), false));
        };
        let mut events = Vec::new();
        let mut buffer = [MaybeUninit::<u8>::uninit(); 16 * 1024];
        let mut reader = inotify::Reader::new(&watch.inotify, &mut buffer);
        loop {
            match reader.next() {
                Ok(event) => events.push((
                    event.wd(),
                    event.events(),
                    event.file_name().map(|name| name.to_bytes().to_vec()),
                )),
                Err(rustix::io::Errno::AGAIN) => break,
                Err(rustix::io::Errno::INTR) => {}
                Err(error) => return Err(from_rustix(error)),
            }
        }
        let mut changed = BTreeSet::new();
        let mut everything = watch.blind;
        for (wd, flags, name) in events {
            let Some(watch) = &mut self.watch else {
                break;
            };
            if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                // Events were lost: give up all of it, this once.
                everything = true;
                continue;
            }
            let Some(&dir) = watch.nodes.get(&wd) else {
                continue;
            };
            if flags.contains(ReadFlags::IGNORED) {
                watch.nodes.remove(&wd);
                watch.watches.remove(&dir);
            }
            changed.insert(Change::Node(dir));
            let (Some(name), Some(path)) = (name, self.nodes.get(&dir)) else {
                continue;
            };
            if let Some(child) = self.by_path.get(&path.join(OsStr::from_bytes(&name))) {
                changed.insert(Change::Node(*child));
            }
            changed.insert(Change::Entry { parent: dir, name });
        }
        if everything {
            // What the guest holds is what it was handed: every node, and the
            // name each was handed by.
            for (node, path) in &self.nodes {
                changed.insert(Change::Node(*node));
                if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
                    && let Some(parent) = self.by_path.get(parent)
                {
                    changed.insert(Change::Entry {
                        parent: *parent,
                        name: name.as_bytes().to_vec(),
                    });
                }
            }
        }
        let Some(watch) = &mut self.watch else {
            return Ok((Vec::new(), false));
        };
        watch.pending.append(&mut changed);
        let mut told = Vec::new();
        let mut bytes = 0_usize;
        while let Some(change) = watch.pending.first() {
            let cost = CHANGE_OVERHEAD.saturating_add(match change {
                Change::Entry { name, .. } => name.len(),
                Change::Node(_) => 0,
            });
            if bytes.saturating_add(cost) > MAX_CHANGE_BYTES {
                break;
            }
            bytes = bytes.saturating_add(cost);
            told.extend(watch.pending.pop_first());
        }
        Ok((told, !watch.pending.is_empty()))
    }

    /// Drop `count` of the lookups the kernel held of `node`. A replaced
    /// file's node keeps its old path while it is still open, so removing a
    /// path entry here only removes the one that still names this node; the
    /// path may already name the file that replaced it.
    fn forget(&mut self, node: u64, count: u64) {
        if node == ROOT_NODE {
            return;
        }
        let Some(lookups) = self.lookups.get_mut(&node) else {
            return;
        };
        *lookups = lookups.saturating_sub(count);
        if *lookups > 0 {
            return;
        }
        self.lookups.remove(&node);
        if let Some(watch) = &mut self.watch
            && let Some(wd) = watch.watches.remove(&node)
        {
            watch.nodes.remove(&wd);
            let _ = inotify::remove_watch(&watch.inotify, wd);
        }
        let Some(path) = self.nodes.remove(&node) else {
            return;
        };
        if self.by_path.get(&path) == Some(&node) {
            self.by_path.remove(&path);
        }
    }

    /// A rename moves the nodes under the old path with it.
    fn moved(&mut self, from: &Path, to: &Path) {
        let moved: Vec<(u64, PathBuf)> = self
            .nodes
            .iter()
            .filter_map(|(node, path)| {
                let rest = path.strip_prefix(from).ok()?;
                // `join("")` would add a trailing `/`.
                let moved = if rest.as_os_str().is_empty() {
                    to.to_path_buf()
                } else {
                    to.join(rest)
                };
                Some((*node, moved))
            })
            .collect();
        for (node, path) in moved {
            if let Some(old) = self.nodes.insert(node, path.clone()) {
                self.by_path.remove(&old);
            }
            self.by_path.insert(path, node);
        }
    }

    fn open_beneath(&self, path: &Path, flags: OFlags, mode: Mode) -> io::Result<OwnedFd> {
        let relative = if path.as_os_str().is_empty() {
            Path::new(".")
        } else {
            path
        };
        rustix::fs::openat2(
            &self.root,
            relative,
            flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            mode,
            RESOLVE,
        )
        .map_err(from_rustix)
    }

    fn open_file(&self, path: &Path, flags: OFlags, mode: Mode) -> io::Result<File> {
        let fd = self.open_beneath(path, flags, mode)?;
        let file = File::from(fd);
        if !file.metadata()?.is_file() {
            return Err(io::Error::from_raw_os_error(libc::EISDIR));
        }
        Ok(file)
    }

    fn keep(&mut self, file: File) -> io::Result<u64> {
        if self.handles.len() >= MAX_HANDLES {
            return Err(io::Error::from_raw_os_error(libc::EMFILE));
        }
        let handle = self.next_handle;
        self.next_handle = self.next_handle.saturating_add(1);
        self.handles.insert(handle, file);
        Ok(handle)
    }

    fn parent_dir<'a>(&self, parent: u64, name: &'a [u8]) -> io::Result<(OwnedFd, &'a OsStr)> {
        if !valid_component(name) {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        let dir = self.open_beneath(
            &self.path(parent)?,
            OFlags::PATH | OFlags::DIRECTORY,
            Mode::empty(),
        )?;
        Ok((dir, OsStr::from_bytes(name)))
    }

    fn split<'a>(&self, path: &'a Path) -> io::Result<(OwnedFd, &'a OsStr)> {
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        let dir = self.open_beneath(parent, OFlags::PATH | OFlags::DIRECTORY, Mode::empty())?;
        Ok((dir, name))
    }

    fn attr_of(&mut self, path: &Path) -> io::Result<Attr> {
        let fd = self.open_beneath(path, OFlags::PATH, Mode::empty())?;
        let stat = rustix::fs::fstat(&fd).map_err(from_rustix)?;
        let node = self.intern(path, &stat);
        Ok(attr(node, &stat))
    }

    fn set_attr(
        &self,
        path: &Path,
        mode: Option<u32>,
        size: Option<u64>,
        atime: Option<SetTime>,
        mtime: Option<SetTime>,
    ) -> io::Result<()> {
        let fd = self.open_beneath(path, OFlags::PATH, Mode::empty())?;
        let stat = rustix::fs::fstat(&fd).map_err(from_rustix)?;
        let symlink = FileType::from_raw_mode(stat.st_mode) == FileType::Symlink;
        // The opened file itself, by its descriptor: nothing can be swapped
        // in under the path between the check and the change.
        let by_fd = PathBuf::from(format!(
            "/proc/self/fd/{}",
            rustix::fd::AsRawFd::as_raw_fd(&fd)
        ));
        if let Some(mode) = mode {
            if symlink {
                return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP));
            }
            rustix::fs::chmod(&by_fd, Mode::from_raw_mode(mode & 0o7777)).map_err(from_rustix)?;
        }
        if let Some(size) = size {
            let file = self.open_file(path, OFlags::WRONLY, Mode::empty())?;
            file.set_len(size)?;
        }
        if atime.is_some() || mtime.is_some() {
            if symlink {
                return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP));
            }
            let times = Timestamps {
                last_access: timespec(atime),
                last_modification: timespec(mtime),
            };
            rustix::fs::utimensat(rustix::fs::CWD, &by_fd, &times, AtFlags::empty())
                .map_err(from_rustix)?;
        }
        Ok(())
    }

    fn read_dir(&mut self, node: u64, offset: u64) -> io::Result<FsReply> {
        if offset == 0
            || self
                .listing
                .as_ref()
                .is_none_or(|(listed, _)| *listed != node)
        {
            let path = self.path(node)?;
            let fd = self.open_beneath(&path, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())?;
            let mut entries = Vec::new();
            for entry in rustix::fs::Dir::read_from(fd.as_fd()).map_err(from_rustix)? {
                let entry = entry.map_err(from_rustix)?;
                let name = entry.file_name().to_bytes();
                if name == b"." || name == b".." {
                    continue;
                }
                entries.push(DirEntry {
                    name: name.to_vec(),
                    kind: kind(entry.file_type()),
                });
            }
            self.listing = Some((node, entries));
        }
        let Some((_, entries)) = &self.listing else {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        };
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(entries.len());
        let end = start.saturating_add(MAX_ENTRIES).min(entries.len());
        Ok(FsReply::Entries {
            entries: entries.get(start..end).unwrap_or_default().to_vec(),
            done: end == entries.len(),
        })
    }
}

fn bad_handle() -> io::Error {
    io::Error::from_raw_os_error(libc::EBADF)
}

const fn changes(request: &FsRequest) -> bool {
    match request {
        FsRequest::Open { write, .. } => *write,
        FsRequest::SetAttr { .. }
        | FsRequest::Write { .. }
        | FsRequest::Create { .. }
        | FsRequest::MkDir { .. }
        | FsRequest::Unlink { .. }
        | FsRequest::RmDir { .. }
        | FsRequest::Rename { .. }
        | FsRequest::Symlink { .. }
        | FsRequest::Link { .. } => true,
        FsRequest::Hello { .. }
        | FsRequest::Lookup { .. }
        | FsRequest::Forget { .. }
        | FsRequest::GetAttr { .. }
        | FsRequest::ReadDir { .. }
        | FsRequest::Read { .. }
        | FsRequest::Release { .. }
        | FsRequest::Fsync { .. }
        | FsRequest::ReadLink { .. }
        | FsRequest::StatFs
        | FsRequest::Changes => false,
    }
}

fn timespec(time: Option<SetTime>) -> Timespec {
    match time {
        None => Timespec {
            tv_sec: 0,
            tv_nsec: UTIME_OMIT,
        },
        Some(SetTime::Now) => Timespec {
            tv_sec: 0,
            tv_nsec: UTIME_NOW,
        },
        Some(SetTime::At(at)) => Timespec {
            tv_sec: at.secs,
            tv_nsec: i64::from(at.nanos),
        },
    }
}

fn kind(file_type: FileType) -> FileKind {
    match file_type {
        FileType::RegularFile => FileKind::File,
        FileType::Directory => FileKind::Directory,
        FileType::Symlink => FileKind::Symlink,
        _ => FileKind::Other,
    }
}

#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "stat's fields are the kernel's; sizes and counts are never negative"
)]
#[allow(
    clippy::unnecessary_cast,
    reason = "a stat's link count is u32 on aarch64 and u64 on x86_64"
)]
fn attr(node: u64, stat: &Stat) -> Attr {
    let time = |secs: i64, nanos: i64| Time {
        secs,
        nanos: nanos.clamp(0, 999_999_999) as u32,
    };
    Attr {
        node,
        kind: kind(FileType::from_raw_mode(stat.st_mode)),
        perm: (stat.st_mode & 0o7777) as u16,
        size: stat.st_size as u64,
        blocks: stat.st_blocks as u64,
        nlink: stat.st_nlink as u32,
        atime: time(
            stat.st_atime,
            i64::try_from(stat.st_atime_nsec).unwrap_or(0),
        ),
        mtime: time(
            stat.st_mtime,
            i64::try_from(stat.st_mtime_nsec).unwrap_or(0),
        ),
        ctime: time(
            stat.st_ctime,
            i64::try_from(stat.st_ctime_nsec).unwrap_or(0),
        ),
    }
}
