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
pub mod held;
pub mod identity;
pub mod lock;
pub mod mapped;
pub mod migration;
pub mod nofollow;
pub mod private;
pub mod retained_home;
#[cfg(unix)]
pub mod socket;
pub mod tree;

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

#[cfg(test)]
mod tests {
    #[test]
    fn native_regular_file_query_preserves_non_file_and_missing_refusals() {
        let root = tempfile::tempdir().expect("temporary native directory");
        assert!(!super::is_regular_file(root.path()).expect("directory metadata"));
        let path = root.path().join("file");
        assert_eq!(
            super::is_regular_file(&path)
                .expect_err("missing file")
                .kind(),
            std::io::ErrorKind::NotFound
        );
        std::fs::write(&path, b"held").expect("native file");
        assert!(super::is_regular_file(&path).expect("regular file metadata"));
    }

    #[cfg(unix)]
    #[test]
    fn native_regular_file_query_rejects_matching_and_dangling_symlinks() {
        let root = tempfile::tempdir().expect("temporary native directory");
        let target = root.path().join("file");
        let alias = root.path().join("alias");
        std::fs::write(&target, b"held").expect("native file");
        std::os::unix::fs::symlink(&target, &alias).expect("symlink fixture");
        assert!(!super::is_regular_file(&alias).expect("symlink metadata"));
        std::fs::remove_file(&target).expect("remove owned fixture target");
        assert!(!super::is_regular_file(&alias).expect("dangling symlink metadata"));
    }

    #[test]
    fn native_directory_creation_preserves_recursive_and_failure_contracts() {
        let root = tempfile::tempdir().expect("temporary native directory");
        let nested = root.path().join("parent/child");
        super::create_dir_all(&nested).expect("recursive creation");
        super::create_dir_all(&nested).expect("existing directory remains valid");
        assert!(nested.is_dir());
        let file = root.path().join("file");
        std::fs::write(&file, b"held").expect("native file");
        let requested = file.join("child");
        let expected = std::fs::create_dir_all(&requested).expect_err("file is not a directory");
        let actual = super::create_dir_all(&requested).expect_err("same native refusal");
        assert_eq!(actual.kind(), expected.kind());
        assert_eq!(actual.raw_os_error(), expected.raw_os_error());
        assert_eq!(std::fs::read(file).expect("original file remains"), b"held");
    }

    #[test]
    #[cfg(unix)]
    fn a_file_counts_its_blocks_and_a_hard_link_names_the_same_file() {
        let root = tempfile::tempdir().expect("temporary native directory");
        let sparse = root.path().join("disk.img");
        std::fs::File::create(&sparse)
            .and_then(|file| file.set_len(1_073_741_824))
            .expect("sparse file");
        let sparse = std::fs::metadata(&sparse).expect("sparse metadata");
        assert!(
            super::on_disk(&sparse) < 1_048_576,
            "an empty 1 GiB file holds no GiB of blocks"
        );
        assert_eq!(super::linked_identity(&sparse), None, "one name");

        let first = root.path().join("first");
        let second = root.path().join("second");
        // Bytes no filesystem compresses away.
        let bytes: Vec<u8> = (0..8192_u32)
            .map(|i| i.wrapping_mul(2_654_435_761).to_le_bytes()[3])
            .collect();
        std::fs::write(&first, bytes).expect("native file");
        std::fs::hard_link(&first, &second).expect("hard link");
        let first = std::fs::metadata(&first).expect("first name");
        let second = std::fs::metadata(&second).expect("second name");
        assert!(super::on_disk(&first) > 0, "written bytes take blocks");
        assert!(super::linked_identity(&first).is_some());
        assert_eq!(
            super::linked_identity(&first),
            super::linked_identity(&second)
        );
    }
}
