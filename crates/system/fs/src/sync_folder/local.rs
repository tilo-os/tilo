//! The folder side: what is in it, and writing into it so that no reader
//! ever sees a partial file.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Prefix of the files a pass stages before renaming them into place.
/// Never imported, and left over only by a crash.
pub const STAGING_PREFIX: &str = ".tilo-sync-";

static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Names that belong to the host rather than to the workspace.
fn ignored(name: &str) -> bool {
    name.starts_with(STAGING_PREFIX) || name == ".DS_Store"
}

/// What identifies one version of a local file without reading it: a
/// change to any of these is an edit, a replacement or a move.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub size: u64,
    pub mtime_ns: i128,
    pub dev: u64,
    pub ino: u64,
}

impl Stamp {
    fn of(meta: &fs::Metadata) -> Self {
        Self {
            size: meta.len(),
            mtime_ns: i128::from(meta.mtime())
                .saturating_mul(1_000_000_000)
                .saturating_add(i128::from(meta.mtime_nsec())),
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }

    pub fn mtime_ms(&self) -> u64 {
        u64::try_from(self.mtime_ns / 1_000_000).unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalEntry {
    pub dir: bool,
    pub stamp: Stamp,
}

/// The stamp of `path` now, `None` when it is gone or is not a plain file
/// or directory.
pub fn stat(path: &Path) -> io::Result<Option<LocalEntry>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(entry_of(&meta)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Symbolic links and special files are neither followed nor imported:
/// the folder must not become a way to pull host files into the workspace.
fn entry_of(meta: &fs::Metadata) -> Option<LocalEntry> {
    let kind = meta.file_type();
    (kind.is_dir() || kind.is_file()).then(|| LocalEntry {
        dir: kind.is_dir(),
        stamp: Stamp::of(meta),
    })
}

/// Every plain file and directory below `root`, by `/`-separated relative
/// path. Names that are not UTF-8 are skipped, as the workspace names
/// files in UTF-8.
pub fn scan(root: &Path) -> io::Result<BTreeMap<String, LocalEntry>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![String::new()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(root.join(&dir)) {
            Ok(entries) => entries,
            // Removed while the scan ran: the next pass sees it gone.
            Err(error) if error.kind() == io::ErrorKind::NotFound && !dir.is_empty() => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if ignored(&name) {
                continue;
            }
            let Some(local) = entry.metadata().ok().as_ref().and_then(entry_of) else {
                continue;
            };
            let rel = if dir.is_empty() {
                name
            } else {
                format!("{dir}/{name}")
            };
            if local.dir {
                stack.push(rel.clone());
            }
            out.insert(rel, local);
        }
    }
    Ok(out)
}

/// Replaces `path` with `bytes` modified at `mtime_ms`: staged beside it,
/// synced and renamed into place, so a reader sees the old file or the
/// new one. Returns the new file's stamp.
pub fn write_atomically(path: &Path, bytes: &[u8], mtime_ms: u64) -> io::Result<Stamp> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let staging = parent.join(format!(
        "{STAGING_PREFIX}{}-{}",
        std::process::id(),
        STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let written = stage_and_rename(&staging, path, bytes, mtime_ms);
    if written.is_err() {
        let _ = fs::remove_file(&staging);
    }
    let stamp = written?;
    tilo_files::native::atomic::sync_directory(parent)?;
    Ok(stamp)
}

fn stage_and_rename(staging: &Path, path: &Path, bytes: &[u8], mtime_ms: u64) -> io::Result<Stamp> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(staging)?;
    file.write_all(bytes)?;
    let mtime = UNIX_EPOCH
        .checked_add(Duration::from_millis(mtime_ms))
        .unwrap_or(UNIX_EPOCH);
    file.set_modified(mtime)?;
    file.sync_all()?;
    let stamp = Stamp::of(&file.metadata()?);
    fs::rename(staging, path)?;
    Ok(stamp)
}

/// Copies `path` to `staging` and returns the stamp `path` had throughout
/// the copy, `None` when it changed meanwhile: the copy is then not one
/// version of the file, and the next pass takes it again.
pub fn snapshot(path: &Path, staging: &Path) -> io::Result<Option<Stamp>> {
    let Some(before) = stat(path)? else {
        return Ok(None);
    };
    // Clones on APFS and reflinking Linux filesystems.
    fs::copy(path, staging)?;
    let after = stat(path)?;
    Ok((after == Some(before)).then_some(before.stamp))
}

/// Removes the host's own files (Finder metadata, staging a crash left)
/// directly in `dir`, which would otherwise keep it from being removed.
pub fn remove_host_files(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let is_host_file = entry.file_name().to_str().is_some_and(ignored);
        if is_host_file && entry.file_type()?.is_file() {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Creates the directory `path` and its parents; returns its stamp.
pub fn create_dir(path: &Path) -> io::Result<Stamp> {
    fs::create_dir_all(path)?;
    Ok(Stamp::of(&fs::symlink_metadata(path)?))
}
