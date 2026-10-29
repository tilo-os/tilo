//! Granted files on a disk this place can read, through the standard
//! library.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use ush_shell::{DirEntry, Kind, Metadata};

use crate::Files;

/// The files under `/` of the place running the shell. The host asks it
/// only about paths the environment's grants allow.
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeFiles;

fn kind_of(file_type: std::fs::FileType) -> Kind {
    if file_type.is_symlink() {
        Kind::Symlink
    } else if file_type.is_dir() {
        Kind::Dir
    } else if file_type.is_file() {
        Kind::File
    } else {
        // A fifo, a socket or a device.
        Kind::Other
    }
}

#[cfg(unix)]
fn metadata_of(meta: &std::fs::Metadata) -> Metadata {
    use std::os::unix::fs::MetadataExt as _;
    Metadata {
        kind: kind_of(meta.file_type()),
        len: meta.len(),
        mode: meta.mode() & 0o7777,
        modified: meta.mtime(),
        uid: meta.uid(),
        gid: meta.gid(),
        nlink: meta.nlink(),
        dev: meta.dev(),
        ino: meta.ino(),
    }
}

#[cfg(not(unix))]
fn metadata_of(meta: &std::fs::Metadata) -> Metadata {
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0);
    let kind = kind_of(meta.file_type());
    Metadata {
        kind,
        len: meta.len(),
        mode: match (kind, meta.permissions().readonly()) {
            (Kind::Dir, _) => 0o755,
            (_, true) => 0o444,
            (_, false) => 0o644,
        },
        modified,
        uid: 0,
        gid: 0,
        nlink: 1,
        dev: 0,
        ino: 0,
    }
}

impl Files for NativeFiles {
    fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        std::fs::metadata(path).map(|m| metadata_of(&m))
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<Metadata> {
        std::fs::symlink_metadata(path).map(|m| metadata_of(&m))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            out.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind: entry.file_type().map_or(Kind::Other, kind_of),
            });
        }
        Ok(out)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        let file = std::fs::File::open(path)?;
        if file.metadata()?.is_dir() {
            return Err(io::ErrorKind::IsADirectory.into());
        }
        Ok(Box::new(file))
    }

    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::read_link(path)
    }

    fn resolve(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }
}
