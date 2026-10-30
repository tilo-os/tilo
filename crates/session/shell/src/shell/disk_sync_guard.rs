//! Test support: prove that a compositor-loop entry point never waits for
//! the disk ([`tilo_test_env::disk_sync`]), with the process's state writer
//! and session-start worker started before the filter goes on, so the work
//! the entry point hands them still syncs on their threads.

pub(crate) use tilo_test_env::disk_sync::blocking_source;

/// Start the threads that sync on the guarded thread's behalf, so they do
/// not inherit its filter.
fn start_writers() {
    let _ = crate::shell::durable_writer::global();
    let _ = crate::session_runtime::start_worker();
}

/// Make every fsync-family syscall fail with [`SYNC_DENIED`] on the calling
/// thread only.
pub(crate) fn deny_disk_syncs_on_this_thread() {
    start_writers();
    tilo_test_env::disk_sync::deny_disk_syncs_on_this_thread();
}

/// Run `body` on a new thread that cannot sync to disk, and return its
/// result.
pub(crate) fn on_a_thread_that_cannot_sync<T: Send>(body: impl FnOnce() -> T + Send) -> T {
    start_writers();
    tilo_test_env::disk_sync::on_a_thread_that_cannot_sync(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_denied_thread_cannot_sync_but_the_state_writer_still_can() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("written");
        let target = path.clone();
        let refused = on_a_thread_that_cannot_sync(move || {
            let direct = tilo_files::native::atomic::write_with_mode(&target, b"x", 0o600);
            crate::shell::durable_writer::global().submit(target.clone(), move || {
                tilo_files::native::atomic::write_with_mode(&target, b"queued", 0o600)
                    .expect("the state writer syncs on its own thread");
            });
            direct.is_err()
        });
        assert!(refused, "a synced write on the guarded thread must fail");
        crate::shell::durable_writer::global().flush();
        assert_eq!(std::fs::read(&path).unwrap(), b"queued");
    }
}
