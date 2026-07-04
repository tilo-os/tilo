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

use crate::dbus_proxy::{self, DBusProxyGuard};
use crate::manifest::{MinimalManifest, PortalPolicy, read_manifest};
use crate::sandbox::{DBusBindKind, DBusBindPlan, build_bwrap_argv};
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
        LaunchMode::Gui { passthrough_argv: Vec::new() }
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
    /// Explicit host path to the user identity JSON. When `None`,
    /// the launcher resolves the default location from the process's
    /// `$XDG_CONFIG_HOME` / `$HOME` env vars and uses it only if the
    /// file actually exists. Tests supply an override so they can
    /// point at a tempdir without shimming `$HOME`.
    ///
    /// When resolved (either explicitly or from env), the file is
    /// ro-bound into the sandbox at
    /// [`crate::sandbox::SANDBOX_IDENTITY_PATH`] and
    /// `QOS_USER_IDENTITY` is set to that path — so the sandboxed
    /// `runtime-identity::load()` finds a real identity to attach to
    /// join_channel presence + signed-op attribution.
    pub identity_host_path: Option<PathBuf>,
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

/// Resolve the host path to the user identity JSON.
///
/// Priority:
/// 1. `opts.identity_host_path` explicit override — used verbatim,
///    without an existence check (tests may point at a not-yet-created
///    file that a fixture will materialise before the sandbox runs).
/// 2. `$XDG_CONFIG_HOME/qos/identity/primary.json` if the env var is
///    set + the file exists.
/// 3. `$HOME/.config/qos/identity/primary.json` if it exists.
/// 4. `None` — no identity to plumb; apps fall back to the "no
///    identity" path unchanged.
///
/// The env-var path also matches what `shell::firstboot` writes on
/// first login and what `shell::identity_provisioning::DiskIdentityStore`
/// uses, so a machine with a fresh boot chain converges on one file.
pub fn resolve_identity_host_path(opts: &RunOptions) -> Option<PathBuf> {
    if let Some(p) = &opts.identity_host_path {
        return Some(p.clone());
    }
    let file_name = "primary.json";
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            let p = PathBuf::from(xdg)
                .join("qos")
                .join("identity")
                .join(file_name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() {
            let p = PathBuf::from(home)
                .join(".config")
                .join("qos")
                .join("identity")
                .join(file_name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
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
/// Unless `allow_unsigned_runtime` is set, a bundle must be signed.
///
/// Publisher policy is:
///   1. If `opts.require_runtime_publisher` is set, the bundle's
///      publisher must equal that (case-insensitive hex).
///   2. Else, if the system trust-list file
///      (`/etc/qos/runtime-bundle-trusted-publishers.txt` — or the
///      `QOS_LAUNCHER_TRUSTED_PUBLISHERS_FILE` override) exists and
///      is non-empty, the publisher must be listed in it.
///   3. Else, any signed publisher is accepted (the pre-trust-file
///      default the runtime-bundle crate ships).
///
/// This layered check is what lets the OS image bake a fresh key at
/// build time and pin it into the trust list, without every launcher
/// caller having to plumb the pubkey through as a flag.
fn enforce_bundle_policy(path: &Path, opts: &RunOptions) -> Result<(), RunError> {
    let require_publisher = match opts.require_runtime_publisher.clone() {
        Some(explicit) => Some(explicit),
        None => trusted_publisher_pin_from_file(),
    };
    let verify_opts = runtime_bundle::VerifyOptions {
        require_publisher,
        require_signature: !opts.allow_unsigned_runtime,
        require_arch: None,
    };
    match runtime_bundle::verify_bundle_with(path, &verify_opts) {
        Ok(manifest) => {
            // Multi-publisher trust list: if the file names more than
            // one key, `require_publisher` above was `None` (since we
            // could only pin one). Do the check by hand here.
            if let Some(list) = load_trusted_publisher_list() {
                if !list.is_empty() {
                    let ok = manifest
                        .publisher_hex
                        .as_deref()
                        .map(|pk| list.iter().any(|t| t.eq_ignore_ascii_case(pk)))
                        .unwrap_or(false);
                    if !ok {
                        return Err(RunError::RuntimeBundleSignature {
                            path: path.to_path_buf(),
                            reason: format!(
                                "publisher {:?} not in trust list",
                                manifest.publisher_hex.as_deref().unwrap_or("<none>"),
                            ),
                        });
                    }
                }
            }
            Ok(())
        }
        Err(runtime_bundle::BundleError::NotSigned) => {
            Err(RunError::RuntimeBundleUnsigned { path: path.to_path_buf() })
        }
        Err(e) => Err(RunError::RuntimeBundleSignature {
            path: path.to_path_buf(),
            reason: e.to_string(),
        }),
    }
}

/// If the trust list contains exactly one entry, that's the pin we
/// hand to `runtime_bundle::verify_bundle_with` — the crate's
/// `require_publisher` field takes a single hex string. Multi-entry
/// lists are handled in a second pass after verify returns.
fn trusted_publisher_pin_from_file() -> Option<String> {
    let list = load_trusted_publisher_list()?;
    if list.len() == 1 {
        Some(list.into_iter().next().unwrap())
    } else {
        None
    }
}

/// Read the trusted-publisher list from disk. Ignores blank + `#`
/// lines. Returns `None` when neither the env override nor the
/// default file exists (so \"no policy\" is distinct from \"empty
/// list\").
fn load_trusted_publisher_list() -> Option<Vec<String>> {
    let path: PathBuf = match std::env::var_os("QOS_LAUNCHER_TRUSTED_PUBLISHERS_FILE") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from("/etc/qos/runtime-bundle-trusted-publishers.txt"),
    };
    let bytes = std::fs::read(&path).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let list: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect();
    Some(list)
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
    /// GUI-flavour bin (the default). `passthrough_argv` is what
    /// follows `--` on `qos-launcher run` / `qos-install run`;
    /// forwarded verbatim to the entrypoint as its argv[1..]. Empty
    /// in the usual case where no extra args were provided.
    Gui { passthrough_argv: Vec<String> },
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

    // 2b. D-Bus proxy / direct-bind decision. Manifest asks for
    // portals? Try to spawn xdg-dbus-proxy; on failure fall back
    // to a raw-path bind. `_dbus_guard` stays alive until we finish
    // waiting on bwrap so Drop can SIGTERM the proxy.
    let (dbus_plan, _dbus_guard) = resolve_dbus_bind(&manifest.sandbox.portals);

    // 3. Argv.
    // Runtime bundle resolution. A resolved bundle wins over
    // --minimal-libs: when the app has a proper runtime, we mount that
    // and skip the loader-bind stopgap entirely.
    let resolved_runtime = resolve_runtime(manifest, opts)?;
    let runtime_arg = resolved_runtime
        .as_ref()
        .map(|(p, v)| (p.as_path(), v.as_str()));

    // Identity resolution: host location of the user identity JSON.
    // Threaded through both bwrap-argv builders so the child receives
    // a `QOS_USER_IDENTITY` env var pointing at the sandbox-side path
    // — that's the loader the `runtime-identity::load` SDK calls.
    let identity_host = resolve_identity_host_path(opts);
    let identity_arg = identity_host.as_deref();

    // Pick the entrypoint bin per launch mode. GUI uses the default
    // `bin`; CLI uses `cli_bin()` if set (falling back to `bin`) and
    // splices QOS_CLI_COMMAND / forwards passthrough argv.
    let entry_bin_owned: String;
    let (mut argv, extra_setenv, cli_passthrough) = match &opts.mode {
        LaunchMode::Gui { passthrough_argv } => (
            build_bwrap_argv(
                manifest,
                payload_root,
                runtime_arg,
                &binds,
                dbus_plan.as_ref(),
                identity_arg,
            ),
            None,
            passthrough_argv.clone(),
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
                dbus_plan.as_ref(),
                identity_arg,
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
        // If we bound a D-Bus socket in for portal use, Landlock must
        // also grant read on the sandbox-side path so ashpd's connect()
        // doesn't surface EACCES. Path is either `/run/user/<uid>/bus`
        // (proxy) or the raw bus path (direct fallback); the plan
        // carries it.
        if let Some(plan) = &dbus_plan {
            argv.push("--ro".to_string());
            argv.push(plan.sandbox_path.display().to_string());
        }
        // Same story for the ro-bound identity file: without a
        // Landlock grant on the sandbox-side path, the app's
        // `runtime-identity::load()` open() surfaces EACCES and the
        // whole feature quietly no-ops.
        if identity_arg.is_some() {
            argv.push("--ro".to_string());
            argv.push(crate::sandbox::SANDBOX_IDENTITY_PATH.to_string());
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
    // D-Bus plan (same decision path as `run_with`).
    let (dbus_plan, _dbus_guard) = resolve_dbus_bind(&manifest.sandbox.portals);
    let identity_host = resolve_identity_host_path(opts);
    let identity_arg = identity_host.as_deref();
    let mut argv = build_bwrap_argv(
        manifest,
        payload_root,
        runtime_arg,
        &binds,
        dbus_plan.as_ref(),
        identity_arg,
    );
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

/// Decide how (or whether) to expose D-Bus into the sandbox based on the
/// manifest's portal declaration.
///
/// Returns a pair:
/// - `Option<DBusBindPlan>` — the plan to hand to `build_bwrap_argv`.
///   `None` means no D-Bus at all (no portals declared, all `Deny`, or
///   the host has no session bus).
/// - `Option<DBusProxyGuard>` — RAII handle keeping the spawned proxy
///   alive. When it drops the proxy is SIGTERMed and its socket unlinked.
///   `None` on the direct-bind fallback (nothing to keep alive) or when
///   no bind is happening.
///
/// The preferred path is: spawn `xdg-dbus-proxy` filtered to portals
/// + notifications + MPRIS, bind ITS socket into the sandbox. That
/// blocks the sandboxed app from reaching every other well-known name
/// the host session bus carries.
///
/// The fallback fires only when `xdg-dbus-proxy` isn't installed OR
/// couldn't be spawned. It emits a `tracing::warn!` so operators
/// notice, then bind-mounts the raw session bus path. Abstract bus
/// addresses can't be bind-mounted at all, so the fallback also
/// returns `(None, None)` when the address is `unix:abstract=...` —
/// that host really does need the proxy.
fn resolve_dbus_bind(
    portals: &std::collections::BTreeMap<String, PortalPolicy>,
) -> (Option<DBusBindPlan>, Option<DBusProxyGuard>) {
    let wants_bus = portals.values().any(|p| matches!(p, PortalPolicy::ViaHost));
    if !wants_bus {
        return (None, None);
    }
    let Ok(bus_addr) = std::env::var("DBUS_SESSION_BUS_ADDRESS") else {
        tracing::warn!(
            "manifest declares a via-host portal but DBUS_SESSION_BUS_ADDRESS is unset; \
             portals will be unavailable to the sandboxed app"
        );
        return (None, None);
    };
    if bus_addr.is_empty() {
        return (None, None);
    }
    let uid = current_uid();

    // Preferred path: xdg-dbus-proxy filtered to portal-adjacent names.
    if let Some(binary) = dbus_proxy::find_proxy_binary() {
        let sock = dbus_proxy::proxy_socket_path(uid, std::process::id());
        match dbus_proxy::spawn_dbus_proxy(&binary, &bus_addr, &sock) {
            Ok(guard) => {
                let plan = DBusBindPlan {
                    host_socket: guard.socket_path().to_path_buf(),
                    sandbox_path: dbus_proxy::sandbox_bus_path(uid),
                    kind: DBusBindKind::Proxy,
                };
                tracing::info!(
                    proxy_binary = %binary.display(),
                    socket = %guard.socket_path().display(),
                    "xdg-dbus-proxy spawned for portal access"
                );
                return (Some(plan), Some(guard));
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "xdg-dbus-proxy failed to spawn; falling back to raw bus bind"
                );
                // Fall through to direct-bind fallback.
            }
        }
    } else {
        tracing::warn!(
            "xdg-dbus-proxy not found on PATH; falling back to raw bus bind. \
             Install `flatpak-xdg-utils` to filter portal traffic."
        );
    }

    // Fallback: raw bus path. Only works for unix:path=... — abstract
    // sockets are unbindable and the app degrades to "portal unavailable".
    match crate::sandbox::parse_unix_path_bus_address(&bus_addr) {
        Some(path) => {
            let path_buf = PathBuf::from(&path);
            let plan = DBusBindPlan {
                host_socket: path_buf.clone(),
                sandbox_path: path_buf,
                kind: DBusBindKind::Direct,
            };
            (Some(plan), None)
        }
        None => {
            tracing::warn!(
                bus_addr = %bus_addr,
                "cannot bind D-Bus into sandbox: address is not `unix:path=` and no proxy is available"
            );
            (None, None)
        }
    }
}

/// Current process UID via libc — avoids pulling `users` or `nix` in
/// just for this. Falls back to 1000 if the syscall somehow fails,
/// which matches the sandbox module's `for_workspace_test` default.
fn current_uid() -> u32 {
    // SAFETY: getuid always succeeds and returns a uid_t.
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod identity_resolution_tests {
    use super::*;

    // A serial guard: these tests mutate `XDG_CONFIG_HOME` / `HOME`,
    // which are shared with every other test in the process. Cargo's
    // parallel runner would race them without the mutex.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn snapshot_and_clear() -> (Option<std::ffi::OsString>, Option<std::ffi::OsString>) {
        let x = std::env::var_os("XDG_CONFIG_HOME");
        let h = std::env::var_os("HOME");
        // SAFETY: guarded by ENV_MUTEX in each test.
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("HOME");
        }
        (x, h)
    }

    fn restore(x: Option<std::ffi::OsString>, h: Option<std::ffi::OsString>) {
        // SAFETY: guarded by ENV_MUTEX in each test.
        unsafe {
            match x {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match h {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn explicit_override_wins_and_skips_existence_check() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let saved = snapshot_and_clear();
        let opts = RunOptions {
            identity_host_path: Some(PathBuf::from("/nowhere/does/not/exist.json")),
            ..RunOptions::default()
        };
        let resolved = resolve_identity_host_path(&opts);
        restore(saved.0, saved.1);
        assert_eq!(
            resolved,
            Some(PathBuf::from("/nowhere/does/not/exist.json")),
            "explicit path is used verbatim, no exists() check"
        );
    }

    #[test]
    fn xdg_wins_when_file_exists_there() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let saved = snapshot_and_clear();
        let tmp = tempfile::tempdir().unwrap();
        let ident = tmp
            .path()
            .join("qos")
            .join("identity")
            .join("primary.json");
        std::fs::create_dir_all(ident.parent().unwrap()).unwrap();
        std::fs::write(&ident, b"{}").unwrap();
        // SAFETY: guarded by ENV_MUTEX.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", tmp.path()) };
        let opts = RunOptions::default();
        let resolved = resolve_identity_host_path(&opts);
        restore(saved.0, saved.1);
        assert_eq!(resolved, Some(ident));
    }

    #[test]
    fn home_fallback_used_when_no_xdg() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let saved = snapshot_and_clear();
        let tmp = tempfile::tempdir().unwrap();
        let ident = tmp
            .path()
            .join(".config")
            .join("qos")
            .join("identity")
            .join("primary.json");
        std::fs::create_dir_all(ident.parent().unwrap()).unwrap();
        std::fs::write(&ident, b"{}").unwrap();
        // SAFETY: guarded by ENV_MUTEX.
        unsafe { std::env::set_var("HOME", tmp.path()) };
        let opts = RunOptions::default();
        let resolved = resolve_identity_host_path(&opts);
        restore(saved.0, saved.1);
        assert_eq!(resolved, Some(ident));
    }

    #[test]
    fn none_when_nothing_resolves() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let saved = snapshot_and_clear();
        let opts = RunOptions::default();
        let resolved = resolve_identity_host_path(&opts);
        restore(saved.0, saved.1);
        assert_eq!(resolved, None);
    }
}

// The prior HEAD block above the identity_resolution_tests module
// carried the resolve_dbus_bind helper; find_proxy_binary and
// spawn_dbus_proxy live in the crate::dbus_proxy module.
