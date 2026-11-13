//! Archives: reading, extracting and creating them, in zip, 7z and tar plain or
//! compressed with gzip, xz, zstd or bzip2, every codec in pure Rust.
//!
//! Entry names are untrusted. A name that is absolute or climbs out with `..`
//! ([`safe_relative`]) makes the archive unreadable rather than being skipped,
//! so a crafted archive is refused whole. [`extract_into`] writes only below the
//! folder it is given, never follows a link it made, creates no device nodes or
//! pipes, strips setuid, setgid and sticky bits, and caps the rest at
//! `rwxr-xr-x`.
//!
//! Reading streams: a compressed tar is decoded once, front to back, without
//! writing anything. Long work reports the archive bytes it has read and stops
//! when asked ([`Observer`]).

mod pack;
mod stream;
mod unpack;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

pub use pack::{Packing, create};
pub use stream::{Flow, list, read_member, walk};
pub use unpack::{Extracted, extract_into};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Compression {
    None,
    Gzip,
    Xz,
    Zstd,
    Bzip2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Zip,
    SevenZ,
    Tar(Compression),
}

/// The archive extensions; the longest match wins, so `.tar.gz` is a gzipped tar.
const EXTENSIONS: [(&str, Format); 13] = [
    (".tar.gz", Format::Tar(Compression::Gzip)),
    (".tar.xz", Format::Tar(Compression::Xz)),
    (".tar.zst", Format::Tar(Compression::Zstd)),
    (".tar.bz2", Format::Tar(Compression::Bzip2)),
    (".tzst", Format::Tar(Compression::Zstd)),
    (".tbz2", Format::Tar(Compression::Bzip2)),
    (".tgz", Format::Tar(Compression::Gzip)),
    (".txz", Format::Tar(Compression::Xz)),
    (".tbz", Format::Tar(Compression::Bzip2)),
    (".tar", Format::Tar(Compression::None)),
    (".zip", Format::Zip),
    (".7z", Format::SevenZ),
    (".tar.zstd", Format::Tar(Compression::Zstd)),
];

impl Format {
    #[must_use]
    pub fn of_name(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        EXTENSIONS
            .iter()
            .filter(|(extension, _)| lower.len() > extension.len() && lower.ends_with(extension))
            .max_by_key(|(extension, _)| extension.len())
            .map(|(_, format)| *format)
    }

    #[must_use]
    pub fn of_path(path: &Path) -> Option<Self> {
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(Self::of_name)
    }

    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::SevenZ => "7z",
            Self::Tar(Compression::None) => "tar",
            Self::Tar(Compression::Gzip) => "tar.gz",
            Self::Tar(Compression::Xz) => "tar.xz",
            Self::Tar(Compression::Zstd) => "tar.zst",
            Self::Tar(Compression::Bzip2) => "tar.bz2",
        }
    }
}

/// `name` without its archive extension (`photos` for `photos.tar.gz`); a name
/// that is only an extension, or no archive's, stays as it is.
#[must_use]
pub fn stem(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    EXTENSIONS
        .iter()
        .filter(|(extension, _)| lower.len() > extension.len() && lower.ends_with(extension))
        .map(|(extension, _)| extension.len())
        .max()
        .and_then(|len| name.len().checked_sub(len))
        .and_then(|end| name.get(..end))
        .unwrap_or(name)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    Symlink,
    HardLink,
    /// A device node, pipe or socket. Never created.
    Special,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub size: u64,
    pub kind: Kind,
    /// Unix permission bits as stored, when the archive keeps them.
    pub mode: Option<u32>,
    pub modified: Option<SystemTime>,
    pub link: Option<PathBuf>,
}

pub trait Observer {
    /// `bytes` more were read from the archive, or from the files being packed.
    fn advance(&self, bytes: u64) {
        let _ = bytes;
    }

    fn cancelled(&self) -> bool {
        false
    }

    /// `bytes` more were written below the destination; an error stops the
    /// extraction with it.
    fn wrote(&self, bytes: u64) -> io::Result<()> {
        let _ = bytes;
        Ok(())
    }
}

impl Observer for () {}

#[derive(Debug)]
struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

#[must_use]
pub fn cancelled() -> io::Error {
    io::Error::other(Cancelled)
}

#[must_use]
pub fn is_cancelled(error: &io::Error) -> bool {
    caused_by::<Cancelled>(error)
}

fn caused_by<T: std::error::Error + 'static>(error: &io::Error) -> bool {
    let mut cause: Option<&(dyn std::error::Error + 'static)> = error
        .get_ref()
        .map(|inner| inner as &(dyn std::error::Error + 'static));
    while let Some(current) = cause {
        if current.is::<T>() {
            return true;
        }
        cause = match current.downcast_ref::<io::Error>() {
            Some(io) => io
                .get_ref()
                .map(|inner| inner as &(dyn std::error::Error + 'static)),
            None => current.source(),
        };
    }
    false
}

#[derive(Debug)]
struct Unsafe(String);

impl std::fmt::Display for Unsafe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "“{}” would be outside the archive's folder", self.0)
    }
}

impl std::error::Error for Unsafe {}

fn unsafe_name(name: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        Unsafe(name.display().to_string()),
    )
}

#[must_use]
pub fn is_unsafe(error: &io::Error) -> bool {
    caused_by::<Unsafe>(error)
}

/// `name` as a path relative to the archive's folder, with `.` components
/// dropped. `None` for a name that is only the folder itself (`./`).
///
/// # Errors
///
/// An absolute name, a drive or UNC prefix, or a `..` component: the name
/// would land outside the folder ([`is_unsafe`]).
pub fn safe_relative(name: &Path) -> io::Result<Option<PathBuf>> {
    let mut clean = PathBuf::new();
    for part in name.components() {
        match part {
            Component::Normal(value) => clean.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(unsafe_name(name));
            }
        }
    }
    Ok((!clean.as_os_str().is_empty()).then_some(clean))
}

/// Whether a symlink at `link` (relative to the folder) pointing at
/// `target` stays inside the folder, read without following anything: an
/// absolute target never does, and `..` may not climb above the folder.
#[must_use]
pub fn link_stays_inside(link: &Path, target: &Path) -> bool {
    let mut depth = link.parent().map_or(0, |parent| {
        parent
            .components()
            .filter(|part| matches!(part, Component::Normal(_)))
            .count()
    });
    for part in target.components() {
        match part {
            Component::Normal(_) => depth = depth.saturating_add(1),
            Component::CurDir => {}
            Component::ParentDir => match depth.checked_sub(1) {
                Some(up) => depth = up,
                None => return false,
            },
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// The direct children of `prefix` among `entries`, as a folder shows them:
/// a folder the archive names only through what is inside it still gets a
/// row, sized 0, last changed when the newest thing inside it was.
#[must_use]
pub fn children(entries: &[Entry], prefix: &Path) -> Vec<Entry> {
    let mut rows: BTreeMap<PathBuf, Entry> = BTreeMap::new();
    let mut named: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    for entry in entries {
        let Ok(relative) = entry.path.strip_prefix(prefix) else {
            continue;
        };
        let mut parts = relative.components();
        let Some(Component::Normal(first)) = parts.next() else {
            continue;
        };
        let path = prefix.join(first);
        let inside = parts.next().is_some();
        if !inside {
            if entry.kind == Kind::Directory {
                named.insert(path.clone());
            }
            let row = Entry {
                path: path.clone(),
                ..entry.clone()
            };
            match rows.get_mut(&path) {
                Some(shown) if entry.kind == Kind::Directory => *shown = row,
                Some(_) => {}
                None => {
                    rows.insert(path, row);
                }
            }
            continue;
        }
        let shown = rows.entry(path.clone()).or_insert_with(|| Entry {
            path: path.clone(),
            size: 0,
            kind: Kind::Directory,
            mode: None,
            modified: None,
            link: None,
        });
        shown.kind = Kind::Directory;
        shown.size = 0;
        if !named.contains(&path) && entry.modified > shown.modified {
            shown.modified = entry.modified;
        }
    }
    rows.into_values().collect()
}

#[must_use]
pub fn has_directory(entries: &[Entry], path: &Path) -> bool {
    path.as_os_str().is_empty()
        || entries.iter().any(|entry| {
            (entry.path == path && entry.kind == Kind::Directory)
                || (entry.path != path && entry.path.starts_with(path))
        })
}
