//! How an app worker confines itself before it reads a request.
//!
//! An app worker runs an app's own code over a room's records: everything
//! it needs is CPU, memory and the socket it was started with. So once it
//! has prepared what it cannot load later, it:
//!
//! 1. closes every inherited descriptor but stdio and arranges to die with
//!    its supervisor (`tilo-sandbox`'s `detach`), and caps its descriptors
//!    and core dumps;
//! 2. on Linux:
//!    - restricts itself with a Landlock ruleset that handles every
//!      filesystem right and TCP bind and connect and grants none, where the
//!      kernel supports it;
//!    - installs the shared seccomp parser policy (`sandbox-core`: the
//!      application baseline with write-xor-execute, and no socket of any
//!      family, `connect`, `bind` or `exec`), after the `clone3`
//!      compatibility filter that keeps thread creation inspectable;
//!    - installs [`PATH_DENIES`]: no path can be opened, created, removed,
//!      renamed or relinked, and no `io_uring` (which would open paths past
//!      the filter); and signals only to itself. These are enforced on every
//!      kernel with seccomp, so the worker cannot read the supervisor's
//!      state directory (its device keys, `agents.json`) even where the
//!      kernel lacks Landlock;
//! 3. on macOS: the `tilo-sandbox` Seatbelt profile with no readable path.
//!
//! Every step is irreversible for the worker and must succeed: a worker that
//! cannot confine itself reports why and serves nothing, and the supervisor
//! refuses the operation rather than run app code unconfined.

/// Confine the calling worker. Irreversible.
///
/// # Errors
///
/// Why a step failed; the worker then serves nothing.
pub(super) fn confine() -> Result<(), String> {
    tilo_sandbox::confine::detach().map_err(|error| error.to_string())?;
    // The socket, stdio, and nothing else.
    tilo_sandbox::confine::limit_resources(16).map_err(|error| error.to_string())?;
    sys::confine()
}

#[cfg(target_os = "linux")]
mod sys {
    use std::collections::BTreeMap;

    use sandbox_core::landlock::{RulesetSpec, restrict_self};
    use sandbox_core::seccomp;
    use seccompiler::{SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompRule};

    pub(super) fn confine() -> Result<(), String> {
        restrict_self(&RulesetSpec::deny_all(), |_, _| {})
            .map_err(|error| format!("landlock: {error}"))?;
        let pid = std::process::id();
        let paths = seccomp::deny_list_program(super::path_rules(pid)?)
            .map_err(|error| format!("seccomp: {error}"))?;
        let compat = seccomp::clone3_compat_program().map_err(|e| format!("seccomp: {e}"))?;
        let parser = seccomp::parser_program().map_err(|e| format!("seccomp: {e}"))?;
        for program in [&compat, &parser, &paths] {
            seccomp::install(program).map_err(|error| format!("seccomp: {error}"))?;
        }
        Ok(())
    }

    pub(super) fn rules(pid: u32) -> Result<BTreeMap<i64, Vec<SeccompRule>>, String> {
        let compile = |error: seccompiler::BackendError| format!("seccomp: {error}");
        let mut rules: BTreeMap<i64, Vec<SeccompRule>> = super::PATH_DENIES
            .iter()
            .map(|nr| (*nr, Vec::new()))
            .collect();
        // `kill` and `tgkill` only at this process: never the supervisor,
        // its process group (`0`) or every process (`-1`).
        let elsewhere = SeccompRule::new(vec![
            SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, u64::from(pid))
                .map_err(compile)?,
        ])
        .map_err(compile)?;
        rules.insert(libc::SYS_kill, vec![elsewhere.clone()]);
        rules.insert(libc::SYS_tgkill, vec![elsewhere]);
        Ok(rules)
    }
}

#[cfg(target_os = "macos")]
mod sys {
    pub(super) fn confine() -> Result<(), String> {
        tilo_sandbox::confine::confine(&tilo_sandbox::confine::Policy::default())
            .map_err(|error| error.to_string())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod sys {
    pub(super) fn confine() -> Result<(), String> {
        Err("app workers cannot be confined on this platform".into())
    }
}

/// Syscalls an app worker is refused (`EPERM`) on top of the parser policy:
/// everything that names a path to open, create, remove, rename, relink or
/// change, `io_uring` (whose requests open paths without passing the
/// filter), and signals to any process but itself (`tkill`, pidfds).
#[cfg(target_os = "linux")]
pub(crate) const PATH_DENIES: &[libc::c_long] = &[
    #[cfg(target_arch = "x86_64")]
    libc::SYS_open,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_creat,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_mkdir,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_mknod,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_unlink,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_rmdir,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_rename,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_link,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_symlink,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_chmod,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_chown,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_lchown,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_utimes,
    libc::SYS_openat,
    libc::SYS_openat2,
    libc::SYS_name_to_handle_at,
    libc::SYS_mkdirat,
    libc::SYS_mknodat,
    libc::SYS_unlinkat,
    libc::SYS_renameat,
    libc::SYS_renameat2,
    libc::SYS_linkat,
    libc::SYS_symlinkat,
    libc::SYS_truncate,
    libc::SYS_fchmodat,
    libc::SYS_fchownat,
    libc::SYS_utimensat,
    libc::SYS_inotify_add_watch,
    libc::SYS_io_uring_setup,
    libc::SYS_io_uring_enter,
    libc::SYS_io_uring_register,
    libc::SYS_tkill,
    libc::SYS_pidfd_open,
    libc::SYS_pidfd_send_signal,
];

#[cfg(target_os = "linux")]
fn path_rules(
    pid: u32,
) -> Result<std::collections::BTreeMap<i64, Vec<seccompiler::SeccompRule>>, String> {
    sys::rules(pid)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn the_path_filter_compiles_and_covers_every_way_to_open_a_path() {
        let rules = path_rules(std::process::id()).unwrap();
        for opening in [
            libc::SYS_openat,
            libc::SYS_openat2,
            libc::SYS_io_uring_setup,
            libc::SYS_kill,
            libc::SYS_tgkill,
        ] {
            assert!(rules.contains_key(&opening), "{opening} is not refused");
        }
        sandbox_core::seccomp::deny_list_program(rules).unwrap();
    }
}
