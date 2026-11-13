//! Shared helpers for the setup tests.

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

#[cfg(not(target_os = "linux"))]
pub(crate) fn qualified_tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("create temporary directory")
}
