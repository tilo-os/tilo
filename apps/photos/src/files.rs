//! Photos' own state files, and the photographs a user names by path, through `tilo-files`.

use std::path::Path;

use tilo_files::{Dir, VPath, blocking};
use ui::framework::ResourceApp;

use crate::resource::PhotosShell;

#[derive(Clone, Debug)]
pub(crate) struct StateFile {
    dir: Dir,
    name: VPath,
}

impl StateFile {
    pub(crate) fn in_state(rel: &str) -> Option<Self> {
        let paths = tilo_paths::AppPaths::for_app(<PhotosShell as ResourceApp>::ID);
        let dir = Dir::open(paths.state()).ok()?;
        Some(Self {
            dir,
            name: VPath::new(rel).ok()?,
        })
    }

    #[cfg(test)]
    pub(crate) fn at(path: &Path) -> Option<Self> {
        let (dir, name) = tilo_files::file_at(path).ok()?;
        Some(Self { dir, name })
    }

    pub(crate) fn read(&self) -> Option<Vec<u8>> {
        blocking::read(&self.dir, &self.name).ok()
    }

    pub(crate) fn write(&self, bytes: &[u8]) -> Result<(), String> {
        blocking::write_atomic(&self.dir, &self.name, bytes).map_err(|error| error.to_string())
    }

    pub(crate) fn write_json(&self, value: &impl serde::Serialize) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        self.write(&bytes)
    }
}

pub(crate) fn read_native(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let (dir, name) = tilo_files::file_at(path).ok()?;
    blocking::read_bounded(&dir, &name, usize::try_from(limit).unwrap_or(usize::MAX)).ok()?
}
