//! Confinement of a worker, applied once it holds its pipes and before it
//! reads a request.
//!
//! A worker needs CPU, memory and the two pipes it was started with; nothing
//! else. So before it announces itself ready it:
//!
//! 1. restricts itself with a Landlock ruleset that handles every filesystem
//!    right and TCP bind and connect, and grants none;
//! 2. sets `no_new_privs` and installs a seccomp allow-list ([`rules`]):
//!    reads from the request pipe, writes to the reply pipe and stdio,
//!    anonymous non-executable memory, futexes, clocks, its idle timer,
//!    signals to itself and exit. Any other syscall kills the worker with `SIGSYS`, which the
//!    caller sees as a dead worker and reports as [`crate::Rejected::Crashed`];
//! 3. stacks a second filter (installed first) that answers [`REFUSED`] (`openat`) with `EPERM`
//!    before the call reaches the kernel. glibc's allocator opens
//!    `/proc/sys/vm/overcommit_memory` the first time it shrinks a thread
//!    arena's heap and treats a failure as "do not shrink", so killing there
//!    would turn an ordinary `free` into a crash. The kernel applies the
//!    stricter of the two filters' answers, so `openat` never runs.
//!
//! The allow-list is the guarantee: with `openat` refused and no `open`,
//! `socket`, `execve` or other descriptor-creating call, a compromised
//! decoder can reach nothing beyond the pipes. It fails the worker's start when the kernel refuses it.
//! Landlock is a second layer and best effort, since the kernel may lack
//! some of its rights; it also covers only the calling thread, which in a
//! program that called [`crate::dispatch_early`] first is the only one (a
//! test binary's harness thread is still bound by the allow-list, which
//! applies to every thread).
//!
//! Descriptors the worker inherited besides its pipes stay open but inert:
//! read, write and mmap are allowed only on the pipes and stdio, and nothing
//! else takes a descriptor.

use std::collections::BTreeMap;

use sandbox_core::landlock::{RulesetSpec, restrict_self};
use seccompiler::{BackendError, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompRule};

use crate::worker::{REPLY_FD, REQUEST_FD};

/// Syscalls allowed with any arguments.
const UNCONDITIONAL: &[libc::c_long] = &[
    // Allocation. `mremap` and `madvise` keep a mapping's protection.
    libc::SYS_brk,
    libc::SYS_munmap,
    libc::SYS_mremap,
    libc::SYS_madvise,
    // Closing the pipes on the way out.
    libc::SYS_close,
    // Locks and time.
    libc::SYS_futex,
    libc::SYS_sched_yield,
    libc::SYS_clock_gettime,
    libc::SYS_clock_getres,
    libc::SYS_gettimeofday,
    // Hash-map seeds.
    libc::SYS_getrandom,
    // Signal state, used by `abort` and the stack-overflow handler.
    libc::SYS_rt_sigaction,
    libc::SYS_rt_sigprocmask,
    libc::SYS_rt_sigreturn,
    libc::SYS_sigaltstack,
    libc::SYS_restart_syscall,
    libc::SYS_getpid,
    libc::SYS_gettid,
    libc::SYS_exit,
    libc::SYS_exit_group,
];

/// Syscalls refused with `EPERM` rather than killing the worker: libc calls
/// them on paths it treats as optional.
pub(crate) const REFUSED: &[libc::c_long] = &[libc::SYS_openat];

/// Descriptors a worker may write: stdout and stderr (null and the caller's
/// diagnostics) and the reply pipe.
const WRITABLE_FDS: &[i32] = &[1, 2, REPLY_FD];

/// The worker's allow-list. `pid` is the worker's own, the only process it
/// may signal (`abort` raises `SIGABRT` with `tgkill`).
pub(crate) fn rules(pid: u32) -> Result<BTreeMap<i64, Vec<SeccompRule>>, BackendError> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = UNCONDITIONAL
        .iter()
        .chain(REFUSED)
        .map(|nr| (*nr, Vec::new()))
        .collect();
    for nr in [libc::SYS_read, libc::SYS_readv] {
        rules.insert(nr, on_fds(&[REQUEST_FD])?);
    }
    for nr in [libc::SYS_write, libc::SYS_writev] {
        rules.insert(nr, on_fds(WRITABLE_FDS)?);
    }
    // Anonymous (`fd == -1`) and never executable: no file can be mapped,
    // and no received byte can become code.
    rules.insert(
        libc::SYS_mmap,
        vec![SeccompRule::new(vec![
            not_executable()?,
            SeccompCondition::new(4, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, 0xffff_ffff)?,
        ])?],
    );
    rules.insert(
        libc::SYS_mprotect,
        vec![SeccompRule::new(vec![not_executable()?])?],
    );
    // The idle timer, which only this process's real-time clock drives.
    rules.insert(
        libc::SYS_setitimer,
        vec![SeccompRule::new(vec![SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Eq,
            u64::from(libc::ITIMER_REAL.unsigned_abs()),
        )?])?],
    );
    rules.insert(
        libc::SYS_tgkill,
        vec![SeccompRule::new(vec![SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Eq,
            u64::from(pid),
        )?])?],
    );
    Ok(rules)
}

/// One rule per descriptor: argument 0 is `fd`.
fn on_fds(fds: &[i32]) -> Result<Vec<SeccompRule>, BackendError> {
    fds.iter()
        .map(|fd| {
            SeccompRule::new(vec![SeccompCondition::new(
                0,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::Eq,
                u64::from(fd.unsigned_abs()),
            )?])
        })
        .collect()
}

/// Argument 2, the protection, without `PROT_EXEC`. `Dword` because `prot`
/// is an `int`.
fn not_executable() -> Result<SeccompCondition, BackendError> {
    let exec = libc::PROT_EXEC.unsigned_abs().into();
    SeccompCondition::new(2, SeccompCmpArgLen::Dword, SeccompCmpOp::MaskedEq(exec), 0)
}

/// Confine the calling worker. Irreversible.
pub(crate) fn confine() -> Result<(), String> {
    restrict_self(&RulesetSpec::deny_all(), |_, _| {}).map_err(|e| format!("landlock: {e}"))?;
    let pid = std::process::id();
    let rules = rules(pid).map_err(|e| format!("seccomp: {e}"))?;
    let allowed =
        sandbox_core::seccomp::allow_list_program(rules).map_err(|e| format!("seccomp: {e}"))?;
    let refused = sandbox_core::seccomp::deny_list_program(
        REFUSED.iter().map(|nr| (*nr, Vec::new())).collect(),
    )
    .map_err(|e| format!("seccomp: {e}"))?;
    // The refusal first: installing a filter calls prctl and seccomp, which
    // the allow-list does not permit.
    for program in [&refused, &allowed] {
        sandbox_core::seccomp::install(program).map_err(|e| format!("seccomp: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_allow_list_compiles_and_names_no_path_or_socket_syscall() {
        let rules = rules(std::process::id()).unwrap();
        for forbidden in [
            libc::SYS_socket,
            libc::SYS_connect,
            libc::SYS_execve,
            libc::SYS_clone,
            libc::SYS_clone3,
            libc::SYS_pipe2,
            libc::SYS_dup3,
            libc::SYS_newfstatat,
            libc::SYS_ioctl,
        ] {
            assert!(!rules.contains_key(&forbidden), "{forbidden} is allowed");
        }
        sandbox_core::seccomp::allow_list_program(rules).unwrap();
    }
}
