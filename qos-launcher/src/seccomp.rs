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
    libc::SYS_clone3,
];

fn build_baseline_filter() -> Result<SeccompFilter, SeccompError> {
    let arch = host_target_arch()?;
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    for nr in BLANKET_DENIES {
        rules.insert(*nr as i64, Vec::new());
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
pub fn compile_baseline() -> Result<Vec<u8>, SeccompError> {
    let filter = build_baseline_filter()?;
    let program: BpfProgram = filter.try_into()?;
    Ok(bpf_to_bytes(&program))
}

/// Write the compiled blob to a path inside `dir`, returning that path.
/// Caller is responsible for opening it and handing the fd to bwrap.
pub fn write_baseline_to(dir: &Path) -> Result<PathBuf, SeccompError> {
    std::fs::create_dir_all(dir)?;
    let bytes = compile_baseline()?;
    let path = dir.join("qos-default.bpf");
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// Path the launcher checks for a pre-baked blob. If present, prefer it
/// over compiling. Looked up relative to `$CARGO_MANIFEST_DIR` at build
/// time and `<crate_root>/share/qos-default.bpf` at runtime — but in
/// production the file lives in a fixed location alongside the binary,
/// so this is a developer convenience only.
pub fn baked_blob_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("share/qos-default.bpf")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_compiles_and_blob_alignment() {
        let bytes = compile_baseline().expect("compile");
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
        let path = write_baseline_to(dir.path()).unwrap();
        assert!(path.exists());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len() % 8, 0);
    }
}
