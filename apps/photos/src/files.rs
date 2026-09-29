//! Photos' own state files, and the photographs a user names by path, through
//! `tilo-files`.

use std::path::Path;

use tilo_files::{Dir, Kind, VPath, blocking};

/// Whether chosen files have native paths here; without them (a browser
/// page) Photos imports by contents.
pub(crate) fn native_paths() -> bool {
    matches!(
        tilo_files::availability(tilo_files::Feature::NativePaths),
        tilo_files::Availability::Available
    )
}

/// One of Photos' state files: in the shared state location
/// (`$XDG_STATE_HOME/tilo` natively, the page's storage on the web), or at
/// an explicit native path.
#[derive(Clone, Debug)]
pub(crate) struct StateFile {
    dir: Dir,
    name: VPath,
}

impl StateFile {
    /// `rel` below the shared state location.
    pub(crate) fn in_state(rel: &str) -> Option<Self> {
        let dir = Dir::open(tilo_paths::AppPaths::for_app("tilo").state()).ok()?;
        Some(Self {
            dir,
            name: VPath::new(rel).ok()?,
        })
    }

    /// The file at `path`.
    pub(crate) fn at(path: &Path) -> Option<Self> {
        let (dir, name) = tilo_files::file_at(path).ok()?;
        Some(Self { dir, name })
    }

    pub(crate) fn read(&self) -> Option<Vec<u8>> {
        blocking::read(&self.dir, &self.name).ok()
    }

    /// Replace the file atomically, creating its folders.
    pub(crate) fn write(&self, bytes: &[u8]) -> bool {
        blocking::write_atomic(&self.dir, &self.name, bytes).is_ok()
    }
}

/// A regular file the user named, whole, when it is at most `limit` bytes.
pub(crate) fn read_native(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let (dir, name) = tilo_files::file_at(path).ok()?;
    let meta = blocking::metadata(&dir, &name).ok()?;
    if meta.kind != Kind::File || meta.len > limit {
        return None;
    }
    let bytes = blocking::read(&dir, &name).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}
