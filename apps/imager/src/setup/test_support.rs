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
        assert_qualified(&root);
        root
    })
}

/// The cache tests' precondition, checked once with the reason a run lacks it,
/// rather than as the cache's own refusal deep inside each test.
#[cfg(target_os = "linux")]
fn assert_qualified(root: &Path) {
    use std::os::unix::ffi::OsStrExt as _;
    let path = std::ffi::CString::new(root.as_os_str().as_bytes()).expect("test root path");
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: NUL-terminated path and writable statfs storage.
    let result = unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) };
    assert_eq!(
        result,
        0,
        "statfs {}: {}",
        root.display(),
        std::io::Error::last_os_error()
    );
    // SAFETY: a successful statfs initialized the storage.
    let kind = unsafe { stat.assume_init() }.f_type;
    assert!(
        matches!(kind as u64, 0xef53 | 0x5846_5342 | 0x9123_683e),
        "the imager's cache tests need {} on ext4, XFS or Btrfs, not filesystem type {kind:#x}; \
         point {ROOT_VAR} at a directory on one",
        root.display(),
    );
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
