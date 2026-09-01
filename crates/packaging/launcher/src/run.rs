//! Top-level `run` orchestration, matching the CLI sequence: read the
//! manifest; resolve service sockets (per `--no-services`); stage the
//! seccomp blob to a tempfile; build the bwrap argv; spawn `bwrap` with
//! `pre_exec` installing Landlock (best-effort) and the seccomp fd; wait
//! and relay the exit code.

use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use crate::capabilities::{CapabilityPlan, load_capability_file, now_unix};
use crate::dbus_proxy::{self, DBusProxyGuard};
use crate::manifest::{MinimalManifest, PortalPolicy, read_manifest};
use crate::sandbox::{DBusBindKind, DBusBindPlan, build_bwrap_argv};
use crate::seccomp::compile_baseline;
use crate::services::{
    EXIT_REQUIRED_SERVICE_MISSING, ServiceError, resolve_identity_bind, resolve_service_binds,
    to_service_binds,
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
    #[error("refusing URI open: {0}")]
    InvalidOpenUri(String),
    #[error("refusing authored launch: identity signing broker could not start ({0})")]
    IdentityBroker(String),
    #[error(
        "internal error: the sandbox argv lost its trailing entrypoint token before a splice \
         step could run. This should never happen — refusing to launch rather than risk a \
         command line silently missing one of its sandboxing directives."
    )]
    ArgvInvariant,
}

impl RunError {
    /// Translate to a process exit code; required-service-missing gets its
    /// own code, distinct from a generic launch failure.
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent CLI-exposed launch knob (no_services, minimal_libs, \
              landlock_enforce, allow_missing_runtime, allow_unsigned_runtime), not combinatorial \
              state — grouping them into an enum would not make any combination less valid"
)]
pub struct RunOptions {
    /// Skip all service-socket resolution.
    pub no_services: bool,
    /// Explicit override for the qos-runtime bundle directory; wins over
    /// the manifest's `runtime` field.
    pub runtime_root: Option<PathBuf>,
    /// Override the bwrap binary (defaults to `bwrap`).
    pub bwrap_bin: Option<PathBuf>,
    /// Bind-mount the minimum host paths a dynamically-linked ELF needs to
    /// `execve()`, read-only. Ignored (silently) when a runtime bundle resolves.
    pub minimal_libs: bool,
    /// Route exec through the `__sandbox-exec` shim so Landlock installs
    /// inside bwrap's mount namespace. Requires this binary bind-mounted at
    /// [`SHIM_MOUNT_PATH`]. Default on; off is a logged debug escape hatch.
    pub landlock_enforce: bool,
    /// Launch mode — GUI (default) or CLI. CLI picks `cli_bin()` (falling
    /// back to `bin`), forwards argv, and inherits stdio instead of a Wayland surface.
    pub mode: LaunchMode,
    /// Debug escape hatch: when a runtime is declared but no bundle resolves,
    /// fall back to `minimal_libs` instead of aborting. Default off.
    pub allow_missing_runtime: bool,
    /// Accept an unsigned runtime bundle. Default refuse — the OS image only
    /// ships signed bundles; this is a bring-up/debug escape hatch.
    pub allow_unsigned_runtime: bool,
    /// Require the bundle's `publisher_hex` to equal this. Used by
    /// policy-strict callers and tests.
    pub require_runtime_publisher: Option<String>,
    /// Host path of the Wayland socket, bound into the sandbox at
    /// `/run/user/<uid>/wayland-0`. `None` means no display — never falls
    /// back to the launcher's own `$WAYLAND_DISPLAY`.
    pub wayland_socket: Option<PathBuf>,
    /// The compositor's DRM render node. A client must render on the same
    /// GPU as the compositor or buffer imports fail or cost a copy; taken
    /// from the compositor's own `zwp_linux_dmabuf_v1` decision rather than
    /// re-derived. `None` means loaders keep their own defaults.
    pub render_node: Option<PathBuf>,
    /// Host path to user identity metadata; `None` resolves the
    /// `$XDG_CONFIG_HOME`/`$HOME` default. It is read only by the launcher to
    /// create a broker capability and is never bound into the sandbox.
    pub identity_host_path: Option<PathBuf>,
    /// Host path of the session's bounded, non-secret human identity
    /// projection. Honoured only for Files and mounted read-only over its
    /// otherwise writable app directory.
    pub trusted_identities_host_path: Option<PathBuf>,
    /// Workspace this launch belongs to — the CRDT cluster id
    /// ([`crate::sandbox::WORKSPACE_ID_ENV`]). `None` means no session.
    pub workspace_id: Option<String>,
    /// Host path of the compositor-written `Vec<CapOverride>` JSON. `None`,
    /// missing, unreadable or malformed all mean the manifest's sandbox as-is.
    pub capabilities_path: Option<PathBuf>,
    /// The app id capabilities are addressed to (defaults to the manifest's
    /// package id); re-checked per capability rather than trusted.
    pub app_id: Option<String>,
    /// A document to open, forwarded as `argv[1]`. Only an argument — binds
    /// and unlocks nothing on its own. `None` launches with no arguments.
    pub open_document: Option<PathBuf>,
    /// A URI to open, forwarded as one opaque argv value. The caller must
    /// validate its scheme against the freshly-read installed manifest.
    pub open_uri: Option<String>,
}

fn gui_open_args(opts: &RunOptions) -> Vec<String> {
    if let Some(uri) = &opts.open_uri {
        vec![uri.clone()]
    } else {
        opts.open_document
            .as_ref()
            .map(|path| vec![path.display().to_string()])
            .unwrap_or_default()
    }
}

fn validate_open_uri(manifest: &MinimalManifest, opts: &RunOptions) -> Result<(), RunError> {
    let Some(uri) = opts.open_uri.as_deref() else {
        return Ok(());
    };
    if uri.is_empty()
        || uri.len() > 8 * 1024
        || uri.trim() != uri
        || uri.chars().any(char::is_control)
    {
        return Err(RunError::InvalidOpenUri("malformed URI".into()));
    }
    let (scheme, rest) = uri
        .split_once(':')
        .ok_or_else(|| RunError::InvalidOpenUri("URI has no scheme".into()))?;
    let scheme = scheme.to_ascii_lowercase();
    if rest.is_empty() || !manifest.opens_schemes.iter().any(|claim| claim == &scheme) {
        return Err(RunError::InvalidOpenUri(format!(
            "{} does not declare scheme {scheme}",
            manifest.package_id
        )));
    }
    Ok(())
}

#[cfg(test)]
mod resource_claim_tests {
    use super::*;

    fn browser_manifest() -> MinimalManifest {
        toml::from_str(
            "package_id = \"dev.qos.browser\"\nversion = \"1\"\n\
             [entrypoint]\nbin = \"bin/browser\"\n\
             [providers]\nschemes = [\"https\"]\n",
        )
        .unwrap()
    }

    #[test]
    fn uri_is_revalidated_against_the_launch_manifest() {
        let manifest = browser_manifest();
        let accepted = RunOptions {
            open_uri: Some("https://example.test/a --not-an-argument".into()),
            ..Default::default()
        };
        validate_open_uri(&manifest, &accepted).unwrap();

        for uri in [
            "http://example.test",
            "file:///etc/passwd",
            "https://x\n--flag",
        ] {
            let opts = RunOptions {
                open_uri: Some(uri.into()),
                ..Default::default()
            };
            assert!(
                validate_open_uri(&manifest, &opts).is_err(),
                "accepted {uri:?}"
            );
        }
    }
}

/// Resolve the capability plan: read the compositor's file, evaluate it
/// for this app now. Split out so [`run_with_captured`] and tests share it.
pub fn resolve_capabilities(manifest: &MinimalManifest, opts: &RunOptions) -> CapabilityPlan {
    let app_id = opts
        .app_id
        .clone()
        .unwrap_or_else(|| manifest.package_id.clone());
    let records = opts
        .capabilities_path
        .as_deref()
        .map(load_capability_file)
        .unwrap_or_default();
    let mut plan = CapabilityPlan::for_app(&app_id, &records, Some(now_unix()));
    // A workspace launch with no explicit author grant is a viewer. Treating
    // missing/malformed policy as authorship made every failure permissive.
    if opts.workspace_id.is_some()
        && plan.participation == crate::capabilities::Participation::Unscoped
    {
        plan.participation = crate::capabilities::Participation::ReadOnly;
    }
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

/// Priority: explicit override > manifest declaration (per-user, then
/// system) > no bind. `Ok(None)` (no declaration) is distinct from `Err`.
///
/// # Errors
///
/// Returns [`RunError`] if the manifest's runtime reference is malformed, if
/// the resolved bundle fails signature/publisher verification, or if no
/// bundle is found and `opts.allow_missing_runtime` is unset.
pub fn resolve_runtime_test_helper(
    manifest: &crate::manifest::MinimalManifest,
    opts: &RunOptions,
) -> Result<Option<(PathBuf, String)>, RunError> {
    resolve_runtime(manifest, opts)
}

/// Resolve/create/migrate the one directory this launch may write to,
/// keyed by `opts.workspace_id` — no list to walk.
fn resolve_app_dir(manifest: &MinimalManifest, opts: &RunOptions) -> Result<PathBuf, RunError> {
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

/// Priority: explicit override (no existence check) >
/// the active identity selected in the per-user identity store > `None`.
pub fn resolve_identity_host_path(opts: &RunOptions) -> Option<PathBuf> {
    if let Some(p) = &opts.identity_host_path {
        return Some(p.clone());
    }
    let dir = runtime_identity::identity_dir_from_env().ok()?;
    runtime_identity::resolve_active_identity(&dir)
}

fn start_identity_broker(
    path: &Path,
) -> Result<(UnixStream, UnixStream, std::thread::JoinHandle<()>), RunError> {
    // Standard store records are migrated before loading. Explicit debug
    // paths outside the store still stay host-side, but are not rewritten in
    // an unexpected directory.
    if let Ok(store) = runtime_identity::identity_dir_from_env()
        && path.parent() == Some(store.as_path())
        && let Some(id) = path.file_stem().and_then(|value| value.to_str())
        && path.extension().and_then(|value| value.to_str()) == Some("json")
        && path == store.join(format!("{id}.json"))
    {
        runtime_identity::secure_identity(&store, id)
            .map_err(|error| RunError::IdentityBroker(error.to_string()))?;
    }
    let identity = runtime_identity::load_from_path(path.to_path_buf())
        .map_err(|error| RunError::IdentityBroker(error.to_string()))?;
    let (host, app) =
        UnixStream::pair().map_err(|error| RunError::IdentityBroker(error.to_string()))?;
    let control = host
        .try_clone()
        .map_err(|error| RunError::IdentityBroker(error.to_string()))?;
    let thread = std::thread::Builder::new()
        .name("qos-identity-broker".into())
        .spawn(move || {
            if let Err(error) = runtime_identity::broker::serve(host, identity) {
                tracing::debug!(%error, "identity broker connection ended");
            }
        })
        .map_err(|error| RunError::IdentityBroker(error.to_string()))?;
    Ok((app, control, thread))
}

fn stop_identity_broker(control: Option<UnixStream>, thread: Option<std::thread::JoinHandle<()>>) {
    if let Some(control) = control {
        let _ = control.shutdown(std::net::Shutdown::Both);
    }
    if let Some(thread) = thread {
        let _ = thread.join();
    }
}

/// Derive GPU pinning, or `None` when nothing to pin. Every decline is
/// logged — landing on the wrong GPU must never happen silently.
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
        egl = selection.egl_vendor_json.as_ref().map_or_else(|| "<loader default>".into(), |p| p.display().to_string()),
        vulkan = selection.vulkan_icd_json.as_ref().map_or_else(|| "<loader default>".into(), |p| p.display().to_string()),
        "pinning the app to the compositor's render node",
    );
    Some(selection)
}

/// Priority: explicit override > manifest declaration (per-user, then
/// system) > no bind. `Ok(None)` (no declaration) is distinct from `Err`.
///
/// # Errors
///
/// Returns [`RunError`] if the manifest's runtime reference is malformed, if
/// the resolved bundle fails signature/publisher verification, or if no
/// bundle is found and `opts.allow_missing_runtime` is unset.
pub fn resolve_runtime(
    manifest: &crate::manifest::MinimalManifest,
    opts: &RunOptions,
) -> Result<Option<(PathBuf, String)>, RunError> {
    // 1. Explicit override wins, verified against the same signature policy
    //    as the manifest path.
    if let Some(explicit) = &opts.runtime_root {
        let version = manifest
            .runtime_ref()
            .map_or_else(|| "1".to_string(), |(_, v)| v.to_string());
        enforce_bundle_policy(explicit, opts, false)?;
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
            enforce_bundle_policy(cand, opts, cand == &system)?;
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

/// Verify the bundle at `path` against the signature policy. Unless
/// `allow_unsigned_runtime`, it must be signed. Publisher policy:
///   1. `opts.require_runtime_publisher`, if set, must match.
///   2. Else the system trust-list file, if present and non-empty
///      (`/etc/qos/runtime-bundle-trusted-publishers.txt`, or
///      `QOS_LAUNCHER_TRUSTED_PUBLISHERS_FILE`).
///   3. Else any signed publisher is accepted.
///
/// Layering lets the OS image bake and pin a fresh key at build time.
fn enforce_bundle_policy(
    path: &Path,
    opts: &RunOptions,
    system_bundle: bool,
) -> Result<(), RunError> {
    let require_publisher = match opts.require_runtime_publisher.clone() {
        Some(explicit) => Some(explicit),
        None => trusted_publisher_pin_from_file(),
    };
    let verify_opts = runtime_bundle::VerifyOptions {
        require_publisher,
        require_signature: !opts.allow_unsigned_runtime,
        require_arch: None,
    };
    let verified = if system_bundle {
        runtime_bundle::verify_bundle_metadata_with(path, &verify_opts).and_then(|manifest| {
            if system_bundle_tree_is_root_owned(path, &manifest) {
                Ok(manifest)
            } else {
                Err(runtime_bundle::BundleError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "system runtime is not an immutable root-owned tree",
                )))
            }
        })
    } else {
        runtime_bundle::verify_bundle_with(path, &verify_opts)
    };
    match verified {
        Ok(manifest) => {
            // Multi-publisher trust list: `require_publisher` above was `None`
            // (only one key can be pinned there), so check by hand here.
            if let Some(list) = load_trusted_publisher_list()
                && !list.is_empty()
            {
                let ok = manifest
                    .publisher_hex
                    .as_deref()
                    .is_some_and(|pk| list.iter().any(|t| t.eq_ignore_ascii_case(pk)));
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
            Ok(())
        }
        Err(runtime_bundle::BundleError::NotSigned) => Err(RunError::RuntimeBundleUnsigned {
            path: path.to_path_buf(),
        }),
        Err(e) => Err(RunError::RuntimeBundleSignature {
            path: path.to_path_buf(),
            reason: e.to_string(),
        }),
    }
}

#[cfg(unix)]
fn system_bundle_tree_is_root_owned(
    root: &Path,
    manifest: &runtime_bundle::BundleManifest,
) -> bool {
    fn secure(path: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            return false;
        };
        !metadata.file_type().is_symlink() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0
    }

    // Protect against replacing the otherwise-secure version directory through
    // a writable ancestor, then check every signed file and its subdirectories.
    let mut ancestor = Some(root);
    while let Some(path) = ancestor {
        if !secure(path) {
            return false;
        }
        ancestor = path.parent();
    }
    if !secure(&root.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME)) {
        return false;
    }
    for file in &manifest.files {
        let path = root.join(&file.path);
        if !secure(&path) {
            return false;
        }
        let mut parent = path.parent();
        while let Some(dir) = parent.filter(|dir| dir.starts_with(root) && *dir != root) {
            if !secure(dir) {
                return false;
            }
            parent = dir.parent();
        }
    }
    true
}

#[cfg(not(unix))]
fn system_bundle_tree_is_root_owned(
    _root: &Path,
    _manifest: &runtime_bundle::BundleManifest,
) -> bool {
    false
}

/// A single-entry trust list is the pin handed to
/// `verify_bundle_with`; multi-entry lists are checked after it returns.
fn trusted_publisher_pin_from_file() -> Option<String> {
    let list = load_trusted_publisher_list()?;
    if list.len() == 1 {
        list.into_iter().next()
    } else {
        None
    }
}

/// Read the trusted-publisher list from disk, ignoring blank/`#` lines.
/// `None` when neither the env override nor the default file exists.
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
        .map(std::string::ToString::to_string)
        .collect();
    Some(list)
}

/// Top-level entry: reads the manifest, builds the sandbox, execs.
/// Does not call `process::exit` — that's `main.rs`'s job.
///
/// # Errors
///
/// Returns [`RunError`] if `manifest_path` can't be read or parsed, or if
/// [`run_with`] fails resolving the runtime/sandbox or execing the payload.
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
#[derive(Debug, Clone, Default)]
pub enum LaunchMode {
    /// The default: GUI-flavour bin, used implicitly by `run`/`run_with`.
    #[default]
    Gui,
    /// CLI-flavour bin. `passthrough_argv` follows `--` on the shim command
    /// line, forwarded verbatim; `command` names the manifest entry resolved.
    Cli {
        command: String,
        passthrough_argv: Vec<String>,
    },
    /// An application qOS did not package (a host `.desktop` entry, Flatpak
    /// or otherwise).
    ///
    /// **This mode never reaches the sandbox** — `run_with` refuses it with
    /// [`RunError::HostAppNotAPackage`]; [`crate::host`] spawns the entry's
    /// command line directly, with a controlled environment and no bubblewrap.
    ///
    /// Deliberate: the baseline seccomp filter denies the `unshare`/`mount`/
    /// `pivot_root` a Flatpak needs, and a host binary has no manifest to
    /// sandbox from — so this is a hard error here, not a mystery failure later.
    Host {
        /// Desktop-file id of the entry, e.g. `com.spotify.Client`.
        desktop_id: String,
    },
}

/// Env override that turns the egress gate off, downgrading a host-scoped
/// grant to unrestricted access. Explicit only — never a silent default.
pub const NET_GATE_ENV: &str = "QOS_LAUNCHER_NET_GATE";

/// Start the egress gate if this launch needs one. Returns the bwrap bind
/// description and the guard keeping the gate alive; both `None` when
/// there's nothing to filter.
///
/// ## Degradation
///
/// Needs no privilege and no kernel feature beyond what `bwrap` already
/// requires — unlike `nftables`-in-a-netns, which needs `CAP_NET_ADMIN`
/// for a `veth` pair or a userspace TCP/IP stack. The pieces here are
/// ordinary: the unshared netns every app gets from `--unshare-all`, one
/// bind-mounted `AF_UNIX` socket (same primitive as Wayland/D-Bus), and a
/// process binding an ordinary socket under `$XDG_RUNTIME_DIR`. So "kernel
/// doesn't support this" can't arise, and "we lack privilege" can't arise
/// either — no step is privileged. What can fail is binding the socket:
/// a missing, full or unwritable runtime directory.
///
/// **On that failure we refuse the launch** rather than fall back to
/// `--share-net`, which would silently turn "this host" into "every
/// host" — the false assurance this code exists to remove. Cost is
/// bounded: only a *narrowing* grant needs the gate. `QOS_LAUNCHER_NET_GATE=off`
/// takes the unfiltered path deliberately, logged, so it's on the record.
///
/// Landlock is the one kernel-dependent piece and is deliberately *not*
/// held to this standard — defence in depth behind the gate, not the
/// boundary; without ABI 4 the port pin does nothing and the empty
/// namespace + gate still hold.
fn start_egress_gate(
    egress: &crate::netgate::EgressDecision,
) -> Result<
    (
        Option<crate::sandbox::NetGateBind>,
        Option<crate::netgate::NetGate>,
    ),
    RunError,
> {
    // `Filtered` is the only decision needing a gate; `QOS_LAUNCHER_NET_GATE=off`
    // is handled in `crate::sandbox::egress_decision` so turning it off also
    // shares the network rather than sealing the app in an empty namespace.
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

/// Put every opened service socket on the fd the app expects, in the
/// child, before `exec`. bwrap forwards fds it wasn't asked about
/// untouched, so a placement here survives namespace setup.
///
/// Not bwrap's `--file FD DEST`: that copies the descriptor's *contents*
/// into a regular file, so a connected socket blocks reading for an EOF
/// that never comes — the launch hangs before `exec` with no symptom.
///
/// Two passes: target numbers start at 17 and a source can already sit on
/// an unwritten target, so every source is relocated above the target
/// window first, so no `dup2` clobbers one still waiting to be placed.
///
/// Errors rather than continuing quietly: a failure here is a bug, not
/// an absent service, and collaboration silently starting off is exactly
/// the outcome worth refusing.
///
/// # Safety
/// Call only from `Command::pre_exec` (post-fork, pre-exec). Uses only
/// `fcntl`, `dup2` and `close`, all async-signal-safe; allocates nothing.
unsafe fn place_service_fds(pairs: &mut [(RawFd, RawFd)]) -> std::io::Result<()> {
    let Some(highest) = pairs.iter().map(|(_, dst)| *dst).max() else {
        return Ok(());
    };
    let floor = highest.checked_add(1).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "service fd relocation floor overflowed RawFd",
        )
    })?;
    for (src, _) in pairs.iter_mut() {
        // F_DUPFD returns the lowest free fd >= floor, i.e. clear of
        // every target.
        let moved = unsafe { libc::fcntl(*src, libc::F_DUPFD, floor) };
        if moved >= 0 {
            // The original number is done with — a second live descriptor at an
            // unnamed number is one the app cannot close.
            unsafe { libc::close(*src) };
            *src = moved;
        }
    }
    for (src, dst) in pairs.iter() {
        if unsafe { libc::dup2(*src, *dst) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
        // dup2 leaves the target without FD_CLOEXEC — it must survive bwrap's and
        // the app's exec. Cleared again explicitly since relocation above is
        // best-effort.
        unsafe {
            let flags = libc::fcntl(*dst, libc::F_GETFD);
            if flags != -1 {
                let _ = libc::fcntl(*dst, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
        }
    }
    // Close relocated copies now that every target holds one, so the app gets
    // exactly one descriptor per service. Skip numbers that are themselves a
    // target — closing one could shut a descriptor `dup2` just placed there.
    for (src, dst) in pairs.iter() {
        if *src != *dst && !pairs.iter().any(|(_, other)| other == src) {
            unsafe { libc::close(*src) };
        }
    }
    Ok(())
}

/// Pop the trailing entrypoint token that `build_bwrap_argv` /
/// `build_bwrap_argv_for_entry` always leave at the end of `argv`, so a
/// splice function can insert its own flags before it and push it back.
///
/// # Errors
///
/// Returns [`RunError::ArgvInvariant`] if `argv` is empty. This should never
/// happen on any real launch path; failing here beats silently continuing
/// with an argv that is missing whichever sandboxing directive the caller
/// was about to splice in.
fn pop_entry(argv: &mut Vec<String>) -> Result<String, RunError> {
    argv.pop().ok_or(RunError::ArgvInvariant)
}

/// Splice in the `QOS_CLI_COMMAND` setenv before the trailing entrypoint
/// token so bwrap still parses it as a flag.
fn splice_cli_setenv(
    argv: &mut Vec<String>,
    extra_setenv: Option<(String, String)>,
) -> Result<(), RunError> {
    if let Some((k, v)) = extra_setenv {
        let entry = pop_entry(argv)?;
        argv.push("--setenv".to_string());
        argv.push(k);
        argv.push(v);
        argv.push(entry);
    }
    Ok(())
}

/// Narrow dynamic-loader binds only (not all of `/usr`), for a plain
/// dynamically-linked `execve()`. Skipped when a runtime bundle (which already
/// carries the loader) is mounted. Returns the extra ro paths bound, needed
/// again by the shim's Landlock plan.
fn splice_minimal_libs(
    argv: &mut Vec<String>,
    opts: &RunOptions,
    resolved_runtime_present: bool,
) -> Result<Vec<String>, RunError> {
    let mut extra_ro_paths: Vec<String> = Vec::new();
    let use_minimal_libs = opts.minimal_libs && !resolved_runtime_present;
    if opts.minimal_libs && resolved_runtime_present {
        tracing::info!(
            "runtime bundle mounted; ignoring --minimal-libs (loader + libc come from the bundle)"
        );
    }
    if use_minimal_libs {
        let entry = pop_entry(argv)?;
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
    Ok(extra_ro_paths)
}

/// Workspace id, spliced before the entrypoint token so the child learns
/// which room it was opened into. Absent when the launch has no session.
/// Shared by `run_with` and `run_with_captured`.
fn splice_workspace_id_bind(argv: &mut Vec<String>, opts: &RunOptions) -> Result<(), RunError> {
    if let Some(workspace_id) = opts.workspace_id.as_deref() {
        let entry = pop_entry(argv)?;
        crate::sandbox::push_workspace_id(argv, workspace_id);
        argv.push(entry);
    }
    Ok(())
}

/// A trusted shell launch may attach a small typed context for the target
/// app (for example, a VPN profile selected in Settings). It carries no
/// authority and is deliberately bounded + JSON-object checked before it
/// crosses the clean-environment sandbox boundary.
fn splice_tool_context(argv: &mut Vec<String>) -> Result<(), RunError> {
    if let Some(context) = validated_tool_context_from_env() {
        let entry = pop_entry(argv)?;
        argv.push("--setenv".to_string());
        argv.push("QOS_TOOL_CONTEXT".to_string());
        argv.push(context);
        argv.push(entry);
    }
    Ok(())
}

/// App directory: bound read-write at a fixed sandbox path, HOME/XDG vars
/// pointed inside it. Spliced here (not in `build_bwrap_argv`) since the
/// room is a launch property, not the package's.
fn splice_app_dir(
    argv: &mut Vec<String>,
    app_dir: &Path,
    capabilities: &CapabilityPlan,
) -> Result<(), RunError> {
    let entry = pop_entry(argv)?;
    crate::appdir::push_app_dir(argv, app_dir);
    // After `push_app_dir` (which repoints `HOME`): this says where the
    // *user's* home is, only when a grant actually bound it.
    if let Some(user_home) = crate::sandbox::push_user_home(argv, capabilities) {
        tracing::debug!(
            user_home = %user_home.display(),
            "a granted bind covers the user's home; telling the app where it is"
        );
    }
    // And where the room's shared mounts are, on the same terms: a
    // workspace the app cannot read is not one it should be told about.
    if let Some(roots) = crate::sandbox::push_workspace_roots(argv, capabilities) {
        tracing::debug!(
            workspace_roots = %roots,
            "granted binds cover workspace roots; telling the app where they are"
        );
    }
    argv.push(entry);
    Ok(())
}

/// Mount Files' live room-governance identity directory read-only. The
/// projection is package-scoped and must remain visible across atomic host
/// replacement, so the containing directory—not one inode—is bound.
fn splice_trusted_identities(
    argv: &mut Vec<String>,
    manifest: &MinimalManifest,
    opts: &RunOptions,
) -> Result<(), RunError> {
    if manifest.package_id == "dev.qos.file-explorer"
        && let Some(host_path) = opts.trusted_identities_host_path.as_deref()
    {
        let entry = pop_entry(argv)?;
        crate::sandbox::push_trusted_identities_bind(argv, host_path);
        argv.push(entry);
    }
    Ok(())
}

/// Wayland display, spliced before the entrypoint token like the
/// minimal-libs binds. No `wayland_socket` means no display at all. Returns
/// the resolved bind, needed again by the shim's Landlock plan.
fn splice_wayland(
    argv: &mut Vec<String>,
    opts: &RunOptions,
) -> Result<Option<crate::sandbox::WaylandBind>, RunError> {
    let wayland_bind = opts
        .wayland_socket
        .as_ref()
        .map(|host| crate::sandbox::wayland_bind_for(host, current_uid()));
    if let Some(bind) = &wayland_bind {
        let entry = pop_entry(argv)?;
        crate::sandbox::push_wayland_bind(argv, bind);
        argv.push(entry);
    }
    Ok(wayland_bind)
}

/// Egress gate socket, spliced like Wayland — the app's only route out of an
/// otherwise empty network namespace. Shared by `run_with` and
/// `run_with_captured`.
fn splice_net_gate_bind(
    argv: &mut Vec<String>,
    net_gate: Option<&crate::sandbox::NetGateBind>,
) -> Result<(), RunError> {
    if let Some(bind) = net_gate {
        let entry = pop_entry(argv)?;
        crate::sandbox::push_net_gate(argv, bind);
        argv.push(entry);
    }
    Ok(())
}

/// GPU: the compositor's render node decides EGL/Vulkan visibility, spliced
/// after the manifest's own `/dev/dri` grant to narrow it. Returns the
/// selection, needed again by the shim's Landlock plan.
fn splice_gpu(
    argv: &mut Vec<String>,
    manifest: &MinimalManifest,
    opts: &RunOptions,
    capabilities: &CapabilityPlan,
) -> Result<Option<crate::gpu::GpuSelection>, RunError> {
    let gpu = resolve_gpu_selection(manifest, opts);
    if let Some(sel) = &gpu {
        let entry = pop_entry(argv)?;
        crate::sandbox::push_gpu_selection(
            argv,
            sel,
            crate::sandbox::grants_drm(manifest, capabilities),
        );
        argv.push(entry);
    }
    Ok(gpu)
}

/// Everything [`splice_sandbox_shim`] needs beyond `argv` and `opts`.
struct ShimSpliceCtx<'a> {
    manifest: &'a MinimalManifest,
    net_gate: &'a Option<crate::sandbox::NetGateBind>,
    runtime_arg: Option<(&'a Path, &'a str)>,
    extra_ro_paths: &'a [String],
    egress: &'a crate::netgate::EgressDecision,
    capabilities: &'a CapabilityPlan,
    dbus_plan: &'a Option<DBusBindPlan>,
    wayland_bind: &'a Option<crate::sandbox::WaylandBind>,
    gpu: &'a Option<crate::gpu::GpuSelection>,
}

/// In-sandbox shim: bind this launcher binary in and rewrite the trailing
/// entrypoint token into `<shim> __sandbox-exec [rules…] -- <entry>`. Two
/// jobs justify it alone: apply Landlock after bwrap's mount namespace is
/// up, and start the egress relay before the app's first connect. A no-op
/// when neither Landlock enforcement nor a net gate is requested.
fn splice_sandbox_shim(
    argv: &mut Vec<String>,
    opts: &RunOptions,
    ctx: &ShimSpliceCtx<'_>,
) -> Result<(), RunError> {
    let need_shim = opts.landlock_enforce || ctx.net_gate.is_some();
    if !need_shim {
        return Ok(());
    }
    let entry = pop_entry(argv)?;
    let launcher = std::env::current_exe()
        .map_err(|e| RunError::Io(std::io::Error::other(format!("current_exe: {e}"))))?;
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
    if let Some(bind) = ctx.net_gate {
        argv.push("--net-gate".to_string());
        argv.push(bind.sandbox_socket.display().to_string());
        argv.push("--net-gate-port".to_string());
        argv.push(bind.port.to_string());
    }
    // Sandbox-side ro paths for Landlock: the payload mount, the runtime
    // mount (if any), minimal-libs binds, and the manifest's read paths.
    let payload_mount = crate::sandbox::payload_mount_for(&ctx.manifest.package_id);
    argv.push("--ro".to_string());
    argv.push(payload_mount.display().to_string());
    if let Some((rt_root, ver)) = ctx.runtime_arg {
        let rt = crate::sandbox::runtime_mount_for(ver);
        argv.push("--ro".to_string());
        argv.push(rt.display().to_string());
        // Also allow the canonical loader paths re-bound out of the bundle —
        // otherwise the entry ELF's INTERP is blocked by Landlock despite the bind.
        for canonical in crate::sandbox::CANONICAL_LOADER_PATHS {
            let no_slash = canonical.trim_start_matches('/');
            if rt_root.join("lib").join(no_slash).exists() {
                argv.push("--ro".to_string());
                argv.push((*canonical).to_string());
            }
        }
    }
    for p in ctx.extra_ro_paths {
        argv.push("--ro".to_string());
        argv.push(p.clone());
    }
    // System read profile, derived from the same list the binds came from —
    // an unnamed bind is visible but returns EACCES on every open, which
    // looks like a corrupted system rather than a policy.
    for path in crate::profile::landlock_ro_paths(ctx.egress.shares_host_network()) {
        argv.push("--ro".to_string());
        argv.push(path.display().to_string());
    }
    // Capability-granted paths need matching Landlock rules from the same
    // plan, or the bind is visible but every open() returns EACCES.
    for path in &ctx.capabilities.ro_paths {
        argv.push("--ro".to_string());
        argv.push(path.display().to_string());
    }
    for path in &ctx.capabilities.rw_paths {
        argv.push("--rw".to_string());
        argv.push(path.display().to_string());
    }
    // A bound D-Bus socket also needs a Landlock read grant on the
    // sandbox-side path, or ashpd's connect() surfaces EACCES.
    if let Some(plan) = ctx.dbus_plan {
        argv.push("--ro".to_string());
        argv.push(plan.sandbox_path.display().to_string());
    }
    // Wayland needs read+write in Landlock: connect(2) on a unix socket is
    // write-flavoured, so read-only surfaces as EACCES at `wl_display_connect`.
    if let Some(bind) = ctx.wayland_bind {
        argv.push("--rw".to_string());
        argv.push(bind.sandbox_socket.display().to_string());
    }
    // GPU selection is only real if Landlock agrees: loader manifests need
    // read, the render node needs read+write (opened O_RDWR) — read-only
    // looks like no GPU present.
    if let Some(sel) = ctx.gpu {
        for file in sel.manifest_files() {
            argv.push("--ro".to_string());
            argv.push(file.display().to_string());
        }
        argv.push("--rw".to_string());
        argv.push(sel.node.display().to_string());
    }
    // The app's own directory, named by its in-sandbox path — the host path
    // isn't reachable from inside, so a rule on it would match nothing.
    argv.push("--rw".to_string());
    argv.push(crate::appdir::SANDBOX_APP_DIR.to_string());
    // The GPU device nodes the profile bound — same O_RDWR reasoning
    // as above.
    for path in crate::profile::landlock_rw_device_paths() {
        argv.push("--rw".to_string());
        argv.push(path.display().to_string());
    }
    // Standard tmpfs mounts the app expects writable; --rw grants full FS
    // access, and both vanish with the process (scratch, not hidden state).
    for p in ["/tmp", "/dev"] {
        argv.push("--rw".to_string());
        argv.push(p.to_string());
    }
    // Service sockets need no Landlock rule: they arrive as already-connected
    // descriptors dup2'd in, never mounted or opened by path.
    //
    // The gate socket is reached via connect(2) (write-flavoured), so it
    // needs the same read+write rule as Wayland above.
    if let Some(bind) = ctx.net_gate {
        argv.push("--rw".to_string());
        argv.push(bind.sandbox_socket.display().to_string());
    }
    // Landlock's network restriction mirrors the namespace decision: Blocked
    // denies all TCP on an already-empty netns; Filtered pins the app to the
    // relay port instead.
    if matches!(ctx.egress, crate::netgate::EgressDecision::Blocked) {
        argv.push("--net-restricted".to_string());
    }
    argv.push("--".to_string());
    argv.push(entry);
    Ok(())
}

/// Finish building `argv` (seccomp prefix + passthrough tail), spawn `bwrap`
/// with the seccomp fd and service sockets wired into the child via
/// `pre_exec`, then wait for it. Logs the derived Landlock/egress plan from
/// the parent for observability (the shim enforces Landlock; this only
/// records intent).
#[allow(clippy::too_many_arguments)]
fn spawn_and_wait(
    mut argv: Vec<String>,
    cli_passthrough: Vec<String>,
    opts: &RunOptions,
    seccomp_fd: OwnedFd,
    resolved: Vec<crate::services::ResolvedService>,
    egress: &crate::netgate::EgressDecision,
    net_gate: Option<&crate::sandbox::NetGateBind>,
    identity_broker: (
        Option<UnixStream>,
        Option<UnixStream>,
        Option<std::thread::JoinHandle<()>>,
    ),
) -> Result<ExitStatus, RunError> {
    const SECCOMP_CHILD_FD: i32 = 16;
    let (identity_broker_app, identity_broker_control, identity_broker_thread) = identity_broker;

    // Insert --seccomp <child_fd> at the front so bwrap installs it
    // before any other action (matches the compositor pattern).
    let mut prefix = vec!["--seccomp".to_string(), SECCOMP_CHILD_FD.to_string()];
    prefix.append(&mut argv);
    argv = prefix;

    // Passthrough: everything after the entrypoint token is opaque to bwrap,
    // forwarded as the sandboxed program's argv[1..].
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
    let mut service_fds: Vec<(RawFd, RawFd)> = resolved
        .iter()
        .map(|r| (r.host_fd(), r.child_fd as RawFd))
        .collect();
    if let Some(stream) = &identity_broker_app {
        service_fds.push((
            stream.as_raw_fd(),
            runtime_identity::broker::BROKER_CHILD_FD,
        ));
    }
    for r in &resolved {
        tracing::info!(
            service = %r.name,
            child_fd = r.child_fd,
            "service socket connected; handing the descriptor to the app",
        );
    }
    // Landlock plan: derived here so we can log it from the parent.
    let landlock_plan = crate::landlock::plan_from_paths(
        &crate::profile::landlock_ro_paths(egress.shares_host_network()),
        &[PathBuf::from(crate::appdir::SANDBOX_APP_DIR)],
    );
    let net_restricted = matches!(egress, crate::netgate::EgressDecision::Blocked);
    // The shim enforces Landlock via opts.landlock_enforce (argv splice
    // above); this just logs intent — the shim's own line has the real outcome.
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
        hosts = ?egress.policy().map(super::netgate::EgressPolicy::hosts),
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
            // Landlock is NOT installed here — bwrap needs broad FS access to set up
            // the namespace; the `__sandbox-exec` shim installs it after. See `landlock_shim`.
            Ok(())
        });
    }

    // 5. Spawn + wait.
    let mut child = cmd.spawn().map_err(RunError::Spawn)?;
    // Important: keep `seccomp_fd` alive until after `spawn` returns,
    // so its dup2'd copy in the child remains valid. Drop here.
    drop(seccomp_fd);
    // Same rule: the child holds its own copies, so the launcher's are dead
    // weight, dropped before the wait so the far end learns of the exit.
    drop(resolved);
    drop(identity_broker_app);
    let status = child.wait()?;
    stop_identity_broker(identity_broker_control, identity_broker_thread);
    Ok(status)
}

/// Seccomp baseline → tmpfile → fd. The manifest *asks* for a profile; the
/// image decides whether this package may have it. An unrecognised name is a
/// packaging mistake; a recognised-but-unentitled one is an app trying to
/// lift a protection off itself — logged distinctly.
fn stage_seccomp(manifest: &MinimalManifest) -> Result<OwnedFd, RunError> {
    let decision = crate::seccomp::resolve_profile(&manifest.sandbox.seccomp, &manifest.package_id);
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
    Ok(stage_seccomp_blob(&seccomp_blob)?)
}

/// Resolve the selected identity record on the host, migrate legacy storage,
/// then mint one app-scoped broker connection. Returns `identity_host`
/// alongside the broker handles since `identity_arg` (a presence marker for
/// the argv builder, computed by the caller) borrows from it.
/// Broker handles + `identity_host` returned by [`resolve_identity_broker`]:
/// the app- and control-side ends of the identity broker socketpair, the
/// thread serving the control side, and the host path `identity_arg`
/// borrows from.
type IdentityBrokerResolution = (
    Option<UnixStream>,
    Option<UnixStream>,
    Option<std::thread::JoinHandle<()>>,
    Option<PathBuf>,
);

fn resolve_identity_broker(
    capabilities: &CapabilityPlan,
    opts: &RunOptions,
) -> Result<IdentityBrokerResolution, RunError> {
    let identity_host = resolve_identity_host_path(opts);
    let (app, control, thread) = if capabilities.participation.carries_identity() {
        match identity_host.as_deref() {
            Some(path) => {
                let (app, control, thread) = start_identity_broker(path)?;
                (Some(app), Some(control), Some(thread))
            }
            None => (None, None, None),
        }
    } else {
        (None, None, None)
    };
    Ok((app, control, thread, identity_host))
}

/// What an argv builder produces: the bwrap command line, an optional
/// `(name, value)` environment pair the caller must splice in, and the
/// trailing arguments passed through to the app.
type BuiltArgv = (Vec<String>, Option<(String, String)>, Vec<String>);

/// Pick the entrypoint bin per launch mode: GUI uses `bin`; CLI uses
/// `cli_bin()` (falling back to `bin`) and splices `QOS_CLI_COMMAND` /
/// forwards passthrough argv. Returns `(argv, extra_setenv, passthrough)`.
#[allow(
    clippy::too_many_arguments,
    reason = "every argument is an independently-resolved piece of the sandbox \
              (runtime, service binds, dbus plan, identity, capabilities); bundling \
              them would only move the same list behind a struct built at the one \
              call site"
)]
fn build_initial_argv(
    manifest: &MinimalManifest,
    payload_root: &Path,
    opts: &RunOptions,
    runtime_arg: Option<(&Path, &str)>,
    binds: &[crate::sandbox::ServiceBind],
    dbus_plan: Option<&DBusBindPlan>,
    identity_arg: Option<&Path>,
    capabilities: &CapabilityPlan,
) -> Result<BuiltArgv, RunError> {
    match &opts.mode {
        LaunchMode::Gui => Ok((
            build_bwrap_argv(
                manifest,
                payload_root,
                runtime_arg,
                binds,
                dbus_plan,
                identity_arg,
                capabilities,
            ),
            None,
            // A GUI launch carries at most one argument — the document to open —
            // riding the same tail as CLI passthrough (opaque bytes after entrypoint).
            gui_open_args(opts),
        )),
        LaunchMode::Cli {
            command,
            passthrough_argv,
        } => {
            let entry_bin_owned = manifest
                .entrypoint
                .cli_bin()
                .unwrap_or(&manifest.entrypoint.bin)
                .to_string();
            let v = crate::sandbox::build_bwrap_argv_for_entry(
                manifest,
                payload_root,
                runtime_arg,
                binds,
                dbus_plan,
                identity_arg,
                &entry_bin_owned,
                capabilities,
            );
            Ok((
                v,
                Some(("QOS_CLI_COMMAND".to_string(), command.clone())),
                passthrough_argv.clone(),
            ))
        }
        // Refused at the top of `run_with`, before any of the setup above
        // ran. Reaching here would mean that guard was removed; fail the
        // launch instead of trusting it never will.
        LaunchMode::Host { desktop_id } => Err(RunError::HostAppNotAPackage {
            desktop_id: desktop_id.clone(),
        }),
    }
}

/// Build and exec the sandbox for an already-parsed `manifest`: validates the
/// launch-mode/fs policy, resolves service/identity binds, the runtime
/// bundle, the app's writable directory and its network egress gate, then
/// spawns `bwrap` and waits for it.
///
/// # Errors
///
/// Returns [`RunError`] if the launch is refused by policy (a host app
/// reached the sandbox path, or the manifest names a filesystem path), if
/// any bind/runtime/identity/egress resolution step fails, or if spawning or
/// waiting on the sandboxed process fails.
pub fn run_with(
    manifest: &MinimalManifest,
    payload_root: &Path,
    opts: &RunOptions,
) -> Result<ExitStatus, RunError> {
    validate_open_uri(manifest, opts)?;
    // 0a. A host application has no manifest and no sandbox. Refusing here,
    // before any socket/directory/argv exists, keeps the two launch kinds
    // from quietly converging.
    if let LaunchMode::Host { desktop_id } = &opts.mode {
        return Err(RunError::HostAppNotAPackage {
            desktop_id: desktop_id.clone(),
        });
    }

    // 0. Policy: a manifest naming a filesystem path is refused before
    // anything else exists — silently dropping it would surface as an app
    // bug, not a policy one. Checked at launch too, not just install.
    crate::manifest::enforce_fs_policy(manifest)?;

    // 1. Service sockets.
    let launch_app_id = opts.app_id.as_deref().unwrap_or(&manifest.package_id);
    let mut resolved = resolve_service_binds(&manifest.sandbox.services, opts.no_services)?;
    crate::services::authenticate_shell_ui_bind(&resolved, launch_app_id)?;
    if let Some(identity) = resolve_identity_bind(
        launch_app_id,
        opts.workspace_id.as_deref(),
        opts.no_services,
    )? {
        resolved.push(identity);
    }
    let binds = to_service_binds(&resolved);

    // 2. Seccomp baseline → tmpfile → fd.
    let seccomp_fd: OwnedFd = stage_seccomp(manifest)?;

    // 2b. D-Bus proxy / direct-bind decision: try xdg-dbus-proxy, fall back
    // to a raw-path bind on failure. `_dbus_guard` stays alive until bwrap
    // is waited on so Drop can SIGTERM the proxy.
    let (dbus_plan, _dbus_guard) = resolve_dbus_bind(&manifest.sandbox.portals);

    // 3. Argv. Runtime bundle resolution: a resolved bundle wins over
    // --minimal-libs and skips the loader-bind stopgap entirely.
    let resolved_runtime = resolve_runtime(manifest, opts)?;
    let runtime_arg = resolved_runtime
        .as_ref()
        .map(|(p, v)| (p.as_path(), v.as_str()));

    // The app's one writable directory, for the room it's opened into.
    // Created here since bwrap can only bind what exists.
    let app_dir = resolve_app_dir(manifest, opts)?;

    // The user's capability decision. Empty unless the compositor wrote a
    // file — an app launched without one is confined to its manifest alone.
    let capabilities = resolve_capabilities(manifest, opts);

    // Resolve the selected record on the host, migrate legacy storage, then
    // mint one app-scoped broker connection. The path is only a presence
    // marker for the argv builder; it is never mounted.
    let (identity_broker_app, identity_broker_control, identity_broker_thread, identity_host) =
        resolve_identity_broker(&capabilities, opts)?;
    let identity_arg = identity_broker_app.as_ref().and(identity_host.as_deref());

    // Egress: a grant naming specific hosts runs a gate; `*` or bare `host`
    // mode shares the host network with no allowlist to enforce, so neither
    // needs a gate. Guard bound here (not in the `if let`) so it outlives the child.
    let egress = crate::sandbox::egress_decision(manifest, &capabilities);
    let (net_gate, _gate_guard) = start_egress_gate(&egress)?;

    // Pick the entrypoint bin per launch mode and build the initial argv.
    let (mut argv, extra_setenv, cli_passthrough) = build_initial_argv(
        manifest,
        payload_root,
        opts,
        runtime_arg,
        &binds,
        dbus_plan.as_ref(),
        identity_arg,
        &capabilities,
    )?;
    splice_cli_setenv(&mut argv, extra_setenv)?;
    let extra_ro_paths = splice_minimal_libs(&mut argv, opts, resolved_runtime.is_some())?;
    splice_workspace_id_bind(&mut argv, opts)?;
    splice_tool_context(&mut argv)?;
    splice_app_dir(&mut argv, &app_dir, &capabilities)?;
    splice_trusted_identities(&mut argv, manifest, opts)?;
    let wayland_bind = splice_wayland(&mut argv, opts)?;
    splice_net_gate_bind(&mut argv, net_gate.as_ref())?;
    let gpu = splice_gpu(&mut argv, manifest, opts, &capabilities)?;
    splice_sandbox_shim(
        &mut argv,
        opts,
        &ShimSpliceCtx {
            manifest,
            net_gate: &net_gate,
            runtime_arg,
            extra_ro_paths: &extra_ro_paths,
            egress: &egress,
            capabilities: &capabilities,
            dbus_plan: &dbus_plan,
            wayland_bind: &wayland_bind,
            gpu: &gpu,
        },
    )?;

    spawn_and_wait(
        argv,
        cli_passthrough,
        opts,
        seccomp_fd,
        resolved,
        &egress,
        net_gate.as_ref(),
        (
            identity_broker_app,
            identity_broker_control,
            identity_broker_thread,
        ),
    )
}

fn validated_tool_context_from_env() -> Option<String> {
    let context = std::env::var("QOS_TOOL_CONTEXT").ok()?;
    if context.len() > 4 * 1024 {
        tracing::warn!("dropping oversized QOS_TOOL_CONTEXT");
        return None;
    }
    if let Ok(serde_json::Value::Object(_)) = serde_json::from_str::<serde_json::Value>(&context) {
        Some(context)
    } else {
        tracing::warn!("dropping malformed QOS_TOOL_CONTEXT");
        None
    }
}

/// `run_with` variant that captures the child's stdout. Returns the
/// exit status alongside the captured bytes. Used by tests.
/// Entrypoint + trailing passthrough args for the lighter `run_with_captured`
/// path — resolved the same way `run_with` does, so both entries agree on
/// binary and arguments.
fn resolve_entry_and_passthrough(
    manifest: &MinimalManifest,
    opts: &RunOptions,
) -> Result<(String, Vec<String>), RunError> {
    match &opts.mode {
        LaunchMode::Gui => Ok((
            manifest
                .entrypoint
                .gui_bin()
                .unwrap_or(&manifest.entrypoint.bin)
                .to_string(),
            gui_open_args(opts),
        )),
        LaunchMode::Cli {
            passthrough_argv, ..
        } => Ok((
            manifest
                .entrypoint
                .cli_bin()
                .unwrap_or(&manifest.entrypoint.bin)
                .to_string(),
            passthrough_argv.clone(),
        )),
        // Refused at the top of `run_with_captured`; fail the launch instead
        // of trusting that guard never moves or disappears.
        LaunchMode::Host { desktop_id } => Err(RunError::HostAppNotAPackage {
            desktop_id: desktop_id.clone(),
        }),
    }
}

/// Finish building `argv` (seccomp prefix + passthrough tail), spawn `bwrap`
/// with its stdout piped, capture it, and wait. The `run_with_captured`
/// counterpart of [`spawn_and_wait`] — no Landlock shim on this path, so
/// none of that wiring.
fn spawn_and_capture(
    mut argv: Vec<String>,
    passthrough: Vec<String>,
    opts: &RunOptions,
    seccomp_fd: OwnedFd,
    resolved: Vec<crate::services::ResolvedService>,
    identity_broker: (
        Option<UnixStream>,
        Option<UnixStream>,
        Option<std::thread::JoinHandle<()>>,
    ),
) -> Result<(ExitStatus, Vec<u8>), RunError> {
    const SECCOMP_CHILD_FD: i32 = 16;
    let (identity_broker_app, identity_broker_control, identity_broker_thread) = identity_broker;

    let mut prefix = vec!["--seccomp".to_string(), SECCOMP_CHILD_FD.to_string()];
    prefix.append(&mut argv);
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
    let mut service_fds: Vec<(RawFd, RawFd)> = resolved
        .iter()
        .map(|r| (r.host_fd(), r.child_fd as RawFd))
        .collect();
    if let Some(stream) = &identity_broker_app {
        service_fds.push((
            stream.as_raw_fd(),
            runtime_identity::broker::BROKER_CHILD_FD,
        ));
    }
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
    // As in `run_with`: the child has its own copies, so holding these open
    // only keeps the service from seeing the app end.
    drop(resolved);
    drop(identity_broker_app);
    let mut buf = Vec::new();
    if let Some(mut so) = child.stdout.take() {
        use std::io::Read;
        so.read_to_end(&mut buf)?;
    }
    let status = child.wait()?;
    stop_identity_broker(identity_broker_control, identity_broker_thread);
    Ok((status, buf))
}

/// Lighter sibling of [`run_with`] that skips Landlock + minimal-libs and
/// captures the child's stdout instead of inheriting it, for callers that
/// need the sandboxed process's output rather than a live session.
///
/// # Errors
///
/// Returns [`RunError`] under the same conditions as [`run_with`]: launch
/// refused by policy, a bind/runtime/identity/egress resolution failure, or
/// a failure spawning or waiting on the sandboxed process.
///
/// # Panics
///
/// Panics if [`crate::sandbox::build_bwrap_argv_for_entry`] ever returns an
/// empty argv — it always appends the entrypoint binary as the last
/// element, so this would indicate a bug in that builder, not a runtime
/// condition.
pub fn run_with_captured(
    manifest: &MinimalManifest,
    payload_root: &Path,
    opts: &RunOptions,
) -> Result<(ExitStatus, Vec<u8>), RunError> {
    validate_open_uri(manifest, opts)?;
    // Same refusals as `run_with`: this is a second entry into the sandbox
    // builder, and a policy with one enforced door is a door beside it.
    if let LaunchMode::Host { desktop_id } = &opts.mode {
        return Err(RunError::HostAppNotAPackage {
            desktop_id: desktop_id.clone(),
        });
    }
    crate::manifest::enforce_fs_policy(manifest)?;

    // Service sockets.
    let launch_app_id = opts.app_id.as_deref().unwrap_or(&manifest.package_id);
    let mut resolved = resolve_service_binds(&manifest.sandbox.services, opts.no_services)?;
    crate::services::authenticate_shell_ui_bind(&resolved, launch_app_id)?;
    if let Some(identity) = resolve_identity_bind(
        launch_app_id,
        opts.workspace_id.as_deref(),
        opts.no_services,
    )? {
        resolved.push(identity);
    }
    let binds = to_service_binds(&resolved);

    // Seccomp baseline resolved identically to `run_with` — two paths
    // reaching different conclusions would make capture a way around the filter.
    let seccomp_blob = compile_baseline(manifest.sandbox.seccomp_profile(&manifest.package_id))?;
    let seccomp_fd: OwnedFd = stage_seccomp_blob(&seccomp_blob)?;

    // Argv. run_with_captured is a lighter sibling that skips Landlock +
    // minimal-libs, but still honours a manifest-declared runtime bundle.
    let resolved_runtime = resolve_runtime(manifest, opts)?;
    let runtime_arg = resolved_runtime
        .as_ref()
        .map(|(p, v)| (p.as_path(), v.as_str()));
    // D-Bus plan (same decision path as `run_with`).
    let (dbus_plan, _dbus_guard) = resolve_dbus_bind(&manifest.sandbox.portals);
    let capabilities = resolve_capabilities(manifest, opts);
    let (identity_broker_app, identity_broker_control, identity_broker_thread, identity_host) =
        resolve_identity_broker(&capabilities, opts)?;
    let identity_arg = identity_broker_app.as_ref().and(identity_host.as_deref());
    // Same egress decision as `run_with`: without the gate spliced in here
    // too, a host-scoped grant would be silently offline. No shim on this
    // path, so no loopback relay — ALL_PROXY has nothing listening behind it.
    let egress = crate::sandbox::egress_decision(manifest, &capabilities);
    let (net_gate, _gate_guard) = start_egress_gate(&egress)?;
    // Entrypoint + trailing arguments, resolved the same way `run_with`
    // does — both entries must agree on binary and arguments.
    let (entry_bin, passthrough) = resolve_entry_and_passthrough(manifest, opts)?;
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
    splice_net_gate_bind(&mut argv, net_gate.as_ref())?;
    splice_workspace_id_bind(&mut argv, opts)?;
    {
        let app_dir = resolve_app_dir(manifest, opts)?;
        let entry = pop_entry(&mut argv)?;
        crate::appdir::push_app_dir(&mut argv, &app_dir);
        argv.push(entry);
    }
    splice_trusted_identities(&mut argv, manifest, opts)?;

    spawn_and_capture(
        argv,
        passthrough,
        opts,
        seccomp_fd,
        resolved,
        (
            identity_broker_app,
            identity_broker_control,
            identity_broker_thread,
        ),
    )
}

/// Give `bwrap` itself an environment worth inheriting, so a dropped
/// `--clearenv` cannot silently become a leak.
///
/// The argv already carries `--clearenv`
/// ([`crate::sandbox::push_clean_environment`]), deciding what the *app*
/// sees. This is the same statement made where the process is spawned:
/// bwrap is handed nothing to pass on, so a lost flag fails closed with
/// an empty environment instead of handing over the compositor's.
///
/// `PATH` survives because spawn needs it: `bwrap_bin` defaults to the
/// bare name `bwrap`, resolved against the child's own
/// [`crate::sandbox::SANDBOX_PATH`], not the session's.
fn clear_bwrap_environment(cmd: &mut Command) {
    cmd.env_clear();
    cmd.env("PATH", crate::sandbox::SANDBOX_PATH);
}

/// Write the seccomp blob to a `memfd` so its fd outlives the parent's
/// tempdir teardown and the bwrap exec; falls back to a real tempfile.
fn stage_seccomp_blob(bytes: &[u8]) -> std::io::Result<OwnedFd> {
    // Try memfd first.
    let name = b"qos-launcher-seccomp\0";
    let raw_fd = unsafe {
        libc::syscall(
            libc::SYS_memfd_create,
            name.as_ptr().cast::<libc::c_char>(),
            0u32,
        )
    } as RawFd;
    if raw_fd >= 0 {
        // memfd_create returns a regular fd; safe to wrap.
        let owned = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(raw_fd) };
        write_all_pwrite(raw_fd, bytes)?;
        return Ok(owned);
    }

    // Fallback: O_TMPFILE under /tmp. If even that fails we surface the IO
    // error — the launcher cannot safely continue without a seccomp blob fd.
    // "/tmp" is a fixed literal with no interior NUL, so this cannot fail in
    // practice; still routed through `?` rather than asserted.
    let path = std::ffi::CString::new("/tmp")
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
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
        // `written < bytes.len()` holds here, so this cannot run past the
        // buffer in practice, but a real file offset is not the place to
        // trust that silently: an error beats a payload written to the
        // wrong offset. `remaining` is derived from the same slice `pwrite`
        // is handed, rather than re-deriving its length separately.
        let Some(remaining_bytes) = bytes.get(written..) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "pwrite: write cursor ran past the buffer length",
            ));
        };
        let remaining = remaining_bytes.len();
        let offset = libc::off_t::try_from(written).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "pwrite: write offset overflowed off_t",
            )
        })?;
        let n = unsafe {
            libc::pwrite(
                fd,
                remaining_bytes.as_ptr().cast::<libc::c_void>(),
                remaining,
                offset,
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
        let n = usize::try_from(n).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "pwrite returned a negative byte count",
            )
        })?;
        written = written.checked_add(n).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "pwrite: written byte count overflowed the buffer length",
            )
        })?;
    }
    Ok(())
}

/// Narrow host paths a dynamically-linked ELF needs to `execve()`.
/// Excludes `/usr/bin` etc. — stopgap until every app carries a qos-runtime bundle.
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
        // Dynamic linker's cache. Without it the loader falls back to a slower
        // search but still works — bound because launch-latency matters.
        "/etc/ld.so.cache",
    ]
}

/// Path inside the bwrap sandbox where the launcher binary is bind-mounted
/// to be exec'd as the Landlock shim. Kept out of `/usr` and `/tmp`.
pub const SHIM_MOUNT_PATH: &str = "/qos-launcher";

/// Decide how to expose D-Bus per the manifest's portal declaration.
/// Preferred: spawn `xdg-dbus-proxy` filtered to portals/notifications/MPRIS.
/// Fallback (proxy missing/failed, logged): bind-mount the raw session
/// bus path — abstract addresses can't be bind-mounted, so those hosts
/// get `(None, None)` and need the proxy.
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
    // sockets degrade to "portal unavailable".
    if let Some(path) = crate::sandbox::parse_unix_path_bus_address(&bus_addr) {
        let path_buf = PathBuf::from(&path);
        let plan = DBusBindPlan {
            host_socket: path_buf.clone(),
            sandbox_path: path_buf,
            kind: DBusBindKind::Direct,
        };
        (Some(plan), None)
    } else {
        tracing::warn!(
            bus_addr = %bus_addr,
            "cannot bind D-Bus into sandbox: address is not `unix:path=` and no proxy is available"
        );
        (None, None)
    }
}

/// Current process UID via libc, avoiding a `users`/`nix` dependency.
/// Falls back to 1000 if the syscall somehow fails.
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

    #[cfg(unix)]
    #[test]
    fn explicit_identity_outside_the_managed_store_is_never_rewritten() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.path().join("custom.json");
        let secret = [31u8; 32];
        let key = ed25519_dalek::SigningKey::from_bytes(&secret);
        let original = serde_json::to_vec(&serde_json::json!({
            "display_name": "Explicit debug identity",
            "avatar_color": [1, 2, 3],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": key.verifying_key().to_bytes(),
                "threshold": 1,
                "total_shares": 1,
                "share_index": 1,
                "secret_bytes": secret,
            },
            "created_at": "2026-08-30T00:00:00Z",
            "schema_version": 1,
        }))
        .unwrap();
        std::fs::write(&path, &original).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let (app, control, thread) = start_identity_broker(&path).unwrap();
        drop(app);
        drop(control);
        thread.join().unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o755,
            "an explicit path must never chmod its parent"
        );
        assert!(!dir.path().join("secrets").exists());
        assert!(!dir.path().join(".identity.lock").exists());
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn xdg_wins_when_file_exists_there() {
        let saved = snapshot_and_clear();
        let tmp = tempfile::tempdir().unwrap();
        let ident = tmp.path().join("qos").join("identity").join("primary.json");
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

    #[test]
    fn workspace_launch_without_an_author_grant_is_read_only() {
        let m = manifest("dev.qos.fixture");
        let workspace = resolve_capabilities(&m, &opts_in(Some("room-a")));
        assert_eq!(
            workspace.participation,
            crate::capabilities::Participation::ReadOnly
        );

        let personal = resolve_capabilities(&m, &opts_in(None));
        assert_eq!(
            personal.participation,
            crate::capabilities::Participation::Unscoped
        );
    }

    /// The launch path resolves the directory rather than the manifest
    /// naming it — the room launched into decides which one.
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

    /// A package cannot make the launcher create a directory it named: the
    /// retired `sandbox.fs.write` block is refused before the directory step.
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
