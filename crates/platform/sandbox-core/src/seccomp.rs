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
//! The exception is a process whose whole syscall surface is known because
//! it runs no foreign program and loads nothing after it is confined: a
//! worker that computes over bytes on a pipe. [`allow_list_program`] builds
//! its filter, default-kill, so anything outside the list ends the process
//! rather than giving hostile input a second try.

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

/// Compile an allow-list to a BPF program: every call not matched by a rule
/// kills the whole process with `SIGSYS`. A syscall mapped to an empty rule
/// list is allowed unconditionally; otherwise any one matching rule allows
/// the call.
#[cfg(target_os = "linux")]
pub fn allow_list_program(
    rules: BTreeMap<i64, Vec<SeccompRule>>,
) -> Result<BpfProgram, SeccompError> {
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::KillProcess,
        SeccompAction::Allow,
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
/// `clone3` is handled by [`clone3_compat_program`]. Classic seccomp cannot
/// inspect the pointed-to `clone_args`, so that filter returns `ENOSYS` and
/// lets glibc retry ordinary thread creation with inspectable legacy `clone`.
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
    libc::SYS_kexec_load,
    libc::SYS_finit_module,
    libc::SYS_init_module,
    libc::SYS_delete_module,
    libc::SYS_reboot,
    libc::SYS_swapon,
    libc::SYS_swapoff,
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

/// The `io_uring` interface: I/O submitted through rings shared with the
/// kernel. One of the largest recent sources of kernel vulnerabilities, and
/// nothing an ordinary app needs: every runtime that uses it (tokio's
/// `io-uring` driver, libuv, liburing users) falls back to `epoll` and
/// threads when `io_uring_setup` fails. Refused by the application baseline
/// unless the app holds the `io-uring` permission ([`SyscallGrants`]).
#[cfg(target_os = "linux")]
pub const IO_URING_SYSCALLS: &[libc::c_long] = &[
    libc::SYS_io_uring_setup,
    libc::SYS_io_uring_enter,
    libc::SYS_io_uring_register,
];

/// Syscalls the application baseline refuses unless the app was granted
/// them. Orthogonal to [`SeccompProfile`]: the profile is the image's
/// per-package decision about generated code, these are permissions a
/// manifest declares and the person (or the image, for an app it ships)
/// grants. The default grants nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyscallGrants {
    pub io_uring: bool,
    /// The 32-bit `set_thread_area` (i386 system call 243, reached through `int 0x80`)
    /// and no other call of the 32-bit system-call ABI. Wine keeps 32-bit Windows programs
    /// inside its 64-bit process (`WoW64`) and makes this one call to give each thread the
    /// 32-bit segment its Windows thread block is addressed by; the kernel has no 64-bit
    /// call that sets one. Every other 32-bit call still ends the process, as it does for
    /// every program without this grant. The call reads only a descriptor of the caller's
    /// own thread-local storage entries.
    pub wow64_thread_area: bool,
}

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
    /// `tilo-web-engine` — `tilo-jit` for a web engine that confines its own
    /// renderers, as Chromium does: it may create the user, PID and network
    /// namespaces and `chroot` that its per-renderer sandbox is built from.
    /// Every other namespace flag, `setns`, `mount` and `pivot_root` are still
    /// refused, so the nested namespaces can only narrow what the app has.
    TiloWebEngine,
}

impl SeccompProfile {
    /// Resolve a manifest profile name.
    ///
    /// Unknown names resolve to [`SeccompProfile::TiloDefault`], the strict
    /// one — a typo must not be a way to acquire a weaker sandbox.
    pub fn parse(name: &str) -> Self {
        match name {
            "tilo-jit" => Self::TiloJit,
            "tilo-web-engine" => Self::TiloWebEngine,
            _ => Self::TiloDefault,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::TiloDefault => "tilo-default",
            Self::TiloJit => "tilo-jit",
            Self::TiloWebEngine => "tilo-web-engine",
        }
    }

    /// Whether this profile lets the app map memory writable and executable
    /// at once.
    pub fn allows_dynamic_code(self) -> bool {
        matches!(self, Self::TiloJit | Self::TiloWebEngine)
    }

    /// Whether this profile lets the app build a nested sandbox from
    /// [`WEB_ENGINE_SANDBOX_NAMESPACES`] and `chroot`.
    pub fn allows_nested_sandbox(self) -> bool {
        matches!(self, Self::TiloWebEngine)
    }
}

/// The namespaces Chromium's renderer sandbox creates (`clone` or `unshare`
/// with these flags), which [`SeccompProfile::TiloWebEngine`] permits.
#[cfg(target_os = "linux")]
pub const WEB_ENGINE_SANDBOX_NAMESPACES: &[u64] = &[
    libc::CLONE_NEWUSER as u64,
    libc::CLONE_NEWPID as u64,
    libc::CLONE_NEWNET as u64,
];

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
    grants: SyscallGrants,
) -> Result<BTreeMap<i64, Vec<SeccompRule>>, SeccompError> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    for nr in BLANKET_DENIES {
        rules.insert(*nr, Vec::new());
    }
    if !grants.io_uring {
        for nr in IO_URING_SYSCALLS {
            rules.insert(*nr, Vec::new());
        }
    }

    // pthreads still need clone(), but an app may not use it to manufacture
    // another namespace boundary inside the one Tilo constructed. One rule per
    // bit expresses "any forbidden flag" because rules for a syscall are ORed.
    let forbidden_namespaces = |flags: &[u64]| {
        flags
            .iter()
            .filter(|flag| {
                !(profile.allows_nested_sandbox() && WEB_ENGINE_SANDBOX_NAMESPACES.contains(flag))
            })
            .map(|flag| {
                SeccompRule::new(vec![SeccompCondition::new(
                    0,
                    SeccompCmpArgLen::Qword,
                    SeccompCmpOp::MaskedEq(*flag),
                    *flag,
                )?])
            })
            .collect::<Result<Vec<_>, BackendError>>()
    };
    rules.insert(
        libc::SYS_clone,
        forbidden_namespaces(CLONE_NAMESPACE_FLAGS)?,
    );

    // A web engine's renderer sandbox is a user namespace holding a PID and a
    // network namespace, entered by `clone` or `unshare`, with the filesystem
    // dropped by `chroot` into an empty directory. Inside it the engine holds
    // capabilities over those namespaces only: mounting, pivoting, joining
    // another namespace and every other namespace kind stay refused.
    if profile.allows_nested_sandbox() {
        rules.insert(
            libc::SYS_unshare,
            forbidden_namespaces(CLONE_NAMESPACE_FLAGS)?,
        );
        rules.remove(&libc::SYS_chroot);
    }

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

#[cfg(target_os = "linux")]
pub fn baseline_program(profile: SeccompProfile) -> Result<BpfProgram, SeccompError> {
    baseline_program_with(profile, SyscallGrants::default())
}

#[cfg(target_os = "linux")]
pub fn baseline_program_with(
    profile: SeccompProfile,
    grants: SyscallGrants,
) -> Result<BpfProgram, SeccompError> {
    let program = deny_list_program(baseline_rules(profile, grants)?)?;
    if grants.wow64_thread_area {
        allow_i386_set_thread_area(program)
    } else {
        Ok(program)
    }
}

/// `program` with one exception to its architecture check: the i386 `set_thread_area`
/// ([`SyscallGrants::wow64_thread_area`]) is allowed, every other i386 call is still killed.
///
/// seccompiler compiles for one architecture and kills a call of any other before its rules
/// are checked, so the exception goes in ahead of them: the program's first three instructions (load
/// the architecture, go on when it is `x86_64`, kill otherwise) become a check that also
/// lets this one i386 call through. Jumps are relative and only go forward, so the rules
/// after the check keep their meaning.
#[cfg(target_os = "linux")]
#[cfg_attr(
    not(target_arch = "x86_64"),
    expect(
        clippy::unnecessary_wraps,
        reason = "the check the exception replaces is x86_64's; another architecture hands the program back whole, and the caller keeps one shape"
    )
)]
fn allow_i386_set_thread_area(program: BpfProgram) -> Result<BpfProgram, SeccompError> {
    #[cfg(target_arch = "x86_64")]
    {
        use seccompiler::sock_filter;
        const LOAD_WORD_ABSOLUTE: u16 = 0x20;
        const JUMP_IF_EQUAL: u16 = 0x15;
        const RETURN: u16 = 0x06;
        const ARCH_OFFSET: u32 = 4;
        const NR_OFFSET: u32 = 0;
        const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
        const AUDIT_ARCH_I386: u32 = 0x4000_0003;
        const I386_SET_THREAD_AREA: u32 = 243;
        let stmt = |code, k| sock_filter {
            code,
            jt: 0,
            jf: 0,
            k,
        };
        let jump = |k, jt, jf| sock_filter {
            code: JUMP_IF_EQUAL,
            jt,
            jf,
            k,
        };
        let expected = [
            stmt(LOAD_WORD_ABSOLUTE, ARCH_OFFSET),
            jump(AUDIT_ARCH_X86_64, 1, 0),
            stmt(RETURN, libc::SECCOMP_RET_KILL_PROCESS),
        ];
        if program.get(..3) != Some(&expected[..]) {
            return Err(SeccompError::Install(
                "the compiled filter does not start with the architecture check the 32-bit \
                 thread-area exception replaces"
                    .to_string(),
            ));
        }
        let mut patched = vec![
            stmt(LOAD_WORD_ABSOLUTE, ARCH_OFFSET),
            // x86_64 goes on to the rules (instruction 7).
            jump(AUDIT_ARCH_X86_64, 5, 0),
            // Any architecture but i386 is killed (instruction 6).
            jump(AUDIT_ARCH_I386, 0, 3),
            stmt(LOAD_WORD_ABSOLUTE, NR_OFFSET),
            // An i386 call other than set_thread_area is killed (instruction 6).
            jump(I386_SET_THREAD_AREA, 0, 1),
            stmt(RETURN, libc::SECCOMP_RET_ALLOW),
            stmt(RETURN, libc::SECCOMP_RET_KILL_PROCESS),
        ];
        patched.extend(program.into_iter().skip(3));
        Ok(patched)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        // Only an x86_64 kernel takes i386 calls from a 64-bit process.
        Ok(program)
    }
}

/// Compatibility filter for modern glibc thread creation. Returning `ENOSYS`
/// for `clone3` makes pthreads fall back to legacy `clone`, whose namespace
/// flags the baseline filter checks. This must be installed in addition to,
/// and before, [`baseline_program`].
///
/// # Errors
///
/// Returns [`SeccompError`] when the host architecture is unsupported or the
/// compatibility filter cannot be compiled.
#[cfg(target_os = "linux")]
pub fn clone3_compat_program() -> Result<BpfProgram, SeccompError> {
    let filter = SeccompFilter::new(
        BTreeMap::from([(libc::SYS_clone3, Vec::new())]),
        SeccompAction::Allow,
        SeccompAction::Errno(libc::ENOSYS as u32),
        host_target_arch()?,
    )?;
    Ok(filter.try_into()?)
}

/// [`clone3_compat_program`] serialized for bubblewrap's seccomp-fd input.
///
/// # Errors
///
/// Returns [`SeccompError`] when [`clone3_compat_program`] cannot be built.
#[cfg(target_os = "linux")]
pub fn compile_clone3_compat() -> Result<Vec<u8>, SeccompError> {
    Ok(program_bytes(&clone3_compat_program()?))
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
#[cfg(target_os = "linux")]
pub fn parser_program() -> Result<BpfProgram, SeccompError> {
    let mut rules = baseline_rules(SeccompProfile::TiloDefault, SyscallGrants::default())?;
    for nr in PARSER_DENIES {
        rules.insert(*nr, Vec::new());
    }
    deny_list_program(rules)
}

/// [`parser_program`], allowing `socketpair`: for a helper whose media
/// framework wakes its own threads through a connected pair (every
/// `GStreamer` bus and clock makes one). A pair has no address and reaches
/// nothing outside the process; `socket`, `connect` and `bind` stay refused,
/// so no endpoint can be made.
///
/// # Errors
///
/// Returns [`SeccompError`] if the rules or their compilation fail.
#[cfg(target_os = "linux")]
pub fn parser_program_with_socket_pairs() -> Result<BpfProgram, SeccompError> {
    let mut rules = baseline_rules(SeccompProfile::TiloDefault, SyscallGrants::default())?;
    for nr in PARSER_DENIES
        .iter()
        .filter(|nr| **nr != libc::SYS_socketpair)
    {
        rules.insert(*nr, Vec::new());
    }
    deny_list_program(rules)
}

/// Syscalls that address another process by number: with one shared uid,
/// two confined drivers could otherwise signal each other.
#[cfg(target_os = "linux")]
const SIGNAL_TARGET_SYSCALLS: &[libc::c_long] = &[
    libc::SYS_kill,
    libc::SYS_tkill,
    libc::SYS_tgkill,
    libc::SYS_rt_sigqueueinfo,
    libc::SYS_rt_tgsigqueueinfo,
];

/// The policy for a foreign file-system driver (such as `ntfs-3g`) that runs
/// over hostile on-disk data in process `own_pid`: the
/// [`SeccompProfile::TiloDefault`] baseline, no socket of any kind, no
/// pidfds, and signals only to itself.
///
/// Unlike [`parser_program`], `execve` stays allowed: the filter is installed
/// before the driver's program is executed. Installing it is what makes that
/// program run confined; what it may then reach is decided by Landlock and
/// the namespaces it was started in.
#[cfg(target_os = "linux")]
pub fn driver_program(own_pid: u32) -> Result<BpfProgram, SeccompError> {
    let mut rules = baseline_rules(SeccompProfile::TiloDefault, SyscallGrants::default())?;
    for nr in PARSER_DENIES {
        if !matches!(*nr, libc::SYS_execve | libc::SYS_execveat) {
            rules.insert(*nr, Vec::new());
        }
    }
    rules.insert(libc::SYS_pidfd_open, Vec::new());
    rules.insert(libc::SYS_pidfd_send_signal, Vec::new());
    for nr in SIGNAL_TARGET_SYSCALLS {
        rules.insert(
            *nr,
            vec![SeccompRule::new(vec![SeccompCondition::new(
                0,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::Ne,
                u64::from(own_pid),
            )?])?],
        );
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

/// Decode the exact native-endian `sock_filter` bytes used by Bubblewrap.
/// Shape validation precedes the irreversible installation on every thread.
///
/// # Errors
///
/// Refuses empty, partial or oversized arrays, and kernel installation failures.
#[cfg(target_os = "linux")]
pub fn install_bytes(bytes: &[u8]) -> Result<(), SeccompError> {
    install(&decode_program_bytes(bytes)?)
}

#[cfg(target_os = "linux")]
fn decode_program_bytes(bytes: &[u8]) -> Result<BpfProgram, SeccompError> {
    if bytes.is_empty() || bytes.len() > 4096 * 8 || !bytes.len().is_multiple_of(8) {
        return Err(SeccompError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "Seccomp instruction array shape differs",
        )));
    }
    bytes
        .chunks_exact(8)
        .map(|word| {
            let &[code0, code1, jt, jf, k0, k1, k2, k3] = word else {
                return Err(SeccompError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Seccomp instruction is incomplete",
                )));
            };
            Ok(seccompiler::sock_filter {
                code: u16::from_ne_bytes([code0, code1]),
                jt,
                jf,
                k: u32::from_ne_bytes([k0, k1, k2, k3]),
            })
        })
        .collect()
}

#[cfg(all(test, target_os = "linux"))]
mod byte_codec_tests {
    use super::*;
    tilo_test_env::scenario! {
        fn compiled_policies_survive_the_byte_codec_or_refuse_before_installation;

        phase native_compiled_policy_roundtrips_without_changing_instructions;
        {
            for profile in [
                SeccompProfile::TiloDefault,
                SeccompProfile::TiloJit,
                SeccompProfile::TiloWebEngine,
            ] {
                let program = baseline_program(profile).unwrap();
                let bytes = program_bytes(&program);
                assert_eq!(program_bytes(&decode_program_bytes(&bytes).unwrap()), bytes);
            }
            let word = [0x12, 0x34, 5, 6, 0x78, 0x9a, 0xbc, 0xde];
            let decoded = decode_program_bytes(&word).unwrap();
            assert_eq!(decoded[0].code, u16::from_ne_bytes([0x12, 0x34]));
            assert_eq!((decoded[0].jt, decoded[0].jf), (5, 6));
            assert_eq!(decoded[0].k, u32::from_ne_bytes([0x78, 0x9a, 0xbc, 0xde]));
        }

        phase incomplete_and_oversized_arrays_refuse_before_installation;
        {
            for bytes in [Vec::new(), vec![0; 7], vec![0; 9], vec![0; 4097 * 8]] {
                assert!(install_bytes(&bytes).is_err());
            }
            assert!(decode_program_bytes(&vec![0; 4096 * 8]).is_ok());
        }
    }
}

/// Compile the baseline policy to a raw BPF blob.
///
/// Takes the profile explicitly rather than defaulting, so every call
/// site has decided whether the program is allowed to generate code.
#[cfg(target_os = "linux")]
pub fn compile_baseline(profile: SeccompProfile) -> Result<Vec<u8>, SeccompError> {
    Ok(program_bytes(&baseline_program(profile)?))
}

/// [`compile_baseline`] for an app holding `grants`.
///
/// # Errors
///
/// As [`compile_baseline`].
#[cfg(target_os = "linux")]
pub fn compile_app_baseline(
    profile: SeccompProfile,
    grants: SyscallGrants,
) -> Result<Vec<u8>, SeccompError> {
    Ok(program_bytes(&baseline_program_with(profile, grants)?))
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

        wait_for(pid)
    }

    /// Install the baseline for `profile` in a forked child and ask for the
    /// namespaces and `chroot` of a nested sandbox.
    ///
    /// Every request is one the kernel itself refuses without side effects
    /// (`CLONE_UNTRACED` is not an `unshare` flag, and the empty path does not
    /// exist), so the errno tells the filter's `EPERM` from the kernel's
    /// answer on any host, whatever its user-namespace policy.
    ///
    /// Exit codes:
    ///   0  = behaved exactly as the profile says it should
    ///   20 = a nested-sandbox namespace got the wrong answer
    ///   21 = another namespace kind reached the kernel
    ///   22 = `chroot` got the wrong answer
    ///   12 = the filter itself would not install
    #[allow(
        clippy::expect_used,
        reason = "a test probe: a failure to set the experiment up is the test failing"
    )]
    pub fn probe_namespaces_under(profile: SeccompProfile) -> i32 {
        let program = baseline_program(profile).expect("compile filter");
        let nested = profile.allows_nested_sandbox();

        // SAFETY: the child performs only async-signal-safe work (seccomp
        // install via prctl, unshare, chroot, _exit) and never returns into
        // the test harness.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let bail = |code: i32| -> ! {
                // SAFETY: _exit is async-signal-safe and does not unwind.
                unsafe { libc::_exit(code) }
            };
            let filtered = |result: libc::c_int| {
                // SAFETY: reads this thread's errno right after the call.
                result != 0 && unsafe { *libc::__errno_location() } == libc::EPERM
            };
            if seccompiler::apply_filter(&program).is_err() {
                bail(12);
            }
            for flag in super::WEB_ENGINE_SANDBOX_NAMESPACES {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "namespace flags are 32-bit clone flags"
                )]
                let flags = (*flag | libc::CLONE_UNTRACED as u64) as libc::c_int;
                // SAFETY: an invalid flag set; the kernel changes nothing.
                if filtered(unsafe { libc::unshare(flags) }) == nested {
                    bail(20);
                }
            }
            // SAFETY: as above.
            if !filtered(unsafe { libc::unshare(libc::CLONE_NEWNS | libc::CLONE_UNTRACED) }) {
                bail(21);
            }
            // SAFETY: the empty path never names a directory.
            if filtered(unsafe { libc::chroot(c"".as_ptr()) }) == nested {
                bail(22);
            }
            bail(0);
        }
        wait_for(pid)
    }

    fn wait_for(pid: libc::pid_t) -> i32 {
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
    #[cfg(target_os = "linux")]
    use tilo_test_env::SpawnUnderEnvLock as _;

    tilo_test_env::scenario! {
        #[cfg(target_os = "linux")]
        fn seccomp_profiles_compile_to_the_rules_their_grants_allow;

        phase seccomp_baselines_compile_serialise_and_select_write_exec_rules;
        {
            {
                let bytes = compile_baseline(SeccompProfile::TiloDefault).expect("compile");
                assert!(!bytes.is_empty(), "empty BPF");
                assert_eq!(
                    bytes.len() % 8,
                    0,
                    "BPF blob length {} not aligned to 8-byte sock_filter",
                    bytes.len()
                );
            }
            {
                let dir = tempfile::tempdir().unwrap();
                let path = write_baseline_to(dir.path(), SeccompProfile::TiloDefault).unwrap();
                assert!(path.exists());
                let bytes = std::fs::read(&path).unwrap();
                assert_eq!(
                    bytes,
                    compile_baseline(SeccompProfile::TiloDefault).unwrap()
                );
            }
            {
                let strict = compile_baseline(SeccompProfile::TiloDefault).unwrap();
                let jit = compile_baseline(SeccompProfile::TiloJit).unwrap();
                assert!(
                    strict.len() > jit.len(),
                    "the strict profile must carry more program than the JIT one \
                 (strict={} bytes, jit={} bytes)",
                    strict.len(),
                    jit.len()
                );
                assert!(!jit.is_empty());
                assert_eq!(jit.len() % 8, 0);
            }
            {
                let mut rules = BTreeMap::new();
                rules.insert(libc::SYS_ptrace, Vec::new());
                let program = deny_list_program(rules.clone()).unwrap();
                assert_eq!(compile_deny_list(rules).unwrap(), program_bytes(&program));
                assert_eq!(program_bytes(&program).len(), program.len() * 8);
            }
        }

        phase io_uring_is_refused_unless_the_app_holds_the_grant;
        // `io_uring` is a permission: the baseline refuses it, an app holding
        // the grant reaches the kernel. EPERM is what tokio's driver and libuv
        // treat as "no `io_uring` here" and fall back to `epoll` from.
        {
            // `io_uring_setup(0, NULL)`: the kernel itself answers EINVAL (no
            // entries) or, where the host disabled io_uring, EPERM or ENOSYS.
            fn setup_errno() -> i32 {
                // SAFETY: zero entries and a null parameter block; the kernel
                // rejects the call before touching memory.
                let rc = unsafe { libc::syscall(libc::SYS_io_uring_setup, 0_i64, 0_i64) };
                if rc >= 0 {
                    // SAFETY: closing the descriptor the kernel just returned.
                    unsafe { libc::close(i32::try_from(rc).unwrap_or(-1)) };
                    0
                } else {
                    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
                }
            }
            fn under(grants: SyscallGrants) -> i32 {
                let program = baseline_program_with(SeccompProfile::TiloDefault, grants).unwrap();
                // SAFETY: the child installs a filter, makes one raw syscall and
                // exits without returning into the test harness.
                let pid = unsafe { libc::fork() };
                assert!(pid >= 0, "fork failed");
                if pid == 0 {
                    let code = if seccompiler::apply_filter(&program).is_err() {
                        255
                    } else {
                        setup_errno()
                    };
                    // SAFETY: `_exit` does not unwind.
                    unsafe { libc::_exit(code) };
                }
                let mut status = 0;
                // SAFETY: waiting on our own child.
                unsafe { libc::waitpid(pid, &raw mut status, 0) };
                assert!(libc::WIFEXITED(status), "status {status:#x}");
                libc::WEXITSTATUS(status)
            }

            let rules = baseline_rules(SeccompProfile::TiloDefault, SyscallGrants::default()).unwrap();
            let granted = baseline_rules(
                SeccompProfile::TiloDefault,
                SyscallGrants {
                    io_uring: true,
                    ..SyscallGrants::default()
                },
            )
            .unwrap();
            for nr in IO_URING_SYSCALLS {
                assert_eq!(rules.get(nr), Some(&Vec::new()), "syscall {nr} not refused");
                assert!(
                    !granted.contains_key(nr),
                    "syscall {nr} refused despite the grant"
                );
            }
            // Every profile refuses it by default; the grant changes nothing else.
            for profile in [
                SeccompProfile::TiloDefault,
                SeccompProfile::TiloJit,
                SeccompProfile::TiloWebEngine,
            ] {
                let mut denied = baseline_rules(profile, SyscallGrants::default()).unwrap();
                let allowed = baseline_rules(profile, SyscallGrants {
                    io_uring: true,
                    ..SyscallGrants::default()
                }).unwrap();
                for nr in IO_URING_SYSCALLS {
                    assert!(denied.remove(nr).is_some());
                }
                assert_eq!(denied, allowed, "{}", profile.as_str());
            }

            assert_eq!(
                under(SyscallGrants::default()),
                libc::EPERM,
                "the baseline must refuse io_uring_setup with EPERM"
            );
            let host = setup_errno();
            if host != libc::EPERM {
                assert_eq!(
                    under(SyscallGrants {
                    io_uring: true,
                    ..SyscallGrants::default()
                }),
                    host,
                    "with the grant the kernel answers, as it does unconfined"
                );
            }
        }

        phase only_the_web_engine_profile_builds_a_nested_sandbox;
        // The web engine gets the user, PID and network namespaces and chroot of
        // Chromium's renderer sandbox; every profile refuses the rest.
        {
            for profile in [
                SeccompProfile::TiloDefault,
                SeccompProfile::TiloJit,
                SeccompProfile::TiloWebEngine,
            ] {
                assert_eq!(
                    testing::probe_namespaces_under(profile),
                    0,
                    "{}: 20 = a sandbox namespace got the wrong answer, 21 = another \
                 namespace kind was allowed, 22 = chroot got the wrong answer, \
                 12 = the filter would not install",
                    profile.as_str()
                );
            }
        }
    }

    /// A 32-bit call through `int 0x80`, as Wine's `WoW64` mode makes it. `ebx` holds the one
    /// argument; LLVM keeps `rbx` for itself, so it is swapped in and back around the call.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn i386_call(nr: i32, arg: u32) -> i32 {
        let result: i32;
        // SAFETY: `int 0x80` enters the kernel's 32-bit entry and returns; rbx is restored.
        unsafe {
            std::arch::asm!(
                "xchg {arg:r}, rbx",
                "int 0x80",
                "xchg {arg:r}, rbx",
                arg = inout(reg) u64::from(arg) => _,
                inlateout("eax") nr => result,
                options(nostack),
            );
        }
        result
    }

    /// How a forked child ended.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[derive(Debug, PartialEq, Eq)]
    enum Ended {
        /// It exited with the call's result (its magnitude, at most 99).
        Exited(i32),
        /// A signal killed it.
        Killed(i32),
    }

    /// How a forked child that installs `program` (or none) and makes one i386 call ended.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn i386_call_under(program: Option<&BpfProgram>, nr: i32) -> Ended {
        // SAFETY: the child installs a filter, makes one call and exits without unwinding.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            if let Some(program) = program
                && seccompiler::apply_filter(program).is_err()
            {
                // SAFETY: `_exit` does not unwind.
                unsafe { libc::_exit(100) };
            }
            // A null descriptor: an allowed set_thread_area answers EFAULT.
            let result = i386_call(nr, 0);
            // SAFETY: `_exit` does not unwind.
            unsafe { libc::_exit(i32::try_from(result.unsigned_abs().min(99)).unwrap_or(99)) };
        }
        let mut status = 0;
        // SAFETY: waiting for the child forked above.
        assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
        if libc::WIFSIGNALED(status) {
            Ended::Killed(libc::WTERMSIG(status))
        } else {
            Ended::Exited(libc::WEXITSTATUS(status))
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn wine_may_set_its_32_bit_thread_area_and_make_no_other_32_bit_call() {
        const I386_GETPID: i32 = 20;
        const I386_SET_THREAD_AREA: i32 = 243;
        if matches!(i386_call_under(None, I386_GETPID), Ended::Killed(_)) {
            eprintln!("this kernel takes no 32-bit calls; nothing to check");
            return;
        }
        let wine = baseline_program_with(
            SeccompProfile::TiloJit,
            SyscallGrants {
                wow64_thread_area: true,
                ..SyscallGrants::default()
            },
        )
        .expect("compile the Wine filter");
        let plain = baseline_program(SeccompProfile::TiloJit).expect("compile the baseline");
        assert_eq!(
            i386_call_under(Some(&wine), I386_SET_THREAD_AREA),
            Ended::Exited(libc::EFAULT),
            "the granted call reaches the kernel"
        );
        assert_eq!(
            i386_call_under(Some(&wine), I386_GETPID),
            Ended::Killed(libc::SIGSYS),
            "every other 32-bit call is still killed"
        );
        assert_eq!(
            i386_call_under(Some(&plain), I386_SET_THREAD_AREA),
            Ended::Killed(libc::SIGSYS),
            "without the grant the call is killed"
        );
        assert_eq!(
            plain.len() + 4,
            wine.len(),
            "the grant adds only the exception ahead of the same rules"
        );
        assert_eq!(plain[3..], wine[7..]);
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

    tilo_test_env::scenario! {
        #[cfg(target_os = "linux")]
        fn seccomp_programs_are_enforced_by_the_kernel;

        phase seccomp_filters_are_enforced_by_the_kernel;
        {
            // Phase `clone3_compat_preserves_pthreads_without_exposing_clone3`.
            {
                let current = std::env::current_exe().expect("current test executable");
                let status = std::process::Command::new(current)
                    .arg("seccomp::tests::thread_creation_probe_in_clean_process")
                    .arg("--exact")
                    .arg("--test-threads=1")
                    .env("TILO_SECCOMP_THREAD_PROBE", "1")
                    .status_locked()
                    .expect("run clean seccomp probe process");
                assert_eq!(
                    status.code(),
                    Some(0),
                    "11 = pthread creation was refused, 12 = a filter would not install"
                );
            }
            {
                assert_eq!(
                    testing::probe_mappings_under(SeccompProfile::TiloDefault),
                    0,
                    "10 = W+X was allowed, 11 = an ordinary mapping was refused, \
                 12 = the filter would not install"
                );
            }
            {
                for profile in [SeccompProfile::TiloJit, SeccompProfile::TiloWebEngine] {
                    assert_eq!(
                        testing::probe_mappings_under(profile),
                        0,
                        "{}: 10 = W+X was allowed, 11 = an ordinary mapping was refused, \
                     12 = the filter would not install",
                        profile.as_str()
                    );
                }
            }
            {
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
            {
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
                        if fd != -1
                            || std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM)
                        {
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

        phase parser_program_with_socket_pairs_allows_pairs_and_no_socket;
        // A media helper's filter lets it make a connected pair, which its
        // framework's buses wake through, and still no socket that could be
        // bound or connected.
        {
            let program = parser_program_with_socket_pairs().expect("compile filter");
            // SAFETY: the child installs a filter, invokes raw syscalls with inert
            // arguments, and exits without returning into the test harness.
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0, "fork failed");
            if pid == 0 {
                if seccompiler::apply_filter(&program).is_err() {
                    // SAFETY: `_exit` does not unwind.
                    unsafe { libc::_exit(12) };
                }
                let mut pair = [-1_i32; 2];
                // SAFETY: socketpair(2) writes two descriptors into `pair`.
                let rc =
                    unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, pair.as_mut_ptr()) };
                if rc != 0 {
                    // SAFETY: `_exit` does not unwind.
                    unsafe { libc::_exit(13) };
                }
                for family in [libc::AF_UNIX, libc::AF_INET, libc::AF_INET6] {
                    // SAFETY: socket(2) with constant arguments.
                    let fd = unsafe { libc::socket(family, libc::SOCK_STREAM, 0) };
                    if fd != -1 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM) {
                        // SAFETY: `_exit` does not unwind.
                        unsafe { libc::_exit(10) };
                    }
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
                "10 = a socket was created, 12 = the filter would not install, 13 = no pair"
            );
        }

        phase driver_program_signals_only_itself_and_opens_no_socket;
        {
            {
                // SAFETY: the child compiles and installs a filter, invokes
                // syscalls with inert arguments, and exits without returning into
                // the test harness.
                let pid = unsafe { libc::fork() };
                assert!(pid >= 0, "fork failed");
                if pid == 0 {
                    // SAFETY: getpid/getppid cannot fail.
                    let (own, parent) = unsafe { (libc::getpid(), libc::getppid()) };
                    let installed = u32::try_from(own)
                        .ok()
                        .and_then(|own| driver_program(own).ok())
                        .is_some_and(|program| seccompiler::apply_filter(&program).is_ok());
                    if !installed {
                        // SAFETY: `_exit` does not unwind.
                        unsafe { libc::_exit(12) };
                    }
                    // SAFETY: socket(2) with constant arguments.
                    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
                    if fd != -1 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM) {
                        // SAFETY: `_exit` does not unwind.
                        unsafe { libc::_exit(10) };
                    }
                    // SAFETY: signal 0 only probes; the filter decides first.
                    let other = unsafe { libc::kill(parent, 0) };
                    if other != -1
                        || std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM)
                    {
                        // SAFETY: `_exit` does not unwind.
                        unsafe { libc::_exit(11) };
                    }
                    // SAFETY: as above, aimed at this process.
                    if unsafe { libc::kill(own, 0) } != 0 {
                        // SAFETY: `_exit` does not unwind.
                        unsafe { libc::_exit(13) };
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
                    "10 = a socket was created, 11 = another process was signalled, \
                 12 = the filter would not install, 13 = it could not signal itself"
                );
            }
        }

        phase allow_list_kills_on_any_other_syscall_in_the_kernel;
        {
            let mut rules = BTreeMap::new();
            for nr in [libc::SYS_getpid, libc::SYS_exit_group, libc::SYS_exit] {
                rules.insert(nr, Vec::new());
            }
            let program = allow_list_program(rules).expect("compile filter");
            // SAFETY: the child installs a filter, makes raw syscalls and exits
            // without returning into the test harness.
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0, "fork failed");
            if pid == 0 {
                // The kill is the outcome under test, not a crash to debug: it
                // must not leave a core file in the directory the tests run in.
                let no_core = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                // SAFETY: `setrlimit` only reads the limit it is handed.
                if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &raw const no_core) } != 0
                    || seccompiler::apply_filter(&program).is_err()
                {
                    // SAFETY: `_exit` does not unwind.
                    unsafe { libc::_exit(12) };
                }
                // SAFETY: getpid has no arguments; allowed by the filter.
                unsafe { libc::syscall(libc::SYS_getpid) };
                // SAFETY: getppid is not listed, so the filter kills us here.
                unsafe { libc::syscall(libc::SYS_getppid) };
                // SAFETY: `_exit` does not unwind.
                unsafe { libc::_exit(0) };
            }
            let mut status = 0;
            // SAFETY: waiting on our own child.
            unsafe { libc::waitpid(pid, &raw mut status, 0) };
            assert!(
                libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGSYS,
                "the unlisted syscall must kill with SIGSYS (status {status:#x})"
            );
        }
    }

    /// A clean process for `seccomp_filters_are_enforced_by_the_kernel` to
    /// re-exec with `TILO_SECCOMP_THREAD_PROBE` set, since the parent's own
    /// threads were made before its filters. Anywhere else it returns at once.
    #[test]
    #[cfg(target_os = "linux")]
    #[allow(
        clippy::exit,
        reason = "the parent test distinguishes seccomp setup and pthread failures by helper-process exit status"
    )]
    fn thread_creation_probe_in_clean_process() {
        if std::env::var_os("TILO_SECCOMP_THREAD_PROBE").is_none() {
            return;
        }
        let compat = clone3_compat_program().expect("build clone3 compatibility filter");
        let baseline = baseline_program(SeccompProfile::TiloDefault).expect("build baseline");
        if seccompiler::apply_filter(&compat).is_err()
            || seccompiler::apply_filter(&baseline).is_err()
        {
            std::process::exit(12);
        }
        let result = std::thread::Builder::new()
            .name("seccomp-thread-probe".into())
            .spawn(|| 42_u8)
            .and_then(|thread| {
                thread
                    .join()
                    .map_err(|_| std::io::Error::other("thread panicked"))
            });
        if !matches!(result, Ok(42)) {
            std::process::exit(11);
        }
    }
}
