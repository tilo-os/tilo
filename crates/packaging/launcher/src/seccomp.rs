//! Baseline seccomp-bpf policy.
//!
//! Policy source: based on Flatpak's published seccomp profile —
//! `common/flatpak-run.c`'s `setup_seccomp` in
//! <https://github.com/flatpak/flatpak> — which itself is derived from
//! the Bubblewrap reference deny-list. Specifically we copy the
//! "always denied" set: namespace manipulation (`unshare`, `setns`),
//! `mount`/`umount2`/`pivot_root`/`chroot`, `ptrace`, raw socket
//! creation (`socket` with `AF_PACKET`), `bpf`, `keyctl`, and
//! `clone3` (Flatpak treats this as a transitional always-deny so
//! older container managers behave consistently).
//!
//! We deliberately keep the policy small and *deny-list*, not a
//! sweeping allow-list: a hand-rolled allow-list breaks unpredictably
//! when glibc adds a new syscall (e.g. `getrandom`, `clone3`,
//! `faccessat2`) and the syscall surface differs per architecture.
//! The same shape as the per-workspace filter in
//! `shell/src/sandbox/seccomp.rs` — kept independent so the launcher
//! has zero dep on the compositor.
//!
//! The write-xor-execute rules below exist only here, not in that copy.
//! That is on purpose: the compositor does not spawn applications, it
//! execs this launcher, and `shell/src/sandbox/mod.rs` says outright that
//! its own profile is on no launch path. Hardening a filter nothing
//! installs would read as protection that is not there.
//!
//! At runtime the launcher compiles this filter into a BPF blob,
//! writes it to a temp file, and passes the open fd to `bwrap`'s
//! `--seccomp <fd>`. (Alternative: build it at *build time* and ship
//! the blob in `share/qos-default.bpf` — see [`baked_blob_path`].)
//!
//! The compiled blob is identical to `compile_baseline()`'s output for
//! the current build arch. We expose both APIs:
//!
//! - [`compile_baseline`] — compile at runtime; default fast path.
//! - [`baked_blob_path`] — return the on-disk blob path under
//!   `share/qos-default.bpf` if it has been pre-populated. Useful for
//!   environments where seccompiler can't run inside the sandbox.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use seccompiler::{
    BackendError, BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition,
    SeccompFilter, SeccompRule, TargetArch,
};

/// Built-in baseline policy errors.
#[derive(Debug, thiserror::Error)]
pub enum SeccompError {
    #[error("unsupported host architecture for seccomp baseline: {0}")]
    UnsupportedArch(&'static str),
    #[error("seccompiler compile error: {0}")]
    Compile(#[from] BackendError),
    #[error("io error while staging baseline blob: {0}")]
    Io(#[from] io::Error),
}

fn host_target_arch() -> Result<TargetArch, SeccompError> {
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

/// The deny-list, named so a future Flatpak-policy bump diffs cleanly.
///
/// `clone3` is deliberately NOT denied — Flatpak's historical rationale
/// for blocking it was "some older container tooling doesn't understand
/// clone3, force the clone(2) fallback for consistency." That deny
/// broke modern glibc's `pthread_create`, which uses clone3 as the
/// primary syscall and only occasionally falls back to clone. Symptom
/// was `zbus` (portal client) panicking with "Operation not permitted"
/// on worker-thread spawn. Since we already allow `clone`, denying
/// `clone3` blocks nothing of substance — it just makes a common code
/// path unreliable. Enabling clone3 restores the portal-inside-sandbox
/// path.
const BLANKET_DENIES: &[libc::c_long] = &[
    libc::SYS_ptrace,
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_pivot_root,
    libc::SYS_chroot,
    libc::SYS_unshare,
    libc::SYS_setns,
    libc::SYS_bpf,
    libc::SYS_keyctl,
];

/// Which named seccomp profile a package's `[sandbox] seccomp = "…"` asked
/// for.
///
/// The only thing the name currently decides is whether the app may create
/// memory that is writable and executable at the same time. That is a
/// per-package question and not a per-user one, which is why it is answered by
/// the signed manifest rather than by a runtime grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeccompProfile {
    /// `qos-default` — write-xor-execute enforced. An app cannot turn data it
    /// received over the network into code.
    #[default]
    QosDefault,
    /// `qos-jit` — the baseline minus the write-xor-execute rules, for the
    /// one class of program that genuinely compiles code at run time: a web
    /// engine's JavaScript tier. Everything else in the deny-list still
    /// applies.
    QosJit,
}

impl SeccompProfile {
    /// Resolve a manifest profile name.
    ///
    /// Unknown names resolve to [`SeccompProfile::QosDefault`], i.e. the
    /// strict one. A typo must not be a way to acquire a weaker sandbox: the
    /// app then fails visibly on its first attempt to allocate W+X memory,
    /// which is the direction of failure we want.
    pub fn parse(name: &str) -> Self {
        match name {
            "qos-jit" => Self::QosJit,
            _ => Self::QosDefault,
        }
    }

    /// The manifest spelling, for logging and round-tripping.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QosDefault => "qos-default",
            Self::QosJit => "qos-jit",
        }
    }

    /// Whether this profile lets the app map memory writable and executable
    /// at once.
    pub fn allows_dynamic_code(self) -> bool {
        matches!(self, Self::QosJit)
    }
}

/// Syscalls that take a page-protection bitmask in argument 2 and can
/// therefore produce a writable, executable mapping.
const PROT_ARG_SYSCALLS: &[libc::c_long] = &[
    libc::SYS_mmap,
    libc::SYS_mprotect,
    libc::SYS_pkey_mprotect,
];

/// `PROT_WRITE | PROT_EXEC`, used both as the mask and as the value so the
/// rule reads "both of these bits are set" rather than "the protection is
/// exactly this" — `PROT_READ` is almost always set alongside them.
fn write_exec_bits() -> u64 {
    (libc::PROT_WRITE | libc::PROT_EXEC) as u64
}

fn build_baseline_filter(profile: SeccompProfile) -> Result<SeccompFilter, SeccompError> {
    let arch = host_target_arch()?;
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    for nr in BLANKET_DENIES {
        rules.insert(*nr as i64, Vec::new());
    }

    // Write-xor-execute. A page that is writable and executable at the same
    // time is what turns a heap or stack write into code execution: the
    // attacker no longer needs to find existing instructions to reuse, they
    // can write their own. Refusing the combination costs an app nothing
    // unless it compiles code at run time, and the only such app here is the
    // browser's web engine, which asks for `qos-jit`.
    //
    // `MaskedEq` compares `(prot & mask) == (value & mask)`, so this matches
    // any protection with both bits set and leaves `PROT_READ|PROT_WRITE`
    // (ordinary data) and `PROT_READ|PROT_EXEC` (a loaded library, or the
    // second half of a well-behaved JIT's write-then-flip) alone. `Dword`
    // because `prot` is an `int`; a `Qword` compare could pick up whatever
    // occupies the upper half of the register.
    //
    // Honest limit: this does not stop an app that maps the same memfd twice,
    // once writable and once executable. Blocking that needs the file
    // descriptor's history, which a syscall-argument filter cannot see. What
    // it does remove is every one-call route to W+X, which is the shape that
    // actually appears in exploit chains against a renderer.
    if !profile.allows_dynamic_code() {
        for nr in PROT_ARG_SYSCALLS {
            rules.insert(
                *nr as i64,
                vec![SeccompRule::new(vec![SeccompCondition::new(
                    2,
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::MaskedEq(write_exec_bits()),
                    write_exec_bits(),
                )?])?],
            );
        }
    }

    // socket(family, type, protocol) where family == AF_PACKET.
    rules.insert(
        libc::SYS_socket as i64,
        vec![SeccompRule::new(vec![SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Eq,
            libc::AF_PACKET as u64,
        )?])?],
    );

    // Default-allow, denied-syscalls return -EPERM (Flatpak convention:
    // a clean errno rather than SIGSYS, so apps log the failure
    // instead of getting killed).
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )?;
    Ok(filter)
}

fn bpf_to_bytes(prog: &BpfProgram) -> Vec<u8> {
    let mut out = Vec::with_capacity(prog.len() * 8);
    for stmt in prog {
        out.extend_from_slice(&stmt.code.to_ne_bytes());
        out.push(stmt.jt);
        out.push(stmt.jf);
        out.extend_from_slice(&stmt.k.to_ne_bytes());
    }
    out
}

/// Compile the baseline policy to a raw BPF blob.
///
/// Takes the profile explicitly rather than defaulting, so every call site has
/// to have decided whether the program it is about to confine is allowed to
/// generate code.
pub fn compile_baseline(profile: SeccompProfile) -> Result<Vec<u8>, SeccompError> {
    let filter = build_baseline_filter(profile)?;
    let program: BpfProgram = filter.try_into()?;
    Ok(bpf_to_bytes(&program))
}

/// Write the compiled blob to a path inside `dir`, returning that path.
/// Caller is responsible for opening it and handing the fd to bwrap.
pub fn write_baseline_to(dir: &Path, profile: SeccompProfile) -> Result<PathBuf, SeccompError> {
    std::fs::create_dir_all(dir)?;
    let bytes = compile_baseline(profile)?;
    let path = dir.join(format!("{}.bpf", profile.as_str()));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// Path the launcher checks for a pre-baked blob. If present, prefer it
/// over compiling. Looked up relative to `$CARGO_MANIFEST_DIR` at build
/// time and `<crate_root>/share/qos-default.bpf` at runtime — but in
/// production the file lives in a fixed location alongside the binary,
/// so this is a developer convenience only.
pub fn baked_blob_path(profile: SeccompProfile) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("share")
        .join(format!("{}.bpf", profile.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_compiles_and_blob_alignment() {
        let bytes = compile_baseline(SeccompProfile::QosDefault).expect("compile");
        assert!(!bytes.is_empty(), "empty BPF");
        assert_eq!(
            bytes.len() % 8,
            0,
            "BPF blob length {} not aligned to 8-byte sock_filter",
            bytes.len()
        );
    }

    #[test]
    fn write_baseline_creates_blob() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_baseline_to(dir.path(), SeccompProfile::QosDefault).unwrap();
        assert!(path.exists());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len() % 8, 0);
    }

    #[test]
    fn unknown_profile_name_resolves_to_the_strict_one() {
        for name in ["qos-default", "", "qos-jitt", "QOS-JIT", "anything"] {
            assert_eq!(
                SeccompProfile::parse(name),
                SeccompProfile::QosDefault,
                "{name:?} must not resolve to a weaker profile"
            );
        }
        assert_eq!(SeccompProfile::parse("qos-jit"), SeccompProfile::QosJit);
    }

    #[test]
    fn jit_profile_is_the_only_one_that_drops_the_write_exec_rules() {
        let strict = compile_baseline(SeccompProfile::QosDefault).unwrap();
        let jit = compile_baseline(SeccompProfile::QosJit).unwrap();
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

    /// The filter is only worth anything if the kernel enforces it, so install
    /// it for real and try the mappings.
    ///
    /// Runs in a forked child because a seccomp filter cannot be removed once
    /// applied — the test process has to survive to run the rest of the suite.
    /// The child does nothing but raw syscalls and `_exit`, so no allocator or
    /// lock is touched after the fork.
    ///
    /// Exit codes are the report channel:
    ///   0  = behaved exactly as the profile says it should
    ///   10 = a mapping that should have been refused succeeded
    ///   11 = a mapping that should have been allowed was refused
    ///   12 = the filter itself would not install
    fn probe_mappings_under(profile: SeccompProfile) -> i32 {
        let filter = build_baseline_filter(profile).expect("build filter");
        let program: BpfProgram = filter.try_into().expect("compile filter");
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
        unsafe { libc::waitpid(pid, &mut status, 0) };
        assert!(
            libc::WIFEXITED(status),
            "child did not exit normally (status {status:#x})"
        );
        libc::WEXITSTATUS(status)
    }

    #[test]
    fn strict_profile_refuses_writable_executable_memory_in_the_kernel() {
        assert_eq!(
            probe_mappings_under(SeccompProfile::QosDefault),
            0,
            "10 = W+X was allowed, 11 = an ordinary mapping was refused, \
             12 = the filter would not install"
        );
    }

    #[test]
    fn jit_profile_still_permits_writable_executable_memory() {
        assert_eq!(
            probe_mappings_under(SeccompProfile::QosJit),
            0,
            "10 = W+X was allowed, 11 = an ordinary mapping was refused, \
             12 = the filter would not install"
        );
    }
}
