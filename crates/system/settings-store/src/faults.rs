//! Faults a test injects into the store's own writes, so what the store does
//! when a write fails is tested the same way whoever runs the test: a
//! directory's permissions do not stop a process running as root.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

static REFUSED_COPIES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// While held, every replacement of the settings copy at its path fails as
/// a write the file system refused. Reading the copy still works.
#[must_use = "the fault lasts as long as the guard"]
pub struct CopyWritesRefused(PathBuf);

/// Refuse replacements of the settings copy at `copy` until the returned
/// guard is dropped.
pub fn refuse_copy_writes(copy: &Path) -> CopyWritesRefused {
    REFUSED_COPIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(copy.to_path_buf());
    CopyWritesRefused(copy.to_path_buf())
}

impl Drop for CopyWritesRefused {
    fn drop(&mut self) {
        let mut refused = REFUSED_COPIES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(at) = refused.iter().position(|path| *path == self.0) {
            refused.remove(at);
        }
    }
}

/// The failure a refused replacement of `copy` answers, if one is injected.
pub(crate) fn copy_write_refused(copy: &Path) -> Option<std::io::Error> {
    REFUSED_COPIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|path| path == copy)
        .then(|| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "a test refused replacing the settings copy",
            )
        })
}
