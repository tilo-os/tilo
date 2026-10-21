//! Waiting until a test's thread sleeps on the lock the test holds.

/// The calling thread's kernel id, as `/proc/self/task` names it.
pub(super) fn current_tid() -> libc::pid_t {
    // SAFETY: gettid takes no arguments and cannot fail.
    unsafe { libc::gettid() }
}

/// Wait until thread `tid` of this process sleeps on a contended lock (a
/// futex wait). Any other lock it takes on the way is uncontended and never
/// enters the kernel, so the one it sleeps on is the one the test holds.
pub(super) fn wait_until_blocked_on_a_lock(tid: libc::pid_t, what: &str) {
    let futex = format!("{} ", libc::SYS_futex);
    tilo_test_env::wait_until(format!("{what} to wait on the lock"), || {
        std::fs::read_to_string(format!("/proc/self/task/{tid}/syscall"))
            .is_ok_and(|syscall| syscall.starts_with(&futex))
    });
}
