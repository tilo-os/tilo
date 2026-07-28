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

use crate::capabilities::{CapabilityPlan, load_capability_file, now_unix};
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
    /// Host path of the Wayland socket a GUI app should connect to
    /// (typically `$XDG_RUNTIME_DIR/wayland-N` for the running
    /// compositor). When set, the socket is bound into the sandbox at
    /// `/run/user/<uid>/wayland-0` and `WAYLAND_DISPLAY` /
    /// `XDG_RUNTIME_DIR` are set to match, so `wl_display_connect`
    /// inside the sandbox finds it.
    ///
    /// `None` means the app gets no display at all — which is correct
    /// for CLI launches and for a GUI app launched from a context with
    /// no compositor. A GUI app without this simply fails to connect;
    /// we never fall back to the launcher process's own
    /// `$WAYLAND_DISPLAY`, because "whatever display the launcher
    /// happened to inherit" is not a security decision anyone made.
    pub wayland_socket: Option<PathBuf>,
    /// The DRM render node the compositor resolved for itself, e.g.
    /// `/dev/dri/renderD128`.
    ///
    /// A Wayland client has to render on the *same* GPU as the
    /// compositor: buffers allocated on a different device either cost
    /// a cross-device copy on every frame or cannot be imported at all,
    /// and the tile shows an import error instead of the app. The
    /// compositor already made this decision (it advertises the node as
    /// `zwp_linux_dmabuf_v1`'s `main_device`) and passes it down here
    /// rather than letting the launcher re-derive it — two independent
    /// probes can disagree, and a client on the wrong GPU is exactly
    /// the failure this plumbing exists to prevent.
    ///
    /// `None` means no compositor told us anything: the app's loaders
    /// keep their own defaults, which is right for a CLI launch and for
    /// a GUI launch from a context with no compositor.
    pub render_node: Option<PathBuf>,
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
    /// Workspace (collaboration room) this launch belongs to, as the
    /// compositor names it. Forwarded into the sandbox as
    /// [`crate::sandbox::WORKSPACE_ID_ENV`].
    ///
    /// This is what makes collaboration between two locally-hosted
    /// apps possible at all: the toolkit turns it into the cluster id
    /// every CRDT delta is addressed to, so apps sharing a workspace
    /// share a room and apps in different workspaces do not.
    ///
    /// `None` — the default — means the child sees no
    /// `QOS_WORKSPACE_ID`, which is the correct answer for a launch
    /// that isn't inside a session (`qos-launcher run` from a shell,
    /// the CLI shim, tests). Apps then fall back to their
    /// standalone-dev identity.
    pub workspace_id: Option<String>,
    /// Host path of a JSON file holding the `Vec<CapOverride>` the user
    /// granted in the workspace this app is being launched into. Written
    /// by the compositor immediately before spawning us.
    ///
    /// `None` — and any file that is missing, unreadable or malformed —
    /// means the app gets exactly the sandbox its manifest describes.
    /// There is no failure mode here that widens the sandbox.
    pub capabilities_path: Option<PathBuf>,
    /// The app id capabilities are addressed to
    /// (`Subject::Principal(Principal::App(..))`). Defaults to the
    /// manifest's package id. The launcher re-checks every capability
    /// against this id rather than trusting the file to have been
    /// pre-filtered, so a capability file that names another app's
    /// grants still binds nothing for this one.
    pub app_id: Option<String>,
    /// A document the app should open on start, as an absolute host
    /// path. Forwarded to the entrypoint as its `argv[1]`.
    ///
    /// The path is *only* an argument. Whether the app can actually read
    /// it is decided entirely by the manifest and the capability file:
    /// naming a path here binds nothing and unlocks nothing, so a caller
    /// that wants the app to succeed has to grant the path as well
    /// (`Scope::FsPath`). That separation is deliberate — an argument
    /// the app chose to pass itself must never be a way to widen its own
    /// sandbox.
    ///
    /// `None` — the default — launches the app with no arguments, which
    /// is every launch that isn't opening something.
    pub open_document: Option<PathBuf>,
}

/// Resolve the capability plan for a launch: read the file the
/// compositor wrote, then evaluate it for this app at the current time.
///
/// Split out of [`run_with`] so both it and [`run_with_captured`] reach
/// the same answer, and so tests can exercise the file → plan step
/// without spawning bwrap.
pub fn resolve_capabilities(manifest: &MinimalManifest, opts: &RunOptions) -> CapabilityPlan {
    let Some(path) = opts.capabilities_path.as_deref() else {
        return CapabilityPlan::empty();
    };
    let app_id = opts
        .app_id
        .clone()
        .unwrap_or_else(|| manifest.package_id.clone());
    let records = load_capability_file(path);
    let plan = CapabilityPlan::for_app(&app_id, &records, Some(now_unix()));
    tracing::info!(
        target: "qos_launcher::capabilities",
        app = %app_id,
        ro = plan.ro_paths.len(),
        rw = plan.rw_paths.len(),
        devices = ?plan.devices,
        network = plan.allow_network,
        "workspace capabilities resolved",
    );
    plan
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
/// Create the write grants an app owns but that don't exist on the host
/// yet.
///
/// bwrap binds existing inodes; it cannot conjure one. A first launch of
/// an app whose manifest declares `~/.local/share/qos-chat` would
/// otherwise get no such directory in its namespace — and, because every
/// path in the sandbox is an explicit bind, no writable parent to create
/// it from either. So the app would come up unable to save anything, on
/// every machine, until something outside the sandbox happened to make
/// the directory first.
///
/// Only paths under the user's own home and runtime dir are created:
/// those are the ones the app is the owner of. System locations
/// (`/dev/snd`, `/var/workspace`, `/etc/…`) are the session's or the
/// distribution's to provide — the launcher binds them if they exist and
/// silently skips them if they don't, which is what `--bind-try` gives.
///
/// A trailing component carrying a file extension (`settings.json`,
/// `Untitled.md`) is created as an empty file, everything else as a
/// directory. Failures are logged and ignored: a grant we could not
/// materialise degrades to "not bound", never to a refused launch.
fn materialise_write_grants(manifest: &crate::manifest::MinimalManifest) {
    let owned_roots: Vec<PathBuf> = [
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
    ]
    .into_iter()
    .flatten()
    .collect();

    for entry in &manifest.sandbox.fs.write {
        let host = crate::manifest::expand_path(entry);
        if host.exists() {
            continue;
        }
        if !owned_roots.iter().any(|root| host.starts_with(root)) {
            continue;
        }
        let is_file = host
            .file_name()
            .and_then(|n| n.to_str())
            // `.config` is a dotdir, not an extension — only a dot past
            // the first character marks a file name.
            .is_some_and(|n| n.rfind('.').is_some_and(|i| i > 0));
        let result = if is_file {
            host.parent()
                .map(std::fs::create_dir_all)
                .unwrap_or(Ok(()))
                .and_then(|()| {
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&host)
                        .map(|_| ())
                })
        } else {
            std::fs::create_dir_all(&host)
        };
        match result {
            Ok(()) => tracing::debug!(path = %host.display(), "created declared write grant"),
            Err(e) => tracing::warn!(
                path = %host.display(),
                "could not create declared write grant: {e}"
            ),
        }
    }
}

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

/// Derive the GPU pinning for this launch, or `None` when there is
/// nothing to pin.
///
/// `None` in two distinct situations, both correct:
/// - the caller passed no `render_node` (a CLI launch, or a GUI launch
///   with no compositor), so nobody has decided which GPU is right;
/// - the manifest grants the app no DRM access *and* nothing could be
///   derived, so there is no loader to steer.
///
/// Everything the derivation declined to pin is logged. "The app quietly
/// came up on the other GPU" is precisely the failure this is here to
/// stop, so it must never happen silently.
pub fn resolve_gpu_selection(
    manifest: &crate::manifest::MinimalManifest,
    opts: &RunOptions,
) -> Option<crate::gpu::GpuSelection> {
    let node = opts.render_node.as_ref()?;
    let selection = crate::gpu::select_for_node(node, &crate::gpu::IcdSearch::default());
    for note in &selection.notes {
        tracing::warn!(target: "qos_launcher::gpu", package = %manifest.package_id, "{note}");
    }
    tracing::info!(
        target: "qos_launcher::gpu",
        package = %manifest.package_id,
        node = %selection.node.display(),
        driver = selection.driver.as_deref().unwrap_or("<unknown>"),
        egl = selection.egl_vendor_json.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "<loader default>".into()),
        vulkan = selection.vulkan_icd_json.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "<loader default>".into()),
        "pinning the app to the compositor's render node",
    );
    Some(selection)
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

    // Write grants the app owns but that don't exist yet — its state
    // directory on a first run, a shader cache before the GPU driver
    // has ever written one. bwrap can only bind what is already there,
    // so create them here rather than letting a first launch come up
    // with no place to save.
    materialise_write_grants(manifest);

    // Identity resolution: host location of the user identity JSON.
    // Threaded through both bwrap-argv builders so the child receives
    // a `QOS_USER_IDENTITY` env var pointing at the sandbox-side path
    // — that's the loader the `runtime-identity::load` SDK calls.
    let identity_host = resolve_identity_host_path(opts);
    let identity_arg = identity_host.as_deref();

    // The user's decision, as capabilities. Empty unless the compositor
    // wrote a capability file for this launch — so an app launched
    // without one is confined to its manifest, which is the same
    // sandbox it had before capabilities existed.
    let capabilities = resolve_capabilities(manifest, opts);

    // Pick the entrypoint bin per launch mode. GUI uses the default
    // `bin`; CLI uses `cli_bin()` if set (falling back to `bin`) and
    // splices QOS_CLI_COMMAND / forwards passthrough argv.
    let entry_bin_owned: String;
    let (mut argv, extra_setenv, cli_passthrough) = match &opts.mode {
        LaunchMode::Gui => (
            build_bwrap_argv(
                manifest,
                payload_root,
                runtime_arg,
                &binds,
                dbus_plan.as_ref(),
                identity_arg,
                &capabilities,
            ),
            None,
            // A GUI launch carries at most one argument: the document
            // it was asked to open. It rides the same tail as the CLI
            // passthrough because it is the same thing to bwrap —
            // opaque bytes after the entrypoint token.
            opts.open_document
                .as_ref()
                .map(|p| vec![p.display().to_string()])
                .unwrap_or_default(),
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
                &capabilities,
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

    // Workspace id. Spliced the same way — in front of the entrypoint
    // token — so the child learns which collaboration room it was
    // opened into. Absent when the launch has no session behind it.
    if let Some(workspace_id) = opts.workspace_id.as_deref() {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::sandbox::push_workspace_id(&mut argv, workspace_id);
        argv.push(entry);
    }

    // Wayland display. Spliced in front of the entrypoint token (the
    // last argv entry) so bwrap parses it as a flag, exactly like the
    // minimal-libs binds above. A GUI app with no `wayland_socket`
    // simply has no display in its namespace.
    let wayland_bind = opts
        .wayland_socket
        .as_ref()
        .map(|host| crate::sandbox::wayland_bind_for(host, current_uid()));
    if let Some(bind) = &wayland_bind {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::sandbox::push_wayland_bind(&mut argv, bind);
        argv.push(entry);
    }

    // GPU. The compositor's render node decides which EGL vendor and
    // which Vulkan ICD the app is allowed to load, and which device
    // node it can see at all. Spliced in after the manifest's own
    // `/dev/dri` grant so the narrowing lands on top of it.
    let gpu = resolve_gpu_selection(manifest, opts);
    if let Some(sel) = &gpu {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::sandbox::push_gpu_selection(
            &mut argv,
            sel,
            crate::sandbox::grants_drm(manifest, &capabilities),
        );
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
        // Capability-granted paths need matching Landlock rules, or the
        // bind would be visible and every open() on it would still
        // return EACCES. The bind and the rule are derived from the same
        // plan so the two layers cannot disagree about what was granted.
        for path in &capabilities.ro_paths {
            argv.push("--ro".to_string());
            argv.push(path.display().to_string());
        }
        for path in &capabilities.rw_paths {
            argv.push("--rw".to_string());
            argv.push(path.display().to_string());
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
        // The Wayland socket needs read+write in the Landlock ruleset:
        // `connect(2)` on a Unix socket is a write-flavoured access, so
        // a read-only rule surfaces as EACCES at `wl_display_connect`
        // and the app dies with "failed to connect to display".
        if let Some(bind) = &wayland_bind {
            argv.push("--rw".to_string());
            argv.push(bind.sandbox_socket.display().to_string());
        }
        // The GPU selection is only real if Landlock agrees with it.
        // The loader manifests need read; the render node needs
        // read+write, because a GPU context is opened `O_RDWR` and a
        // read-only rule surfaces as EACCES at device open — which
        // looks exactly like "no GPU present" from inside the app.
        if let Some(sel) = &gpu {
            for file in sel.manifest_files() {
                argv.push("--ro".to_string());
                argv.push(file.display().to_string());
            }
            argv.push("--rw".to_string());
            argv.push(sel.node.display().to_string());
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
        // Landlock's network restriction mirrors the namespace decision
        // above: an app the user granted network keeps TCP, everything
        // else is denied bind/connect on top of an already-empty netns.
        if manifest.sandbox.net.mode == "off" && !capabilities.allow_network {
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

    // Passthrough: everything after the entrypoint token is opaque to
    // bwrap and forwarded to the sandboxed program as its argv[1..].
    // For a CLI launch that's the user's own argv; for a GUI launch
    // it's the document `--open` named, or nothing.
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
    let net_restricted = manifest.sandbox.net.mode == "off" && !capabilities.allow_network;
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
    let capabilities = resolve_capabilities(manifest, opts);
    let mut argv = build_bwrap_argv(
        manifest,
        payload_root,
        runtime_arg,
        &binds,
        dbus_plan.as_ref(),
        identity_arg,
        &capabilities,
    );
    if let Some(workspace_id) = opts.workspace_id.as_deref() {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::sandbox::push_workspace_id(&mut argv, workspace_id);
        argv.push(entry);
    }
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

    fn snapshot_and_clear() -> (Option<std::ffi::OsString>, Option<std::ffi::OsString>) {
        let x = std::env::var_os("XDG_CONFIG_HOME");
        let h = std::env::var_os("HOME");
        // SAFETY: the callers are all in the `process_env` serial group.
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("HOME");
        }
        (x, h)
    }

    fn restore(x: Option<std::ffi::OsString>, h: Option<std::ffi::OsString>) {
        // SAFETY: the callers are all in the `process_env` serial group.
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
    #[serial_test::serial(process_env)]
    fn explicit_override_wins_and_skips_existence_check() {
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
    #[serial_test::serial(process_env)]
    fn xdg_wins_when_file_exists_there() {
        let saved = snapshot_and_clear();
        let tmp = tempfile::tempdir().unwrap();
        let ident = tmp
            .path()
            .join("qos")
            .join("identity")
            .join("primary.json");
        std::fs::create_dir_all(ident.parent().unwrap()).unwrap();
        std::fs::write(&ident, b"{}").unwrap();
        // SAFETY: this test is in the `process_env` serial group.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", tmp.path()) };
        let opts = RunOptions::default();
        let resolved = resolve_identity_host_path(&opts);
        restore(saved.0, saved.1);
        assert_eq!(resolved, Some(ident));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn home_fallback_used_when_no_xdg() {
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
        // SAFETY: this test is in the `process_env` serial group.
        unsafe { std::env::set_var("HOME", tmp.path()) };
        let opts = RunOptions::default();
        let resolved = resolve_identity_host_path(&opts);
        restore(saved.0, saved.1);
        assert_eq!(resolved, Some(ident));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn none_when_nothing_resolves() {
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

#[cfg(test)]
mod write_grant_tests {
    use super::*;

    fn manifest_with_writes(writes: &[&str]) -> crate::manifest::MinimalManifest {
        let list = writes
            .iter()
            .map(|w| format!("{w:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!(
            r#"
[package]
id = "dev.qos.fixture"
version = "0.1.0"

[entrypoint]
bin = "bin/fixture"

[sandbox.fs]
write = [{list}]
"#
        );
        toml::from_str(&src).expect("fixture manifest decodes")
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_first_launch_gets_its_declared_state_directory() {
        let home = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        // SAFETY: this test is in the `process_env` serial group.
        unsafe { std::env::set_var("HOME", home.path()) };

        let m = manifest_with_writes(&["~/.local/share/qos-fixture"]);
        let target = home.path().join(".local/share/qos-fixture");
        assert!(!target.exists(), "precondition: nothing there yet");

        materialise_write_grants(&m);

        assert!(
            target.is_dir(),
            "an app's own declared state dir must exist before bwrap tries \
             to bind it, or the app comes up with nowhere to save"
        );

        // SAFETY: this test is in the `process_env` serial group.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_grant_naming_a_file_is_created_as_a_file_not_a_directory() {
        let home = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        // SAFETY: this test is in the `process_env` serial group.
        unsafe { std::env::set_var("HOME", home.path()) };

        let m = manifest_with_writes(&["~/.config/qos/settings.json"]);
        materialise_write_grants(&m);

        let target = home.path().join(".config/qos/settings.json");
        assert!(target.is_file(), "settings.json must be a file: {target:?}");
        assert!(
            home.path().join(".config/qos").is_dir(),
            "the dotdir on the way to it is still a directory"
        );

        // SAFETY: this test is in the `process_env` serial group.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn system_paths_are_left_to_the_system() {
        let home = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        // SAFETY: this test is in the `process_env` serial group.
        unsafe { std::env::set_var("HOME", home.path()) };

        // A device node and a mount point the launcher has no business
        // fabricating — if they are absent the grant is simply unused.
        let m = manifest_with_writes(&["/dev/snd", "/var/workspace/nope-qos-fixture"]);
        materialise_write_grants(&m);

        assert!(
            !Path::new("/var/workspace/nope-qos-fixture").exists(),
            "the launcher must not create paths outside the user's own dirs"
        );

        // SAFETY: this test is in the `process_env` serial group.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}
