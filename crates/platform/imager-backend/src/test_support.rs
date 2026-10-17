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

/// Parse with the same implementation used by `tilo-staging-handoff`.
///
/// # Errors
/// Refuses malformed, oversized or inconsistent handoff data.
#[cfg(all(feature = "test-support", target_os = "linux"))]
pub fn parse_live_handoff(bytes: &[u8]) -> Result<serde_json::Value, String> {
    tilo_install_helpers::staging_handoff::parse_handoff(bytes)
        .map(|handoff| handoff.json())
        .map_err(|error| error.to_string())
}

/// Runs one merged phase on a fresh thread so the thread-local fault hooks,
/// scripts and counters start clean, exactly as when the phase was its own
/// test; a failing phase re-raises its original panic payload.
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
