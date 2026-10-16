//! Test support: prove that a compositor-loop entry point never waits for
//! the disk.
//!
//! Under I/O or memory pressure one `fsync` can take seconds, and on the
//! compositor's loop thread that is a frozen desktop. A test runs the
//! loop-side entry point through [`on_a_thread_that_cannot_sync`]: a fresh
//! thread whose seccomp filter makes `fsync`, `fdatasync`, `syncfs` and
//! `sync` fail with [`SYNC_DENIED`]. A path that still syncs then fails (or
//! reports the error) instead of passing silently.
//!
//! The filter binds only the calling thread and threads it spawns afterwards
//! (no `SECCOMP_FILTER_FLAG_TSYNC`), never the test harness. The process's
//! state writer and session-start worker are started before the filter goes
//! on, so the work the entry point hands them still syncs on their threads.

/// The errno a denied sync returns, distinctive enough to name in an
/// assertion: waiting for the disk is what would deadlock the desktop.
pub(crate) const SYNC_DENIED: i32 = libc::EDEADLK;

/// Make every fsync-family syscall fail with [`SYNC_DENIED`] on the calling
/// thread only.
///
/// # Panics
///
/// When the filter cannot be installed.
pub(crate) fn deny_disk_syncs_on_this_thread() {
    const DENIED: [libc::c_long; 4] = [
        libc::SYS_fsync,
        libc::SYS_fdatasync,
        libc::SYS_syncfs,
        libc::SYS_sync,
    ];
    // Started here, the writer thread and the session-start worker do not
    // inherit the filter.
    let _ = crate::shell::durable_writer::global();
    let _ = crate::session_runtime::start_worker();
    let statement = |code: u32, k: u32| libc::sock_filter {
        code: u16::try_from(code).expect("BPF opcode fits"),
        jt: 0,
        jf: 0,
        k,
    };
    // Load the syscall number (offset 0 of `seccomp_data`); the architecture
    // word is not checked because the test only issues native syscalls.
    let mut filters = vec![statement(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0)];
    for (index, number) in DENIED.into_iter().enumerate() {
        filters.push(libc::sock_filter {
            // On a match, skip the remaining comparisons and the allow.
            jt: u8::try_from(DENIED.len().saturating_sub(index)).expect("jump fits"),
            ..statement(
                libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
                u32::try_from(number).expect("syscall number fits"),
            )
        });
    }
    filters.push(statement(
        libc::BPF_RET | libc::BPF_K,
        libc::SECCOMP_RET_ALLOW,
    ));
    filters.push(statement(
        libc::BPF_RET | libc::BPF_K,
        libc::SECCOMP_RET_ERRNO | u32::try_from(SYNC_DENIED).expect("errno fits"),
    ));
    let program = libc::sock_fprog {
        len: u16::try_from(filters.len()).expect("program length fits"),
        filter: filters.as_mut_ptr(),
    };
    // SAFETY: scalar prctl calls on the current thread. The kernel copies the
    // program while `filters` is live; the filter only makes syncs fail on
    // this test thread and grants nothing.
    unsafe {
        assert_eq!(
            libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0),
            0,
            "no_new_privs: {}",
            std::io::Error::last_os_error()
        );
        assert_eq!(
            libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER,
                &raw const program
            ),
            0,
            "installing the sync filter: {}",
            std::io::Error::last_os_error()
        );
    }
    // A guard that cannot observe its subject proves nothing.
    let probe = tempfile::tempfile().expect("probe file");
    for denied in [probe.sync_all(), probe.sync_data()] {
        assert_eq!(
            denied
                .expect_err("the sync filter is not in force")
                .raw_os_error(),
            Some(SYNC_DENIED),
            "the sync filter is not in force"
        );
    }
}

/// Run `body` on a new thread that cannot sync to disk, and return its
/// result. `body` may borrow from the caller; a panic in it fails the
/// calling test.
pub(crate) fn on_a_thread_that_cannot_sync<T: Send>(body: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                deny_disk_syncs_on_this_thread();
                body()
            })
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// A FIFO nobody has written yet, at `name` in `directory`: opening it to
/// read blocks until a writer comes, as a file on the FUSE workspace mount
/// does while it hydrates from a peer. A test hands it to a loop entry point
/// to prove the read happens off the loop, then writes the bytes to let it
/// finish.
///
/// # Panics
///
/// When the FIFO cannot be made.
pub(crate) fn blocking_source(directory: &std::path::Path, name: &str) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt as _;
    let path = directory.join(name);
    let raw = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path has no NUL");
    // SAFETY: `raw` is a NUL-terminated path that outlives the call.
    let made = unsafe { libc::mkfifo(raw.as_ptr(), 0o600) };
    assert_eq!(made, 0, "mkfifo: {}", std::io::Error::last_os_error());
    path
}

/// Run `body` on this thread with `fifo` (from [`blocking_source`])
/// watched. Should anything open the FIFO to read while `body` runs, a
/// rescuer writes `bytes` so that read finishes instead of hanging the test,
/// and the second value is `true`: `body` waited on a file. A rescuer that
/// cannot open the FIFO (no reader) leaves it alone.
///
/// # Panics
///
/// When the rescuer thread cannot be started.
pub(crate) fn read_while<T>(
    fifo: &std::path::Path,
    bytes: &[u8],
    body: impl FnOnce() -> T,
) -> (T, bool) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::sync::atomic::{AtomicBool, Ordering};
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let rescuer = scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                // Opening the write end without blocking succeeds only
                // while a reader has the FIFO open.
                if let Ok(mut writer) = std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(fifo)
                {
                    let _ = writer.write_all(bytes);
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            false
        });
        let value = body();
        done.store(true, Ordering::Release);
        let read = rescuer.join().expect("the rescuer finished");
        (value, read)
    })
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
