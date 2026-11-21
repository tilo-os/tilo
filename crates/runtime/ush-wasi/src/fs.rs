//! A program's files: WASI's descriptors over the shell's host.
//!
//! Two directories are preopened, the program's working directory as `.` and the root as `/`,
//! so relative and absolute paths both resolve, and both are the host's view, under the
//! environment's grants. A file opened for reading is read whole when it is opened. A file
//! written is kept in memory while it is open and handed to [`Host::open_write`] when it is
//! closed: one `write` of its contents (or one `append` of what was appended), which is one
//! change of the run's. A file opened for writing and never written changes nothing.

use std::collections::BTreeMap;
use std::io::{Read as _, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use ush_shell::{FileCall, Host, Kind, Metadata, Run, Stdio, WriteMode};
use wasmi::{Caller, Linker};

use crate::{
    EACCES, EBADF, EEXIST, EINVAL, EIO, EISDIR, ENOENT, ENOTDIR, ENOTEMPTY, ENOTSUP, ESPIPE,
    SUCCESS, State, WASI, errno, path_of, write_bytes, write_u32, write_u64,
};

const FILETYPE_UNKNOWN: u8 = 0;
const FILETYPE_CHARACTER_DEVICE: u8 = 2;
const FILETYPE_DIRECTORY: u8 = 3;
const FILETYPE_REGULAR_FILE: u8 = 4;
const FILETYPE_SYMBOLIC_LINK: u8 = 7;

const OFLAGS_CREAT: i32 = 1;
const OFLAGS_DIRECTORY: i32 = 2;
const OFLAGS_EXCL: i32 = 4;
const OFLAGS_TRUNC: i32 = 8;

const FDFLAGS_APPEND: i32 = 1;

const RIGHTS_FD_READ: i64 = 1 << 1;
const RIGHTS_FD_WRITE: i64 = 1 << 6;

const LOOKUP_SYMLINK_FOLLOW: i32 = 1;

/// One open descriptor of the program's.
pub(crate) enum Fd {
    Dir {
        path: PathBuf,
        /// The name it was preopened under, for a preopened one.
        preopened: Option<String>,
    },
    File(Open),
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "how WASI opened it: for reading, writing, appending, and whether it changed"
)]
pub(crate) struct Open {
    path: PathBuf,
    data: Vec<u8>,
    pos: u64,
    read: bool,
    write: bool,
    /// Only appended to: `data` holds what was appended.
    append: bool,
    mode: WriteMode,
    /// What it is written through when it closes: taken at its first change, which is when
    /// the grants are asked.
    sink: Option<Box<dyn Write + Send>>,
    changed: bool,
}

pub(crate) struct Table {
    fds: BTreeMap<i32, Fd>,
}

impl Table {
    pub(crate) fn new(cwd: &Path) -> Self {
        let mut fds = BTreeMap::new();
        fds.insert(
            3,
            Fd::Dir {
                path: cwd.to_path_buf(),
                preopened: Some(".".to_owned()),
            },
        );
        fds.insert(
            4,
            Fd::Dir {
                path: PathBuf::from("/"),
                preopened: Some("/".to_owned()),
            },
        );
        Self { fds }
    }

    fn insert(&mut self, fd: Fd) -> i32 {
        let next = (5..i32::MAX)
            .find(|n| !self.fds.contains_key(n))
            .unwrap_or(i32::MAX);
        self.fds.insert(next, fd);
        next
    }

    /// The path `path` names under directory `dir`, `..` taken lexically and never above `/`.
    fn resolve(&self, dir: i32, path: &str) -> Result<PathBuf, i32> {
        let base = match self.fds.get(&dir) {
            Some(Fd::Dir { path, .. }) => path.clone(),
            Some(Fd::File(_)) => return Err(ENOTDIR),
            None => return Err(EBADF),
        };
        let joined = if path.starts_with('/') {
            PathBuf::from(path)
        } else {
            base.join(path)
        };
        let mut out = PathBuf::from("/");
        for component in joined.components() {
            match component {
                Component::Normal(part) => out.push(part),
                Component::ParentDir => {
                    out.pop();
                }
                Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
            }
        }
        Ok(out)
    }

    pub(crate) fn write(
        &mut self,
        fd: i32,
        bytes: &[u8],
        at: Option<i64>,
        host: &Arc<dyn Host>,
        run: Option<&Run>,
    ) -> Result<usize, i32> {
        let open = self.file(fd)?;
        if !open.write {
            return Err(EBADF);
        }
        open.ensure_sink(host, run)?;
        if open.append {
            open.data.extend_from_slice(bytes);
        } else {
            let start = match at {
                Some(at) => u64::try_from(at).map_err(|_| EINVAL)?,
                None => open.pos,
            };
            let start = usize::try_from(start).map_err(|_| EINVAL)?;
            let end = start.checked_add(bytes.len()).ok_or(EINVAL)?;
            if open.data.len() < end {
                open.data.resize(end, 0);
            }
            open.data
                .get_mut(start..end)
                .ok_or(EINVAL)?
                .copy_from_slice(bytes);
            if at.is_none() {
                open.pos = end as u64;
            }
        }
        open.changed = true;
        Ok(bytes.len())
    }

    pub(crate) fn read(&mut self, fd: i32, into: &mut [u8], at: Option<u64>) -> Result<usize, i32> {
        let open = match self.fds.get_mut(&fd) {
            Some(Fd::File(open)) => open,
            Some(Fd::Dir { .. }) => return Err(EISDIR),
            None => return Err(EBADF),
        };
        if !open.read {
            return Err(EBADF);
        }
        let from = usize::try_from(at.unwrap_or(open.pos)).map_err(|_| EINVAL)?;
        let available = open.data.get(from..).unwrap_or_default();
        let count = available.len().min(into.len());
        into.get_mut(..count)
            .ok_or(EINVAL)?
            .copy_from_slice(available.get(..count).ok_or(EINVAL)?);
        if at.is_none() {
            open.pos = u64::try_from(from.saturating_add(count)).unwrap_or(u64::MAX);
        }
        Ok(count)
    }

    fn file(&mut self, fd: i32) -> Result<&mut Open, i32> {
        match self.fds.get_mut(&fd) {
            Some(Fd::File(open)) => Ok(open),
            Some(Fd::Dir { .. }) => Err(EISDIR),
            None => Err(EBADF),
        }
    }

    /// Close `fd`: a file it changed is written, as one change of the run's.
    fn close(&mut self, fd: i32, host: &Arc<dyn Host>, run: Option<&Run>) -> Result<(), i32> {
        match self.fds.remove(&fd) {
            Some(Fd::File(open)) => open.finish(host, run),
            Some(Fd::Dir { .. }) => Ok(()),
            None => Err(EBADF),
        }
    }

    /// Close every descriptor the program left open.
    pub(crate) fn close_all(&mut self, host: &Arc<dyn Host>, run: Option<&Run>) {
        let open: Vec<i32> = self.fds.keys().copied().collect();
        for fd in open {
            let _ = self.close(fd, host, run);
        }
    }
}

impl Open {
    fn ensure_sink(&mut self, host: &Arc<dyn Host>, run: Option<&Run>) -> Result<(), i32> {
        if self.sink.is_none() {
            self.sink = Some(
                host.open_write(&self.path, self.mode, run)
                    .map_err(|error| io_errno(&error))?,
            );
        }
        Ok(())
    }

    fn finish(mut self, host: &Arc<dyn Host>, run: Option<&Run>) -> Result<(), i32> {
        if !self.changed {
            return Ok(());
        }
        self.ensure_sink(host, run)?;
        let mut sink = self.sink.take().ok_or(EIO)?;
        sink.write_all(&self.data).map_err(|_| EIO)?;
        // Dropping the sink is what makes it the environment's action.
        drop(sink);
        Ok(())
    }
}

fn io_errno(error: &std::io::Error) -> i32 {
    match error.kind() {
        std::io::ErrorKind::NotFound => ENOENT,
        std::io::ErrorKind::PermissionDenied => EACCES,
        std::io::ErrorKind::AlreadyExists => EEXIST,
        std::io::ErrorKind::Unsupported => ENOTSUP,
        _ => EIO,
    }
}

fn filetype(kind: Kind) -> u8 {
    match kind {
        Kind::File => FILETYPE_REGULAR_FILE,
        Kind::Dir => FILETYPE_DIRECTORY,
        Kind::Symlink => FILETYPE_SYMBOLIC_LINK,
        Kind::Other => FILETYPE_UNKNOWN,
    }
}

/// A `filestat`, as preview 1 lays it out: device, inode, type, links, size and three times.
fn filestat(meta: &Metadata, size: Option<u64>) -> Vec<u8> {
    let time = u64::try_from(meta.modified)
        .unwrap_or(0)
        .saturating_mul(1_000_000_000);
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&meta.dev.to_le_bytes());
    out.extend_from_slice(&meta.ino.to_le_bytes());
    out.extend_from_slice(&[filetype(meta.kind), 0, 0, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&meta.nlink.max(1).to_le_bytes());
    out.extend_from_slice(&size.unwrap_or(meta.len).to_le_bytes());
    for _ in 0..3 {
        out.extend_from_slice(&time.to_le_bytes());
    }
    out
}

/// The `filestat` of one of the program's streams: a character device and nothing more.
fn stream_filestat() -> Vec<u8> {
    let mut out = vec![0_u8; 16];
    out.push(FILETYPE_CHARACTER_DEVICE);
    out.resize(64, 0);
    out
}

fn host_of(c: &Caller<'_, State>) -> (Arc<dyn Host>, Option<Run>) {
    (Arc::clone(&c.data().wasi.host), c.data().wasi.run.clone())
}

/// One of the environment's file verbs, as the shell's own command would run it.
fn verb(c: &Caller<'_, State>, name: &str, args: &[String]) -> Result<(), i32> {
    let (host, run) = host_of(c);
    let io = Stdio::null();
    let call = FileCall {
        name,
        args,
        cwd: Path::new("/"),
        io: &io,
        command: run.as_ref(),
    };
    match host.file_action(&call) {
        0 => Ok(()),
        _ => Err(EACCES),
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// How `path_open` was asked to open a path: WASI's flags for it.
#[derive(Clone, Copy)]
struct Opening {
    dirflags: i32,
    oflags: i32,
    rights: i64,
    fdflags: i32,
}

fn path_open(
    c: &mut Caller<'_, State>,
    dir: i32,
    path: (i32, i32),
    opening: Opening,
    opened: i32,
) -> Result<(), i32> {
    let Opening {
        dirflags,
        oflags,
        rights,
        fdflags,
    } = opening;
    let name = path_of(c, path.0, path.1)?;
    let full = c.data().wasi.fds.resolve(dir, &name)?;
    let (host, run) = host_of(c);
    let follow = dirflags & LOOKUP_SYMLINK_FOLLOW != 0;
    let meta = match if follow {
        host.metadata(&full)
    } else {
        host.symlink_metadata(&full)
    } {
        Ok(meta) => Some(meta),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(io_errno(&error)),
    };
    let create = oflags & OFLAGS_CREAT != 0;
    let exclusive = oflags & OFLAGS_EXCL != 0;
    let truncate = oflags & OFLAGS_TRUNC != 0;
    let append = fdflags & FDFLAGS_APPEND != 0;
    let write = rights & RIGHTS_FD_WRITE != 0 || truncate || append;
    let read = rights & RIGHTS_FD_READ != 0;
    let fd = match &meta {
        Some(meta) if meta.is_dir() => {
            if write {
                return Err(EISDIR);
            }
            Fd::Dir {
                path: full,
                preopened: None,
            }
        }
        _ if oflags & OFLAGS_DIRECTORY != 0 => {
            return Err(if meta.is_some() { ENOTDIR } else { ENOENT });
        }
        Some(_) if create && exclusive => return Err(EEXIST),
        None if !create => return Err(ENOENT),
        existing => {
            let exists = existing.is_some();
            // Appending alone keeps what was appended; appending and reading rewrites the
            // whole file, as it was read and as it ends.
            let only_appended = append && !read;
            let mut data = Vec::new();
            if exists && !truncate && !only_appended {
                host.open_read(&full)
                    .map_err(|error| io_errno(&error))?
                    .read_to_end(&mut data)
                    .map_err(|_| EIO)?;
            }
            let mode = if only_appended {
                WriteMode::Append
            } else if exclusive {
                WriteMode::CreateNew
            } else {
                WriteMode::Truncate
            };
            let pos = if append { data.len() as u64 } else { 0 };
            let mut open = Open {
                path: full,
                data,
                pos,
                read,
                write,
                append: only_appended,
                mode,
                sink: None,
                changed: false,
            };
            // A file made, or emptied, is a change already, asked of the grants now.
            if !exists || truncate {
                open.ensure_sink(&host, run.as_ref())?;
                open.changed = true;
            }
            Fd::File(open)
        }
    };
    let fd = c.data_mut().wasi.fds.insert(fd);
    write_u32(c, opened, u32::try_from(fd).map_err(|_| EBADF)?)
}

fn fd_readdir(
    c: &mut Caller<'_, State>,
    fd: i32,
    buf: i32,
    len: i32,
    cookie: i64,
    used: i32,
) -> Result<(), i32> {
    let path = match c.data().wasi.fds.fds.get(&fd) {
        Some(Fd::Dir { path, .. }) => path.clone(),
        Some(Fd::File(_)) => return Err(ENOTDIR),
        None => return Err(EBADF),
    };
    let (host, _) = host_of(c);
    let mut entries: Vec<(String, u8)> = vec![
        (".".to_owned(), FILETYPE_DIRECTORY),
        ("..".to_owned(), FILETYPE_DIRECTORY),
    ];
    let mut listed = host.read_dir(&path).map_err(|error| io_errno(&error))?;
    listed.sort_by(|a, b| a.name.cmp(&b.name));
    entries.extend(
        listed
            .into_iter()
            .map(|entry| (entry.name, filetype(entry.kind))),
    );
    let mut out = Vec::new();
    let skip = usize::try_from(cookie).map_err(|_| EINVAL)?;
    for (index, (name, kind)) in entries.iter().enumerate().skip(skip) {
        // The cookie of the entry after it, an inode, the name's length and the type.
        let next = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
        out.extend_from_slice(&next.to_le_bytes());
        out.extend_from_slice(&next.to_le_bytes());
        out.extend_from_slice(&u32::try_from(name.len()).map_err(|_| EINVAL)?.to_le_bytes());
        out.extend_from_slice(&[*kind, 0, 0, 0]);
        out.extend_from_slice(name.as_bytes());
        if out.len() >= usize::try_from(len).map_err(|_| EINVAL)? {
            break;
        }
    }
    let capacity = usize::try_from(len).map_err(|_| EINVAL)?;
    out.truncate(capacity);
    write_bytes(c, buf, &out)?;
    write_u32(c, used, u32::try_from(out.len()).map_err(|_| EINVAL)?)
}

fn fd_filestat(c: &mut Caller<'_, State>, fd: i32, at: i32) -> Result<(), i32> {
    let (host, _) = host_of(c);
    let stat = match c.data().wasi.fds.fds.get(&fd) {
        None if (0..=2).contains(&fd) => stream_filestat(),
        None => return Err(EBADF),
        Some(Fd::Dir { path, .. }) => filestat(
            &host.metadata(path).map_err(|error| io_errno(&error))?,
            None,
        ),
        Some(Fd::File(open)) => {
            let size = (open.read || open.changed).then_some(open.data.len() as u64);
            match host.metadata(&open.path) {
                Ok(meta) => filestat(&meta, size),
                Err(_) => filestat(
                    &Metadata {
                        kind: Kind::File,
                        len: 0,
                        mode: 0o644,
                        modified: 0,
                        uid: 0,
                        gid: 0,
                        nlink: 1,
                        dev: 0,
                        ino: 0,
                    },
                    size,
                ),
            }
        }
    };
    write_bytes(c, at, &stat)
}

fn fd_fdstat(c: &mut Caller<'_, State>, fd: i32, at: i32) -> Result<(), i32> {
    let (kind, flags) = match c.data().wasi.fds.fds.get(&fd) {
        None if (0..=2).contains(&fd) => (FILETYPE_CHARACTER_DEVICE, 0_u16),
        None => return Err(EBADF),
        Some(Fd::Dir { .. }) => (FILETYPE_DIRECTORY, 0),
        Some(Fd::File(open)) => (FILETYPE_REGULAR_FILE, u16::from(open.append)),
    };
    // Its type, its flags, and every right, to read and to give.
    let mut out = vec![kind, 0];
    out.extend_from_slice(&flags.to_le_bytes());
    out.resize(8, 0);
    out.extend_from_slice(&u64::MAX.to_le_bytes());
    out.extend_from_slice(&u64::MAX.to_le_bytes());
    write_bytes(c, at, &out)
}

fn fd_seek(
    c: &mut Caller<'_, State>,
    fd: i32,
    offset: i64,
    whence: i32,
    at: i32,
) -> Result<(), i32> {
    if (0..=2).contains(&fd) && !c.data().wasi.fds.fds.contains_key(&fd) {
        return Err(ESPIPE);
    }
    let open = c.data_mut().wasi.fds.file(fd)?;
    let base = match whence {
        0 => 0_i64,
        1 => i64::try_from(open.pos).map_err(|_| EINVAL)?,
        2 => i64::try_from(open.data.len()).map_err(|_| EINVAL)?,
        _ => return Err(EINVAL),
    };
    let pos = u64::try_from(base.checked_add(offset).ok_or(EINVAL)?).map_err(|_| EINVAL)?;
    open.pos = pos;
    write_u64(c, at, pos)
}

fn path_filestat(
    c: &mut Caller<'_, State>,
    dir: i32,
    flags: i32,
    path: (i32, i32),
    at: i32,
) -> Result<(), i32> {
    let name = path_of(c, path.0, path.1)?;
    let full = c.data().wasi.fds.resolve(dir, &name)?;
    let (host, _) = host_of(c);
    let meta = if flags & LOOKUP_SYMLINK_FOLLOW != 0 {
        host.metadata(&full)
    } else {
        host.symlink_metadata(&full)
    }
    .map_err(|error| io_errno(&error))?;
    write_bytes(c, at, &filestat(&meta, None))
}

fn remove(c: &Caller<'_, State>, dir: i32, path: (i32, i32), directory: bool) -> Result<(), i32> {
    let name = path_of(c, path.0, path.1)?;
    let full = c.data().wasi.fds.resolve(dir, &name)?;
    let (host, _) = host_of(c);
    let meta = host
        .symlink_metadata(&full)
        .map_err(|error| io_errno(&error))?;
    match (directory, meta.is_dir()) {
        (true, false) => return Err(ENOTDIR),
        (false, true) => return Err(EISDIR),
        (true, true)
            if !host
                .read_dir(&full)
                .map_err(|error| io_errno(&error))?
                .is_empty() =>
        {
            return Err(ENOTEMPTY);
        }
        _ => {}
    }
    verb(c, if directory { "rmdir" } else { "rm" }, &[text(&full)])
}

fn rename(c: &Caller<'_, State>, from: (i32, i32, i32), to: (i32, i32, i32)) -> Result<(), i32> {
    let old = path_of(c, from.1, from.2)?;
    let new = path_of(c, to.1, to.2)?;
    let old = c.data().wasi.fds.resolve(from.0, &old)?;
    let new = c.data().wasi.fds.resolve(to.0, &new)?;
    let (host, _) = host_of(c);
    host.symlink_metadata(&old)
        .map_err(|error| io_errno(&error))?;
    // `mv` into an existing directory would move into it; WASI's rename replaces, and a
    // directory is replaced only by an empty one, which this does not do.
    if host.metadata(&new).is_ok_and(|meta| meta.is_dir()) {
        return Err(ENOTEMPTY);
    }
    verb(c, "mv", &[text(&old), text(&new)])
}

#[allow(
    clippy::too_many_lines,
    reason = "one entry per call of WASI preview 1's files, in its order"
)]
pub(crate) fn link(linker: &mut Linker<State>) -> Result<(), wasmi::Error> {
    linker.func_wrap(
        WASI,
        "fd_advise",
        |_: Caller<'_, State>, _: i32, _: i64, _: i64, _: i32| SUCCESS,
    )?;
    linker.func_wrap(
        WASI,
        "fd_allocate",
        |_: Caller<'_, State>, _: i32, _: i64, _: i64| ENOTSUP,
    )?;
    linker.func_wrap(WASI, "fd_close", |c: Caller<'_, State>, fd: i32| {
        let (host, run) = host_of(&c);
        let mut c = c;
        errno(c.data_mut().wasi.fds.close(fd, &host, run.as_ref()))
    })?;
    linker.func_wrap(WASI, "fd_datasync", |_: Caller<'_, State>, _: i32| SUCCESS)?;
    linker.func_wrap(WASI, "fd_sync", |_: Caller<'_, State>, _: i32| SUCCESS)?;
    linker.func_wrap(
        WASI,
        "fd_fdstat_get",
        |mut c: Caller<'_, State>, fd: i32, at: i32| errno(fd_fdstat(&mut c, fd, at)),
    )?;
    linker.func_wrap(
        WASI,
        "fd_fdstat_set_flags",
        |_: Caller<'_, State>, _: i32, flags: i32| {
            if flags == 0 { SUCCESS } else { ENOTSUP }
        },
    )?;
    linker.func_wrap(
        WASI,
        "fd_fdstat_set_rights",
        |_: Caller<'_, State>, _: i32, _: i64, _: i64| SUCCESS,
    )?;
    linker.func_wrap(
        WASI,
        "fd_filestat_get",
        |mut c: Caller<'_, State>, fd: i32, at: i32| errno(fd_filestat(&mut c, fd, at)),
    )?;
    linker.func_wrap(
        WASI,
        "fd_filestat_set_size",
        |mut c: Caller<'_, State>, fd: i32, size: i64| {
            let (host, run) = host_of(&c);
            errno((|| {
                let open = c.data_mut().wasi.fds.file(fd)?;
                if !open.write || open.append {
                    return Err(EBADF);
                }
                open.ensure_sink(&host, run.as_ref())?;
                open.data
                    .resize(usize::try_from(size).map_err(|_| EINVAL)?, 0);
                open.changed = true;
                Ok(())
            })())
        },
    )?;
    linker.func_wrap(
        WASI,
        "fd_filestat_set_times",
        |_: Caller<'_, State>, _: i32, _: i64, _: i64, _: i32| ENOTSUP,
    )?;
    linker.func_wrap(
        WASI,
        "fd_prestat_get",
        |mut c: Caller<'_, State>, fd: i32, at: i32| {
            let name = match c.data().wasi.fds.fds.get(&fd) {
                Some(Fd::Dir {
                    preopened: Some(name),
                    ..
                }) => name.clone(),
                _ => return EBADF,
            };
            // A directory, and the length of its name.
            let mut out = vec![0_u8; 4];
            out.extend_from_slice(&u32::try_from(name.len()).unwrap_or(0).to_le_bytes());
            errno(write_bytes(&mut c, at, &out))
        },
    )?;
    linker.func_wrap(
        WASI,
        "fd_prestat_dir_name",
        |mut c: Caller<'_, State>, fd: i32, at: i32, len: i32| {
            let name = match c.data().wasi.fds.fds.get(&fd) {
                Some(Fd::Dir {
                    preopened: Some(name),
                    ..
                }) => name.clone(),
                _ => return EBADF,
            };
            let capacity = usize::try_from(len).unwrap_or(0);
            errno(write_bytes(
                &mut c,
                at,
                name.as_bytes()
                    .get(..name.len().min(capacity))
                    .unwrap_or_default(),
            ))
        },
    )?;
    linker.func_wrap(
        WASI,
        "fd_readdir",
        |mut c: Caller<'_, State>, fd: i32, buf: i32, len: i32, cookie: i64, used: i32| {
            errno(fd_readdir(&mut c, fd, buf, len, cookie, used))
        },
    )?;
    linker.func_wrap(
        WASI,
        "fd_renumber",
        |mut c: Caller<'_, State>, fd: i32, to: i32| {
            let table = &mut c.data_mut().wasi.fds.fds;
            match table.remove(&fd) {
                Some(moved) => {
                    table.insert(to, moved);
                    SUCCESS
                }
                None => EBADF,
            }
        },
    )?;
    linker.func_wrap(
        WASI,
        "fd_seek",
        |mut c: Caller<'_, State>, fd: i32, offset: i64, whence: i32, at: i32| {
            errno(fd_seek(&mut c, fd, offset, whence, at))
        },
    )?;
    linker.func_wrap(
        WASI,
        "fd_tell",
        |mut c: Caller<'_, State>, fd: i32, at: i32| errno(fd_seek(&mut c, fd, 0, 1, at)),
    )?;
    linker.func_wrap(
        WASI,
        "path_create_directory",
        |c: Caller<'_, State>, dir: i32, path: i32, len: i32| {
            errno((|| {
                let name = path_of(&c, path, len)?;
                let full = c.data().wasi.fds.resolve(dir, &name)?;
                if host_of(&c).0.symlink_metadata(&full).is_ok() {
                    return Err(EEXIST);
                }
                verb(&c, "mkdir", &[text(&full)])
            })())
        },
    )?;
    linker.func_wrap(
        WASI,
        "path_filestat_get",
        |mut c: Caller<'_, State>, dir: i32, flags: i32, path: i32, len: i32, at: i32| {
            errno(path_filestat(&mut c, dir, flags, (path, len), at))
        },
    )?;
    linker.func_wrap(
        WASI,
        "path_filestat_set_times",
        |_: Caller<'_, State>, _: i32, _: i32, _: i32, _: i32, _: i64, _: i64, _: i32| ENOTSUP,
    )?;
    linker.func_wrap(
        WASI,
        "path_link",
        |_: Caller<'_, State>, _: i32, _: i32, _: i32, _: i32, _: i32, _: i32, _: i32| ENOTSUP,
    )?;
    linker.func_wrap(
        WASI,
        "path_open",
        |mut c: Caller<'_, State>,
         dir: i32,
         dirflags: i32,
         path: i32,
         len: i32,
         oflags: i32,
         rights: i64,
         _inheriting: i64,
         fdflags: i32,
         opened: i32| {
            let opening = Opening {
                dirflags,
                oflags,
                rights,
                fdflags,
            };
            errno(path_open(&mut c, dir, (path, len), opening, opened))
        },
    )?;
    linker.func_wrap(
        WASI,
        "path_readlink",
        |mut c: Caller<'_, State>,
         dir: i32,
         path: i32,
         len: i32,
         buf: i32,
         buf_len: i32,
         used: i32| {
            errno((|| {
                let name = path_of(&c, path, len)?;
                let full = c.data().wasi.fds.resolve(dir, &name)?;
                let target = host_of(&c)
                    .0
                    .read_link(&full)
                    .map_err(|error| io_errno(&error))?;
                let target = text(&target).into_bytes();
                let count = target
                    .len()
                    .min(usize::try_from(buf_len).map_err(|_| EINVAL)?);
                write_bytes(&mut c, buf, target.get(..count).unwrap_or_default())?;
                write_u32(&mut c, used, u32::try_from(count).map_err(|_| EINVAL)?)
            })())
        },
    )?;
    linker.func_wrap(
        WASI,
        "path_remove_directory",
        |c: Caller<'_, State>, dir: i32, path: i32, len: i32| {
            errno(remove(&c, dir, (path, len), true))
        },
    )?;
    linker.func_wrap(
        WASI,
        "path_rename",
        |c: Caller<'_, State>,
         dir: i32,
         old: i32,
         old_len: i32,
         new_dir: i32,
         new: i32,
         new_len: i32| { errno(rename(&c, (dir, old, old_len), (new_dir, new, new_len))) },
    )?;
    linker.func_wrap(
        WASI,
        "path_symlink",
        |c: Caller<'_, State>, old: i32, old_len: i32, dir: i32, new: i32, new_len: i32| {
            errno((|| {
                let target = path_of(&c, old, old_len)?;
                let name = path_of(&c, new, new_len)?;
                let full = c.data().wasi.fds.resolve(dir, &name)?;
                verb(&c, "ln", &["-s".to_owned(), target, text(&full)])
            })())
        },
    )?;
    linker.func_wrap(
        WASI,
        "path_unlink_file",
        |c: Caller<'_, State>, dir: i32, path: i32, len: i32| {
            errno(remove(&c, dir, (path, len), false))
        },
    )?;
    Ok(())
}
