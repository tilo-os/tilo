//! The host side of a hosted VM's shared directories ([`files`]).
//!
//! Firecracker has no virtio-fs, so the guest agent mounts every shared
//! directory as FUSE and asks this server for each operation over vsock.
//! The server runs as its own process per VM (`tilo-legacy hosted
//! files-serve`), started through `tilo-legacy confine-fs`, so Landlock
//! holds it to the VM's shares exactly as it holds `virtiofsd`.
//!
//! Inside that, every path is resolved with `openat2(RESOLVE_BENEATH |
//! RESOLVE_NO_MAGICLINKS)` from the share's root and never follows a
//! final symbolic link: a guest that plants a link to `/etc` reaches the
//! link, not `/etc`. Metadata changes go through the opened file
//! (`/proc/self/fd`), never through a path that could be swapped in
//! between. A read-only share refuses every change with `EROFS`.
//!
//! [`files`]: legacy_agent_protocol::files

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::FileExt as _;
use std::path::{Path, PathBuf};

use legacy_agent_protocol::files::{
    Attr, DirEntry, FileKind, FsReply, FsRequest, MAX_ENTRIES, MAX_IO_BYTES, ROOT_NODE, SetTime,
    StatFs, Time, valid_component,
};
use legacy_agent_protocol::{recv, send};
use rustix::fs::{
    AtFlags, FileType, Mode, OFlags, RenameFlags, ResolveFlags, Stat, Timespec, Timestamps,
    UTIME_NOW, UTIME_OMIT,
};

const RESOLVE: ResolveFlags = ResolveFlags::BENEATH.union(ResolveFlags::NO_MAGICLINKS);

/// The most files one connection keeps open at once.
const MAX_HANDLES: usize = 1024;

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
    read_only: bool,
    nodes: HashMap<u64, PathBuf>,
    by_path: HashMap<PathBuf, u64>,
    next_node: u64,
    handles: HashMap<u64, File>,
    next_handle: u64,
    /// The last directory listed, so paging through it sees one order.
    listing: Option<(u64, Vec<DirEntry>)>,
}

impl ShareServer {
    pub fn open(share: &Served) -> io::Result<Self> {
        let root = rustix::fs::open(
            &share.path,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(from_rustix)?;
        let mut nodes = HashMap::new();
        nodes.insert(ROOT_NODE, PathBuf::new());
        let mut by_path = HashMap::new();
        by_path.insert(PathBuf::new(), ROOT_NODE);
        Ok(Self {
            root,
            read_only: share.read_only,
            nodes,
            by_path,
            next_node: ROOT_NODE + 1,
            handles: HashMap::new(),
            next_handle: 1,
            listing: None,
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
                FsReply::Attr(self.attr_of(&path)?)
            }
            FsRequest::Forget { node } => {
                if node != ROOT_NODE
                    && let Some(path) = self.nodes.remove(&node)
                {
                    self.by_path.remove(&path);
                }
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
                let attr = self.attr_of(&path)?;
                FsReply::Created {
                    attr,
                    handle: self.keep(file)?,
                }
            }
            FsRequest::MkDir { parent, name, mode } => {
                let (dir, name) = self.parent_dir(parent, &name)?;
                rustix::fs::mkdirat(&dir, name, Mode::from_raw_mode(mode & 0o7777))
                    .map_err(from_rustix)?;
                FsReply::Attr(self.attr_of(&self.child(parent, name.as_bytes())?)?)
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
                FsReply::Attr(self.attr_of(&self.child(parent, &name)?)?)
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
                FsReply::Attr(self.attr_of(&self.child(new_parent, &new_name)?)?)
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

    fn intern(&mut self, path: &Path) -> u64 {
        if let Some(node) = self.by_path.get(path) {
            return *node;
        }
        let node = self.next_node;
        self.next_node = self.next_node.saturating_add(1);
        self.nodes.insert(node, path.to_path_buf());
        self.by_path.insert(path.to_path_buf(), node);
        node
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

    /// `path` opened beneath the root, its final link not followed.
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

    /// The directory `parent` names, opened, and `name` checked as one
    /// component in it.
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

    /// The directory holding `path`, opened, and its last component.
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
        let node = self.intern(path);
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

/// Whether `request` would change the share.
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
        | FsRequest::StatFs => false,
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

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;

    use legacy_agent_protocol::files::FsRequest as R;

    use super::*;

    struct Guest(UnixStream);

    impl Guest {
        #[allow(clippy::needless_pass_by_value, reason = "requests are built in place")]
        fn ask(&mut self, request: R) -> FsReply {
            send(&mut self.0, &request).unwrap();
            recv(&mut self.0).unwrap().unwrap()
        }

        fn lookup(&mut self, parent: u64, name: &str) -> FsReply {
            self.ask(R::Lookup {
                parent,
                name: name.as_bytes().to_vec(),
            })
        }

        fn node(&mut self, parent: u64, name: &str) -> u64 {
            match self.lookup(parent, name) {
                FsReply::Attr(attr) => attr.node,
                other => panic!("lookup {name}: {other:?}"),
            }
        }
    }

    fn connect(shares: Vec<Served>, path: &Path) -> (Guest, std::thread::JoinHandle<()>) {
        let (host, guest) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || serve(host, &shares).unwrap());
        let mut guest = Guest(guest);
        let reply = guest.ask(R::Hello {
            path: path.display().to_string(),
        });
        assert_eq!(reply, FsReply::Ok);
        (guest, server)
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one scenario, phase by phase")]
    fn a_share_is_served_beneath_its_root_only() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"host only").unwrap();
        let share = tempfile::tempdir().unwrap();
        let root = share.path().to_path_buf();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), b"fn main() {}\n").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), root.join("escape")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("out")).unwrap();
        let shares = vec![Served {
            path: root.clone(),
            read_only: false,
        }];
        // phase: a share the server was not given is refused at hello
        {
            let (host, guest) = UnixStream::pair().unwrap();
            let given = shares.clone();
            let server = std::thread::spawn(move || serve(host, &given).unwrap());
            let mut guest = Guest(guest);
            assert_eq!(
                guest.ask(R::Hello {
                    path: outside.path().display().to_string()
                }),
                FsReply::Error(libc::EACCES)
            );
            server.join().unwrap();
        }
        let (mut guest, server) = connect(shares, &root);
        // phase: lookups, reads and listings stay inside the share
        {
            let src = guest.node(ROOT_NODE, "src");
            let main = guest.node(src, "main.rs");
            let FsReply::Opened { handle } = guest.ask(R::Open {
                node: main,
                write: false,
                truncate: false,
            }) else {
                panic!("open");
            };
            assert_eq!(
                guest.ask(R::Read {
                    handle,
                    offset: 3,
                    size: 4
                }),
                FsReply::Data(b"main".to_vec())
            );
            let FsReply::Entries { entries, done } = guest.ask(R::ReadDir {
                node: ROOT_NODE,
                offset: 0,
            }) else {
                panic!("readdir");
            };
            assert!(done);
            let mut names: Vec<_> = entries
                .iter()
                .map(|e| (String::from_utf8(e.name.clone()).unwrap(), e.kind))
                .collect();
            names.sort();
            assert_eq!(
                names,
                [
                    ("escape".to_string(), FileKind::Symlink),
                    ("out".to_string(), FileKind::Symlink),
                    ("src".to_string(), FileKind::Directory)
                ]
            );
            for name in ["..", ".", "a/b", ""] {
                assert_eq!(guest.lookup(ROOT_NODE, name), FsReply::Error(libc::EINVAL));
            }
        }
        // phase: links out of the share lead nowhere
        {
            let escape = guest.node(ROOT_NODE, "escape");
            let FsReply::Attr(attr) = guest.ask(R::GetAttr { node: escape }) else {
                panic!("getattr");
            };
            assert_eq!(attr.kind, FileKind::Symlink, "the link, not its target");
            assert_eq!(
                guest.ask(R::Open {
                    node: escape,
                    write: false,
                    truncate: false
                }),
                FsReply::Error(libc::ELOOP)
            );
            assert_eq!(
                guest.ask(R::SetAttr {
                    node: escape,
                    mode: Some(0o777),
                    size: None,
                    atime: None,
                    mtime: None
                }),
                FsReply::Error(libc::EOPNOTSUPP)
            );
            let out = guest.node(ROOT_NODE, "out");
            assert_eq!(
                guest.lookup(out, "secret"),
                FsReply::Error(libc::EXDEV),
                "a path through a link out is refused"
            );
            assert_eq!(
                guest.ask(R::Create {
                    parent: out,
                    name: b"planted".to_vec(),
                    mode: 0o644,
                    exclusive: true,
                    truncate: false
                }),
                FsReply::Error(libc::EXDEV)
            );
            assert!(!outside.path().join("planted").exists());
            assert_eq!(
                std::fs::read(outside.path().join("secret")).unwrap(),
                b"host only"
            );
        }
        // phase: writes, renames and removals land in the share
        {
            let FsReply::Created { attr, handle } = guest.ask(R::Create {
                parent: ROOT_NODE,
                name: b"out.txt".to_vec(),
                mode: 0o640,
                exclusive: true,
                truncate: false,
            }) else {
                panic!("create");
            };
            assert_eq!(attr.perm, 0o640);
            assert_eq!(
                guest.ask(R::Write {
                    handle,
                    offset: 0,
                    data: b"built".to_vec()
                }),
                FsReply::Written(5)
            );
            assert_eq!(guest.ask(R::Release { handle }), FsReply::Ok);
            let FsReply::Attr(dir) = guest.ask(R::MkDir {
                parent: ROOT_NODE,
                name: b"target".to_vec(),
                mode: 0o755,
            }) else {
                panic!("mkdir");
            };
            assert_eq!(
                guest.ask(R::Rename {
                    parent: ROOT_NODE,
                    name: b"out.txt".to_vec(),
                    new_parent: dir.node,
                    new_name: b"done.txt".to_vec(),
                    no_replace: true
                }),
                FsReply::Ok
            );
            assert_eq!(
                std::fs::read(root.join("target/done.txt")).unwrap(),
                b"built"
            );
            let done = guest.node(dir.node, "done.txt");
            let reply = guest.ask(R::SetAttr {
                node: done,
                mode: Some(0o600),
                size: Some(2),
                atime: None,
                mtime: Some(SetTime::At(Time {
                    secs: 1_000_000,
                    nanos: 0,
                })),
            });
            let FsReply::Attr(truncated) = reply else {
                panic!("setattr: {reply:?}");
            };
            assert_eq!(
                (truncated.size, truncated.perm, truncated.mtime.secs),
                (2, 0o600, 1_000_000)
            );
            assert_eq!(
                guest.ask(R::Unlink {
                    parent: dir.node,
                    name: b"done.txt".to_vec()
                }),
                FsReply::Ok
            );
            assert_eq!(
                guest.ask(R::RmDir {
                    parent: ROOT_NODE,
                    name: b"target".to_vec()
                }),
                FsReply::Ok
            );
            assert!(!root.join("target").exists());
        }
        drop(guest);
        server.join().unwrap();
        // phase: a read-only share refuses every change
        {
            let read_only = vec![Served {
                path: root.clone(),
                read_only: true,
            }];
            let (mut guest, server) = connect(read_only, &root);
            assert_eq!(
                guest.ask(R::Create {
                    parent: ROOT_NODE,
                    name: b"x".to_vec(),
                    mode: 0o644,
                    exclusive: true,
                    truncate: false
                }),
                FsReply::Error(libc::EROFS)
            );
            let src = guest.node(ROOT_NODE, "src");
            let main = guest.node(src, "main.rs");
            assert_eq!(
                guest.ask(R::Open {
                    node: main,
                    write: true,
                    truncate: true
                }),
                FsReply::Error(libc::EROFS)
            );
            assert!(matches!(
                guest.ask(R::Open {
                    node: main,
                    write: false,
                    truncate: false
                }),
                FsReply::Opened { .. }
            ));
            drop(guest);
            server.join().unwrap();
            assert_eq!(
                std::fs::read(root.join("src/main.rs")).unwrap(),
                b"fn main() {}\n"
            );
        }
    }
}
