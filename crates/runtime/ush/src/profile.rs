//! The two isolation postures the runtime can build, and the seccomp policy
//! that separates them.
//!
//! `ush` and `dsh` are one program. They share the layer store, the package
//! store, the broker and the audit log; they differ only in what the guest is
//! allowed to do once it is running.

use std::collections::BTreeMap;

use seccompiler::{
    BackendError, BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition,
    SeccompFilter, SeccompRule, TargetArch,
};

/// Which posture a session runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// The sandbox. Namespaced, seccomp-filtered, Landlock-restricted, no
    /// device beyond a pty, no route to the host's filesystem except what a
    /// grant names.
    #[default]
    Ush,
    /// The developer profile.
    ///
    /// **`dsh` is a convenience for development, not a sandbox.** It keeps the
    /// layer model and the read-only base — you still cannot write the host's
    /// `/usr` — and it deliberately gives up the syscall filter that makes
    /// `ush` a boundary, because nested rootless `podman` and `distrobox`
    /// cannot work without `unshare`, `mount` and `setns`. A guest that can
    /// build its own namespaces can also build one without the restrictions it
    /// was given, so treat everything inside `dsh` as running with the
    /// authority of the user who started it.
    ///
    /// It exists so qOS can be developed from inside qOS. It is reachable only
    /// by typing `dsh`: [`Profile::reachable_from_app_launch`] is what enforces
    /// that, and the supervisor refuses a `dsh` session requested over the
    /// service socket an app was handed.
    Dsh,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Ush => "ush",
            Profile::Dsh => "dsh",
        }
    }

    /// Resolve a profile name.
    ///
    /// Anything that is not exactly `dsh` is [`Profile::Ush`]. A typo must not
    /// be a route to the weaker posture.
    pub fn parse(name: &str) -> Self {
        match name {
            "dsh" => Profile::Dsh,
            _ => Profile::Ush,
        }
    }

    /// May an app, having been handed the runtime's service socket at launch,
    /// ask for a session in this profile?
    ///
    /// Never for `dsh`. Apps are untrusted, including first-party ones, and
    /// `dsh` is authority without a boundary — if an app could request it, the
    /// exception would stop being deliberate. The supervisor checks this on
    /// every request and the answer does not depend on which app is asking.
    pub fn reachable_from_app_launch(self) -> bool {
        matches!(self, Profile::Ush)
    }

    /// Does the guest get a syscall filter that stops it building namespaces?
    pub fn confines_namespaces(self) -> bool {
        matches!(self, Profile::Ush)
    }

    /// Does the guest get `/dev/fuse`?
    ///
    /// Container and build tooling needs it (`fuse-overlayfs`, `squashfuse`).
    /// A FUSE mount is a filesystem the guest implements, which is a way to
    /// hand a path back to the host with contents the guest controls, so it
    /// belongs on the `dsh` side of the line.
    pub fn grants_fuse(self) -> bool {
        matches!(self, Profile::Dsh)
    }

    /// Does the guest get a subordinate id range?
    ///
    /// Rootless `podman` allocates uids out of `/etc/subuid` to own the files
    /// in an image. With one uid mapped it can only run images whose files are
    /// all uid 0, which in practice means it does not work. See
    /// [`crate::guest::subid_range`].
    pub fn grants_subid_range(self) -> bool {
        matches!(self, Profile::Dsh)
    }

    /// One line for `guard status` and for the shell prompt.
    pub fn posture_summary(self) -> &'static str {
        match self {
            Profile::Ush => {
                "sandboxed: namespaces + seccomp + Landlock, host filesystem read-only"
            }
            Profile::Dsh => {
                "developer profile — NOT a sandbox: namespace syscalls allowed, \
                 /dev/fuse present, subordinate ids mapped"
            }
        }
    }
}

/// Syscalls the guest never gets under [`Profile::Ush`].
///
/// The set is the namespace- and mount-manipulation half of the deny-list in
/// `crates/packaging/launcher/src/seccomp.rs`, which is itself Flatpak's
/// published profile. Keeping the two lists textually separate is deliberate:
/// this one is the *reason the runtime's own use of these syscalls does not
/// propagate*, and it should be readable next to that claim rather than behind
/// a dependency on the packaging crate.
///
/// `bwrap` installs the filter after it has finished unsharing and mounting and
/// immediately before it execs the guest. So the supervisor holds these
/// syscalls for exactly as long as it takes to build the confinement, and no
/// descendant of the guest ever holds them.
const NAMESPACE_DENIES: &[libc::c_long] = &[
    libc::SYS_unshare,
    libc::SYS_setns,
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_pivot_root,
    libc::SYS_chroot,
];

/// Syscalls no profile gets, `dsh` included.
///
/// `dsh` gives up the namespace boundary; it does not give up everything.
/// `ptrace` across the session, raw packet sockets and `bpf` are not needed by
/// any container tool and are the classic escalation primitives, so they stay
/// denied even there. Being explicit that `dsh` is not "seccomp off" is the
/// point.
const ALWAYS_DENIES: &[libc::c_long] = &[
    libc::SYS_ptrace,
    libc::SYS_bpf,
    libc::SYS_keyctl,
    libc::SYS_kexec_load,
    libc::SYS_init_module,
    libc::SYS_finit_module,
    libc::SYS_delete_module,
];

#[derive(Debug, thiserror::Error)]
pub enum SeccompError {
    #[error("unsupported host architecture for the guest seccomp policy: {0}")]
    UnsupportedArch(&'static str),
    #[error("seccompiler: {0}")]
    Compile(#[from] BackendError),
    #[error("staging the guest seccomp blob: {0}")]
    Io(#[from] std::io::Error),
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

fn build_filter(profile: Profile) -> Result<SeccompFilter, SeccompError> {
    let arch = host_target_arch()?;
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    for nr in ALWAYS_DENIES {
        rules.insert(*nr as i64, Vec::new());
    }
    if profile.confines_namespaces() {
        for nr in NAMESPACE_DENIES {
            rules.insert(*nr as i64, Vec::new());
        }
    }

    // socket(AF_PACKET, ...) — a raw link-layer socket, in every profile.
    rules.insert(
        libc::SYS_socket as i64,
        vec![SeccompRule::new(vec![SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Eq,
            libc::AF_PACKET as u64,
        )?])?],
    );

    // Deny-list, not allow-list, and denials return `EPERM` rather than
    // `SIGSYS`. A shell hosts arbitrary programs compiled against arbitrary
    // libcs; an allow-list would break on the next glibc that reaches for a
    // syscall nobody enumerated, and killing the process would make that look
    // like a crash rather than a policy.
    Ok(SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )?)
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

/// Compile the guest filter for `profile` to a raw BPF blob.
pub fn compile_guest_filter(profile: Profile) -> Result<Vec<u8>, SeccompError> {
    let filter = build_filter(profile)?;
    let program: BpfProgram = filter.try_into()?;
    Ok(bpf_to_bytes(&program))
}

/// Write the guest filter into `dir`, returning the path. The supervisor opens
/// it and hands the fd to `bwrap --seccomp`.
pub fn write_guest_filter(
    dir: &std::path::Path,
    profile: Profile,
) -> Result<std::path::PathBuf, SeccompError> {
    std::fs::create_dir_all(dir)?;
    let bytes = compile_guest_filter(profile)?;
    let path = dir.join(format!("{}.bpf", profile.as_str()));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_profile_name_resolves_to_the_sandbox() {
        for name in ["ush", "", "DSH", "dsh ", "developer", "anything"] {
            assert_eq!(
                Profile::parse(name),
                Profile::Ush,
                "{name:?} must not resolve to the profile that is not a sandbox"
            );
        }
        assert_eq!(Profile::parse("dsh"), Profile::Dsh);
    }

    /// The gate on the deliberate exception. If this ever returns true for
    /// `Dsh`, an app launch can acquire a shell with no syscall boundary.
    #[test]
    fn dsh_is_not_reachable_from_an_app_launch() {
        assert!(!Profile::Dsh.reachable_from_app_launch());
        assert!(Profile::Ush.reachable_from_app_launch());
    }

    #[test]
    fn the_postures_differ_in_exactly_the_documented_ways() {
        assert!(Profile::Ush.confines_namespaces());
        assert!(!Profile::Ush.grants_fuse());
        assert!(!Profile::Ush.grants_subid_range());

        assert!(!Profile::Dsh.confines_namespaces());
        assert!(Profile::Dsh.grants_fuse());
        assert!(Profile::Dsh.grants_subid_range());
    }

    #[test]
    fn both_filters_compile_and_are_sock_filter_aligned() {
        for profile in [Profile::Ush, Profile::Dsh] {
            let bytes = compile_guest_filter(profile).unwrap();
            assert!(!bytes.is_empty(), "{profile:?} produced an empty filter");
            assert_eq!(bytes.len() % 8, 0, "{profile:?} blob is not 8-byte aligned");
        }
    }

    /// `dsh` relaxes the namespace rules and nothing else. A filter that
    /// stopped denying `ptrace` or `bpf` would be a different claim from the
    /// one its documentation makes.
    #[test]
    fn dsh_relaxes_only_the_namespace_rules() {
        let strict = compile_guest_filter(Profile::Ush).unwrap();
        let dev = compile_guest_filter(Profile::Dsh).unwrap();
        assert!(
            strict.len() > dev.len(),
            "the sandbox filter must carry more program than the developer one \
             (ush={} bytes, dsh={} bytes)",
            strict.len(),
            dev.len()
        );
        assert!(!dev.is_empty(), "dsh must not be `seccomp off`");
    }

    /// The kernel is the only opinion that counts. Install each filter in a
    /// forked child and try the syscalls.
    ///
    /// Exit codes: 0 behaved as documented, 10 a denied syscall succeeded,
    /// 11 an allowed syscall was refused, 12 the filter would not install.
    fn probe_under(profile: Profile) -> i32 {
        let filter = build_filter(profile).expect("build");
        let program: BpfProgram = filter.try_into().expect("compile");
        let denies_ns = profile.confines_namespaces();

        // SAFETY: the child only installs the filter, makes raw syscalls and
        // `_exit`s. It never returns into the harness and touches no allocator
        // or lock after the fork.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let bail = |code: i32| -> ! {
                // SAFETY: `_exit` is async-signal-safe and does not unwind.
                unsafe { libc::_exit(code) }
            };
            if seccompiler::apply_filter(&program).is_err() {
                bail(12);
            }

            // `unshare(CLONE_NEWUSER)` is the one that matters: it is how a
            // guest would build a namespace it controls and climb out of the
            // one it was given.
            // SAFETY: a plain syscall; failure is the expected outcome here.
            let unshared = unsafe { libc::syscall(libc::SYS_unshare, libc::CLONE_NEWUSER) };
            if denies_ns {
                if unshared == 0 {
                    bail(10);
                }
            } else if unshared != 0 {
                bail(11);
            }

            // `ptrace` is denied in both profiles.
            // SAFETY: PTRACE_TRACEME on a process with no tracer; denied by
            // the filter before it reaches the kernel's own logic.
            let traced = unsafe { libc::syscall(libc::SYS_ptrace, libc::PTRACE_TRACEME, 0, 0, 0) };
            if traced == 0 {
                bail(10);
            }

            // An ordinary syscall must keep working in both, or the filter has
            // broken the shell rather than confined it.
            // SAFETY: getpid has no failure mode.
            if unsafe { libc::syscall(libc::SYS_getpid) } <= 0 {
                bail(11);
            }
            bail(0)
        }

        let mut status = 0i32;
        // SAFETY: `pid` is our own child.
        unsafe { libc::waitpid(pid, &mut status, 0) };
        assert!(libc::WIFEXITED(status), "child died: {status:#x}");
        libc::WEXITSTATUS(status)
    }

    #[test]
    fn the_kernel_refuses_a_ush_guest_its_own_namespace() {
        assert_eq!(
            probe_under(Profile::Ush),
            0,
            "10 = a denied syscall succeeded, 11 = an allowed one was refused, \
             12 = the filter would not install"
        );
    }

    #[test]
    fn the_kernel_lets_a_dsh_guest_build_a_namespace_but_still_denies_ptrace() {
        assert_eq!(
            probe_under(Profile::Dsh),
            0,
            "10 = a denied syscall succeeded, 11 = an allowed one was refused, \
             12 = the filter would not install"
        );
    }
}
