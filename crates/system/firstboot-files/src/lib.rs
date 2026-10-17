//! `tilo-firstboot-files`: durable first-boot intent and retryable, no-follow
//! home defaults for `tilo-firstboot-user`.
//!
//! ```text
//! tilo-firstboot-files intent PATH                 publish stdin's intent record at PATH
//! tilo-firstboot-files home HOME UID GID [SKELETON] create HOME, optionally add missing
//!                                                  defaults, give everything to UID:GID
//! ```
//!
//! Omit SKELETON when a separate attesting helper owns default publication.
//!
//! Only complete files are published: each is written under a random
//! temporary name, synced, and renamed into place with `RENAME_NOREPLACE`, so
//! an interrupted run leaves the final name absent and a retry starts clean.
//! An existing file is never replaced, which keeps the installer's seeds and an
//! unexpected intent. Destination directory descriptors anchor every edge,
//! including the ownership repair, and no symlink below the account's parent
//! directory is ever followed.
//!
//! Exit status 0 when done, 1 on any failure (with the reason on stderr).
//!
//! The calls tests need to interrupt or observe (listing a directory, syncing
//! a descriptor, publishing one name) go through [`Files`], whose provided
//! methods are the production behaviour.

pub mod py;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use rustix::fs::{
    AtFlags, CWD, Dir, FileType, Gid, Mode, OFlags, RenameFlags, Uid, chownat, fchmod, fchown,
    fstat, mkdirat, openat, readlinkat, renameat_with, statat, symlinkat, unlinkat,
};
use rustix::io::Errno;

/// How every directory is opened: read-only, a directory, never a symlink.
pub const DIR: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// The intent record: an account name and its random locked shadow value.
/// One byte more than this is read so that a longer record is refused too.
const INTENT_LIMIT: usize = 1025;
const INTENT_LOCK: &[u8] = b":!tilo-firstboot-";
const COPY_BLOCK: usize = 1024 * 1024;
const HEX: &[u8; 16] = b"0123456789abcdef";

/// A failure, by the kind of exception the original raised.
#[derive(Debug)]
pub enum Error {
    /// The operating system refused `call` on `name` (`OSError`).
    Os {
        call: &'static str,
        name: Option<OsString>,
        error: io::Error,
    },
    /// The input or the tree had the wrong form (`ValueError`, `IndexError`,
    /// `OverflowError`).
    Value(String),
}

impl Error {
    /// The operating-system error behind this failure, if it is one.
    #[must_use]
    pub const fn os_error(&self) -> Option<&io::Error> {
        match self {
            Self::Os { error, .. } => Some(error),
            Self::Value(_) => None,
        }
    }

    fn value(message: impl Into<String>) -> Self {
        Self::Value(message.into())
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Os {
            call: "",
            name: None,
            error,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Os { call, name, error } => {
                if !call.is_empty() {
                    write!(f, "{call}")?;
                    if let Some(name) = name {
                        write!(f, " {:?}", name.to_string_lossy())?;
                    }
                    write!(f, ": ")?;
                }
                write!(f, "{error}")
            }
            Self::Value(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

/// `Result` with this crate's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Maps a failed `call` on `name` to an [`Error`].
fn at(call: &'static str, name: &OsStr) -> impl FnOnce(Errno) -> Error {
    let name = name.to_owned();
    move |errno| Error::Os {
        call,
        name: Some(name),
        error: errno.into(),
    }
}

fn on(call: &'static str) -> impl FnOnce(Errno) -> Error {
    move |errno| Error::Os {
        call,
        name: None,
        error: errno.into(),
    }
}

/// Writes one temporary name in a directory; [`Files::publish`] runs it.
pub type Writer<'a> = dyn FnMut(&OsStr) -> Result<()> + 'a;

/// The calls tests interrupt or observe. The provided methods are the
/// production behaviour; [`System`] uses them all.
pub trait Files {
    /// The names in `directory`, without `.` and `..`, in directory order
    /// (`os.listdir`).
    ///
    /// # Errors
    /// The directory cannot be read.
    fn list(&self, directory: BorrowedFd<'_>) -> Result<Vec<OsString>> {
        let mut entries = Dir::read_from(directory).map_err(on("listdir"))?;
        let mut names = Vec::new();
        while let Some(entry) = entries.read() {
            let entry = entry.map_err(on("listdir"))?;
            let name = entry.file_name().to_bytes();
            if name != b"." && name != b".." {
                names.push(OsString::from_vec(name.to_vec()));
            }
        }
        Ok(names)
    }

    /// Flushes `fd` to stable storage.
    ///
    /// # Errors
    /// The kernel refused.
    fn fsync(&self, fd: BorrowedFd<'_>) -> Result<()> {
        rustix::fs::fsync(fd).map_err(on("fsync"))
    }

    /// Publishes `name` in `directory` whole or not at all: see [`publish`].
    ///
    /// # Errors
    /// See [`publish`].
    fn publish(
        &self,
        directory: BorrowedFd<'_>,
        name: &OsStr,
        writer: &mut Writer<'_>,
    ) -> Result<()> {
        publish(self, directory, name, writer)
    }
}

/// The machine itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct System;

impl Files for System {}

/// Who the published files belong to, as the command line gave it: Python
/// integers, refused only when a `chown` needs them (`-1` and `2**32 - 1`
/// leave that id unchanged).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owner {
    pub uid: i128,
    pub gid: i128,
}

impl Owner {
    /// The calling process's real ids.
    #[must_use]
    pub fn current() -> Self {
        Self {
            uid: i128::from(rustix::process::getuid().as_raw()),
            gid: i128::from(rustix::process::getgid().as_raw()),
        }
    }

    fn matches(self, metadata: &rustix::fs::Stat) -> bool {
        self.uid == i128::from(metadata.st_uid) && self.gid == i128::from(metadata.st_gid)
    }

    fn ids(self) -> Result<(Option<Uid>, Option<Gid>)> {
        let uid = id(self.uid, "uid")?;
        let gid = id(self.gid, "gid")?;
        // `(uid_t)-1` is the kernel's "leave unchanged", which is `None` here.
        let keep = |raw: u32| raw != u32::MAX;
        Ok((
            Some(uid).filter(|raw| keep(*raw)).map(Uid::from_raw),
            Some(gid).filter(|raw| keep(*raw)).map(Gid::from_raw),
        ))
    }
}

/// An id as `os.chown` converts it: `-1` is `(uid_t)-1`, any other negative
/// value or one beyond 32 bits is refused.
fn id(value: i128, kind: &str) -> Result<u32> {
    if value == -1 {
        return Ok(u32::MAX);
    }
    if value < 0 {
        return Err(Error::value(format!("{kind} is less than minimum")));
    }
    u32::try_from(value).map_err(|_| Error::value(format!("{kind} is greater than maximum")))
}

/// `.tilo-firstboot-` and 128 random bits in hex.
fn temporary_name() -> Result<OsString> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| Error::from(io::Error::other(format!("getrandom: {error}"))))?;
    let mut name = b".tilo-firstboot-".to_vec();
    for byte in bytes {
        for nibble in [byte >> 4, byte & 0x0f] {
            name.extend(HEX.get(usize::from(nibble)));
        }
    }
    Ok(OsString::from_vec(name))
}

/// Publishes `name` in `directory`: `writer` creates it under a random
/// temporary name, which is then renamed to `name` without replacing anything
/// and with no window in which the published file has a second link. The
/// temporary is removed and the directory synced whatever happened.
///
/// # Errors
/// The writer's error, `EEXIST` when `name` already exists, or a failure to
/// remove the temporary or sync the directory (which then wins).
pub fn publish<F: Files + ?Sized>(
    files: &F,
    directory: BorrowedFd<'_>,
    name: &OsStr,
    writer: &mut Writer<'_>,
) -> Result<()> {
    let temporary = temporary_name()?;
    let outcome = (|| {
        writer(&temporary)?;
        renameat_with(
            directory,
            temporary.as_os_str(),
            directory,
            name,
            RenameFlags::NOREPLACE,
        )
        .map_err(at("rename", name))?;
        files.fsync(directory)
    })();
    match unlinkat(directory, temporary.as_os_str(), AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => {}
        Err(errno) => return Err(at("unlink", &temporary)(errno)),
    }
    files.fsync(directory)?;
    outcome
}

/// Creates `name` in `directory` (never through a symlink, never over an
/// existing file), copies `source` into it, then sets owner and `mode` and
/// syncs it. Ownership comes before the mode, so a set-id bit in `mode` is
/// kept.
///
/// # Errors
/// Any failed call, or the source's read error.
pub fn write_file<F: Files + ?Sized>(
    files: &F,
    directory: BorrowedFd<'_>,
    name: &OsStr,
    source: &mut dyn Read,
    mode: Mode,
    owner: Owner,
) -> Result<()> {
    let fd = openat(
        directory,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(at("open", name))?;
    let mut output = File::from(fd);
    let mut block = vec![0u8; COPY_BLOCK];
    loop {
        let read = source.read(&mut block)?;
        if read == 0 {
            break;
        }
        output.write_all(block.get(..read).unwrap_or_default())?;
    }
    output.flush()?;
    let (uid, gid) = owner.ids()?;
    fchown(&output, uid, gid).map_err(at("chown", name))?;
    fchmod(&output, mode).map_err(at("chmod", name))?;
    files.fsync(output.as_fd())
}

fn open_dir(directory: BorrowedFd<'_>, name: &OsStr) -> Result<OwnedFd> {
    openat(directory, name, DIR, Mode::empty()).map_err(at("open", name))
}

fn lstat_at(directory: BorrowedFd<'_>, name: &OsStr) -> Result<rustix::fs::Stat, Errno> {
    statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
}

/// Adds every default in `source` that `destination` lacks, recursively:
/// directories with their mode, regular files with their content and mode,
/// symlinks with their target, each owned by `owner`. Whatever already exists
/// in `destination` is kept, and a directory there is descended into only
/// through a descriptor that refuses a symlink.
///
/// # Errors
/// Any failed call; a source entry that is not a directory, regular file or
/// symlink.
pub fn defaults<F: Files + ?Sized>(
    files: &F,
    source: BorrowedFd<'_>,
    destination: BorrowedFd<'_>,
    owner: Owner,
) -> Result<()> {
    for name in files.list(source)? {
        let metadata = lstat_at(source, &name).map_err(at("stat", &name))?;
        let existing = match lstat_at(destination, &name) {
            Ok(existing) => Some(existing),
            Err(Errno::NOENT) => None,
            Err(errno) => return Err(at("stat", &name)(errno)),
        };
        let kind = FileType::from_raw_mode(metadata.st_mode);
        let mode = Mode::from_raw_mode(metadata.st_mode);
        if kind == FileType::Directory {
            if existing.is_none() {
                mkdirat(destination, name.as_os_str(), mode).map_err(at("mkdir", &name))?;
            }
            let child = open_dir(destination, &name)?;
            let origin = open_dir(source, &name)?;
            defaults(files, origin.as_fd(), child.as_fd(), owner)?;
            if existing.is_none() {
                fchmod(&child, mode).map_err(at("chmod", &name))?;
            }
            files.fsync(child.as_fd())?;
        } else if existing.is_none() {
            match kind {
                FileType::RegularFile => {
                    let fd = openat(
                        source,
                        name.as_os_str(),
                        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(at("open", &name))?;
                    let mut input = File::from(fd);
                    files.publish(destination, &name, &mut |temporary| {
                        write_file(files, destination, temporary, &mut input, mode, owner)
                    })?;
                }
                FileType::Symlink => {
                    let target = readlinkat(source, name.as_os_str(), Vec::new())
                        .map_err(at("readlink", &name))?;
                    files.publish(destination, &name, &mut |temporary| {
                        symlinkat(target.as_c_str(), destination, temporary)
                            .map_err(at("symlink", temporary))?;
                        let (uid, gid) = owner.ids()?;
                        chownat(destination, temporary, uid, gid, AtFlags::SYMLINK_NOFOLLOW)
                            .map_err(at("chown", temporary))
                    })?;
                }
                _ => return Err(Error::value("unsupported skeleton inode")),
            }
        }
    }
    files.fsync(destination)
}

/// Gives everything in `directory`, and `directory` itself, to `owner`,
/// recursively and without following a symlink. A regular file with a second
/// link is refused rather than handed over, before and after it is opened.
///
/// # Errors
/// Any failed call; a hard-linked file; an entry that is not a directory,
/// regular file or symlink.
pub fn ownership<F: Files + ?Sized>(
    files: &F,
    directory: BorrowedFd<'_>,
    owner: Owner,
) -> Result<()> {
    for name in files.list(directory)? {
        let metadata = lstat_at(directory, &name).map_err(at("stat", &name))?;
        match FileType::from_raw_mode(metadata.st_mode) {
            FileType::Directory => {
                let child = open_dir(directory, &name)?;
                ownership(files, child.as_fd(), owner)?;
            }
            FileType::RegularFile => {
                if metadata.st_nlink != 1 {
                    return Err(Error::value("hardlinked home file"));
                }
                let fd = openat(
                    directory,
                    name.as_os_str(),
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(at("open", &name))?;
                let opened = fstat(&fd).map_err(at("stat", &name))?;
                if opened.st_nlink != 1 {
                    return Err(Error::value("hardlinked home file"));
                }
                // A chown strips set-id bits; ownership transfer must not
                // restore them.
                // Even a no-op chown changes ctime and invalidates an
                // attesting helper's content-source witness on retry.
                if !owner.matches(&opened) {
                    let (uid, gid) = owner.ids()?;
                    fchown(&fd, uid, gid).map_err(at("chown", &name))?;
                }
                files.fsync(fd.as_fd())?;
            }
            FileType::Symlink => {
                if !owner.matches(&metadata) {
                    let (uid, gid) = owner.ids()?;
                    chownat(
                        directory,
                        name.as_os_str(),
                        uid,
                        gid,
                        AtFlags::SYMLINK_NOFOLLOW,
                    )
                    .map_err(at("chown", &name))?;
                }
            }
            _ => return Err(Error::value("unsupported home inode")),
        }
    }
    let metadata = fstat(directory).map_err(on("stat"))?;
    if !owner.matches(&metadata) {
        let (uid, gid) = owner.ids()?;
        fchown(directory, uid, gid).map_err(on("chown"))?;
    }
    files.fsync(directory)
}

/// Whether `record` is one complete intent line:
/// `[a-z_][a-z0-9_-]{0,31}:!tilo-firstboot-[a-f0-9]{32}\n`.
#[must_use]
pub fn is_intent_record(record: &[u8]) -> bool {
    let Some(colon) = record.iter().position(|b| *b == b':') else {
        return false;
    };
    let (user, lock) = record.split_at(colon);
    let user_ok = match user.split_first() {
        Some((first, rest)) => {
            (first.is_ascii_lowercase() || *first == b'_')
                && rest.len() <= 31
                && rest.iter().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-'
                })
        }
        None => false,
    };
    let Some(token) = lock.strip_prefix(INTENT_LOCK) else {
        return false;
    };
    let Some(token) = token.strip_suffix(b"\n") else {
        return false;
    };
    user_ok
        && token.len() == 32
        && token
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

fn read_intent(stdin: &mut dyn Read) -> Result<Vec<u8>> {
    let mut record = Vec::with_capacity(INTENT_LIMIT);
    stdin
        .take(u64::try_from(INTENT_LIMIT).unwrap_or(u64::MAX))
        .read_to_end(&mut record)?;
    Ok(record)
}

/// The command line, without the program name. Sets the umask to 0 first:
/// every mode is the one given.
///
/// # Errors
/// Any failure; nothing incomplete is left at a final name.
pub fn main<F: Files + ?Sized>(files: &F, args: &[OsString], stdin: &mut dyn Read) -> Result<()> {
    rustix::process::umask(Mode::empty());
    let (Some(command), Some(path)) = (args.first(), args.get(1)) else {
        return Err(Error::value(format!(
            "not enough values to unpack (expected 2, got {})",
            args.len().min(2)
        )));
    };
    // /home is Fedora's trusted alias for /var/home; never resolve the final
    // account directory or any edge within it.
    let parent_path = py::realpath(&py::dirname(path))?;
    let parent = openat(CWD, parent_path.as_os_str(), DIR, Mode::empty())
        .map_err(at("open", &parent_path))?;
    let name = py::basename(path);
    match command.as_bytes() {
        b"intent" => {
            let record = read_intent(stdin)?;
            if !is_intent_record(&record) {
                return Err(Error::value("incomplete intent record"));
            }
            let owner = Owner::current();
            files.publish(parent.as_fd(), &name, &mut |temporary| {
                write_file(
                    files,
                    parent.as_fd(),
                    temporary,
                    &mut record.as_slice(),
                    Mode::from_raw_mode(0o600),
                    owner,
                )
            })
        }
        b"home" => {
            let ids: Vec<i128> = args
                .iter()
                .skip(2)
                .take(2)
                .map(|value| py::int(value).map_err(|error| Error::Value(error.0)))
                .collect::<Result<_>>()?;
            let [uid, gid] = ids.as_slice() else {
                return Err(Error::value(format!(
                    "not enough values to unpack (expected 2, got {})",
                    ids.len()
                )));
            };
            let owner = Owner {
                uid: *uid,
                gid: *gid,
            };
            match mkdirat(&parent, name.as_os_str(), Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(errno) => return Err(at("mkdir", &name)(errno)),
            }
            let home = open_dir(parent.as_fd(), &name)?;
            if let Some(skeleton_path) = args.get(4) {
                let skeleton = openat(CWD, skeleton_path.as_os_str(), DIR, Mode::empty())
                    .map_err(at("open", skeleton_path))?;
                defaults(files, skeleton.as_fd(), home.as_fd(), owner)?;
            }
            ownership(files, home.as_fd(), owner)?;
            drop(home);
            files.fsync(parent.as_fd())
        }
        _ => Err(Error::value("unknown operation")),
    }
}
