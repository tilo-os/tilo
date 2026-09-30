//! Native directories through `std::fs` and the absorbed [`native`](crate::native)
//! modules.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::native::atomic;
use crate::native::lock::{FileLock, TryLockError};
use crate::{Entry, FsError, Kind, LockMode, Meta, VPath};

pub(crate) fn resolve(root: &Path, path: &VPath) -> PathBuf {
    path.components()
        .fold(root.to_path_buf(), |path, component| path.join(component))
}

/// `path` below `root`, with its parent directories created. A file in the
/// way of a parent is [`FsError::NotADirectory`], as on every backend, where
/// `create_dir_all` reports the name as taken.
fn prepare(root: &Path, path: &VPath) -> Result<PathBuf, FsError> {
    let target = resolve(root, path);
    if let Some(parent) = target.parent() {
        match fs::create_dir_all(parent) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(FsError::NotADirectory);
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(target)
}

pub(crate) fn read(root: &Path, path: &VPath) -> Result<Vec<u8>, FsError> {
    Ok(fs::read(resolve(root, path))?)
}

/// At most the first `max` bytes, without reading the rest.
pub(crate) fn read_prefix(root: &Path, path: &VPath, max: usize) -> Result<Vec<u8>, FsError> {
    use std::io::Read as _;
    let file = fs::File::open(resolve(root, path))?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(max).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Add `bytes` at the end, creating the file (and its folders) when missing.
pub(crate) fn append(root: &Path, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
    use std::io::Write as _;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(prepare(root, path)?)?;
    file.write_all(bytes)?;
    Ok(())
}

pub(crate) fn write(root: &Path, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
    Ok(fs::write(prepare(root, path)?, bytes)?)
}

pub(crate) fn write_atomic(root: &Path, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
    Ok(atomic::write(&prepare(root, path)?, bytes)?)
}

/// A user's document: through an existing symbolic link, keeping the
/// target's permission bits, into a folder that must exist.
pub(crate) fn save_document(root: &Path, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
    Ok(atomic::write_preserving_target(
        &resolve(root, path),
        bytes,
    )?)
}

/// Owner-only: the file `0600`, its parent `0700`.
pub(crate) fn write_private(root: &Path, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
    Ok(atomic::write_private(&prepare(root, path)?, bytes)?)
}

pub(crate) fn create_new(root: &Path, path: &VPath, bytes: &[u8]) -> Result<bool, FsError> {
    let target = prepare(root, path)?;
    Ok(atomic::create_with_temp_opener(&target, bytes, |temp| {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temp)
    })?)
}

pub(crate) fn rename_noreplace(root: &Path, from: &VPath, to: &VPath) -> Result<(), FsError> {
    let source = resolve(root, from);
    fs::symlink_metadata(&source)?;
    // `renameatx_np` treats a rename onto itself as done; the name is taken.
    if from == to {
        return Err(FsError::AlreadyExists);
    }
    let target = prepare(root, to)?;
    rename_exclusive(&source, &target)?;
    let target_parent = target.parent().unwrap_or(root);
    atomic::sync_directory(target_parent)?;
    match source.parent() {
        Some(parent) if parent != target_parent => Ok(atomic::sync_directory(parent)?),
        _ => Ok(()),
    }
}

/// `renameat2(RENAME_NOREPLACE)` on Linux and `renameatx_np(RENAME_EXCL)` on
/// Apple systems. A file system that refuses the flag gets the portable
/// fallback.
#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    use rustix::io::Errno;

    match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        Err(errno) if errno == Errno::INVAL || errno == Errno::NOSYS || errno == Errno::NOTSUP => {
            link_then_unlink(from, to)
        }
        Err(errno) => Err(errno.into()),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    link_then_unlink(from, to)
}

/// A hard link cannot replace a name, so linking then unlinking the source
/// moves a file without clobbering. Directories cannot be hard-linked.
fn link_then_unlink(from: &Path, to: &Path) -> io::Result<()> {
    if fs::symlink_metadata(from)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this file system cannot move a directory without risking replacement",
        ));
    }
    fs::hard_link(from, to)?;
    fs::remove_file(from)
}

pub(crate) fn remove(root: &Path, path: &VPath) -> Result<(), FsError> {
    let target = resolve(root, path);
    if fs::symlink_metadata(&target)?.is_dir() {
        fs::remove_dir(&target)?;
    } else {
        fs::remove_file(&target)?;
    }
    Ok(())
}

/// A symbolic link is removed, never followed.
pub(crate) fn remove_all(root: &Path, path: &VPath) -> Result<(), FsError> {
    let target = resolve(root, path);
    if fs::symlink_metadata(&target)?.is_dir() {
        fs::remove_dir_all(&target)?;
    } else {
        fs::remove_file(&target)?;
    }
    Ok(())
}

pub(crate) fn create_dir_all(root: &Path, path: &VPath) -> Result<(), FsError> {
    Ok(fs::create_dir_all(resolve(root, path))?)
}

pub(crate) fn list(root: &Path, path: &VPath) -> Result<Vec<Entry>, FsError> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(resolve(root, path))? {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let file_type = entry.file_type()?;
        let kind = if file_type.is_symlink() {
            fs::metadata(entry.path()).map_or(Kind::Other, |meta| kind_of(meta.file_type()))
        } else {
            kind_of(file_type)
        };
        entries.push(Entry { name, kind });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

pub(crate) fn metadata(root: &Path, path: &VPath) -> Result<Meta, FsError> {
    let target = resolve(root, path);
    let link = fs::symlink_metadata(&target)?;
    let symlink = link.file_type().is_symlink();
    let followed = if symlink {
        fs::metadata(&target).ok()
    } else {
        None
    };
    let meta = followed.as_ref().unwrap_or(&link);
    let kind = if symlink && followed.is_none() {
        Kind::Other
    } else {
        kind_of(meta.file_type())
    };
    Ok(Meta {
        len: meta.len(),
        modified: meta
            .modified()
            .map_or(tilo_clock::UNIX_EPOCH, tilo_clock::from_std),
        kind,
        mode: mode_of(meta),
        symlink,
    })
}

pub(crate) fn lock(root: &Path, path: &VPath, mode: LockMode) -> Result<FileLock, FsError> {
    Ok(FileLock::open(&prepare(root, path)?, mode)?)
}

pub(crate) fn try_lock(
    root: &Path,
    path: &VPath,
    mode: LockMode,
) -> Result<Option<FileLock>, FsError> {
    match FileLock::try_open(&prepare(root, path)?, mode) {
        Ok(lock) => Ok(Some(lock)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

fn kind_of(file_type: fs::FileType) -> Kind {
    if file_type.is_file() {
        Kind::File
    } else if file_type.is_dir() {
        Kind::Dir
    } else {
        Kind::Other
    }
}

#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "shares the optional result of targets without permission bits"
)]
fn mode_of(meta: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(meta.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_of(_meta: &fs::Metadata) -> Option<u32> {
    None
}
