//! Archives: reading, extracting and creating them.
//!
//! Formats: zip, 7z, and tar plain or compressed with gzip, xz, zstd or
//! bzip2. Every codec is pure Rust.
//!
//! An archive's entry names are untrusted. Every name is checked before
//! anything sees it ([`safe_relative`]): an absolute name or one that climbs
//! out with `..` makes the archive unreadable rather than being skipped, so
//! a crafted archive is refused as a whole. Extraction ([`extract_into`])
//! writes only below the folder it is given, never follows a link it made,
//! creates no device nodes or pipes, strips setuid, setgid and sticky bits
//! and caps the rest at `rwxr-xr-x`.
//!
//! Reading streams: listing a compressed tar decodes it once, front to back,
//! without writing anything. Long work reports the archive bytes it has read
//! and stops when asked ([`Observer`]).

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

/// How a tar stream is compressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Compression {
    None,
    Gzip,
    Xz,
    Zstd,
    Bzip2,
}

/// An archive format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Zip,
    SevenZ,
    Tar(Compression),
}

/// The archive extensions. The longest that matches wins, so a name ending
/// in `.tar.gz` is a gzipped tar.
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
    /// The format a file name says, by its extension, any case.
    #[must_use]
    pub fn of_name(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        EXTENSIONS
            .iter()
            .filter(|(extension, _)| lower.len() > extension.len() && lower.ends_with(extension))
            .max_by_key(|(extension, _)| extension.len())
            .map(|(_, format)| *format)
    }

    /// The format `path`'s name says.
    #[must_use]
    pub fn of_path(path: &Path) -> Option<Self> {
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(Self::of_name)
    }

    /// The extension a new archive of this format is given.
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

/// `name` without its archive extension: `photos` for `photos.tar.gz`.
/// A name that is only an extension, or not an archive's, stays as it is.
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

/// What an archive entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    /// A symbolic link; its target is [`Entry::link`].
    Symlink,
    /// A second name for a file earlier in the archive, [`Entry::link`].
    HardLink,
    /// A device node, pipe or socket. Never created.
    Special,
}

/// One entry of an archive, its name already checked ([`safe_relative`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Relative, without `.` or `..` components.
    pub path: PathBuf,
    /// The bytes a file holds once extracted.
    pub size: u64,
    pub kind: Kind,
    /// Unix permission bits as stored, when the archive keeps them.
    pub mode: Option<u32>,
    pub modified: Option<SystemTime>,
    /// A link's target as stored: a symlink's text, or the archive path of
    /// the file a hard link names.
    pub link: Option<PathBuf>,
}

/// Hears how far long work is and says when to stop.
///
/// Called on the thread doing the work.
pub trait Observer {
    /// `bytes` more of the archive (or, when creating one, of the files
    /// packed) were read.
    fn advance(&self, bytes: u64) {
        let _ = bytes;
    }

    /// Whether the work should stop now. Checked between reads.
    fn cancelled(&self) -> bool {
        false
    }

    /// `bytes` more were written below the destination. An error stops the
    /// extraction with it, as when the disk is about to be full.
    ///
    /// # Errors
    ///
    /// Whatever should stop the extraction.
    fn wrote(&self, bytes: u64) -> io::Result<()> {
        let _ = bytes;
        Ok(())
    }
}

/// Observes nothing and never stops.
impl Observer for () {}

/// Why work stopped when it was asked to.
#[derive(Debug)]
struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// The error work stops with when its [`Observer`] asked it to.
#[must_use]
pub fn cancelled() -> io::Error {
    io::Error::other(Cancelled)
}

/// Whether `error` is [`cancelled`]: stopped on request, not failed.
#[must_use]
pub fn is_cancelled(error: &io::Error) -> bool {
    caused_by::<Cancelled>(error)
}

/// Whether `T` is behind `error`, through any codec that wrapped it.
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

/// An entry name that would land outside the archive's folder.
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

/// Whether `error` refused an entry that would land outside the archive's
/// folder: an absolute name, `..`, or a link pointing out.
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
    // Folders the archive has an entry of their own for: their time is it.
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
                // A folder seen through its contents first.
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

/// Whether the archive holds a folder at `path`: named, or implied by
/// something inside it.
#[must_use]
pub fn has_directory(entries: &[Entry], path: &Path) -> bool {
    path.as_os_str().is_empty()
        || entries.iter().any(|entry| {
            (entry.path == path && entry.kind == Kind::Directory)
                || (entry.path != path && entry.path.starts_with(path))
        })
}
