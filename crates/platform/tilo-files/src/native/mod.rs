//! Path-based operations for desktop-only code whose paths are a contract with other
//! programs: identity files, sockets' lock files, documents.

pub mod atomic;
#[cfg(all(
    feature = "watch",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
pub(crate) mod change_watch;
#[cfg(all(
    feature = "watch",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
pub use change_watch::{
    ChangeWatch, Event as WatchEvent, THREAD_NAME as CHANGE_WATCH_THREAD, Watcher, watch_changes,
    watch_presence, watch_tree_changes,
};
#[cfg(all(
    feature = "watch",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
mod watched;
#[cfg(all(
    feature = "watch",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
pub use watched::Watched;
pub mod bounded;
pub mod checked;
mod file_stamp;
pub use file_stamp::{FileStamp, SETTLE};
#[cfg(unix)]
pub mod guardian;
pub mod held;
pub mod identity;
pub mod lock;
pub mod mapped;
pub mod migration;
pub mod nofollow;
mod positioned;
pub use positioned::FileAt;
pub mod private;
pub(crate) mod rename;
pub use rename::{FileRename, rename_file_noreplace};
// The browser's origin-private file system has no path-named socket; every
// other host has the module: Unix-domain sockets, and named pipes on Windows.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use tilo_socket as socket;
pub mod tree;
#[cfg(unix)]
pub mod x11_authority;

/// What a file's length and change time came to, where it is there: a follower reads it again only
/// when one of them moves.
#[must_use]
pub fn stamp(path: &std::path::Path) -> Option<(u64, Option<std::time::SystemTime>)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.len(), metadata.modified().ok()))
}

/// Whether the native path itself names a regular file, without following a symlink.
pub fn is_regular_file(path: &std::path::Path) -> std::io::Result<bool> {
    std::fs::symlink_metadata(path).map(|metadata| metadata.is_file())
}

/// The bytes the file takes on its disk, as `du` counts them: its allocated blocks where the platform
/// reports them, so a sparse file counts what it holds rather than the length it claims.
#[must_use]
pub fn on_disk(metadata: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // `st_blocks` counts 512-byte units whatever the filesystem's block size.
        metadata.blocks().saturating_mul(512)
    }
    #[cfg(not(unix))]
    {
        metadata.len()
    }
}

/// The file by device and inode, when it has more than one name (a hard link), so a walk counts it
/// once; `None` for one name, and where the platform has no inodes.
#[must_use]
pub fn linked_identity(metadata: &std::fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        (metadata.nlink() > 1).then(|| (metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

pub fn entry_names(dir: &std::path::Path) -> std::io::Result<Vec<std::ffi::OsString>> {
    std::fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect()
}

/// Creates the parents of a path shared with another program; unlike private-directory provisioning
/// this keeps existing modes.
pub fn create_dir_all(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

/// Watches entries directly in `dir` — created, replaced or removed — but not what happens below its
/// subdirectories, so a busy subtree does not wake the watcher. Paths are reported relative to `dir`,
/// and a file atomically replaced there keeps reporting.
#[cfg(all(
    feature = "watch",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
pub fn watch_entries(dir: &std::path::Path) -> Result<crate::Watch, crate::FsError> {
    change_watch::stream(dir, &crate::VPath::root(), false)
}

/// The extended attribute a FUSE filesystem answers on its directories when it tells this kernel of
/// every change it did not receive through inotify.
pub const CHANGE_SIGNAL_XATTR: &str = "user.tilo.change-signal";

/// Whether a watch on `dir` sees every change to its entries. Not on a FUSE or network mount (virtiofs,
/// NFS, SMB, 9P, Ceph, AFS), whose entries can change without passing through this kernel, unless it is
/// a FUSE mount answering [`CHANGE_SIGNAL_XATTR`]; and not where the filesystem's type cannot be told,
/// where a watch reports local edits only and whoever lists `dir` must read it again for the rest.
#[cfg(all(
    feature = "watch",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
#[must_use]
pub fn watch_sees_every_change(dir: &std::path::Path) -> bool {
    change_watch::sees_every_change(dir)
}
