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
    #[error("refusing to launch: {0}")]
    FsPolicy(#[from] crate::manifest::FsPolicyViolation),
    #[error(
        "refusing to launch `{package}`: could not create its directory at {path} ({source}). \
         An app with nowhere to write loses the user's work on exit, so this is a refusal \
         rather than a launch into a read-only sandbox."
    )]
    AppDir {
        package: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "refusing to launch `{0}`: neither XDG_DATA_HOME nor HOME is set, so there is no \
         user data directory to put this app's per-room state in."
    )]
    NoDataHome(String),
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
    #[error(
        "`{desktop_id}` is a host application, not a qOS package: it has no manifest, so \
         there is no sandbox to build for it. Launch it with `qos-launcher run-host \
         --desktop-id {desktop_id}`, which runs it unconfined and says so."
    )]
    HostAppNotAPackage { desktop_id: String },
    #[error(
        "refusing to launch: this app was granted access to {hosts:?} and that can only be \
         enforced by brokering its traffic, but the egress gate could not start ({source}). \
         Launching anyway would mean giving it unrestricted outbound access, which is not \
         what was granted. Set QOS_LAUNCHER_NET_GATE=off to do that deliberately."
    )]
    EgressGate {
        hosts: Vec<String>,
        #[source]
        source: crate::netgate::GateError,
    },
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

/// Resolve, create and migrate into the one directory this launch may
/// write to.
///
/// This replaces `materialise_write_grants`, which walked
/// `sandbox.fs.write` and created whatever the package named that fell
/// under the user's home — so a package asking for `~` got the launcher
/// to hand it the home directory, and a package asking for another
/// app's state directory got that. There is no list to walk any more:
/// there is one directory, the launcher picks it, and the package has
/// no say in where it is.
///
/// The room comes from `opts.workspace_id` — the same value the
/// collaboration cluster is keyed on — so an app opened into a
/// workspace gets that workspace's directory and cannot name another's.
fn resolve_app_dir(
    manifest: &MinimalManifest,
    opts: &RunOptions,
) -> Result<PathBuf, RunError> {
    let data_home = crate::appdir::host_data_home()
        .ok_or_else(|| RunError::NoDataHome(manifest.package_id.clone()))?;
    crate::appdir::prepare_host_app_dir(
        &data_home,
        opts.workspace_id.as_deref(),
        &manifest.package_id,
    )
    .map_err(|source| RunError::AppDir {
        package: manifest.package_id.clone(),
        path: crate::appdir::host_app_dir(
            &data_home,
            &crate::appdir::room_slug(opts.workspace_id.as_deref()),
            &manifest.package_id,
        ),
        source,
    })
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
    /// An application qOS did not package: a `.desktop` entry already
    /// installed on the host, Flatpak or otherwise.
    ///
    /// **This mode never reaches the sandbox.** `run_with` refuses it
    /// with [`RunError::HostAppNotAPackage`]; the launch is carried out
    /// by [`crate::host`], which spawns the entry's own command line
    /// with a controlled environment and no bubblewrap at all.
    ///
    /// The refusal is the point. A Flatpak cannot start inside qOS's
    /// sandbox — the baseline seccomp filter denies the `unshare`,
    /// `mount` and `pivot_root` it needs to build its own — and a plain
    /// distribution binary has no manifest to derive a sandbox from. A
    /// future refactor that routed one of these into `build_bwrap_argv`
    /// would produce an app that fails to start, so the disagreement is
    /// made a hard error at the seam rather than left to be discovered
    /// as a launch failure.
    Host {
        /// Desktop-file id of the entry, e.g. `com.spotify.Client`.
        desktop_id: String,
    },
}

/// `run` variant taking a pre-parsed manifest. Useful for tests that
/// want to build a manifest in-memory.
/// Env override that turns the egress gate off, downgrading a
/// host-scoped grant to unrestricted access.
///
/// Deliberately an explicit operator action rather than a default: the
/// point of the gate is that a grant naming a host is not silently
/// applied as "every host".
pub const NET_GATE_ENV: &str = "QOS_LAUNCHER_NET_GATE";

/// Start the egress gate if this launch needs one.
///
/// Returns the bind description to splice into the bwrap argv, and the
/// guard that keeps the gate alive. Both are `None` for a launch with
/// nothing to filter.
///
/// ## Degradation
///
/// **This mechanism needs no privilege and no kernel feature that
/// `bwrap` does not already require.** That is why it was chosen over
/// an `nftables`-in-a-netns design, which needs either `CAP_NET_ADMIN`
/// in the *host* namespace to make a `veth` pair, or a userspace TCP/IP
/// stack (`pasta` / `slirp4netns`) as a hard dependency. Here the
/// pieces are: an unshared network namespace, which every app already
/// gets from `--unshare-all`; one bind-mounted `AF_UNIX` socket, the
/// same primitive that already carries Wayland and D-Bus; and an
/// ordinary process binding an ordinary socket under
/// `$XDG_RUNTIME_DIR`.
///
/// So "the kernel does not support this" cannot arise on its own — a
/// kernel that cannot do it cannot run `bwrap`, and the launch has
/// already failed earlier for a clearer reason. And "we lack the
/// privilege" cannot arise at all, because no step is privileged.
///
/// What *can* fail is binding the socket: a missing, full or unwritable
/// runtime directory. That is a broken environment rather than an
/// unsupported one.
///
/// **On that failure we refuse the launch.** A network grant is a
/// statement about *which* hosts; falling back to `--share-net` would
/// apply "this host" as "every host" and reduce the difference to a log
/// line nobody reads, which is the exact false assurance this code
/// exists to remove. The cost is real and worth naming: an app with a
/// host allowlist will not start on a host whose runtime directory is
/// broken. It is bounded — only a *narrowing* grant needs the gate, so
/// apps granted `*`, and apps whose manifest says `host` with no grant
/// narrowing them, are unaffected. `QOS_LAUNCHER_NET_GATE=off` takes
/// the unfiltered path deliberately, and logs the old warning when it
/// does, so the downgrade is an operator's decision on the record.
///
/// Landlock is the one kernel-dependent piece and it is deliberately
/// *not* held to this standard: it is defence in depth behind the gate,
/// not the boundary. On a kernel without ABI 4 the port pin silently
/// does nothing and enforcement is unaffected, because the namespace is
/// still empty and the gate still holds the policy.
fn start_egress_gate(
    egress: &crate::netgate::EgressDecision,
) -> Result<(Option<crate::sandbox::NetGateBind>, Option<crate::netgate::NetGate>), RunError> {
    // `Filtered` is the only decision that needs a gate. The
    // `QOS_LAUNCHER_NET_GATE=off` escape hatch is not handled here: it
    // is applied in `crate::sandbox::egress_decision`, so that turning
    // the gate off also shares the network instead of leaving the app
    // sealed in an empty namespace with nothing to reach.
    let Some(policy) = egress.policy() else {
        return Ok((None, None));
    };

    let host_socket = crate::netgate::gate_socket_path(current_uid(), std::process::id());
    match crate::netgate::NetGate::start(&host_socket, policy.clone()) {
        Ok(gate) => {
            let bind = crate::sandbox::NetGateBind {
                host_socket,
                sandbox_socket: PathBuf::from(crate::netgate::SANDBOX_GATE_PATH),
                port: crate::netgate::RELAY_PORT,
            };
            Ok((Some(bind), Some(gate)))
        }
        Err(source) => Err(RunError::EgressGate {
            hosts: policy.hosts().to_vec(),
            source,
        }),
    }
}

/// Put every opened service socket on the fd number the app was told
/// to look at, in the child, before `exec`.
///
/// This is the whole of the service handoff, and it is how a
/// descriptor actually crosses into the sandbox: left open, at a known
/// number, inherited through bwrap's exec and the `__sandbox-exec`
/// re-exec. bwrap forwards fds it was not asked about untouched, so an
/// fd placed here survives the namespace setup and arrives in the app
/// as a live connection. `child_fd`s start above the fixed seccomp
/// descriptor, so this cannot land on top of it.
///
/// What does *not* work — and used to be what this did — is bwrap's
/// `--file FD DEST`. That is the flag that looks right and is not: it
/// copies the descriptor's *contents* into a regular file at DEST, so
/// pointed at a connected socket it blocks reading for an EOF the
/// broker will never send. The launch hangs before it ever reaches
/// `exec` and the only symptom is an app that never appears.
///
/// The two passes matter. Target numbers start at 17 and sources are
/// whatever `connect` handed out, so a source *can* already be sitting
/// on a target we have not written yet; relocating every source above
/// the target window first means no `dup2` can clobber a descriptor
/// still waiting to be placed.
///
/// Returns an error rather than continuing quietly: we are here only
/// because the socket existed and we connected to it, so a failure at
/// this point is a bug, not an absent service — and an app that starts
/// with collaboration silently off is exactly the outcome worth
/// refusing.
///
/// # Safety
/// Call only from `Command::pre_exec` (post-fork, pre-exec). Uses only
/// `fcntl` and `dup2`, both async-signal-safe; allocates nothing.
unsafe fn place_service_fds(pairs: &mut [(RawFd, RawFd)]) -> std::io::Result<()> {
    let Some(highest) = pairs.iter().map(|(_, dst)| *dst).max() else {
        return Ok(());
    };
    let floor = highest + 1;
    for (src, _) in pairs.iter_mut() {
        // F_DUPFD returns the lowest free fd >= floor, i.e. clear of
        // every target.
        let moved = unsafe { libc::fcntl(*src, libc::F_DUPFD, floor) };
        if moved >= 0 {
            *src = moved;
        }
    }
    for (src, dst) in pairs.iter() {
        if unsafe { libc::dup2(*src, *dst) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
        // dup2 leaves the target without FD_CLOEXEC, which is what we
        // want: it has to survive bwrap's exec and the app's. Cleared
        // again explicitly because `dup2` onto a number that is
        // already correct is a no-op, and the relocation above is only
        // best-effort — a source that could not be moved may still be
        // sitting on its own target.
        unsafe {
            let flags = libc::fcntl(*dst, libc::F_GETFD);
            if flags != -1 {
                let _ = libc::fcntl(*dst, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
        }
    }
    Ok(())
}

pub fn run_with(
    manifest: &MinimalManifest,
    payload_root: &Path,
    opts: &RunOptions,
) -> Result<ExitStatus, RunError> {
    // 0a. A host application has no manifest and no sandbox. Refusing
    // here — before a socket, a directory or a bwrap argv exists — is
    // what keeps the two kinds of launch from quietly converging.
    if let LaunchMode::Host { desktop_id } = &opts.mode {
        return Err(RunError::HostAppNotAPackage {
            desktop_id: desktop_id.clone(),
        });
    }

    // 0. Policy. A manifest that names a filesystem path is refused
    // before anything else happens — before a socket is opened, before
    // a directory is created, before bwrap is spawned. Refused rather
    // than ignored: an app whose declared paths were silently dropped
    // comes up broken in a way that looks like a bug in the app.
    //
    // This runs at launch as well as at install because a manifest can
    // reach the launcher without ever passing an installer —
    // `qos-launcher run --manifest <path>` is the supported way to test
    // a package, and a package installed before this rule existed still
    // has its old manifest on disk.
    crate::manifest::enforce_fs_policy(manifest)?;

    // 1. Service sockets.
    let resolved =
        resolve_service_binds(&manifest.sandbox.services, opts.no_services)?;
    let binds = to_service_binds(&resolved);

    // 2. Seccomp baseline → tmpfile → fd. The manifest *asks* for a profile
    // and the image decides whether this package may have it; both ways of
    // not getting what was asked for are worth a line, and they are
    // different lines. An unrecognised name is a packaging mistake. A
    // recognised weaker name from a package the image does not entitle is an
    // app trying to lift a protection off itself, which is worth saying out
    // loud whether it was an accident or not.
    let decision =
        crate::seccomp::resolve_profile(&manifest.sandbox.seccomp, &manifest.package_id);
    let profile = decision.applied;
    if decision.refused {
        eprintln!(
            "qos-launcher: {} asked for seccomp profile {:?}, which the image does not \
             entitle it to; applying {} (write-xor-execute enforced). Entitlement is \
             decided by {}, not by the package.",
            manifest.package_id,
            decision.requested,
            profile.as_str(),
            crate::seccomp::JIT_POLICY_PATH,
        );
    } else if profile.as_str() != decision.requested {
        eprintln!(
            "qos-launcher: manifest asked for seccomp profile {:?}, which is not one we know; \
             applying {} (write-xor-execute enforced)",
            decision.requested,
            profile.as_str()
        );
    }
    let seccomp_blob = compile_baseline(profile)?;
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

    // The app's one writable directory, for the room it is being opened
    // into. Created here because bwrap can only bind what already
    // exists, and a first launch must not come up with no place to save.
    let app_dir = resolve_app_dir(manifest, opts)?;

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

    // Egress. A grant naming specific hosts leaves the namespace empty
    // and runs a gate the app's traffic is brokered through; `*` and a
    // bare `net.mode = "host"` share the host network as before, and
    // neither needs a gate because neither has an allowlist to enforce.
    //
    // The guard must outlive the child, so it is bound here and not in
    // the `if let`.
    let egress = crate::sandbox::egress_decision(manifest, &capabilities);
    let (net_gate, _gate_guard) = start_egress_gate(&egress)?;

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
        // Refused at the top of this function, before any of the setup
        // above ran. Reaching here would mean that guard was removed.
        LaunchMode::Host { desktop_id } => unreachable!(
            "host application {desktop_id} reached the sandbox builder; \
             the LaunchMode::Host guard at the top of run_with is gone"
        ),
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

    // The app's directory: bound read-write at a fixed in-sandbox path,
    // with HOME and every XDG variable pointed inside it. Spliced here
    // rather than built into `build_bwrap_argv` because the room is a
    // property of the *launch*, not of the package — which is the whole
    // reason the package cannot influence it.
    {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::appdir::push_app_dir(&mut argv, &app_dir);
        // After `push_app_dir`, because that is what repoints `HOME` at
        // the app's own directory: this says where the *user's* home is,
        // and only when a grant has actually bound it.
        if let Some(user_home) = crate::sandbox::push_user_home(&mut argv, &capabilities) {
            tracing::debug!(
                user_home = %user_home.display(),
                "a granted bind covers the user's home; telling the app where it is"
            );
        }
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

    // Egress gate socket. Spliced like the Wayland socket, and for the
    // same reason — it is the app's only route out of an otherwise
    // empty network namespace.
    if let Some(bind) = &net_gate {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::sandbox::push_net_gate(&mut argv, bind);
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

    // In-sandbox shim. Bind this launcher binary into the sandbox and
    // rewrite the trailing entrypoint token into
    //   <shim-in-sandbox> __sandbox-exec [rules…] -- <entry>
    //
    // Two jobs, either of which is reason enough to be here: apply
    // Landlock *after* bwrap has set up the mount namespace, and start
    // the egress relay before the app's first connect. A launch with
    // the gate but without Landlock enforcement still needs the shim,
    // and says `--no-landlock` so the shim does not read an empty rule
    // list as "deny everything".
    let need_shim = opts.landlock_enforce || net_gate.is_some();
    if need_shim {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        let launcher = std::env::current_exe()
            .map_err(|e| RunError::Io(std::io::Error::new(std::io::ErrorKind::Other, format!("current_exe: {e}"))))?;
        argv.push("--ro-bind".to_string());
        argv.push(launcher.display().to_string());
        argv.push(SHIM_MOUNT_PATH.to_string());
        argv.push(SHIM_MOUNT_PATH.to_string());
        argv.push(crate::landlock_shim::SANDBOX_EXEC_SUBCOMMAND.to_string());
        if !opts.landlock_enforce {
            argv.push("--no-landlock".to_string());
        }
        // The relay's endpoints. Passed whenever a gate is running, so
        // an unmodified HTTP client honouring ALL_PROXY reaches it.
        if let Some(bind) = &net_gate {
            argv.push("--net-gate".to_string());
            argv.push(bind.sandbox_socket.display().to_string());
            argv.push("--net-gate-port".to_string());
            argv.push(bind.port.to_string());
        }
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
        // The system read profile. Derived from the same list the binds
        // came from, so the two layers cannot disagree about what is
        // readable — a bind Landlock does not name is visible and
        // returns EACCES on every open, which reads from inside the app
        // as a corrupted system rather than as a policy.
        for path in crate::profile::landlock_ro_paths(egress.shares_host_network()) {
            argv.push("--ro".to_string());
            argv.push(path.display().to_string());
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
        // Same condition as the bind itself: a read-only participant has
        // no identity bound, so there is nothing to grant read on.
        if identity_arg.is_some() && capabilities.participation.carries_identity() {
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
        // The app's own directory — the only writable place it has that
        // survives the process. Named by its *in-sandbox* path: the host
        // path is not reachable from inside, and a rule on it would
        // match nothing.
        argv.push("--rw".to_string());
        argv.push(crate::appdir::SANDBOX_APP_DIR.to_string());
        // The GPU device nodes the profile bound. A GPU context is
        // opened O_RDWR, so a read-only rule surfaces at device open and
        // looks exactly like "no GPU present".
        for path in crate::profile::landlock_rw_device_paths() {
            argv.push("--rw".to_string());
            argv.push(path.display().to_string());
        }
        // Standard tmpfs mounts inside the sandbox — the app expects
        // these to be writable. Marking them --rw grants Landlock's
        // full FS access set on them. Both are per-sandbox tmpfs mounts
        // that vanish with the process, so this is scratch space rather
        // than a second place state can hide.
        for p in ["/tmp", "/dev"] {
            argv.push("--rw".to_string());
            argv.push(p.to_string());
        }
        // Service sockets need no Landlock rule, deliberately. They
        // reach the app as already-connected descriptors dup2'd into
        // place by `place_service_fds`, so nothing under
        // `/run/qos/services` is ever mounted and no `open()` is ever
        // performed on one. A rule for a path that does not exist
        // inside the sandbox would be skipped every launch, and would
        // only make the shim report itself partially enforced.
        //
        // The gate socket is reached with connect(2), which is a
        // write-flavoured access on a pathname socket — a read-only
        // rule surfaces as EACCES at connect time, exactly like the
        // Wayland socket above.
        if let Some(bind) = &net_gate {
            argv.push("--rw".to_string());
            argv.push(bind.sandbox_socket.display().to_string());
        }
        // Landlock's network restriction mirrors the namespace decision
        // above. Blocked: deny every TCP bind and connect, on top of an
        // already-empty netns. Filtered: the shim pins the app to the
        // relay port instead (it derives that from --net-gate-port), so
        // a blanket ban here would lock it out of its own gate.
        // Unrestricted: nothing to say.
        if matches!(egress, crate::netgate::EgressDecision::Blocked) {
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
    clear_bwrap_environment(&mut cmd);

    // pre_exec: dup the seccomp blob fd into SECCOMP_CHILD_FD, and put
    // each service socket on the fd number the app was told to read.
    let host_seccomp_fd = seccomp_fd.as_raw_fd();
    let mut service_fds: Vec<(RawFd, RawFd)> =
        resolved.iter().map(|r| (r.host_fd, r.child_fd as RawFd)).collect();
    for r in &resolved {
        tracing::info!(
            service = %r.name,
            child_fd = r.child_fd,
            "service socket connected; handing the descriptor to the app",
        );
    }
    // Landlock plan: derived here so we can log it from the parent.
    // We deliberately do NOT install Landlock in `pre_exec` on the
    // bwrap process — bwrap needs broad FS access to mount and pivot
    // and would be locked out. The correct wiring is a small shim
    // binary (modeled on `shell/src/sandbox/landlock_shim.rs`) that
    // bwrap execs as its command; the shim restricts itself and then
    // execs the real entrypoint. That shim is a follow-up.
    let landlock_plan = crate::landlock::plan_from_paths(
        &crate::profile::landlock_ro_paths(egress.shares_host_network()),
        &[PathBuf::from(crate::appdir::SANDBOX_APP_DIR)],
    );
    let net_restricted = matches!(egress, crate::netgate::EgressDecision::Blocked);
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
    tracing::info!(
        target: "qos_launcher::netgate",
        gated = net_gate.is_some(),
        hosts = ?egress.policy().map(|p| p.hosts()),
        shares_host_network = egress.shares_host_network(),
        "egress resolved",
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
            place_service_fds(&mut service_fds)?;
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
    // Same refusals as `run_with`, and for the same reason: this is a
    // second entry into the sandbox builder, and a policy with one
    // enforced door is a policy with a door beside it.
    if let LaunchMode::Host { desktop_id } = &opts.mode {
        return Err(RunError::HostAppNotAPackage {
            desktop_id: desktop_id.clone(),
        });
    }
    crate::manifest::enforce_fs_policy(manifest)?;

    // Service sockets.
    let resolved = resolve_service_binds(&manifest.sandbox.services, opts.no_services)?;
    let binds = to_service_binds(&resolved);

    // Seccomp baseline, resolved against the image's entitlement policy
    // exactly as `run_with` does. Two paths reaching different conclusions
    // about the same package would make the capture path a way to run under
    // a filter the launch path refuses.
    let seccomp_blob = compile_baseline(manifest.sandbox.seccomp_profile(&manifest.package_id))?;
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
    // Same egress decision as `run_with`, and for the same reason: an
    // app whose grant names hosts gets an empty namespace, so without
    // the gate spliced in here too it would be silently offline rather
    // than filtered. The guard has to outlive the child.
    //
    // No shim runs on this path, so there is no loopback relay — the
    // gate is reachable at `QOS_NET_GATE` but the `ALL_PROXY` endpoint
    // has nothing listening behind it.
    let egress = crate::sandbox::egress_decision(manifest, &capabilities);
    let (net_gate, _gate_guard) = start_egress_gate(&egress)?;
    // Entrypoint + trailing arguments, resolved the same way `run_with`
    // resolves them. This path used to build a GUI argv unconditionally
    // and drop `opts.mode` and `opts.open_document` on the floor, so a
    // caller asking for a CLI launch got the GUI binary with no
    // arguments — a silent disagreement between the two entries into the
    // same sandbox.
    let (entry_bin, passthrough): (String, Vec<String>) = match &opts.mode {
        LaunchMode::Gui => (
            manifest
                .entrypoint
                .gui_bin()
                .unwrap_or(&manifest.entrypoint.bin)
                .to_string(),
            opts.open_document
                .as_ref()
                .map(|p| vec![p.display().to_string()])
                .unwrap_or_default(),
        ),
        LaunchMode::Cli { passthrough_argv, .. } => (
            manifest
                .entrypoint
                .cli_bin()
                .unwrap_or(&manifest.entrypoint.bin)
                .to_string(),
            passthrough_argv.clone(),
        ),
        // Refused at the top of this function. See `run_with`.
        LaunchMode::Host { desktop_id } => unreachable!(
            "host application {desktop_id} reached the sandbox builder; \
             the LaunchMode::Host guard at the top of run_with_captured is gone"
        ),
    };
    let mut argv = crate::sandbox::build_bwrap_argv_for_entry(
        manifest,
        payload_root,
        runtime_arg,
        &binds,
        dbus_plan.as_ref(),
        identity_arg,
        &entry_bin,
        &capabilities,
    );
    if let Some(bind) = &net_gate {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::sandbox::push_net_gate(&mut argv, bind);
        argv.push(entry);
    }
    if let Some(workspace_id) = opts.workspace_id.as_deref() {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::sandbox::push_workspace_id(&mut argv, workspace_id);
        argv.push(entry);
    }
    {
        let app_dir = resolve_app_dir(manifest, opts)?;
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::appdir::push_app_dir(&mut argv, &app_dir);
        argv.push(entry);
    }
    let mut prefix = vec!["--seccomp".to_string(), SECCOMP_CHILD_FD.to_string()];
    prefix.extend(argv.drain(..));
    argv = prefix;
    // Everything after the entrypoint token is opaque to bwrap and
    // becomes the child's argv[1..].
    argv.extend(passthrough);

    let bwrap = opts
        .bwrap_bin
        .clone()
        .unwrap_or_else(|| PathBuf::from("bwrap"));
    let mut cmd = Command::new(&bwrap);
    cmd.args(&argv);
    clear_bwrap_environment(&mut cmd);
    cmd.stdout(std::process::Stdio::piped());

    let host_seccomp_fd = seccomp_fd.as_raw_fd();
    let mut service_fds: Vec<(RawFd, RawFd)> =
        resolved.iter().map(|r| (r.host_fd, r.child_fd as RawFd)).collect();
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(host_seccomp_fd, SECCOMP_CHILD_FD) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let flags = libc::fcntl(SECCOMP_CHILD_FD, libc::F_GETFD);
            if flags != -1 {
                let _ = libc::fcntl(SECCOMP_CHILD_FD, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
            place_service_fds(&mut service_fds)?;
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

/// Give `bwrap` itself an environment worth inheriting, so that a dropped
/// `--clearenv` cannot silently become a leak.
///
/// The argv already carries `--clearenv`
/// ([`crate::sandbox::push_clean_environment`]), and that is the mechanism
/// that decides what the *app* sees. This is the second half of the same
/// statement, made where the process is spawned rather than where its
/// arguments are assembled: bwrap is handed nothing to pass on, so an argv
/// that lost its flag — a refactor, a splice in the wrong order — fails
/// closed with an app that finds an empty environment instead of quietly
/// handing it the compositor's.
///
/// `PATH` survives because it is the one variable the spawn itself needs:
/// `bwrap_bin` defaults to the bare name `bwrap`, and `Command` resolves a
/// bare program name against the *child's* `PATH`. Clearing it outright
/// turns every launch into `ENOENT`. It is the sandbox's own
/// [`crate::sandbox::SANDBOX_PATH`], not the session's, so the value is one
/// the app would have been given anyway.
fn clear_bwrap_environment(cmd: &mut Command) {
    cmd.env_clear();
    cmd.env("PATH", crate::sandbox::SANDBOX_PATH);
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
mod app_dir_tests {
    use super::*;

    fn manifest(id: &str) -> crate::manifest::MinimalManifest {
        toml::from_str(&format!(
            "[package]\nid = {id:?}\nversion = \"0.1.0\"\n\n[entrypoint]\nbin = \"bin/x\"\n"
        ))
        .expect("fixture manifest decodes")
    }

    fn opts_in(room: Option<&str>) -> RunOptions {
        RunOptions {
            workspace_id: room.map(str::to_string),
            ..RunOptions::default()
        }
    }

    /// The launch path resolves the directory rather than the manifest
    /// naming it, and the room it was launched into is what decides
    /// which one.
    #[test]
    #[serial_test::serial(process_env)]
    fn the_launch_resolves_one_directory_per_room() {
        let data = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("XDG_DATA_HOME");
        // SAFETY: this test is in the `process_env` serial group.
        unsafe { std::env::set_var("XDG_DATA_HOME", data.path()) };

        let m = manifest("dev.qos.fixture");
        let a = resolve_app_dir(&m, &opts_in(Some("room-a"))).expect("room a");
        let b = resolve_app_dir(&m, &opts_in(Some("room-b"))).expect("room b");
        let personal = resolve_app_dir(&m, &opts_in(None)).expect("personal");

        // SAFETY: this test is in the `process_env` serial group.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }

        assert!(a.is_dir() && b.is_dir() && personal.is_dir());
        assert_ne!(a, b);
        assert_ne!(a, personal);
        assert!(
            !a.starts_with(&b) && !b.starts_with(&a),
            "neither room may contain the other: {a:?} {b:?}"
        );
        for dir in [&a, &b, &personal] {
            assert!(
                dir.starts_with(data.path().join("qos-rooms")),
                "{dir:?} is outside the rooms tree"
            );
        }
    }

    /// A package cannot make the launcher create a directory it named:
    /// the retired `sandbox.fs.write` block is refused, and a manifest
    /// that carries one never reaches the directory step at all.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_declared_write_path_is_never_created() {
        let data = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("XDG_DATA_HOME");
        // SAFETY: this test is in the `process_env` serial group.
        unsafe { std::env::set_var("XDG_DATA_HOME", data.path()) };

        let mut m = manifest("dev.qos.fixture");
        let victim = data.path().join("dev.qos.totp");
        m.sandbox.fs.write.push(victim.display().to_string());
        let refused = run_with(&m, Path::new("/nonexistent"), &opts_in(None));

        // SAFETY: this test is in the `process_env` serial group.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }

        let err = refused.expect_err("a manifest naming a write path must be refused");
        assert!(
            matches!(err, RunError::FsPolicy(_)),
            "expected a policy refusal, got: {err}"
        );
        assert!(
            !victim.exists(),
            "the launcher must not create a path a package named"
        );
    }
}