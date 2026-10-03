//! The two isolation postures the runtime can build, and the seccomp policy
//! that separates them.
//!
//! `ush` and `dsh` are one program. They share the layer store, the package
//! store, the broker and the audit log; they differ only in what the guest is
//! allowed to do once it is running.

use std::collections::BTreeMap;

pub use sandbox_core::seccomp::SeccompError;
use sandbox_core::seccomp::{af_packet_socket_rule, compile_deny_list};
use seccompiler::SeccompRule;

/// Which posture a session runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// The sandbox: namespaced, seccomp-filtered, Landlock-restricted, no
    /// device beyond a pty, no host filesystem route except a grant.
    #[default]
    Ush,
    /// The developer profile.
    ///
    /// **`dsh` is a convenience for development, not a sandbox.** It keeps the
    /// layer model and read-only base, but gives up the syscall
    /// filter that makes `ush` a boundary — nested rootless `podman` and
    /// `distrobox` need `unshare`/`mount`/`setns`. A guest that can build its
    /// own namespaces can build one without the restrictions it was given, so
    /// treat everything inside `dsh` as running with the user's own authority.
    ///
    /// Reachable only by typing `dsh`: [`Profile::reachable_from_app_launch`]
    /// enforces that, and the supervisor refuses a `dsh` session requested over
    /// an app's service socket.
    Dsh,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Ush => "ush",
            Profile::Dsh => "dsh",
        }
    }

    /// Resolve a profile name: anything not exactly `dsh` is [`Profile::Ush`] —
    /// a typo must not be a route to the weaker posture.
    pub fn parse(name: &str) -> Self {
        match name {
            "dsh" => Profile::Dsh,
            _ => Profile::Ush,
        }
    }

    /// May an app, handed the runtime's service socket at launch, ask for a
    /// session in this profile? Never for `dsh`: apps are untrusted (including
    /// first-party ones), and `dsh` is authority without a boundary — the
    /// supervisor checks this on every request regardless of which app asks.
    pub fn reachable_from_app_launch(self) -> bool {
        matches!(self, Profile::Ush)
    }

    /// Does the guest get a syscall filter that stops it building namespaces?
    pub fn confines_namespaces(self) -> bool {
        matches!(self, Profile::Ush)
    }

    /// Does the guest get `/dev/fuse`? Container/build tooling needs it
    /// (`fuse-overlayfs`, `squashfuse`); since a FUSE mount is a filesystem the
    /// guest implements, it belongs on the `dsh` side of the line.
    pub fn grants_fuse(self) -> bool {
        matches!(self, Profile::Dsh)
    }

    /// Does the guest get a subordinate id range? Rootless `podman` allocates
    /// uids out of `/etc/subuid` to own an image's files — with only one uid
    /// mapped it can't run most images. See [`crate::guest::subid_range`].
    pub fn grants_subid_range(self) -> bool {
        matches!(self, Profile::Dsh)
    }

    /// One line for `guard status` and for the shell prompt.
    pub fn posture_summary(self) -> &'static str {
        match self {
            Profile::Ush => "sandboxed: namespaces + seccomp + Landlock, host filesystem read-only",
            Profile::Dsh => {
                "developer profile — NOT a sandbox: namespace syscalls allowed, \
                 /dev/fuse present, subordinate ids mapped"
            }
        }
    }
}

/// Syscalls the guest never gets under [`Profile::Ush`]: the namespace- and
/// mount-manipulation half of the application baseline in
/// `sandbox_core::seccomp::BLANKET_DENIES` (itself Flatpak's published
/// profile). Listed here rather than derived from that list because this
/// is the *reason the runtime's own use of these syscalls does not
/// propagate* — readable next to that claim — and because `dsh` must be
/// able to drop exactly this half and nothing else.
///
/// `bwrap` installs the filter right before it execs the guest, so the
/// supervisor holds these syscalls only as long as it takes to build the
/// confinement, and no descendant of the guest ever holds them.
const NAMESPACE_DENIES: &[libc::c_long] = &[
    libc::SYS_unshare,
    libc::SYS_setns,
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_pivot_root,
    libc::SYS_chroot,
];

/// Syscalls no profile gets, `dsh` included: it gives up the namespace
/// boundary, not everything. `ptrace` across the session, raw packet
/// sockets and `bpf` are classic escalation primitives no container tool
/// needs, so they stay denied — `dsh` is not "seccomp off".
const ALWAYS_DENIES: &[libc::c_long] = &[
    libc::SYS_ptrace,
    libc::SYS_bpf,
    libc::SYS_keyctl,
    libc::SYS_kexec_load,
    libc::SYS_init_module,
    libc::SYS_finit_module,
    libc::SYS_delete_module,
];

/// The guest deny-list for `profile`. Compiled by
/// [`sandbox_core::seccomp::compile_deny_list`]: default-allow, `EPERM`
/// rather than `SIGSYS` — a shell hosts arbitrary programs against
/// arbitrary libcs, so an allow-list would break on the next syscall nobody
/// enumerated, killing the process as if it crashed.
fn guest_rules(profile: Profile) -> Result<BTreeMap<i64, Vec<SeccompRule>>, SeccompError> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    for nr in ALWAYS_DENIES {
        rules.insert(*nr, Vec::new());
    }
    if profile.confines_namespaces() {
        for nr in NAMESPACE_DENIES {
            rules.insert(*nr, Vec::new());
        }
    }

    // socket(AF_PACKET, ...) — a raw link-layer socket, in every profile.
    rules.insert(libc::SYS_socket, af_packet_socket_rule()?);
    Ok(rules)
}

/// Compile the guest filter for `profile` to a raw BPF blob.
///
/// # Errors
///
/// Returns [`SeccompError::UnsupportedArch`] on a host architecture the
/// guest filter has no rules for, or [`SeccompError::Compile`] if
/// `seccompiler` refuses to compile the filter.
pub fn compile_guest_filter(profile: Profile) -> Result<Vec<u8>, SeccompError> {
    compile_deny_list(guest_rules(profile)?)
}

/// Write the guest filter into `dir`, returning the path. The supervisor opens
/// it and hands the fd to `bwrap --seccomp`.
///
/// # Errors
///
/// Returns [`SeccompError`] if the filter can't be compiled (see
/// [`compile_guest_filter`]), or [`SeccompError::Io`] if `dir` can't be
/// created or the blob can't be written.
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
    fn profiles_parse_to_the_sandbox_and_differ_as_documented() {
        // phase: an_unknown_profile_name_resolves_to_the_sandbox
        {
            for name in ["ush", "", "DSH", "dsh ", "developer", "anything"] {
                assert_eq!(
                    Profile::parse(name),
                    Profile::Ush,
                    "{name:?} must not resolve to the profile that is not a sandbox"
                );
            }
            assert_eq!(Profile::parse("dsh"), Profile::Dsh);
        }
        // phase: dsh_is_not_reachable_from_an_app_launch
        // The gate on the deliberate exception. If this ever returns true for
        // `Dsh`, an app launch can acquire a shell with no syscall boundary.
        {
            assert!(!Profile::Dsh.reachable_from_app_launch());
            assert!(Profile::Ush.reachable_from_app_launch());
        }
        // phase: the_postures_differ_in_exactly_the_documented_ways
        {
            assert!(Profile::Ush.confines_namespaces());
            assert!(!Profile::Ush.grants_fuse());
            assert!(!Profile::Ush.grants_subid_range());

            assert!(!Profile::Dsh.confines_namespaces());
            assert!(Profile::Dsh.grants_fuse());
            assert!(Profile::Dsh.grants_subid_range());
        }
    }

    #[test]
    fn both_filters_compile_aligned_and_dsh_relaxes_only_namespaces() {
        // phase: both_filters_compile_and_are_sock_filter_aligned
        {
            for profile in [Profile::Ush, Profile::Dsh] {
                let bytes = compile_guest_filter(profile).unwrap();
                assert!(!bytes.is_empty(), "{profile:?} produced an empty filter");
                assert_eq!(bytes.len() % 8, 0, "{profile:?} blob is not 8-byte aligned");
            }
        }
        // phase: dsh_relaxes_only_the_namespace_rules
        // `dsh` relaxes the namespace rules and nothing else — a filter that
        // stopped denying `ptrace` or `bpf` would be a different claim than documented.
        {
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
    }

    /// The compiled guest programs, byte for byte. A change to what a guest
    /// may call fails here and has to be reviewed as a policy change with a
    /// regenerated golden file. Only `x86_64` is pinned: syscall numbers and
    /// the arch check differ per architecture.
    #[test]
    #[cfg(target_arch = "x86_64")]
    fn compiled_guest_programs_match_the_pinned_golden_files() {
        let cases: [(Profile, &[u8]); 2] = [
            (
                Profile::Ush,
                include_bytes!("../tests/fixtures/seccomp/ush.x86_64.bpf"),
            ),
            (
                Profile::Dsh,
                include_bytes!("../tests/fixtures/seccomp/dsh.x86_64.bpf"),
            ),
        ];
        for (profile, golden) in cases {
            let compiled = compile_guest_filter(profile).unwrap();
            assert!(
                compiled == golden,
                "{} compiled to a different BPF program than the pinned one \
                 ({} bytes vs {} pinned); this is a sandbox policy change",
                profile.as_str(),
                compiled.len(),
                golden.len()
            );
        }
    }

    /// The kernel is the only opinion that counts: install each filter in a
    /// forked child and try the syscalls. Exit codes: 0 behaved as documented,
    /// 10 a denied syscall succeeded, 11 an allowed one was refused, 12 install failed.
    fn probe_under(profile: Profile) -> i32 {
        let program =
            sandbox_core::seccomp::deny_list_program(guest_rules(profile).expect("rules"))
                .expect("compile");
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

            // `unshare(CLONE_NEWUSER)` is the one that matters — how a guest would
            // build and climb out of its own namespace.
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

            // An ordinary syscall must keep working in both, or the filter broke the
            // shell rather than confining it.
            // SAFETY: getpid has no failure mode.
            if unsafe { libc::syscall(libc::SYS_getpid) } <= 0 {
                bail(11);
            }
            bail(0)
        }

        let mut status = 0i32;
        // SAFETY: `pid` is our own child.
        unsafe { libc::waitpid(pid, &raw mut status, 0) };
        assert!(libc::WIFEXITED(status), "child died: {status:#x}");
        libc::WEXITSTATUS(status)
    }

    /// ush: the kernel refuses the guest its own namespace. dsh: the guest can
    /// build a namespace but ptrace is still denied.
    #[test]
    fn the_kernel_enforces_each_profiles_filter() {
        for profile in [Profile::Ush, Profile::Dsh] {
            assert_eq!(
                probe_under(profile),
                0,
                "case {profile:?}: 10 = a denied syscall succeeded, 11 = an allowed one was refused, \
                 12 = the filter would not install"
            );
        }
    }
}
