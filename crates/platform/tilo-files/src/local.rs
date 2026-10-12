//! The backends that answer synchronously: native directories, memory and,
//! on the web, the page's mirror of a profile's app files.
//! [`Dir`](crate::Dir)'s async operations and [`blocking`](crate::blocking)
//! both call these.

use std::path::PathBuf;

use crate::{Entry, FsError, Held, Kind, LockMode, Meta, VPath, disk, memory};

const BAD_NAME: &str = "a listed name is not a path segment";

#[derive(Clone)]
pub(crate) enum Local {
    Disk(PathBuf),
    Memory(memory::Scope),
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    Mirror(crate::web::View),
}

/// The page's thread cannot wait for a lock, and the mirror cannot take a
/// lock other tabs see without waiting.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn no_blocking_locks() -> FsError {
    FsError::Unsupported {
        feature: crate::Feature::Blocking,
        reason: crate::caps::WEB_BLOCKING_LOCKS,
    }
}

impl Local {
    pub(crate) fn read(&self, path: &VPath) -> Result<Vec<u8>, FsError> {
        match self {
            Self::Disk(root) => disk::read(root, path),
            Self::Memory(scope) => scope.read(path),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Self::Mirror(view) => view.read(path),
        }
    }

    pub(crate) fn read_prefix(&self, path: &VPath, max: usize) -> Result<Vec<u8>, FsError> {
        match self {
            Self::Disk(root) => disk::read_prefix(root, path, max),
            Self::Memory(scope) => scope.read(path).map(|bytes| truncated(bytes, max)),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Self::Mirror(view) => view.read(path).map(|bytes| truncated(bytes, max)),
        }
    }

    pub(crate) fn append(&self, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
        match self {
            Self::Disk(root) => disk::append(root, path, bytes),
            Self::Memory(scope) => {
                let joined = appended(scope.read(path), bytes)?;
                scope.write_atomic(path, &joined)
            }
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Self::Mirror(view) => {
                let joined = appended(view.read(path), bytes)?;
                view.write_atomic(path, &joined)
            }
        }
    }

    pub(crate) fn write(&self, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
        match self {
            Self::Disk(root) => disk::write(root, path, bytes),
            Self::Memory(scope) => scope.write_atomic(path, bytes),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Self::Mirror(view) => view.write_atomic(path, bytes),
        }
    }

    pub(crate) fn write_atomic(&self, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
        match self {
            Self::Disk(root) => disk::write_atomic(root, path, bytes),
            Self::Memory(scope) => scope.write_atomic(path, bytes),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Self::Mirror(view) => view.write_atomic(path, bytes),
        }
    }

    pub(crate) fn save_document(&self, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
        match self {
            Self::Disk(root) => disk::save_document(root, path, bytes),
            Self::Memory(scope) => scope.write_atomic(path, bytes),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Self::Mirror(view) => view.write_atomic(path, bytes),
        }
    }

    pub(crate) fn write_private(&self, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
        match self {
            Self::Disk(root) => disk::write_private(root, path, bytes),
            Self::Memory(scope) => scope.write_atomic(path, bytes),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Self::Mirror(view) => view.write_atomic(path, bytes),
        }
    }

    pub(crate) fn create_new(&self, path: &VPath, bytes: &[u8]) -> Result<bool, FsError> {
        match self {
            Self::Disk(root) => disk::create_new(root, path, bytes),
            Self::Memory(scope) => scope.create_new(path, bytes),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Self::Mirror(view) => view.create_new(path, bytes),
        }
    }

    pub(crate) fn rename_noreplace(&self, from: &VPath, to: &VPath) -> Result<(), FsError> {
        match self {
            Self::Disk(root) => disk::rename_noreplace(root, from, to),
            Self::Memory(scope) => scope.rename_noreplace(from, to),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Self::Mirror(view) => view.rename_noreplace(from, to),
        }
    }

    pub(crate) fn remove(&self, path: &VPath) -> Result<(), FsError> {
        match self {
            Self::Disk(root) => disk::remove(root, path),
            Self::Memory(scope) => scope.remove(path),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Self::Mirror(view) => view.remove(path),
        }
    }

    pub(crate) fn remove_all(&self, path: &VPath) -> Result<(), FsError> {
        if let Self::Disk(root) = self {
            return disk::remove_all(root, path);
        }
        // Memory trees and the page's mirror hold no links to follow.
        if self.metadata(path)?.kind == Kind::Dir {
            for entry in self.list(path)? {
                let name = VPath::new(&entry.name).map_err(|_| FsError::InvalidPath(BAD_NAME))?;
                self.remove_all(&path.join(&name))?;
            }
        }
        self.remove(path)
    }

    pub(crate) fn create_dir_all(&self, path: &VPath) -> Result<(), FsError> {
        match self {
            Self::Disk(root) => disk::create_dir_all(root, path),
            Self::Memory(scope) => scope.create_dir_all(path),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Self::Mirror(view) => view.create_dir_all(path),
        }
    }

    pub(crate) fn list(&self, path: &VPath) -> Result<Vec<Entry>, FsError> {
        match self {
            Self::Disk(root) => disk::list(root, path),
            Self::Memory(scope) => scope.list(path),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Self::Mirror(view) => view.list(path),
        }
    }

    pub(crate) fn metadata(&self, path: &VPath) -> Result<Meta, FsError> {
        match self {
            Self::Disk(root) => disk::metadata(root, path),
            Self::Memory(scope) => scope.metadata(path),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Self::Mirror(view) => view.metadata(path),
        }
    }

    /// Waits for the lock: in the kernel natively, on the task's waker in
    /// memory.
    pub(crate) async fn lock(&self, path: &VPath, mode: LockMode) -> Result<Held, FsError> {
        match self {
            Self::Disk(root) => disk::lock(root, path, mode).map(|lock| Held::Disk { _lock: lock }),
            Self::Memory(scope) => scope
                .lock(path, mode)
                .await
                .map(|lock| Held::Memory { _lock: lock }),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Self::Mirror(_) => Err(no_blocking_locks()),
        }
    }

    /// Waits for the lock on this thread.
    pub(crate) fn lock_blocking(&self, path: &VPath, mode: LockMode) -> Result<Held, FsError> {
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        {
            let _ = (self, path, mode);
            Err(no_blocking_locks())
        }
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        match self {
            Self::Disk(root) => disk::lock(root, path, mode).map(|lock| Held::Disk { _lock: lock }),
            Self::Memory(scope) => scope
                .lock_blocking(path, mode)
                .map(|lock| Held::Memory { _lock: lock }),
        }
    }

    /// Takes the lock only if nobody holds a conflicting one.
    pub(crate) fn try_lock(&self, path: &VPath, mode: LockMode) -> Result<Option<Held>, FsError> {
        match self {
            Self::Disk(root) => disk::try_lock(root, path, mode)
                .map(|held| held.map(|lock| Held::Disk { _lock: lock })),
            Self::Memory(scope) => scope
                .try_lock(path, mode)
                .map(|held| held.map(|lock| Held::Memory { _lock: lock })),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            Self::Mirror(_) => Err(no_blocking_locks()),
        }
    }
}

fn truncated(mut bytes: Vec<u8>, max: usize) -> Vec<u8> {
    bytes.truncate(max);
    bytes
}

fn appended(existing: Result<Vec<u8>, FsError>, bytes: &[u8]) -> Result<Vec<u8>, FsError> {
    let mut joined = match existing {
        Ok(existing) => existing,
        Err(FsError::NotFound) => Vec::new(),
        Err(error) => return Err(error),
    };
    joined.extend_from_slice(bytes);
    Ok(joined)
}
