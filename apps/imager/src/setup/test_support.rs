//! Shared helpers for the setup tests.
//!
//! [`isolated`] runs a merged test phase on its own thread. On Linux,
//! [`qualified_tempdir`] provides temporary directories for the native
//! internal-cache tests: the cache refuses filesystems without Linux
//! ext4/XFS/Btrfs semantics, so its fixtures cannot use a tmpfs `/tmp`. They
//! are created below `TILO_IMAGER_TEST_CACHE_ROOT` when set (CI points it at
//! a prepared location), else below the build's own `target/test-fs/imager`,
//! which lives in the checkout and so on the developer's filesystem — never
//! in the cache directory of the account running the tests.

#[cfg(target_os = "linux")]
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// Runs one merged phase on a fresh thread so the thread-local fault hooks,
/// scripts and counters start clean, exactly as when the phase was its own
/// test; a failing phase re-raises its original panic payload.
pub(crate) fn isolated(phase: &str, body: impl FnOnce() + Send) {
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .name(phase.to_owned())
            .spawn_scoped(scope, body)
            .expect("spawn isolated test phase");
        if let Err(panic) = handle.join() {
            std::panic::resume_unwind(panic);
        }
    });
}

/// Environment variable naming a directory on a qualified local filesystem.
#[cfg(target_os = "linux")]
const ROOT_VAR: &str = "TILO_IMAGER_TEST_CACHE_ROOT";

#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
pub(crate) fn qualified_tempdir() -> tempfile::TempDir {
    tempfile::tempdir_in(qualified_root()).expect("create qualified temporary directory")
}
