//! Music's cache, profile and local files by native path, through
//! `tilo-files`.

use std::io;
use std::path::Path;

use tilo_files::{FsError, Meta, blocking};

fn at(path: &Path) -> io::Result<(tilo_files::Dir, tilo_files::VPath)> {
    tilo_files::file_at(path).map_err(io::Error::other)
}

fn io(error: FsError) -> io::Error {
    match error {
        FsError::NotFound => io::ErrorKind::NotFound.into(),
        FsError::AlreadyExists => io::ErrorKind::AlreadyExists.into(),
        other => io::Error::other(other),
    }
}

pub(crate) fn metadata(path: &Path) -> io::Result<Meta> {
    let (dir, name) = at(path)?;
    blocking::metadata(&dir, &name).map_err(io)
}

/// A non-empty regular file.
pub(crate) fn is_nonempty_file(path: &Path) -> bool {
    metadata(path).is_ok_and(|meta| meta.kind == tilo_files::Kind::File && meta.len > 0)
}

pub(crate) fn read(path: &Path) -> io::Result<Vec<u8>> {
    let (dir, name) = at(path)?;
    blocking::read(&dir, &name).map_err(io)
}

/// Replace the file atomically, creating its folders.
pub(crate) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (dir, name) = at(path)?;
    blocking::write_atomic(&dir, &name, bytes).map_err(io)
}

pub(crate) fn remove(path: &Path) {
    if let Ok((dir, name)) = at(path) {
        let _ = blocking::remove(&dir, &name);
    }
}

pub(crate) fn create_dir_all(path: &Path) -> io::Result<()> {
    let (dir, name) = at(path)?;
    blocking::create_dir_all(&dir, &name).map_err(io)
}

/// Move `from` to `to` in the same folder tree, never replacing `to`.
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    let common =
        common_root(from, to).ok_or_else(|| io::Error::other("the paths share no folder"))?;
    let dir = tilo_files::Dir::open(tilo_files::Location::Native(common.to_path_buf()))
        .map_err(io::Error::other)?;
    let rel = |path: &Path| {
        path.strip_prefix(common)
            .ok()
            .and_then(|rel| rel.to_str())
            .and_then(|rel| tilo_files::VPath::new(rel).ok())
            .ok_or_else(|| io::Error::other("the path is not below the shared folder"))
    };
    blocking::rename_noreplace(&dir, &rel(from)?, &rel(to)?).map_err(io)
}

fn common_root<'a>(a: &'a Path, b: &Path) -> Option<&'a Path> {
    a.ancestors()
        .skip(1)
        .find(|ancestor| b.starts_with(ancestor))
}

/// A file a download grows while playback follows it.
#[cfg(all(feature = "audio", feature = "live"))]
pub(crate) struct Growing {
    dir: tilo_files::Dir,
    name: tilo_files::VPath,
}

#[cfg(all(feature = "audio", feature = "live"))]
impl Growing {
    /// Start `path` empty.
    pub(crate) fn create(path: &Path) -> io::Result<Self> {
        let (dir, name) = at(path)?;
        blocking::write_atomic(&dir, &name, &[]).map_err(io)?;
        Ok(Self { dir, name })
    }

    /// Empty it again, for a download that restarts.
    pub(crate) fn reset(&self) -> io::Result<()> {
        blocking::write_atomic(&self.dir, &self.name, &[]).map_err(io)
    }
}

#[cfg(all(feature = "audio", feature = "live"))]
impl io::Write for Growing {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        blocking::append(&self.dir, &self.name, buf).map_err(io)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
