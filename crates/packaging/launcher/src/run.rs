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
    #[error(
        "manifest declares runtime `{runtime}` but no bundle was found at any of: {paths:?}. \
         Install it with `qos-install runtime install --version <v> --from-host`, \
         or pass --allow-missing-runtime to fall back to --minimal-libs / no runtime binds."
    )]
    RuntimeBundleMissing {
        runtime: String,
        paths: Vec<PathBuf>,
    },
    #[error("manifest declares runtime `{0}` but the value is not in `<name>/<version>` form")]
    RuntimeReferenceMalformed(String),
    #[error(
        "runtime bundle at {path} is unsigned — refuse to launch. Pass \
         --allow-unsigned-runtime for bring-up / debugging."
    )]
    RuntimeBundleUnsigned { path: PathBuf },
    #[error(
        "runtime bundle at {path} failed signature check: {reason}. Rebuild the \
         bundle with a trusted publisher or pass --allow-unsigned-runtime."
    )]
    RuntimeBundleSignature { path: PathBuf, reason: String },
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

impl Default for LaunchMode {
    fn default() -> Self {
        LaunchMode::Gui
    }
}

/// Optional knobs for `run_with`.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Skip all service-socket resolution.
    pub no_services: bool,
    /// Explicit override for the host directory holding a qos-runtime
    /// bundle. When set, wins over the manifest's `runtime` field and
    /// the automatic lookup under `$XDG_DATA_HOME/qos-runtime/`.
    /// Version is hard-coded to "1" for compatibility with the older
    /// mount path when this is the only signal; when the manifest
    /// declares a runtime, its version is used instead.
    pub runtime_root: Option<PathBuf>,
    /// Override the bwrap binary (defaults to `bwrap`).
    pub bwrap_bin: Option<PathBuf>,
    /// Bind-mount the minimum host paths a dynamically-linked ELF
    /// needs to `execve()`: ld-linux + libc + libgcc_s + friends,
    /// plus `/etc/ld.so.cache`. Read-only. This is a stopgap that
    /// preceded the runtime-bundle path — apps that declare
    /// `runtime = "qos-runtime/..."` and have that bundle installed
    /// don't need it (and if both are set, the runtime bundle wins
    /// and `minimal_libs` is silently ignored). Deprecated: kept
    /// working for backward compatibility.
    pub minimal_libs: bool,
    /// Route the exec through the `__sandbox-exec` shim so Landlock
    /// is installed *inside* bwrap's mount namespace before the
    /// entrypoint takes over. Requires this launcher binary to be
    /// bind-mounted into the sandbox at [`SHIM_MOUNT_PATH`]. Default:
    /// on. Turning it off is a debug-only escape hatch and is
    /// logged prominently.
    pub landlock_enforce: bool,
    /// Launch mode — GUI (the default) or CLI. In CLI mode we
    /// pick the `cli_bin()` off the entrypoint (falling back to
    /// `bin`), forward argv from the user's shell, and inherit
    /// stdio instead of trying to attach a Wayland surface.
    pub mode: LaunchMode,
    /// When the manifest declares a runtime but no bundle can be
    /// resolved, fall back to whatever `minimal_libs` implies instead
    /// of aborting. Escape hatch for debugging / bring-up; default:
    /// off.
    pub allow_missing_runtime: bool,
    /// Accept an unsigned runtime bundle. Default: refuse. This is a
    /// bring-up / debug escape hatch — signed bundles are what the
    /// OS image ships (see `os/scripts/bake-runtime-bundle.sh`), so
    /// production launches never need it.
    pub allow_unsigned_runtime: bool,
    /// If set, require the bundle's `publisher_hex` to equal this
    /// (Ed25519 pubkey, hex). Left empty in typical launches; used
    /// by policy-strict callers and by tests. The launcher's trust
    /// config (a follow-up) will pin this by default.
    pub require_runtime_publisher: Option<String>,
}

/// Resolve which runtime bundle to mount, if any.
///
/// Priority:
/// 1. `opts.runtime_root` (explicit CLI / test override) — paired
///    with the manifest's declared version when present, else "1"
///    for backward compat with the older `--runtime <path>` shape.
/// 2. `manifest.runtime` → look up under the default per-user path,
///    then the system fallback.
/// 3. Nothing: no runtime bind.
///
/// The `Ok(None)` branch is deliberately distinct from `Err(...)`:
/// no runtime declaration is fine (apps might rely on --minimal-libs
/// still), whereas a declaration that can't be resolved is an error
/// unless `allow_missing_runtime` is set.
/// Shim identical to [`resolve_runtime`], re-exported under a
/// test-friendly name so integration tests don't need to duplicate
/// the resolution logic.
pub fn resolve_runtime_test_helper(
    manifest: &crate::manifest::MinimalManifest,
    opts: &RunOptions,
) -> Result<Option<(PathBuf, String)>, RunError> {
    resolve_runtime(manifest, opts)
}

pub fn resolve_runtime(
    manifest: &crate::manifest::MinimalManifest,
    opts: &RunOptions,
) -> Result<Option<(PathBuf, String)>, RunError> {
    // 1. Explicit override wins. Verify signature policy against the
    //    caller-supplied path just like the manifest path — an operator
    //    passing --runtime <path> to a debug launch is still subject to
    //    the same "signed unless opted out" rule.
    if let Some(explicit) = &opts.runtime_root {
        let version = manifest
            .runtime_ref()
            .map(|(_, v)| v.to_string())
            .unwrap_or_else(|| "1".to_string());
        enforce_bundle_policy(explicit, opts)?;
        return Ok(Some((explicit.clone(), version)));
    }
    // 2. Manifest declaration.
    let Some(raw) = manifest.runtime.as_deref() else {
        return Ok(None);
    };
    let (_, version) = raw
        .split_once('/')
        .ok_or_else(|| RunError::RuntimeReferenceMalformed(raw.to_string()))?;
    // Look up: per-user first, then system.
    let per_user = runtime_bundle::default_bundle_path(version).ok();
    let system = runtime_bundle::system_bundle_path(version);
    let candidates: Vec<PathBuf> = per_user.iter().cloned().chain([system.clone()]).collect();
    for cand in &candidates {
        if cand.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME).exists() {
            enforce_bundle_policy(cand, opts)?;
            return Ok(Some((cand.clone(), version.to_string())));
        }
    }
    if opts.allow_missing_runtime {
        tracing::warn!(
            "manifest declares runtime `{raw}` but no bundle found; \
             continuing with --minimal-libs={} (allow_missing_runtime=on)",
            opts.minimal_libs
        );
        return Ok(None);
    }
    Err(RunError::RuntimeBundleMissing {
        runtime: raw.to_string(),
        paths: candidates,
    })
}

/// Verify the bundle at `path` against the run-time signature policy.
/// Unless `allow_unsigned_runtime` is set, a bundle must be signed. If
/// `require_runtime_publisher` is set, the bundle's publisher_hex must
/// equal that (case-insensitively). Any hash-integrity failure inside
/// the bundle also surfaces here so we fail-fast before spawning bwrap.
fn enforce_bundle_policy(path: &Path, opts: &RunOptions) -> Result<(), RunError> {
    let verify_opts = runtime_bundle::VerifyOptions {
        require_publisher: opts.require_runtime_publisher.clone(),
        require_signature: !opts.allow_unsigned_runtime,
        require_arch: None,
    };
    match runtime_bundle::verify_bundle_with(path, &verify_opts) {
        Ok(_manifest) => Ok(()),
        Err(runtime_bundle::BundleError::NotSigned) => {
            Err(RunError::RuntimeBundleUnsigned { path: path.to_path_buf() })
        }
        Err(e) => Err(RunError::RuntimeBundleSignature {
            path: path.to_path_buf(),
            reason: e.to_string(),
        }),
    }
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

/// Launch mode — which of the entrypoint binaries we're targeting
/// and what extra args to forward.
#[derive(Debug, Clone)]
pub enum LaunchMode {
    /// The default: GUI-flavour bin. `run` / `run_with` use this
    /// implicitly.
    Gui,
    /// CLI-flavour bin. `passthrough_argv` is what follows `--`
    /// on the shim command line — forwarded verbatim to the
    /// entrypoint. `command` names the manifest entry we resolved
    /// against; the shim + launcher tag it into the env so the
    /// child can distinguish (e.g. multi-subcommand packages).
    Cli {
        command: String,
        passthrough_argv: Vec<String>,
    },
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
    // Runtime bundle resolution. A resolved bundle wins over
    // --minimal-libs: when the app has a proper runtime, we mount that
    // and skip the loader-bind stopgap entirely.
    let resolved_runtime = resolve_runtime(manifest, opts)?;
    let runtime_arg = resolved_runtime
        .as_ref()
        .map(|(p, v)| (p.as_path(), v.as_str()));

    // Pick the entrypoint bin per launch mode. GUI uses the default
    // `bin`; CLI uses `cli_bin()` if set (falling back to `bin`) and
    // splices QOS_CLI_COMMAND / forwards passthrough argv.
    let entry_bin_owned: String;
    let (mut argv, extra_setenv, cli_passthrough) = match &opts.mode {
        LaunchMode::Gui => (
            build_bwrap_argv(manifest, payload_root, runtime_arg, &binds),
            None,
            Vec::new(),
        ),
        LaunchMode::Cli { command, passthrough_argv } => {
            entry_bin_owned = manifest
                .entrypoint
                .cli_bin()
                .unwrap_or(&manifest.entrypoint.bin)
                .to_string();
            let v = crate::sandbox::build_bwrap_argv_for_entry(
                manifest,
                payload_root,
                runtime_arg,
                &binds,
                &entry_bin_owned,
            );
            (
                v,
                Some(("QOS_CLI_COMMAND".to_string(), command.clone())),
                passthrough_argv.clone(),
            )
        }
    };
    // Splice in the QOS_CLI_COMMAND setenv before the trailing
    // entrypoint token so bwrap still parses it as a flag.
    if let Some((k, v)) = extra_setenv {
        let entry = argv.pop().expect("argv non-empty");
        argv.push("--setenv".to_string());
        argv.push(k);
        argv.push(v);
        argv.push(entry);
    }

    // Optional: narrow dynamic-loader binds. Deliberately not the whole
    // /usr — just the files a plain dynamically-linked binary needs to
    // execve(). Split off in front of the entrypoint token (last argv
    // entry) so the mounts happen before exec. Skipped when a runtime
    // bundle is mounted — the bundle already carries the loader.
    let mut extra_ro_paths: Vec<String> = Vec::new();
    let use_minimal_libs = opts.minimal_libs && resolved_runtime.is_none();
    if opts.minimal_libs && resolved_runtime.is_some() {
        tracing::info!(
            "runtime bundle mounted; ignoring --minimal-libs (loader + libc come from the bundle)"
        );
    }
    if use_minimal_libs {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        for path in minimal_loader_binds() {
            if std::path::Path::new(path).exists() {
                argv.push("--ro-bind".to_string());
                argv.push(path.to_string());
                argv.push(path.to_string());
                extra_ro_paths.push((*path).to_string());
            }
        }
        argv.push(entry);
    }

    // Landlock shim wiring. Bind this launcher binary into the sandbox
    // and rewrite the trailing entrypoint token into
    //   <shim-in-sandbox> __sandbox-exec --ro <p>* --rw <p>* [--net-restricted] -- <entry>
    // so Landlock is applied AFTER bwrap sets up the mount namespace.
    if opts.landlock_enforce {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        let launcher = std::env::current_exe()
            .map_err(|e| RunError::Io(std::io::Error::new(std::io::ErrorKind::Other, format!("current_exe: {e}"))))?;
        argv.push("--ro-bind".to_string());
        argv.push(launcher.display().to_string());
        argv.push(SHIM_MOUNT_PATH.to_string());
        argv.push(SHIM_MOUNT_PATH.to_string());
        argv.push(crate::landlock_shim::SANDBOX_EXEC_SUBCOMMAND.to_string());
        // Sandbox-side ro paths for Landlock. The payload mount, the
        // runtime mount (if any), the minimal-libs binds, and the
        // manifest's declared read paths.
        let payload_mount = crate::sandbox::payload_mount_for(&manifest.package_id);
        argv.push("--ro".to_string());
        argv.push(payload_mount.display().to_string());
        if let Some((rt_root, ver)) = runtime_arg {
            let rt = crate::sandbox::runtime_mount_for(ver);
            argv.push("--ro".to_string());
            argv.push(rt.display().to_string());
            // Also allow the canonical loader paths we re-bound out of
            // the bundle — otherwise the app entry ELF (which has its
            // INTERP at `/lib64/ld-linux-...`) gets blocked by
            // Landlock even though the bind is there.
            for canonical in crate::sandbox::CANONICAL_LOADER_PATHS {
                let no_slash = canonical.trim_start_matches('/');
                if rt_root.join("lib").join(no_slash).exists() {
                    argv.push("--ro".to_string());
                    argv.push((*canonical).to_string());
                }
            }
        }
        for p in &extra_ro_paths {
            argv.push("--ro".to_string());
            argv.push(p.clone());
        }
        for path in &manifest.sandbox.fs.read {
            let expanded = crate::manifest::expand_path(path);
            argv.push("--ro".to_string());
            argv.push(expanded.display().to_string());
        }
        // If we bound the session D-Bus socket in for portal use,
        // Landlock must also grant the app read on that socket path
        // — otherwise the `ashpd` connect() surfaces EACCES and the
        // portal appears unavailable, defeating the point of the
        // whole plumbing.
        if let Some(bus_addr) =
            crate::sandbox::host_dbus_bus_address_if_needed(&manifest.sandbox.portals)
        {
            if let Some(path) = crate::sandbox::parse_unix_path_bus_address(&bus_addr) {
                argv.push("--ro".to_string());
                argv.push(path);
            }
        }
        for path in &manifest.sandbox.fs.write {
            let expanded = crate::manifest::expand_path(path);
            argv.push("--rw".to_string());
            argv.push(expanded.display().to_string());
        }
        // Standard tmpfs mounts inside the sandbox — the app expects
        // these to be writable. Marking them --rw grants Landlock's
        // full FS access set on them.
        for p in ["/tmp", "/dev"] {
            argv.push("--rw".to_string());
            argv.push(p.to_string());
        }
        // Service sockets: apps need read + write on the socket path
        // to actually talk over them.
        for sb in &binds {
            let path = crate::sandbox::sandbox_service_path(&sb.name);
            argv.push("--rw".to_string());
            argv.push(path.display().to_string());
        }
        if manifest.sandbox.net.mode == "off" {
            argv.push("--net-restricted".to_string());
        }
        argv.push("--".to_string());
        argv.push(entry);
    }

    // Insert --seccomp <child_fd> at the front so bwrap installs it
    // before any other action (matches the compositor pattern).
    let mut prefix = vec!["--seccomp".to_string(), SECCOMP_CHILD_FD.to_string()];
    prefix.extend(argv.drain(..));
    argv = prefix;

    // CLI passthrough: everything after the entrypoint token is
    // opaque to bwrap and forwarded to the sandboxed program as
    // its argv[1..]. The vec is empty for GUI launches.
    argv.extend(cli_passthrough);

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
    // The shim now IS wired via opts.landlock_enforce (see argv splice
    // above). Log intent from the parent for observability; the shim's
    // own "landlock: {fully,partially} enforced" line surfaces the real
    // kernel-side outcome after bwrap execs the shim.
    tracing::info!(
        target: "qos_launcher::landlock",
        rules = landlock_plan.len(),
        net_restricted,
        enforce = opts.landlock_enforce,
        "landlock plan derived",
    );
    let _ = landlock_plan;

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
            // Landlock is NOT installed here — bwrap needs broad FS
            // access to set up the namespace. It's installed by the
            // __sandbox-exec shim after bwrap's mount setup completes.
            // See `qos_launcher::landlock_shim`.
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

    // Argv. run_with_captured is a lighter-weight sibling that skips
    // Landlock + minimal-libs — but must still honour a manifest-declared
    // runtime bundle so exec succeeds inside the sandbox.
    let resolved_runtime = resolve_runtime(manifest, opts)?;
    let runtime_arg = resolved_runtime
        .as_ref()
        .map(|(p, v)| (p.as_path(), v.as_str()));
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

/// Path inside the bwrap sandbox where the launcher binary is bind-mounted
/// so it can be exec'd as the Landlock shim. Kept out of `/usr` and `/tmp`
/// to avoid colliding with the tmpfs / minimal-libs mounts.
pub const SHIM_MOUNT_PATH: &str = "/qos-launcher";
