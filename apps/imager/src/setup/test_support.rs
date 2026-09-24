//! Temporary directories for the native internal-cache tests.
//!
//! The cache refuses filesystems without Linux ext4/XFS/Btrfs semantics, so
//! its fixtures cannot use a tmpfs `/tmp`. They are created below
//! `QOS_IMAGER_TEST_CACHE_ROOT` when set (CI points it at a prepared
//! location), else below the build's own `target/test-fs/imager`, which lives
//! in the checkout and so on the developer's filesystem — never in the cache
//! directory of the account running the tests.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Environment variable naming a directory on a qualified local filesystem.
const ROOT_VAR: &str = "QOS_IMAGER_TEST_CACHE_ROOT";

fn qualified_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::var_os(ROOT_VAR).map_or_else(default_root, PathBuf::from);
        std::fs::create_dir_all(&root)
            .unwrap_or_else(|error| panic!("create test root {}: {error}", root.display()));
        root
    })
}

/// `<target>/test-fs/imager`, found from the running test binary
/// (`<target>/<profile>/deps/imager-…`), so a custom target directory is
/// honoured.
fn default_root() -> PathBuf {
    let exe = std::env::current_exe().expect("test binary path");
    let target = exe
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "deps"))
        .and_then(|deps| deps.parent())
        .and_then(|profile| profile.parent())
        .unwrap_or_else(|| panic!("{} is not inside a cargo target directory", exe.display()));
    target.join("test-fs").join("imager")
}

/// A fresh temporary directory on a qualified filesystem, removed on drop.
pub(crate) fn qualified_tempdir() -> tempfile::TempDir {
    tempfile::tempdir_in(qualified_root()).expect("create qualified temporary directory")
}
