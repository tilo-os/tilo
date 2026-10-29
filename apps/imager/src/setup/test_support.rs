//! Shared helpers for the setup tests.
//!
//! [`isolated`] runs a merged test phase on its own thread. On Linux,
//! [`qualified_tempdir`] is imager-backend's: a temporary directory on a
//! filesystem the setup code accepts (ext4, XFS or Btrfs), never a tmpfs or
//! overlay `/tmp`.

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

#[cfg(target_os = "linux")]
pub(crate) use imager_backend::test_support::qualified_tempdir;

/// A fresh temporary directory for a setup fixture, removed on drop.
#[cfg(not(target_os = "linux"))]
pub(crate) fn qualified_tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("create temporary directory")
}
