//! Real filesystem I/O for the editor: reading a file into a buffer, writing a
//! buffer back atomically, and noticing when a file changed underneath us.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MAX_TEXT_BYTES: u64 = 24 * 1024 * 1024;

/// What a file's bytes were when a buffer last agreed with them: their modified time, in
/// nanoseconds since the epoch, and their length.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Stamp {
    pub mtime: Option<i64>,
    pub len: u64,
}

impl Stamp {
    pub fn of(path: &Path) -> io::Result<Stamp> {
        let md = std::fs::metadata(path)?;
        Ok(Stamp {
            mtime: md
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|since| i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)),
            len: md.len(),
        })
    }
}

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    TooLarge(u64),
    /// The bytes are not UTF-8 text (or contain NULs) — opening it as a text
    /// buffer would corrupt it on the next save.
    NotText,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "{e}"),
            LoadError::TooLarge(n) => write!(
                f,
                "file is {n} bytes — too large to open as text (limit {MAX_TEXT_BYTES} bytes)"
            ),
            LoadError::NotText => write!(f, "not a text file"),
        }
    }
}

#[derive(Debug)]
pub enum SaveError {
    Io(io::Error),
    Conflict,
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Io(e) => write!(f, "{e}"),
            SaveError::Conflict => write!(f, "the file changed on disk since it was opened"),
        }
    }
}

/// A file's bytes as text, plus the stamp they were read at.
#[derive(Debug)]
pub struct Loaded {
    pub text: String,
    pub stamp: Stamp,
}

/// Read `path` as a text buffer, refusing what must not become one. The size check runs on the
/// metadata before the read, so a huge file costs a `stat`.
pub fn load(path: &Path) -> Result<Loaded, LoadError> {
    let md = std::fs::metadata(path).map_err(LoadError::Io)?;
    if md.len() > MAX_TEXT_BYTES {
        return Err(LoadError::TooLarge(md.len()));
    }
    let file = std::fs::File::open(path).map_err(LoadError::Io)?;
    let text = read_body(file)?;
    Ok(Loaded {
        text,
        stamp: Stamp {
            mtime: Stamp::of(path).ok().and_then(|stamp| stamp.mtime),
            len: md.len(),
        },
    })
}

fn read_body(reader: impl Read) -> Result<String, LoadError> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_TEXT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(LoadError::Io)?;
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        return Err(LoadError::TooLarge(bytes.len() as u64));
    }
    if !looks_like_text(&bytes) {
        return Err(LoadError::NotText);
    }
    String::from_utf8(bytes).map_err(|_| LoadError::NotText)
}

/// Write `text` to `path` atomically, refusing when the file moved on. `expected` is the stamp the
/// buffer agreed with; `None` writes unconditionally, which is the overwrite the person chose after
/// seeing a conflict.
pub fn save(path: &Path, text: &str, expected: Option<&Stamp>) -> Result<Stamp, SaveError> {
    if let Some(expected) = expected {
        match Stamp::of(path) {
            Ok(current) if &current != expected => return Err(SaveError::Conflict),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(SaveError::Io(e)),
        }
    }
    write(path, text.as_bytes()).map_err(SaveError::Io)?;
    Stamp::of(path).map_err(SaveError::Io)
}

/// Replace `path`'s bytes atomically. An existing file stays the person's: a script keeps its
/// executable bit, a symbolic link keeps pointing at its target, and a new path is created with its
/// folders.
fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => tilo_files::native::atomic::write_preserving_target(path, bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            tilo_files::native::atomic::write_creating_parent(path, bytes)
        }
        Err(e) => Err(e),
    }
}

pub fn changed_on_disk(path: &Path, stamp: &Stamp) -> bool {
    match Stamp::of(path) {
        Ok(current) => &current != stamp,
        Err(_) => true,
    }
}

/// A NUL byte in the first 8 KiB: source files do not hold one, ELF objects, images and archives do.
fn looks_like_text(bytes: &[u8]) -> bool {
    let head = bytes.get(..bytes.len().min(8192)).unwrap_or(bytes);
    !head.contains(&0)
}

/// Directory names never worth walking or listing in a source project: build
/// output and VCS internals. On this repository `target/` alone is 150+ GB, so
/// this is the difference between a project tree that opens and one that does
/// not.
pub const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".jj",
    "target",
    "node_modules",
    ".venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".cargo",
    ".direnv",
    "dist",
    ".next",
];

pub fn is_ignored_dir(name: &str) -> bool {
    IGNORED_DIRS.contains(&name)
}

pub fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
