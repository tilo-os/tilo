//! The folder side: what is in it, and writing into it so that no reader
//! ever sees a partial file.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write as _};
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

fn ignored(name: &str) -> bool {
    tilo_files::native::atomic::is_temporary_name(std::ffi::OsStr::new(name)) || name == ".DS_Store"
}

/// What identifies one version of a local file without reading it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub size: u64,
    pub mtime_ns: i128,
    pub dev: u64,
    pub ino: u64,
}

impl Stamp {
    /// The stamp of `path`, whose own metadata (not a link's target's) is `meta`.
    #[cfg_attr(
        unix,
        allow(
            clippy::unnecessary_wraps,
            reason = "Windows reads the file's id from the file, which can fail"
        )
    )]
    fn of(path: &Path, meta: &fs::Metadata) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let _ = path;
            Ok(Self::with_id(meta, unix_id(meta)))
        }
        #[cfg(not(unix))]
        {
            let stamp = tilo_files::native::FileStamp::of(path)?
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            Ok(Self::with_id(meta, folded(stamp.id())))
        }
    }

    /// The stamp of the open `file`.
    fn of_file(file: &fs::File) -> io::Result<Self> {
        let meta = file.metadata()?;
        #[cfg(unix)]
        {
            Ok(Self::with_id(&meta, unix_id(&meta)))
        }
        #[cfg(not(unix))]
        {
            let stamp = tilo_files::native::FileStamp::of_file(file)?;
            Ok(Self::with_id(&meta, folded(stamp.id())))
        }
    }

    fn with_id(meta: &fs::Metadata, (dev, ino): (u64, u64)) -> Self {
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| {
                i128::try_from(since.as_nanos()).unwrap_or(i128::MAX)
            });
        Self {
            size: meta.len(),
            mtime_ns,
            dev,
            ino,
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

pub fn stat(path: &Path) -> io::Result<Option<LocalEntry>> {
    match fs::symlink_metadata(path).and_then(|meta| entry_of(path, &meta)) {
        Ok(entry) => Ok(entry),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Symbolic links and special files are neither followed nor imported.
fn entry_of(path: &Path, meta: &fs::Metadata) -> io::Result<Option<LocalEntry>> {
    let kind = meta.file_type();
    if !(kind.is_dir() || kind.is_file()) {
        return Ok(None);
    }
    Ok(Some(LocalEntry {
        dir: kind.is_dir(),
        stamp: Stamp::of(path, meta)?,
    }))
}

/// A Unix file's device and inode.
#[cfg(unix)]
fn unix_id(meta: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt as _;
    (meta.dev(), meta.ino())
}

/// A Windows file's volume and its 128-bit id, folded to the 64 bits a stamp keeps: NTFS ids fit
/// in the low half, which is kept as it is.
#[cfg(not(unix))]
fn folded((volume, file): (u64, u128)) -> (u64, u64) {
    let (high, low) = ((file >> 64) as u64, file as u64);
    (volume, low ^ high)
}

/// Every plain file and directory below `root`, by `/`-separated relative path;
/// names that are not UTF-8 are skipped.
pub fn scan(root: &Path) -> io::Result<BTreeMap<String, LocalEntry>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![String::new()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(root.join(&dir)) {
            Ok(entries) => entries,
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
            let path = entry.path();
            let Some(local) = entry
                .metadata()
                .and_then(|meta| entry_of(&path, &meta))
                .ok()
                .flatten()
            else {
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

/// Replaces `path` with `bytes` modified at `mtime_ms`: staged beside it and
/// renamed into place, so a reader sees the old file or the new one.
pub fn write_atomically(path: &Path, bytes: &[u8], mtime_ms: u64) -> io::Result<Stamp> {
    let mtime = UNIX_EPOCH
        .checked_add(Duration::from_millis(mtime_ms))
        .unwrap_or(UNIX_EPOCH);
    let mut stamp = None;
    tilo_files::native::atomic::write_streaming(path, |file| {
        file.write_all(bytes)?;
        file.set_modified(mtime)?;
        stamp = Some(Stamp::of_file(file)?);
        Ok(())
    })?;
    stamp.ok_or_else(|| io::Error::other("the streamed write computed no stamp"))
}

/// Copies `path` to `staging`, returning the stamp it had throughout, or `None`
/// when it changed meanwhile.
pub fn snapshot(path: &Path, staging: &Path) -> io::Result<Option<Stamp>> {
    let Some(before) = stat(path)? else {
        return Ok(None);
    };
    // Clones on APFS and reflinking Linux filesystems.
    fs::copy(path, staging)?;
    let after = stat(path)?;
    Ok((after == Some(before)).then_some(before.stamp))
}

/// Removes the host's own files (Finder metadata, staging a crash left) in `dir`.
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

pub fn create_dir(path: &Path) -> io::Result<Stamp> {
    fs::create_dir_all(path)?;
    Stamp::of(path, &fs::symlink_metadata(path)?)
}
