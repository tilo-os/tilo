//! Baseline seccomp-bpf policy.
//!
//! Policy source: based on Flatpak's published seccomp profile
//! (`common/flatpak-run.c`'s `setup_seccomp`), itself derived from the
//! Bubblewrap reference deny-list. Copies the "always denied" set:
//! namespace manipulation (`unshare`, `setns`),
//! `mount`/`umount2`/`pivot_root`/`chroot`, `ptrace`, raw socket
//! creation (`AF_PACKET`), `bpf`, `keyctl`, and `clone3` (Flatpak's
//! transitional always-deny for older container managers).
//!
//! Deliberately small and *deny-list*, not a sweeping allow-list: a
//! hand-rolled allow-list breaks unpredictably when glibc adds a new
//! syscall and the surface differs per architecture. Same shape as the
//! per-workspace filter in `crates/session/shell/src/sandbox/seccomp.rs`, kept
//! independent so the launcher has zero dep on the compositor.
//!
//! The write-xor-execute rules below exist only here, not in that copy:
//! the compositor doesn't spawn applications, it execs this launcher,
//! and its own profile is on no launch path — hardening a filter
//! nothing installs would read as protection that isn't there.
//!
//! At runtime the launcher compiles this filter into a BPF blob, writes
//! it to a temp file, and passes the open fd to `bwrap`'s `--seccomp
//! <fd>`. [`compile_baseline`] is the default fast path;
//! [`baked_blob_path`] returns a pre-populated on-disk blob under
//! `share/tilo-default.bpf` for environments where seccompiler can't run
//! inside the sandbox — identical output for the current build arch.

#[cfg(target_os = "linux")]
use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::io;
#[cfg(any(test, target_os = "linux"))]
use std::path::Path;
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use seccompiler::{
    BackendError, BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition,
    SeccompFilter, SeccompRule, TargetArch,
};

/// Built-in baseline policy errors.
#[derive(Debug, thiserror::Error)]
#[cfg(target_os = "linux")]
pub enum SeccompError {
    #[error("unsupported host architecture for seccomp baseline: {0}")]
    UnsupportedArch(&'static str),
    #[error("seccompiler compile error: {0}")]
    Compile(#[from] BackendError),
    #[error("io error while staging baseline blob: {0}")]
    Io(#[from] io::Error),
}

#[cfg(target_os = "linux")]
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
/// `clone3` is deliberately NOT denied: modern glibc's `pthread_create`
/// uses it as the primary syscall, and since `clone` is already
/// allowed, denying `clone3` blocks nothing of substance — it just
/// makes a common code path (including the portal client's
/// worker-thread spawn) unreliable.
#[cfg(target_os = "linux")]
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
/// entitlement list decides. See [`resolve_profile`], and prefer it to
/// [`Self::parse`] anywhere a real launch is being confined.
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

/// Image-owned list of package ids that may run without
/// write-xor-execute.
///
/// One entry per line, `#` starts a comment, a trailing `*` is a prefix
/// match — same format as [`crate::hardening::POLICY_PATH`], so the two
/// image-owned policies cannot drift into different syntaxes.
pub const JIT_POLICY_PATH: &str = "/usr/lib/tilo/jit-entitled.conf";

/// Overrides [`JIT_POLICY_PATH`], so a test can point at a fixture. Honoured
/// only by developer builds (see [`crate::dev_env`]): the file grants
/// exemption from write-xor-execute.
pub const JIT_POLICY_PATH_ENV: &str = "TILO_JIT_ENTITLED_FILE";

/// Fallback when [`JIT_POLICY_PATH`] is absent — the packages in this
/// tree that genuinely compile code at run time.
///
/// One entry, and it should stay that way: the browser's JavaScript
/// tier emits machine code it then executes; anything else almost
/// certainly wants a write-then-flip JIT, which the strict profile
/// already allows.
pub const BUILTIN_JIT_ENTITLED: &[&str] = &["so.tilo.browser"];

/// Whether the image lets `package_id` run without write-xor-execute.
///
/// Same shape as [`crate::hardening::for_package`], for the same
/// reason: a missing file must degrade to the known set, not to
/// "nobody" — which would leave the browser unable to render on any
/// image that didn't ship the file.
pub fn entitled_to_dynamic_code(package_id: &str) -> bool {
    let policy_path = crate::dev_env::dev_override(JIT_POLICY_PATH_ENV)
        .map_or_else(|| PathBuf::from(JIT_POLICY_PATH), PathBuf::from);
    match std::fs::read_to_string(&policy_path) {
        Ok(list) => crate::hardening::policy_matches(&list, package_id),
        Err(_) => BUILTIN_JIT_ENTITLED.contains(&package_id),
    }
}

/// What a manifest asked for, and what it is actually going to get.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileDecision {
    /// The profile that will be compiled and installed.
    pub applied: SeccompProfile,
    /// The name the manifest wrote, verbatim.
    pub requested: String,
    /// Set when the manifest named a real, weaker profile it is not
    /// entitled to — as distinct from a typo, which [`SeccompProfile::parse`]
    /// already resolves strict.
    pub refused: bool,
}

/// Decide the syscall profile for one launch.
///
/// The manifest names a profile and the **image** decides whether this package
/// may have it. A `.tapp` carries its own manifest, so "the package asked for
/// it" is not an authority — it is the applicant filling in their own permit,
/// and write-xor-execute was the one protection a package could switch off by
/// writing a string about itself. Runtime hardening is already decided this
/// way; this is the same rule applied to the syscall filter.
///
/// Fail-closed twice over: an unrecognised name resolves strict, and a
/// recognised weaker name resolves strict too unless the image says this
/// package id may have it.
pub fn resolve_profile(requested: &str, package_id: &str) -> ProfileDecision {
    let asked = SeccompProfile::parse(requested);
    if !asked.allows_dynamic_code() || entitled_to_dynamic_code(package_id) {
        return ProfileDecision {
            applied: asked,
            requested: requested.to_string(),
            refused: false,
        };
    }
    ProfileDecision {
        applied: SeccompProfile::TiloDefault,
        requested: requested.to_string(),
        refused: true,
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
fn build_baseline_filter(profile: SeccompProfile) -> Result<SeccompFilter, SeccompError> {
    let arch = host_target_arch()?;
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

    // socket(family, type, protocol) where family == AF_PACKET.
    rules.insert(
        libc::SYS_socket,
        vec![SeccompRule::new(vec![SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Eq,
            libc::AF_PACKET as u64,
        )?])?],
    );

    // Default-allow, denied syscalls return -EPERM (Flatpak convention: a
    // clean errno so apps log the failure instead of getting killed).
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )?;
    Ok(filter)
}

#[cfg(target_os = "linux")]
fn bpf_to_bytes(prog: &BpfProgram) -> Vec<u8> {
    let mut out = Vec::with_capacity(prog.len().saturating_mul(8));
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
/// Takes the profile explicitly rather than defaulting, so every call
/// site has decided whether the program is allowed to generate code.
///
/// # Errors
///
/// Returns [`SeccompError`] if the filter's rules or its BPF compilation
/// fail to build.
#[cfg(target_os = "linux")]
pub fn compile_baseline(profile: SeccompProfile) -> Result<Vec<u8>, SeccompError> {
    let filter = build_baseline_filter(profile)?;
    let program: BpfProgram = filter.try_into()?;
    Ok(bpf_to_bytes(&program))
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

/// Path the launcher checks for a pre-baked blob, preferred over
/// compiling when present. Looked up relative to `$CARGO_MANIFEST_DIR`
/// at build time and `<crate_root>/share/tilo-default.bpf` at runtime —
/// a developer convenience only; production has a fixed location.
#[cfg(target_os = "linux")]
pub fn baked_blob_path(profile: SeccompProfile) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("share")
        .join(format!("{}.bpf", profile.as_str()))
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
        assert_eq!(bytes.len() % 8, 0);
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

    /// The filter is only worth anything if the kernel enforces it, so
    /// install it for real and try the mappings.
    ///
    /// Runs in a forked child because a seccomp filter cannot be removed
    /// once applied — the test process has to survive to run the rest of
    /// the suite. The child does nothing but raw syscalls and `_exit`, so
    /// no allocator or lock is touched after the fork.
    ///
    /// Exit codes are the report channel:
    ///   0  = behaved exactly as the profile says it should
    ///   10 = a mapping that should have been refused succeeded
    ///   11 = a mapping that should have been allowed was refused
    ///   12 = the filter itself would not install
    #[cfg(target_os = "linux")]
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
        unsafe { libc::waitpid(pid, &raw mut status, 0) };
        assert!(
            libc::WIFEXITED(status),
            "child did not exit normally (status {status:#x})"
        );
        libc::WEXITSTATUS(status)
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn strict_profile_refuses_writable_executable_memory_in_the_kernel() {
        assert_eq!(
            probe_mappings_under(SeccompProfile::TiloDefault),
            0,
            "10 = W+X was allowed, 11 = an ordinary mapping was refused, \
             12 = the filter would not install"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn baseline_refuses_kernel_introspection_and_fault_injection_syscalls() {
        let filter = build_baseline_filter(SeccompProfile::TiloDefault).expect("build filter");
        let program: BpfProgram = filter.try_into().expect("compile filter");
        // SAFETY: the child installs a filter, invokes raw syscalls with inert
        // arguments, and exits without returning into the test harness.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            if seccompiler::apply_filter(&program).is_err() {
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
                    unsafe { libc::_exit(10) };
                }
            }
            unsafe { libc::_exit(0) };
        }
        let mut status = 0_i32;
        unsafe { libc::waitpid(pid, &raw mut status, 0) };
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn jit_profile_still_permits_writable_executable_memory() {
        assert_eq!(
            probe_mappings_under(SeccompProfile::TiloJit),
            0,
            "10 = W+X was allowed, 11 = an ordinary mapping was refused, \
             12 = the filter would not install"
        );
    }

    /// Points the entitlement lookup at a fixture for the duration of a test.
    struct JitPolicy {
        _dir: tempfile::TempDir,
        previous: Option<std::ffi::OsString>,
    }

    impl JitPolicy {
        fn listing(entries: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("jit-entitled.conf");
            std::fs::write(&file, entries).unwrap();
            let previous = std::env::var_os(JIT_POLICY_PATH_ENV);
            // SAFETY: every caller is in the `process_env` serial group, and
            // `Drop` restores what was there.
            unsafe { std::env::set_var(JIT_POLICY_PATH_ENV, &file) };
            Self {
                _dir: dir,
                previous,
            }
        }

        /// A path that does not exist, so the compiled-in fallback decides.
        fn missing() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let previous = std::env::var_os(JIT_POLICY_PATH_ENV);
            // SAFETY: as above.
            unsafe {
                std::env::set_var(JIT_POLICY_PATH_ENV, dir.path().join("does-not-exist.conf"));
            };
            Self {
                _dir: dir,
                previous,
            }
        }
    }

    impl Drop for JitPolicy {
        fn drop(&mut self) {
            // SAFETY: as above.
            unsafe {
                match self.previous.take() {
                    Some(v) => std::env::set_var(JIT_POLICY_PATH_ENV, v),
                    None => std::env::remove_var(JIT_POLICY_PATH_ENV),
                }
            }
        }
    }

    /// A package cannot lift write-xor-execute off itself by saying so: a
    /// `.tapp` carries its own manifest, so naming `tilo-jit` is the applicant
    /// filling in their own permit.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_package_the_image_does_not_entitle_gets_the_strict_profile() {
        let _policy = JitPolicy::listing("so.tilo.browser\n");

        let decision = resolve_profile("tilo-jit", "com.example.evil");

        assert_eq!(
            decision.applied,
            SeccompProfile::TiloDefault,
            "a manifest asking for tilo-jit without entitlement must be confined \
             by the strict profile"
        );
        assert!(
            decision.refused,
            "the launcher has to be able to say a request was refused, or the \
             log cannot tell a typo from an app trying to weaken itself"
        );
        assert_eq!(decision.requested, "tilo-jit");
    }

    /// The refusal reaches the kernel, not just the enum: a test comparing
    /// two enum values would pass on a launcher that resolved strictly and
    /// compiled the other one.
    #[test]
    #[serial_test::serial(process_env)]
    #[cfg(target_os = "linux")]
    fn an_unentitled_request_is_refused_by_the_running_filter() {
        let _policy = JitPolicy::listing("so.tilo.browser\n");

        let applied = resolve_profile("tilo-jit", "com.example.evil").applied;
        assert!(
            !applied.allows_dynamic_code(),
            "fixture sanity: the resolved profile must be the strict one, or \
             the probe below proves nothing"
        );
        assert_eq!(
            probe_mappings_under(applied),
            0,
            "10 = W+X was allowed, 11 = an ordinary mapping was refused, \
             12 = the filter would not install"
        );
    }

    /// Entitlement is a gate, not a ban: a rule that broke the browser's
    /// JavaScript tier would take it off the system rather than harden it.
    #[test]
    #[serial_test::serial(process_env)]
    fn the_package_the_image_entitles_still_gets_what_it_asked_for() {
        let _policy = JitPolicy::listing("so.tilo.browser\n");

        let decision = resolve_profile("tilo-jit", "so.tilo.browser");

        assert_eq!(decision.applied, SeccompProfile::TiloJit);
        assert!(!decision.refused);
    }

    /// The list is a ceiling, not a setting: both sides have to agree.
    #[test]
    #[serial_test::serial(process_env)]
    fn entitlement_without_a_request_changes_nothing() {
        let _policy = JitPolicy::listing("so.tilo.browser\n");

        let decision = resolve_profile("tilo-default", "so.tilo.browser");

        assert_eq!(
            decision.applied,
            SeccompProfile::TiloDefault,
            "being on the list must not weaken a package that did not ask"
        );
        assert!(!decision.refused);
    }

    /// An entitled package must not reach a weaker profile by misspelling one.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_misspelt_profile_resolves_strict_for_everyone() {
        let _policy = JitPolicy::listing("so.tilo.browser\n");

        for id in ["so.tilo.browser", "com.example.evil"] {
            for name in ["tilo-jitt", "TILO-JIT", "", "anything"] {
                let decision = resolve_profile(name, id);
                assert_eq!(
                    decision.applied,
                    SeccompProfile::TiloDefault,
                    "{id} asking for {name:?} must not reach a weaker profile"
                );
                assert!(
                    !decision.refused,
                    "{name:?} is an unrecognised name, not a refused \
                     entitlement; the two read differently in the log"
                );
            }
        }
    }

    /// A present file replaces the fallback rather than adding to it.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_policy_file_replaces_the_builtin_list() {
        let _policy = JitPolicy::listing("# nobody\n");

        assert_eq!(
            resolve_profile("tilo-jit", "so.tilo.browser").applied,
            SeccompProfile::TiloDefault,
            "the image's own file decides; the fallback must not add to it"
        );
    }

    /// A missing file must degrade to "the browser renders pages" rather than
    /// to "nothing may generate code".
    #[test]
    #[serial_test::serial(process_env)]
    fn with_no_policy_file_the_builtin_entitlement_still_applies() {
        let _policy = JitPolicy::missing();

        assert_eq!(
            resolve_profile("tilo-jit", "so.tilo.browser").applied,
            SeccompProfile::TiloJit,
            "fallback list lost the browser"
        );
        assert_eq!(
            resolve_profile("tilo-jit", "com.example.evil").applied,
            SeccompProfile::TiloDefault,
            "the fallback must entitle the browser and nobody else"
        );
    }

    /// `Sandbox::seccomp_profile` must not be a way around [`resolve_profile`].
    #[test]
    #[serial_test::serial(process_env)]
    fn the_manifest_accessor_consults_the_entitlement_policy() {
        let _policy = JitPolicy::listing("so.tilo.browser\n");
        let sandbox = crate::manifest::Sandbox {
            seccomp: "tilo-jit".to_string(),
            ..crate::manifest::Sandbox::default()
        };

        assert_eq!(
            sandbox.seccomp_profile("com.example.evil"),
            SeccompProfile::TiloDefault,
            "the manifest accessor must not hand out what the image refused"
        );
        assert_eq!(
            sandbox.seccomp_profile("so.tilo.browser"),
            SeccompProfile::TiloJit
        );
    }

    /// Two lists that can disagree is a machine whose confinement depends
    /// on whether a file was copied — a browser that behaves differently
    /// on one image than another.
    #[test]
    fn the_shipped_policy_file_and_the_builtin_list_agree() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../os/usr/lib/tilo/jit-entitled.conf");
        let shipped = std::fs::read_to_string(&repo).unwrap_or_else(|e| {
            panic!(
                "the image's entitlement list must be in the tree at {}: {e}",
                repo.display()
            )
        });
        for id in BUILTIN_JIT_ENTITLED {
            assert!(
                crate::hardening::policy_matches(&shipped, id),
                "{id} is in the compiled-in fallback but not in the file the \
                 image ships"
            );
        }
        assert!(
            !crate::hardening::policy_matches(&shipped, "com.example.evil"),
            "the shipped list must not entitle an arbitrary package"
        );
    }
}
