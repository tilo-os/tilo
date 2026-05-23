//! Per-workspace seccomp-bpf filter compiler.
//!
//! Builds a deny-list filter blocking syscalls a sandboxed app should
//! never need: namespace manipulation, ptrace, raw sockets, mount /
//! chroot and friends. The compiled BPF blob is cached on disk so each
//! workspace pays compilation only once per cold start.
//!
//! Phase 2 of `docs/superpowers/plans/2026-05-02-per-workspace-sandbox-permissions.md`.
//! Phase 3+ will swap in fd-passing of the cached blob to `bwrap
//! --seccomp <fd>`; here we only produce and persist the bytes.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use seccompiler::{
    BackendError, BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition,
    SeccompFilter, SeccompRule, TargetArch,
};

/// Default on-disk cache root for compiled seccomp blobs.
///
/// Lives under `/var/cache/qos-fs` so the bootc root can stay
/// read-only and the FUSE driver's cache layout (see
/// `quick-fs-fuse`'s `/var/cache/qos-fs` convention) keeps a single
/// owner for that prefix.
pub const DEFAULT_CACHE_ROOT: &str = "/var/cache/qos-fs";

/// Errors produced by the workspace seccomp pipeline.
///
/// Wraps both compilation failures (bubbled up from `seccompiler`) and
/// the host-architecture-not-supported case so callers can treat
/// "compile" and "cache" as a single fallible step.
#[derive(Debug)]
pub enum SeccompError {
    /// The crate was built for a target architecture seccompiler does
    /// not support (only x86_64, aarch64, riscv64 are wired here).
    UnsupportedArch(&'static str),
    /// Anything that went wrong inside the seccompiler backend — bad
    /// rule, identical actions, filter too large, invalid arg index.
    /// Uses [`BackendError`] directly because every fallible call
    /// surfaced by this module (`SeccompCondition::new`,
    /// `SeccompRule::new`, `SeccompFilter::new`, `try_into()` to
    /// `BpfProgram`) returns it; wrapping in the higher-level
    /// `seccompiler::Error` would just lose detail without buying us
    /// anything.
    Compile(BackendError),
}

impl std::fmt::Display for SeccompError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SeccompError::UnsupportedArch(a) => {
                write!(f, "unsupported host architecture for seccomp filter: {a}")
            }
            SeccompError::Compile(e) => write!(f, "seccompiler compile error: {e}"),
        }
    }
}

impl std::error::Error for SeccompError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SeccompError::Compile(e) => Some(e),
            _ => None,
        }
    }
}

impl From<BackendError> for SeccompError {
    fn from(value: BackendError) -> Self {
        SeccompError::Compile(value)
    }
}

/// Detect the seccompiler [`TargetArch`] for the host this binary was
/// built against.
///
/// We tie the filter to the *build* target rather than runtime
/// detection because the BPF program's arch-validation prologue
/// rejects any other audit arch at install time anyway — a mismatch
/// here would just produce a filter that always traps.
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

/// Build the per-workspace deny-list [`SeccompFilter`].
///
/// Split out from [`compile_workspace_seccomp`] so tests can introspect
/// the rule shape if we ever need to (today's tests only check the
/// compiled byte length, but keeping the IR addressable here is cheap
/// and makes a future "dump filter to JSON for debug" trivial).
fn build_workspace_filter() -> Result<SeccompFilter, SeccompError> {
    let arch = host_target_arch()?;

    // Unconditional denials. Each tuple's second element is an empty
    // rule vector, which seccompiler treats as "match any args for
    // this syscall" — i.e. blanket-deny.
    //
    // Phase 2 v1 simplification (matches plan §Phase 2 step 1):
    //   * `clone3` is fully blocked here. A more nuanced filter that
    //     allowed `clone3` without namespace flags belongs to a later
    //     phase once we actually need glibc threads inside the sandbox
    //     to use clone3 — most apps still fall back to `clone(2)`.
    //   * Raw sockets are filtered via a per-arg condition on
    //     `socket(2)`'s family argument (see the `AF_PACKET` rule
    //     below). `socketpair`/`socketcall` are *not* filtered here.
    let blanket_denies: [libc::c_long; 10] = [
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

    // `SeccompFilter::new` requires a `BTreeMap` keyed by syscall
    // number — the deterministic ordering keeps the compiled BPF
    // stable across runs, which matters for the on-disk cache.
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for nr in blanket_denies {
        rules.insert(nr as i64, Vec::new());
    }

    // Conditional: socket(family, type, protocol) where family ==
    // AF_PACKET. Argument 0 is the family; AF_PACKET is a 4-byte
    // value (Dword) so we don't accidentally match on adjacent bits
    // of a 64-bit register.
    rules.insert(
        libc::SYS_socket as i64,
        vec![SeccompRule::new(vec![SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Eq,
            libc::AF_PACKET as u64,
        )?])?],
    );

    // Default: allow. Matched syscalls return -EPERM rather than
    // killing the process so a misconfigured app gets a clean errno
    // it can log instead of a SIGSYS in journald.
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )?;
    Ok(filter)
}

/// Serialise a `seccompiler::sock_filter` slice into the on-the-wire
/// 8-bytes-per-instruction layout the kernel expects.
///
/// `sock_filter` is `#[repr(C)] { u16 code, u8 jt, u8 jf, u32 k }` —
/// 8 bytes packed (no padding since u32 is aligned at offset 4). We
/// could just memcpy with `unsafe`, but writing the four fields
/// explicitly in little-endian keeps this safe Rust and survives any
/// future struct-shape change in seccompiler without UB.
fn bpf_program_to_bytes(prog: &BpfProgram) -> Vec<u8> {
    let mut out = Vec::with_capacity(prog.len() * 8);
    for stmt in prog {
        out.extend_from_slice(&stmt.code.to_ne_bytes());
        out.push(stmt.jt);
        out.push(stmt.jf);
        out.extend_from_slice(&stmt.k.to_ne_bytes());
    }
    out
}

/// Compile the workspace deny-list filter to a raw BPF blob.
///
/// Returns the BPF bytecode as bytes laid out as a sequence of
/// `struct sock_filter` records (8 bytes each: u16 code, u8 jt, u8
/// jf, u32 k, native endian). Callers can either feed this to the
/// kernel via `bwrap --seccomp <fd>` or persist with [`cache_blob`].
pub fn compile_workspace_seccomp() -> Result<Vec<u8>, SeccompError> {
    let filter = build_workspace_filter()?;
    let program: BpfProgram = filter.try_into()?;
    Ok(bpf_program_to_bytes(&program))
}

/// Cache the BPF blob at `<cache_root>/<ws_id>/seccomp.bpf`.
///
/// Returns the path. Idempotent: writing the same blob twice is fine
/// (the second write is a clobber, not an append).
///
/// Split from [`cache_blob`] so tests can point at a tempdir without
/// needing root access to `/var/cache/qos-fs`.
pub fn cache_blob_at(
    cache_root: &Path,
    ws_id: u64,
    bytes: &[u8],
) -> io::Result<PathBuf> {
    let dir = cache_root.join(ws_id.to_string());
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("seccomp.bpf");
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// Cache the BPF blob at `/var/cache/qos-fs/<ws_id>/seccomp.bpf`.
///
/// Production wrapper around [`cache_blob_at`]. Use the `_at` variant
/// in tests so we don't need write access to `/var`.
pub fn cache_blob(ws_id: u64, bytes: &[u8]) -> io::Result<PathBuf> {
    cache_blob_at(Path::new(DEFAULT_CACHE_ROOT), ws_id, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seccomp_blob_compiles_and_is_nonempty() {
        let bytes = compile_workspace_seccomp().expect("compile");
        assert!(
            !bytes.is_empty(),
            "BPF program must have at least one instruction"
        );
        // Each sock_filter is 8 bytes; the blob must be a whole
        // number of instructions or the kernel rejects it outright.
        assert_eq!(
            bytes.len() % 8,
            0,
            "BPF blob length {} is not a multiple of sock_filter (8)",
            bytes.len()
        );
    }

    #[test]
    fn cache_blob_writes_to_expected_path() {
        // BPF_RET | BPF_K with k=0: a minimal valid 1-instruction
        // program. Used purely as an opaque payload here — we don't
        // load it into the kernel from this test.
        let bytes = vec![0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let tmp = tempdir_under_target();
        let path = cache_blob_at(&tmp, 99999, &bytes).unwrap();
        assert!(
            path.ends_with("99999/seccomp.bpf"),
            "unexpected cache path: {}",
            path.display()
        );
        let read = std::fs::read(&path).unwrap();
        assert_eq!(read, bytes);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(path.parent().unwrap());
        let _ = std::fs::remove_dir(&tmp);
    }

    /// Pick a per-process scratch dir under `target/` so the test
    /// works in sandboxes that disallow writes outside the workspace
    /// (e.g. the worktree CI). Falls back to `std::env::temp_dir()`
    /// only if `CARGO_TARGET_TMPDIR` and the manifest dir are both
    /// unavailable, which shouldn't happen under `cargo test`.
    fn tempdir_under_target() -> PathBuf {
        let base = std::env::var_os("CARGO_TARGET_TMPDIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("CARGO_MANIFEST_DIR")
                    .map(|d| PathBuf::from(d).join("target").join("seccomp-test-tmp"))
            })
            .unwrap_or_else(std::env::temp_dir);
        let unique = format!(
            "seccomp-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let dir = base.join(unique);
        std::fs::create_dir_all(&dir).expect("create test cache root");
        dir
    }
}
