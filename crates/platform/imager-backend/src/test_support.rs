//! Shared helpers for the setup tests, here and in the imager.
//!
//! [`isolated`] runs a merged test phase on its own thread. [`qualified_tempdir`] roots fixture state
//! and scratch on a filesystem the cache, transfer scratch and prepared-media store accept, below
//! `TILO_IMAGER_TEST_CACHE_ROOT` when set, else below the checkout's `target/test-fs/imager`.

#[cfg(target_os = "linux")]
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

#[cfg(all(feature = "test-support", target_os = "linux"))]
pub fn parse_live_handoff(bytes: &[u8]) -> Result<serde_json::Value, String> {
    tilo_install_helpers::staging_handoff::parse_handoff(bytes)
        .map(|handoff| handoff.json())
        .map_err(|error| error.to_string())
}

/// Runs one merged phase on a fresh thread, so thread-local fault hooks, scripts and counters start
/// clean; a failing phase re-raises its original panic payload.
pub fn isolated(phase: &str, body: impl FnOnce() + Send) {
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

#[cfg(target_os = "linux")]
const ROOT_VAR: &str = "TILO_IMAGER_TEST_CACHE_ROOT";

#[cfg(target_os = "linux")]
fn qualified_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = fixture_root();
        let created = std::fs::create_dir_all(&root);
        assert!(
            created.is_ok(),
            "create test root {}: {:?}",
            root.display(),
            created.err()
        );
        assert_qualified(&root);
        root
    })
}

/// The directory the fixtures root their state and scratch in, as configured, without creating or
/// checking it: the ownership check above the fixtures asks for it before any fixture exists.
#[cfg(target_os = "linux")]
pub fn fixture_root() -> PathBuf {
    std::env::var_os(ROOT_VAR).map_or_else(default_root, PathBuf::from)
}

/// The setup tests' precondition, checked once with the reason, rather than as the cache's own
/// refusal deep inside each test.
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
    let kind = unsafe { stat.assume_init() }.f_type;
    assert!(
        matches!(kind as u64, 0xef53 | 0x5846_5342 | 0x9123_683e),
        "the imager's setup tests need {} on ext4, XFS or Btrfs, not filesystem type {kind:#x}; \
         point {ROOT_VAR} at a directory on one",
        root.display(),
    );
}

/// `<target>/test-fs/imager`, found from the running test binary (`<target>/<profile>/deps/...`), so
/// a custom target directory is honoured.
#[cfg(target_os = "linux")]
fn default_root() -> PathBuf {
    let exe = std::env::current_exe().expect("test binary path");
    let target = exe
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "deps"))
        .and_then(|deps| deps.parent())
        .and_then(|profile| profile.parent());
    assert!(
        target.is_some(),
        "{} is not inside a cargo target directory",
        exe.display()
    );
    target.unwrap().join("test-fs").join("imager")
}

#[cfg(target_os = "linux")]
pub fn qualified_tempdir() -> tempfile::TempDir {
    tempfile::tempdir_in(qualified_root()).expect("create qualified temporary directory")
}

#[cfg(not(target_os = "linux"))]
pub fn qualified_tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("create temporary directory")
}
