//! The workspace files served read only on the profile's
//! [`crate::FILES_SOCKET`], for what shows them apart from the runtime's
//! own folder: the macOS File Provider extension (`tilo-file-provider`).

use runtime_fs::{DirEntry, EntryKind, Metadata, workspace_server::WorkspaceBackend};
use std::{ffi::OsStr, os::unix::ffi::OsStrExt as _, path::Path, sync::Arc};
use tilo_fs::fs::{QuickFS, native_ops::TreeEntry};

pub(super) struct Backend(pub(super) Arc<QuickFS>);

fn refused(path: &str, errno: i32) -> String {
    format!("{path}: {}", std::io::Error::from_raw_os_error(errno))
}

fn metadata(entry: &TreeEntry) -> Metadata {
    let seconds = i64::try_from(entry.mtime_ms / 1000).unwrap_or(i64::MAX);
    Metadata {
        kind: if entry.dir {
            EntryKind::Directory
        } else if entry.posix.symlink {
            EntryKind::Symlink
        } else {
            EntryKind::File
        },
        size: entry.size,
        modified_unix_seconds: (seconds > 0).then_some(seconds),
        needs_hydration: !entry.known,
    }
}

impl WorkspaceBackend for Backend {
    fn list(&self, path: &str) -> Result<Vec<DirEntry>, String> {
        let entries = self
            .0
            .list_path(Path::new(path))
            .map_err(|errno| refused(path, errno))?;
        Ok(entries
            .iter()
            .filter_map(|entry| {
                let name = entry.path.last()?;
                Some(DirEntry {
                    name: OsStr::from_bytes(name).to_string_lossy().into_owned(),
                    metadata: metadata(entry),
                })
            })
            .collect())
    }

    fn stat(&self, path: &str) -> Result<Metadata, String> {
        self.0
            .entry_at(Path::new(path))
            .map(|entry| metadata(&entry))
            .ok_or_else(|| refused(path, libc::ENOENT))
    }

    fn read(&self, path: &str, offset: u64, max_len: u32) -> Result<Vec<u8>, String> {
        self.0
            .read_path_range(Path::new(path), offset, u64::from(max_len))
            .map_err(|errno| refused(path, errno))
    }
}
