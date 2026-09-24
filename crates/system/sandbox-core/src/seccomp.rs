//! Seccomp-bpf deny-list compilation, and the baseline application policy.
//!
//! Every Tilo filter has the same shape: default-allow, with listed syscalls
//! (optionally conditioned on their arguments) answered with `EPERM`. A
//! deny-list rather than an allow-list, because a hand-rolled allow-list
//! breaks unpredictably when glibc adds a syscall and differs per
//! architecture; `EPERM` rather than a kill, so a refused program logs the
//! failure instead of appearing to crash. [`compile_deny_list`] is that
//! shape; callers supply only the rule map.
//!
//! ## Baseline application policy
//!
//! Based on Flatpak's published seccomp profile (`common/flatpak-run.c`'s
//! `setup_seccomp`), itself derived from the Bubblewrap reference deny-list:
//! namespace manipulation (`unshare`, `setns`, namespace flags to `clone`),
//! `mount`/`umount2`/`pivot_root`/`chroot`, `ptrace`, raw packet sockets
//! (`AF_PACKET`), `bpf`, `keyctl`, kernel administration and a set of attack
//! surfaces no desktop app needs. [`SeccompProfile::TiloDefault`] adds
//! write-xor-execute; [`SeccompProfile::TiloJit`] omits only that.
//!
//! Which profile a package gets is not decided here: the launcher resolves a
//! manifest's request against the image's entitlement list.

#[cfg(target_os = "linux")]
use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use seccompiler::{
    BackendError, BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition,
    SeccompFilter, SeccompRule, TargetArch,
};

/// Filter compilation errors.
#[derive(Debug, thiserror::Error)]
#[cfg(target_os = "linux")]
pub enum SeccompError {
    #[error("unsupported host architecture for seccomp: {0}")]
    UnsupportedArch(&'static str),
    #[error("seccompiler compile error: {0}")]
    Compile(#[from] BackendError),
    #[error("io error while staging seccomp blob: {0}")]
    Io(#[from] io::Error),
    #[error("installing the seccomp filter failed: {0}")]
    Install(String),
}

/// The architecture a filter is compiled for: the one this binary runs on.
///
/// # Errors
///
/// Returns [`SeccompError::UnsupportedArch`] on an architecture seccompiler
/// cannot target.
#[cfg(target_os = "linux")]
pub fn host_target_arch() -> Result<TargetArch, SeccompError> {
    if cfg!(target_arch = "x86_64") {
        Ok(TargetArch::x86_64)
    } else if cfg!(target_arch = "aarch64") {
        Ok(TargetArch::aarch64)
    } else if cfg!(target_arch = "riscv64") {
        Ok(TargetArch::riscv64)
    } else {
        Err(SeccompError::UnsupportedArch(std::env::consts::ARCH))
    }
}

/// Compile a deny-list to a BPF program: default-allow, every matching rule
/// answered with `EPERM`. A syscall mapped to an empty rule list is denied
/// unconditionally; otherwise any one matching rule denies the call.
///
/// # Errors
///
/// Returns [`SeccompError`] for an unsupported architecture or a rule set
/// seccompiler refuses to compile.
#[cfg(target_os = "linux")]
pub fn deny_list_program(
    rules: BTreeMap<i64, Vec<SeccompRule>>,
) -> Result<BpfProgram, SeccompError> {
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        host_target_arch()?,
    )?;
    Ok(filter.try_into()?)
}

/// Serialise a BPF program to the raw `sock_filter` array `bwrap --seccomp`
/// reads: 8 bytes per instruction, native endian.
#[cfg(target_os = "linux")]
pub fn program_bytes(prog: &BpfProgram) -> Vec<u8> {
    let mut out = Vec::with_capacity(prog.len().saturating_mul(8));
    for stmt in prog {
        out.extend_from_slice(&stmt.code.to_ne_bytes());
        out.push(stmt.jt);
        out.push(stmt.jf);
        out.extend_from_slice(&stmt.k.to_ne_bytes());
    }
    out
}

/// [`deny_list_program`] serialised with [`program_bytes`].
///
/// # Errors
///
/// As [`deny_list_program`].
#[cfg(target_os = "linux")]
pub fn compile_deny_list(rules: BTreeMap<i64, Vec<SeccompRule>>) -> Result<Vec<u8>, SeccompError> {
    Ok(program_bytes(&deny_list_program(rules)?))
}

/// `socket(family, …)` with `family == AF_PACKET`: a raw link-layer socket.
/// Denied by every Tilo filter.
///
/// # Errors
///
/// Returns [`SeccompError::Compile`] if seccompiler rejects the condition.
#[cfg(target_os = "linux")]
pub fn af_packet_socket_rule() -> Result<Vec<SeccompRule>, SeccompError> {
    Ok(vec![SeccompRule::new(vec![SeccompCondition::new(
        0,
        SeccompCmpArgLen::Dword,
        SeccompCmpOp::Eq,
        libc::AF_PACKET as u64,
    )?])?])
}

/// The baseline deny-list, named so a future Flatpak-policy bump diffs cleanly.
///
/// `clone3` is deliberately NOT denied: modern glibc's `pthread_create`
/// uses it as the primary syscall, and since `clone` is already
/// allowed, denying `clone3` blocks nothing of substance — it just
/// makes a common code path (including the portal client's
/// worker-thread spawn) unreliable.
#[cfg(target_os = "linux")]
pub const BLANKET_DENIES: &[libc::c_long] = &[
    libc::SYS_ptrace,
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_pivot_root,
    libc::SYS_chroot,
    libc::SYS_unshare,
    libc::SYS_setns,
    libc::SYS_bpf,
    libc::SYS_keyctl,
    // High-risk kernel administration and dynamic kernel extension.
    libc::SYS_kexec_load,
    libc::SYS_finit_module,
    libc::SYS_init_module,
    libc::SYS_delete_module,
    libc::SYS_reboot,
    libc::SYS_swapon,
    libc::SYS_swapoff,
    // Attack surfaces an ordinary desktop app has no reason to reach.
    libc::SYS_userfaultfd,
    libc::SYS_perf_event_open,
    libc::SYS_open_by_handle_at,
    libc::SYS_process_vm_readv,
    libc::SYS_process_vm_writev,
    libc::SYS_pidfd_getfd,
    libc::SYS_fanotify_init,
    libc::SYS_fsopen,
    libc::SYS_fspick,
    libc::SYS_fsconfig,
    libc::SYS_fsmount,
    libc::SYS_move_mount,
    libc::SYS_mount_setattr,
    libc::SYS_quotactl,
    libc::SYS_acct,
];

#[cfg(target_os = "linux")]
const CLONE_NAMESPACE_FLAGS: &[u64] = &[
    libc::CLONE_NEWCGROUP as u64,
    libc::CLONE_NEWIPC as u64,
    libc::CLONE_NEWNET as u64,
    libc::CLONE_NEWNS as u64,
    libc::CLONE_NEWPID as u64,
    libc::CLONE_NEWTIME as u64,
    libc::CLONE_NEWUSER as u64,
    libc::CLONE_NEWUTS as u64,
];

/// Which named seccomp profile a package's `[sandbox] seccomp = "…"`
/// asked for.
///
/// The only thing the name decides is whether the app may create
/// memory that is writable and executable at once — a per-package
/// question, not a per-user one, so it's not a runtime grant, but it's
/// not the manifest's answer either: the manifest asks, the image's
/// entitlement list decides (the launcher's `seccomp::resolve_profile`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeccompProfile {
    /// `tilo-default` — write-xor-execute enforced. An app cannot turn data it
    /// received over the network into code.
    #[default]
    TiloDefault,
    /// `tilo-jit` — the baseline minus the write-xor-execute rules, for the
    /// one class of program that genuinely compiles code at run time: a
    /// web engine's JavaScript tier. Everything else still applies.
    TiloJit,
}

impl SeccompProfile {
    /// Resolve a manifest profile name.
    ///
    /// Unknown names resolve to [`SeccompProfile::TiloDefault`], the strict
    /// one — a typo must not be a way to acquire a weaker sandbox.
    pub fn parse(name: &str) -> Self {
        match name {
            "tilo-jit" => Self::TiloJit,
            _ => Self::TiloDefault,
        }
    }

    /// The manifest spelling, for logging and round-tripping.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TiloDefault => "tilo-default",
            Self::TiloJit => "tilo-jit",
        }
    }

    /// Whether this profile lets the app map memory writable and executable
    /// at once.
    pub fn allows_dynamic_code(self) -> bool {
        matches!(self, Self::TiloJit)
    }
}

/// Syscalls that take a page-protection bitmask in argument 2 and can
/// therefore produce a writable, executable mapping.
#[cfg(target_os = "linux")]
const PROT_ARG_SYSCALLS: &[libc::c_long] =
    &[libc::SYS_mmap, libc::SYS_mprotect, libc::SYS_pkey_mprotect];

/// `PROT_WRITE | PROT_EXEC`, used as both mask and value so the rule
/// reads "both bits are set" rather than "exactly this" — `PROT_READ`
/// is almost always set alongside them.
#[cfg(target_os = "linux")]
fn write_exec_bits() -> u64 {
    (libc::PROT_WRITE | libc::PROT_EXEC) as u64
}

#[cfg(target_os = "linux")]
fn baseline_rules(
    profile: SeccompProfile,
) -> Result<BTreeMap<i64, Vec<SeccompRule>>, SeccompError> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    for nr in BLANKET_DENIES {
        rules.insert(*nr, Vec::new());
    }

    // pthreads still need clone(), but an app may not use it to manufacture
    // another namespace boundary inside the one Tilo constructed. One rule per
    // bit expresses "any forbidden flag" because rules for a syscall are ORed.
    rules.insert(
        libc::SYS_clone,
        CLONE_NAMESPACE_FLAGS
            .iter()
            .map(|flag| {
                SeccompRule::new(vec![SeccompCondition::new(
                    0,
                    SeccompCmpArgLen::Qword,
                    SeccompCmpOp::MaskedEq(*flag),
                    *flag,
                )?])
            })
            .collect::<Result<Vec<_>, BackendError>>()?,
    );

    // Write-xor-execute. A page that is writable and executable at the
    // same time is what turns a heap or stack write into code execution:
    // the attacker no longer needs to reuse existing instructions, they can
    // write their own. Refusing the combination costs nothing unless an app
    // compiles code at run time — the only one here is the browser's web
    // engine, which asks for `tilo-jit`.
    //
    // `MaskedEq` compares `(prot & mask) == (value & mask)`, matching any
    // protection with both bits set and leaving `PROT_READ|PROT_WRITE`
    // (ordinary data) and `PROT_READ|PROT_EXEC` (a loaded library, or a
    // well-behaved JIT's write-then-flip) alone. `Dword` because `prot` is
    // an `int`; `Qword` could pick up whatever occupies the register's
    // upper half.
    //
    // Honest limit: this does not stop mapping the same memfd twice, once
    // writable and once executable — that needs the fd's history, which a
    // syscall-argument filter can't see. What it removes is every one-call
    // route to W+X, the shape that actually appears in exploit chains
    // against a renderer.
    if !profile.allows_dynamic_code() {
        for nr in PROT_ARG_SYSCALLS {
            rules.insert(
                *nr,
                vec![SeccompRule::new(vec![SeccompCondition::new(
                    2,
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::MaskedEq(write_exec_bits()),
                    write_exec_bits(),
                )?])?],
            );
        }
    }

    rules.insert(libc::SYS_socket, af_packet_socket_rule()?);
    Ok(rules)
}

/// The baseline policy for `profile` as a BPF program, ready to install.
///
/// # Errors
///
/// Returns [`SeccompError`] if the rules or their compilation fail.
#[cfg(target_os = "linux")]
pub fn baseline_program(profile: SeccompProfile) -> Result<BpfProgram, SeccompError> {
    deny_list_program(baseline_rules(profile)?)
}

/// Syscalls a hostile-document parser never needs: it talks only over the
/// descriptors it inherited or was handed, so it may create no socket of any
/// family, reach no address, and run no other program.
#[cfg(target_os = "linux")]
pub const PARSER_DENIES: &[libc::c_long] = &[
    libc::SYS_socket,
    libc::SYS_socketpair,
    libc::SYS_connect,
    libc::SYS_bind,
    libc::SYS_listen,
    libc::SYS_accept,
    libc::SYS_accept4,
    libc::SYS_execve,
    libc::SYS_execveat,
];

/// The policy for a helper that parses untrusted documents (PDF engines,
/// thumbnailers): the [`SeccompProfile::TiloDefault`] baseline, including
/// write-xor-execute, plus [`PARSER_DENIES`].
///
/// # Errors
///
/// Returns [`SeccompError`] if the rules or their compilation fail.
#[cfg(target_os = "linux")]
pub fn parser_program() -> Result<BpfProgram, SeccompError> {
    let mut rules = baseline_rules(SeccompProfile::TiloDefault)?;
    for nr in PARSER_DENIES {
        // Unconditional: replaces the baseline's AF_PACKET-only socket rule.
        rules.insert(*nr, Vec::new());
    }
    deny_list_program(rules)
}

/// Install `program` on every thread of the calling process. Sets
/// `no_new_privs` first, as the kernel requires for an unprivileged filter.
/// Irreversible.
///
/// # Errors
///
/// Returns [`SeccompError::Install`] when the kernel refuses the filter.
#[cfg(target_os = "linux")]
pub fn install(program: &BpfProgram) -> Result<(), SeccompError> {
    seccompiler::apply_filter_all_threads(program).map_err(|e| SeccompError::Install(e.to_string()))
}

/// Compile the baseline policy to a raw BPF blob.
///
/// Takes the profile explicitly rather than defaulting, so every call
/// site has decided whether the program is allowed to generate code.
///
/// # Errors
///
/// Returns [`SeccompError`] if the filter's rules or its BPF compilation
/// fail to build.
#[cfg(target_os = "linux")]
pub fn compile_baseline(profile: SeccompProfile) -> Result<Vec<u8>, SeccompError> {
    Ok(program_bytes(&baseline_program(profile)?))
}

/// Write the compiled blob to a path inside `dir`, returning that path.
/// Caller is responsible for opening it and handing the fd to bwrap.
///
/// # Errors
///
/// Returns [`SeccompError`] if `dir` cannot be created, the filter cannot be
/// compiled, or the blob cannot be written.
#[cfg(target_os = "linux")]
pub fn write_baseline_to(dir: &Path, profile: SeccompProfile) -> Result<PathBuf, SeccompError> {
    std::fs::create_dir_all(dir)?;
    let bytes = compile_baseline(profile)?;
    let path = dir.join(format!("{}.bpf", profile.as_str()));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// Kernel probes proving a compiled baseline is enforced, for tests here and
/// in dependants (feature `test-support`).
#[cfg(all(target_os = "linux", any(test, feature = "test-support")))]
pub mod testing {
    use super::{SeccompProfile, baseline_program};

    /// Install the baseline for `profile` in a forked child and try the
    /// mappings it must refuse and allow.
    ///
    /// Forked because a seccomp filter cannot be removed once applied —
    /// the test process has to survive to run the rest of the suite. The
    /// child does nothing but raw syscalls and `_exit`, so no allocator or
    /// lock is touched after the fork.
    ///
    /// Exit codes are the report channel:
    ///   0  = behaved exactly as the profile says it should
    ///   10 = a mapping that should have been refused succeeded
    ///   11 = a mapping that should have been allowed was refused
    ///   12 = the filter itself would not install
    ///
    /// # Panics
    ///
    /// Panics if the filter does not compile, `fork` fails, or the child
    /// does not exit normally.
    #[allow(
        clippy::expect_used,
        reason = "a test probe: a failure to set the experiment up is the test failing"
    )]
    pub fn probe_mappings_under(profile: SeccompProfile) -> i32 {
        let program = baseline_program(profile).expect("compile filter");
        let deny_wx = !profile.allows_dynamic_code();

        // SAFETY: the child performs only async-signal-safe work (seccomp
        // install via prctl, mmap/mprotect/munmap, _exit) and never returns
        // into the test harness.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let bail = |code: i32| -> ! {
                // SAFETY: _exit is async-signal-safe and does not unwind.
                unsafe { libc::_exit(code) }
            };
            if seccompiler::apply_filter(&program).is_err() {
                bail(12);
            }
            let page = 4096;
            let anon = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS;

            // SAFETY: plain anonymous mappings of one page, unmapped below.
            let wx = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    page,
                    libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                    anon,
                    -1,
                    0,
                )
            };
            if deny_wx {
                if wx != libc::MAP_FAILED {
                    bail(10);
                }
            } else if wx == libc::MAP_FAILED {
                bail(11);
            }

            // Writable-only must always work: this is every ordinary
            // allocation, and a filter that broke it would break everything.
            // SAFETY: as above.
            let rw = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    page,
                    libc::PROT_READ | libc::PROT_WRITE,
                    anon,
                    -1,
                    0,
                )
            };
            if rw == libc::MAP_FAILED {
                bail(11);
            }

            // Write, then flip to executable — the shape a well-behaved code
            // generator uses, and it must keep working under the strict
            // profile. Neither call has both bits set at once.
            // SAFETY: `rw` is a live one-page mapping from the call above.
            if unsafe { libc::mprotect(rw, page, libc::PROT_READ | libc::PROT_EXEC) } != 0 {
                bail(11);
            }
            // Asking for both bits at once is the thing being refused.
            // SAFETY: same live mapping.
            let flip_to_wx = unsafe {
                libc::mprotect(
                    rw,
                    page,
                    libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                )
            };
            if deny_wx {
                if flip_to_wx == 0 {
                    bail(10);
                }
            } else if flip_to_wx != 0 {
                bail(11);
            }

            bail(0);
        }

        let mut status = 0i32;
        // SAFETY: `pid` is our own child from the fork above.
        unsafe { libc::waitpid(pid, &raw mut status, 0) };
        assert!(
            libc::WIFEXITED(status),
            "child did not exit normally (status {status:#x})"
        );
        libc::WEXITSTATUS(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn baseline_compiles_and_blob_alignment() {
        let bytes = compile_baseline(SeccompProfile::TiloDefault).expect("compile");
        assert!(!bytes.is_empty(), "empty BPF");
        assert_eq!(
            bytes.len() % 8,
            0,
            "BPF blob length {} not aligned to 8-byte sock_filter",
            bytes.len()
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn write_baseline_creates_blob() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_baseline_to(dir.path(), SeccompProfile::TiloDefault).unwrap();
        assert!(path.exists());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            bytes,
            compile_baseline(SeccompProfile::TiloDefault).unwrap()
        );
    }

    #[test]
    fn unknown_profile_name_resolves_to_the_strict_one() {
        for name in ["tilo-default", "", "tilo-jitt", "TILO-JIT", "anything"] {
            assert_eq!(
                SeccompProfile::parse(name),
                SeccompProfile::TiloDefault,
                "{name:?} must not resolve to a weaker profile"
            );
        }
        assert_eq!(SeccompProfile::parse("tilo-jit"), SeccompProfile::TiloJit);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn jit_profile_is_the_only_one_that_drops_the_write_exec_rules() {
        let strict = compile_baseline(SeccompProfile::TiloDefault).unwrap();
        let jit = compile_baseline(SeccompProfile::TiloJit).unwrap();
        assert!(
            strict.len() > jit.len(),
            "the strict profile must carry more program than the JIT one \
             (strict={} bytes, jit={} bytes)",
            strict.len(),
            jit.len()
        );
        // Both still deny the blanket set, so neither is a no-op filter.
        assert!(!jit.is_empty());
        assert_eq!(jit.len() % 8, 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn compile_deny_list_is_the_serialised_program() {
        let mut rules = BTreeMap::new();
        rules.insert(libc::SYS_ptrace, Vec::new());
        let program = deny_list_program(rules.clone()).unwrap();
        assert_eq!(compile_deny_list(rules).unwrap(), program_bytes(&program));
        assert_eq!(program_bytes(&program).len(), program.len() * 8);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn strict_profile_refuses_writable_executable_memory_in_the_kernel() {
        assert_eq!(
            testing::probe_mappings_under(SeccompProfile::TiloDefault),
            0,
            "10 = W+X was allowed, 11 = an ordinary mapping was refused, \
             12 = the filter would not install"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn jit_profile_still_permits_writable_executable_memory() {
        assert_eq!(
            testing::probe_mappings_under(SeccompProfile::TiloJit),
            0,
            "10 = W+X was allowed, 11 = an ordinary mapping was refused, \
             12 = the filter would not install"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn baseline_refuses_kernel_introspection_and_fault_injection_syscalls() {
        let program = baseline_program(SeccompProfile::TiloDefault).expect("compile filter");
        // SAFETY: the child installs a filter, invokes raw syscalls with inert
        // arguments, and exits without returning into the test harness.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            if seccompiler::apply_filter(&program).is_err() {
                // SAFETY: `_exit` does not unwind.
                unsafe { libc::_exit(12) };
            }
            for syscall in [
                libc::SYS_userfaultfd,
                libc::SYS_perf_event_open,
                libc::SYS_pidfd_getfd,
                libc::SYS_process_vm_readv,
            ] {
                // SAFETY: arguments are ignored because seccomp returns EPERM
                // before the target syscall runs.
                let result = unsafe { libc::syscall(syscall, -1_i64, 0_i64, 0_i64, 0_i64) };
                if result != -1
                    || std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM)
                {
                    // SAFETY: `_exit` does not unwind.
                    unsafe { libc::_exit(10) };
                }
            }
            // SAFETY: `_exit` does not unwind.
            unsafe { libc::_exit(0) };
        }
        let mut status = 0_i32;
        // SAFETY: `pid` is our own child.
        unsafe { libc::waitpid(pid, &raw mut status, 0) };
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn parser_program_refuses_sockets_and_exec_in_the_kernel() {
        let program = parser_program().expect("compile filter");
        // SAFETY: the child installs a filter, invokes raw syscalls with inert
        // arguments, and exits without returning into the test harness.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            if seccompiler::apply_filter(&program).is_err() {
                // SAFETY: `_exit` does not unwind.
                unsafe { libc::_exit(12) };
            }
            for family in [libc::AF_UNIX, libc::AF_INET, libc::AF_INET6] {
                // SAFETY: socket(2) with constant arguments.
                let fd = unsafe { libc::socket(family, libc::SOCK_STREAM, 0) };
                if fd != -1 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM) {
                    // SAFETY: `_exit` does not unwind.
                    unsafe { libc::_exit(10) };
                }
            }
            let path = c"/bin/true";
            let argv = [path.as_ptr(), std::ptr::null()];
            let envp = [std::ptr::null::<libc::c_char>()];
            // SAFETY: valid NUL-terminated argument vectors.
            let rc = unsafe { libc::execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr()) };
            if rc != -1 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM) {
                // SAFETY: `_exit` does not unwind.
                unsafe { libc::_exit(11) };
            }
            // SAFETY: `_exit` does not unwind.
            unsafe { libc::_exit(0) };
        }
        let mut status = 0;
        // SAFETY: waiting on our own child.
        unsafe { libc::waitpid(pid, &raw mut status, 0) };
        assert!(libc::WIFEXITED(status));
        assert_eq!(
            libc::WEXITSTATUS(status),
            0,
            "10 = a socket was created, 11 = exec was allowed, 12 = the filter would not install"
        );
    }
}
