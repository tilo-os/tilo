//! The guest's seccomp filter: no building namespaces or mounts of its own, and none of
//! the classic escalation primitives no shell tool needs.

use std::collections::BTreeMap;

pub use sandbox_core::seccomp::SeccompError;
use sandbox_core::seccomp::{af_packet_socket_rule, compile_deny_list};
use seccompiler::SeccompRule;

/// The namespace- and mount-manipulation half of the application baseline
/// in `sandbox_core::seccomp::BLANKET_DENIES` (itself Flatpak's published
/// profile). Listed here rather than derived from that list because this
/// is the *reason the runtime's own use of these syscalls does not
/// propagate*, readable next to that claim.
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

const ALWAYS_DENIES: &[libc::c_long] = &[
    libc::SYS_ptrace,
    libc::SYS_bpf,
    libc::SYS_keyctl,
    libc::SYS_kexec_load,
    libc::SYS_init_module,
    libc::SYS_finit_module,
    libc::SYS_delete_module,
];

/// The guest deny-list. Compiled
/// by [`sandbox_core::seccomp::compile_deny_list`]: default-allow, `EPERM`
/// rather than `SIGSYS` — a shell hosts arbitrary programs against
/// arbitrary libcs, so an allow-list would break on the next syscall nobody
/// enumerated, killing the process as if it crashed.
fn guest_rules() -> Result<BTreeMap<i64, Vec<SeccompRule>>, SeccompError> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for nr in ALWAYS_DENIES.iter().chain(NAMESPACE_DENIES) {
        rules.insert(*nr, Vec::new());
    }
    rules.insert(libc::SYS_socket, af_packet_socket_rule()?);
    Ok(rules)
}

pub fn compile_guest_filter() -> Result<Vec<u8>, SeccompError> {
    compile_deny_list(guest_rules()?)
}

pub fn write_guest_filter(dir: &std::path::Path) -> Result<std::path::PathBuf, SeccompError> {
    std::fs::create_dir_all(dir)?;
    let bytes = compile_guest_filter()?;
    let path = dir.join("ush.bpf");
    std::fs::write(&path, bytes)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_compiles_aligned() {
        let bytes = compile_guest_filter().unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(bytes.len() % 8, 0, "blob is not 8-byte aligned");
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn the_compiled_guest_program_matches_the_pinned_golden_file() {
        let golden = hex::decode(
            include_str!("../tests/fixtures/seccomp/ush.x86_64.bpf.hex")
                .lines()
                .collect::<String>(),
        )
        .expect("decode pinned guest BPF hex fixture");
        let compiled = compile_guest_filter().unwrap();
        assert!(
            compiled == golden,
            "the guest filter compiled to a different BPF program than the pinned one \
             ({} bytes vs {} pinned); this is a sandbox policy change",
            compiled.len(),
            golden.len()
        );
    }

    #[test]
    fn the_kernel_enforces_the_filter() {
        let program = sandbox_core::seccomp::deny_list_program(guest_rules().expect("rules"))
            .expect("compile");

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
            if unsafe { libc::syscall(libc::SYS_unshare, libc::CLONE_NEWUSER) } == 0 {
                bail(10);
            }
            if unsafe { libc::syscall(libc::SYS_ptrace, libc::PTRACE_TRACEME, 0, 0, 0) } == 0 {
                bail(10);
            }
            // An ordinary syscall must keep working, or the filter broke the
            // shell rather than confining it.
            // SAFETY: getpid has no failure mode.
            if unsafe { libc::syscall(libc::SYS_getpid) } <= 0 {
                bail(11);
            }
            bail(0)
        }

        let mut status = 0i32;
        unsafe { libc::waitpid(pid, &raw mut status, 0) };
        assert!(libc::WIFEXITED(status), "child died: {status:#x}");
        assert_eq!(
            libc::WEXITSTATUS(status),
            0,
            "10 = a denied syscall succeeded, 11 = an allowed one was refused, \
             12 = the filter would not install"
        );
    }
}
