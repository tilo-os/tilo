//! Top-level `run` orchestration.
//!
//! Sequence (matches the CLI):
//!
//! 1. Read the manifest.
//! 2. Resolve service sockets per `--no-services`.
//! 3. Stage the baseline seccomp blob to a tempfile, open it.
//! 4. Build the bwrap argv.
//! 5. Spawn `bwrap`; arrange `pre_exec` to install Landlock (best-
//!    effort) and dup the seccomp blob fd into a fixed slot.
//! 6. Wait on the child; relay its exit code.

use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use crate::manifest::{MinimalManifest, read_manifest};
use crate::sandbox::build_bwrap_argv;
use crate::seccomp::compile_baseline;
use crate::services::{
    EXIT_REQUIRED_SERVICE_MISSING, ServiceError, resolve_service_binds, to_service_binds,
};

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Manifest(#[from] crate::manifest::ManifestError),
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    Seccomp(#[from] crate::seccomp::SeccompError),
    #[error("spawning bwrap: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl RunError {
    /// Translate to a process exit code. Required-service-missing gets
    /// a dedicated code so callers can distinguish "this app can't run
    /// here" from generic launch failures.
    pub fn to_exit_code(&self) -> i32 {
        match self {
            RunError::Service(ServiceError::RequiredMissing { .. }) => {
                EXIT_REQUIRED_SERVICE_MISSING
            }
            _ => 1,
        }
    }
}

/// Optional knobs for `run_with`.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Skip all service-socket resolution.
    pub no_services: bool,
    /// Host directory holding the qos-runtime tree; mounted ro at
    /// `/usr/lib/qos-runtime/<version>/`. Version is hard-coded to
    /// "1" today — qos-pkg will pin this on integration.
    pub runtime_root: Option<PathBuf>,
    /// Override the bwrap binary (defaults to `bwrap`).
    pub bwrap_bin: Option<PathBuf>,
    /// Bind-mount the minimum host paths a dynamically-linked ELF
    /// needs to `execve()`: ld-linux + libc + libgcc_s + friends,
    /// plus `/etc/ld.so.cache`. Read-only. This is a stopgap until
    /// the qos-runtime bundle path lands — apps that declare
    /// `runtime = "qos-runtime/..."` should NOT need this. The
    /// bind set is deliberately narrow (no full `/usr` or
    /// `/lib64`) to keep the attack surface small.
    pub minimal_libs: bool,
}

/// Top-level entry. Reads the manifest, builds the sandbox, execs.
/// Returns the child exit status (or an error). Does not call
/// `process::exit` directly — that's `main.rs`'s job.
pub fn run(
    manifest_path: &Path,
    payload_root: &Path,
    opts: &RunOptions,
) -> Result<ExitStatus, RunError> {
    let manifest = read_manifest(manifest_path)?;
    run_with(&manifest, payload_root, opts)
}

/// `run` variant taking a pre-parsed manifest. Useful for tests that
/// want to build a manifest in-memory.
pub fn run_with(
    manifest: &MinimalManifest,
    payload_root: &Path,
    opts: &RunOptions,
) -> Result<ExitStatus, RunError> {
    // 1. Service sockets.
    let resolved =
        resolve_service_binds(&manifest.sandbox.services, opts.no_services)?;
    let binds = to_service_binds(&resolved);

    // 2. Seccomp baseline → tmpfile → fd.
    let seccomp_blob = compile_baseline()?;
    let seccomp_fd: OwnedFd = stage_seccomp_blob(&seccomp_blob)?;
    const SECCOMP_CHILD_FD: i32 = 16;

    // 3. Argv.
    let runtime_arg = opts.runtime_root.as_deref().map(|p| (p, "1"));
    let mut argv = build_bwrap_argv(manifest, payload_root, runtime_arg, &binds);

    // Optional: narrow dynamic-loader binds. Deliberately not the whole
    // /usr — just the files a plain dynamically-linked binary needs to
    // execve(). Split off in front of the entrypoint token (last argv
    // entry) so the mounts happen before exec.
    if opts.minimal_libs {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        for path in minimal_loader_binds() {
            if std::path::Path::new(path).exists() {
                argv.push("--ro-bind".to_string());
                argv.push(path.to_string());
                argv.push(path.to_string());
            }
        }
        argv.push(entry);
    }

    // Insert --seccomp <child_fd> at the front so bwrap installs it
    // before any other action (matches the compositor pattern).
    let mut prefix = vec!["--seccomp".to_string(), SECCOMP_CHILD_FD.to_string()];
    prefix.extend(argv.drain(..));
    argv = prefix;

    // 4. Build the bwrap Command.
    let bwrap = opts
        .bwrap_bin
        .clone()
        .unwrap_or_else(|| PathBuf::from("bwrap"));
    let mut cmd = Command::new(&bwrap);
    cmd.args(&argv);

    // pre_exec: dup the seccomp blob fd into SECCOMP_CHILD_FD; install
    // Landlock; clear CLOEXEC on all the service fds we passed via
    // --file (bwrap should keep them, but stripping CLOEXEC is belt +
    // suspenders).
    let host_seccomp_fd = seccomp_fd.as_raw_fd();
    let resolved_fds: Vec<RawFd> = resolved.iter().map(|r| r.host_fd).collect();
    // Landlock plan: derived here so we can log it from the parent.
    // We deliberately do NOT install Landlock in `pre_exec` on the
    // bwrap process — bwrap needs broad FS access to mount and pivot
    // and would be locked out. The correct wiring is a small shim
    // binary (modeled on `shell/src/sandbox/landlock_shim.rs`) that
    // bwrap execs as its command; the shim restricts itself and then
    // execs the real entrypoint. That shim is a follow-up.
    let landlock_plan = crate::landlock::plan_from_paths(
        &manifest
            .sandbox
            .fs
            .read
            .iter()
            .map(|s| crate::manifest::expand_path(s))
            .collect::<Vec<_>>(),
        &manifest
            .sandbox
            .fs
            .write
            .iter()
            .map(|s| crate::manifest::expand_path(s))
            .collect::<Vec<_>>(),
    );
    let net_restricted = manifest.sandbox.net.mode == "off";
    tracing::info!(
        "landlock plan derived ({} rules, net_restricted={net_restricted}); shim wiring is a follow-up",
        landlock_plan.len()
    );
    let _ = landlock_plan;
    let _ = net_restricted;

    // SAFETY: pre_exec runs in the child after fork, before exec. We
    // perform only async-signal-safe operations: dup2, fcntl, plus the
    // landlock crate's syscall wrappers. No allocator calls, no locks.
    unsafe {
        cmd.pre_exec(move || {
            // Plumb seccomp blob fd → fixed child fd, strip CLOEXEC.
            if libc::dup2(host_seccomp_fd, SECCOMP_CHILD_FD) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let flags = libc::fcntl(SECCOMP_CHILD_FD, libc::F_GETFD);
            if flags != -1 {
                let _ = libc::fcntl(SECCOMP_CHILD_FD, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
            for fd in &resolved_fds {
                let flags = libc::fcntl(*fd, libc::F_GETFD);
                if flags != -1 {
                    let _ = libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                }
            }
            // Landlock is NOT installed here: bwrap needs broad FS
            // access to set up the namespace. See the comment in
            // `run_with` above — the shim wiring is a follow-up. The
            // pure plan derivation still happens in the parent so
            // logging shows what the policy would have been.
            Ok(())
        });
    }

    // 5. Spawn + wait.
    let mut child = cmd.spawn().map_err(RunError::Spawn)?;
    // Important: keep `seccomp_fd` alive until after `spawn` returns,
    // so its dup2'd copy in the child remains valid. Drop here.
    drop(seccomp_fd);
    let status = child.wait()?;
    Ok(status)
}

/// `run_with` variant that captures the child's stdout. Returns the
/// exit status alongside the captured bytes. Used by tests.
pub fn run_with_captured(
    manifest: &MinimalManifest,
    payload_root: &Path,
    opts: &RunOptions,
) -> Result<(ExitStatus, Vec<u8>), RunError> {
    // Service sockets.
    let resolved = resolve_service_binds(&manifest.sandbox.services, opts.no_services)?;
    let binds = to_service_binds(&resolved);

    // Seccomp baseline.
    let seccomp_blob = compile_baseline()?;
    let seccomp_fd: OwnedFd = stage_seccomp_blob(&seccomp_blob)?;
    const SECCOMP_CHILD_FD: i32 = 16;

    // Argv.
    let runtime_arg = opts.runtime_root.as_deref().map(|p| (p, "1"));
    let mut argv = build_bwrap_argv(manifest, payload_root, runtime_arg, &binds);
    let mut prefix = vec!["--seccomp".to_string(), SECCOMP_CHILD_FD.to_string()];
    prefix.extend(argv.drain(..));
    argv = prefix;

    let bwrap = opts
        .bwrap_bin
        .clone()
        .unwrap_or_else(|| PathBuf::from("bwrap"));
    let mut cmd = Command::new(&bwrap);
    cmd.args(&argv);
    cmd.stdout(std::process::Stdio::piped());

    let host_seccomp_fd = seccomp_fd.as_raw_fd();
    let resolved_fds: Vec<RawFd> = resolved.iter().map(|r| r.host_fd).collect();
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(host_seccomp_fd, SECCOMP_CHILD_FD) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let flags = libc::fcntl(SECCOMP_CHILD_FD, libc::F_GETFD);
            if flags != -1 {
                let _ = libc::fcntl(SECCOMP_CHILD_FD, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
            for fd in &resolved_fds {
                let flags = libc::fcntl(*fd, libc::F_GETFD);
                if flags != -1 {
                    let _ = libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                }
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(RunError::Spawn)?;
    drop(seccomp_fd);
    let mut buf = Vec::new();
    if let Some(mut so) = child.stdout.take() {
        use std::io::Read;
        so.read_to_end(&mut buf)?;
    }
    let status = child.wait()?;
    Ok((status, buf))
}

/// Write the seccomp blob to a `memfd` so its fd outlives both the
/// parent's tempdir teardown and the bwrap exec. Falls back to a
/// real tempfile if memfd_create is unavailable. Returns an
/// `OwnedFd` so the caller controls lifetime.
fn stage_seccomp_blob(bytes: &[u8]) -> std::io::Result<OwnedFd> {
    // Try memfd first.
    let name = b"qos-launcher-seccomp\0";
    let raw_fd = unsafe {
        libc::syscall(
            libc::SYS_memfd_create,
            name.as_ptr() as *const libc::c_char,
            0u32,
        )
    } as RawFd;
    if raw_fd >= 0 {
        // memfd_create returns a regular fd; safe to wrap.
        let owned = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(raw_fd) };
        write_all_pwrite(raw_fd, bytes)?;
        return Ok(owned);
    }

    // Fallback: O_TMPFILE under /tmp. If even that fails we surface
    // the IO error; the launcher cannot safely continue without a
    // seccomp blob fd.
    let path = std::ffi::CString::new("/tmp").unwrap();
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    write_all_pwrite(fd, bytes)?;
    Ok(unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) })
}

fn write_all_pwrite(fd: RawFd, bytes: &[u8]) -> std::io::Result<()> {
    let mut written = 0usize;
    while written < bytes.len() {
        let n = unsafe {
            libc::pwrite(
                fd,
                bytes[written..].as_ptr() as *const libc::c_void,
                bytes.len() - written,
                written as libc::off_t,
            )
        };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "pwrite returned 0",
            ));
        }
        written += n as usize;
    }
    Ok(())
}

/// The narrow set of host paths a dynamically-linked ELF needs to
/// `execve()` successfully. Deliberately excludes `/usr/bin` etc. —
/// the sandbox binds the app's own payload, not the host binaries.
///
/// This is a stopgap until the qos-runtime bundle path lands. Apps
/// declaring `runtime = "qos-runtime/..."` should not need `--minimal-libs`.
fn minimal_loader_binds() -> &'static [&'static str] {
    &[
        // Dynamic loader itself.
        "/lib64/ld-linux-x86-64.so.2",
        "/lib/ld-linux-x86-64.so.2",
        "/lib/ld-linux.so.2",
        "/usr/lib64/ld-linux-x86-64.so.2",
        // libc + friends that most Rust binaries need.
        "/lib64/libc.so.6",
        "/lib64/libgcc_s.so.1",
        "/lib64/libm.so.6",
        "/lib64/libdl.so.2",
        "/lib64/libpthread.so.0",
        "/lib64/librt.so.1",
        "/lib64/libutil.so.1",
        // Dynamic linker's cache. Without it the loader falls back
        // to a slower search but still works — we bind it because
        // launch-latency matters.
        "/etc/ld.so.cache",
    ]
}
