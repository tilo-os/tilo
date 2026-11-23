//! Prove that a private file has its final mode at creation, before any
//! `chmod` or `fchmod` can narrow it.
//!
//! Under `umask(0)`, a writer that creates a file with `0666` (`File::create`,
//! `fs::write`) and narrows it with `set_permissions(0o600)` leaves a window
//! where the file is readable by anyone. A test runs the writer inside
//! [`on_a_thread_without_chmod`]: a fresh thread whose seccomp filter turns
//! `chmod`, `fchmod`, `fchmodat` and `fchmodat2` into no-ops returning `0`.
//! Neither a writer's own `set_permissions` nor an atomic backend's post-open
//! `fchmod` can change an inode's mode on that thread, so a file reaches its
//! final mode only if `open`/`openat` created it with that mode.
//!
//! The filter binds only the calling thread and threads it spawns afterwards
//! (no `SECCOMP_FILTER_FLAG_TSYNC`), never the test harness.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test support: failing the calling test is the point"
)]

use std::os::unix::fs::PermissionsExt as _;

/// Run `body` with the process `umask` set to `0` and restore the previous mask
/// on return or unwind.
///
/// The umask is process-wide; call this only inside [`crate::in_own_process`].
pub fn with_zero_umask<T>(body: impl FnOnce() -> T) -> T {
    struct Restore(libc::mode_t);
    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: restores the process umask saved before this isolated test.
            unsafe {
                libc::umask(self.0);
            }
        }
    }
    // SAFETY: called inside an isolated test process; restored by `Restore`.
    let _restore = Restore(unsafe { libc::umask(0) });
    body()
}

/// Turn `chmod`-family syscalls into no-ops returning `0` on the calling thread
/// only.
///
/// # Panics
///
/// When the filter cannot be installed or does not hold on a probe file.
pub fn ignore_chmod_on_this_thread() {
    // Linux 6.6 assigned `fchmodat2` universal syscall number 452 across all
    // architectures; `libc` omits `SYS_fchmodat2` on `aarch64`.
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
    const SYS_FCHMODAT2: libc::c_long = 452;
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    const INTERCEPTED: &[libc::c_long] = &[
        libc::SYS_chmod,
        libc::SYS_fchmod,
        libc::SYS_fchmodat,
        libc::SYS_fchmodat2,
    ];
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
    const INTERCEPTED: &[libc::c_long] = &[libc::SYS_fchmod, libc::SYS_fchmodat, SYS_FCHMODAT2];

    let statement = |code: u32, k: u32| libc::sock_filter {
        code: u16::try_from(code).expect("BPF opcode fits"),
        jt: 0,
        jf: 0,
        k,
    };
    // Load the syscall number (offset 0 of `seccomp_data`); the architecture
    // word is not checked because the test only issues native syscalls.
    let mut filters = vec![statement(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0)];
    for (index, number) in INTERCEPTED.iter().copied().enumerate() {
        filters.push(libc::sock_filter {
            // On a match, skip the remaining comparisons and the allow.
            jt: u8::try_from(INTERCEPTED.len().saturating_sub(index)).expect("jump fits"),
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
    // `SECCOMP_RET_ERRNO` with data 0 skips the syscall and returns 0 (success)
    // to userspace without modifying the inode's mode.
    filters.push(statement(
        libc::BPF_RET | libc::BPF_K,
        libc::SECCOMP_RET_ERRNO,
    ));
    let program = libc::sock_fprog {
        len: u16::try_from(filters.len()).expect("program length fits"),
        filter: filters.as_mut_ptr(),
    };
    // SAFETY: scalar prctl calls on the current thread. The kernel copies the
    // program while `filters` is live; the filter only neutralizes chmod on
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
            "installing the chmod filter: {}",
            std::io::Error::last_os_error()
        );
    }

    // A guard that cannot observe its subject proves nothing.
    let probe_dir = tempfile::tempdir().expect("chmod probe directory");
    let probe_path = probe_dir.path().join("probe");
    let probe = std::fs::File::create(&probe_path).expect("probe file");
    let before = probe
        .metadata()
        .expect("probe metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_ne!(
        before, 0o600,
        "probe file must start wider than 0600 so a chmod no-op is observable"
    );
    probe
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .expect("fchmod returns 0 under the chmod filter");
    std::fs::set_permissions(&probe_path, std::fs::Permissions::from_mode(0o600))
        .expect("chmod returns 0 under the chmod filter");
    let after = std::fs::metadata(&probe_path)
        .expect("probe metadata after chmod")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        after, before,
        "the chmod filter is not in force: mode changed from {before:#o} to {after:#o}"
    );
}

/// Run `body` on a new thread where `chmod`, `fchmod`, `fchmodat` and
/// `fchmodat2` are kernel no-ops returning `0`, and return its result. `body`
/// may borrow from the caller; a panic in it fails the calling test.
pub fn on_a_thread_without_chmod<T: Send>(body: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                ignore_chmod_on_this_thread();
                body()
            })
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::OpenOptionsExt as _;

    #[test]
    fn create_then_chmod_stays_wide_while_mode_at_open_is_private() {
        crate::in_own_process(
            module_path!(),
            "create_then_chmod_stays_wide_while_mode_at_open_is_private",
            || {
                with_zero_umask(|| {
                    let dir = tempfile::tempdir().expect("tempdir");
                    on_a_thread_without_chmod(|| {
                        let wide = dir.path().join("wide");
                        let file = std::fs::File::create(&wide).expect("create wide");
                        file.set_permissions(std::fs::Permissions::from_mode(0o600))
                            .expect("fchmod is a no-op");
                        std::fs::set_permissions(&wide, std::fs::Permissions::from_mode(0o600))
                            .expect("chmod is a no-op");
                        assert_eq!(
                            std::fs::metadata(&wide)
                                .expect("wide metadata")
                                .permissions()
                                .mode()
                                & 0o777,
                            0o666
                        );

                        let private = dir.path().join("private");
                        std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(0o600)
                            .open(&private)
                            .expect("create private");
                        assert_eq!(
                            std::fs::metadata(&private)
                                .expect("private metadata")
                                .permissions()
                                .mode()
                                & 0o777,
                            0o600
                        );
                    });
                });
            },
        );
    }
}
