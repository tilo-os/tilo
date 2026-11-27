//! `run` turns a manifest into a running app: the sandbox argv, the `bwrap` exec and
//! the app's exit code.

use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::time::{Duration, Instant};

use tilo_clock::unix_now;

use crate::capabilities::{CapabilityPlan, load_capability_file};
use crate::dbus_proxy::{self, DBusProxyGuard};
use crate::manifest::{ManifestView, PortalPolicy, read_manifest};
use crate::sandbox::{DBusBindPlan, build_bwrap_argv};
use crate::seccomp::compile_app_baseline;
use crate::services::{
    ACCOUNT_CHILD_FD, EXIT_REQUIRED_SERVICE_MISSING, ServiceError, ServiceLaunchContext,
    resolve_blob_bind, resolve_chooser_bind, resolve_clipboard_bind, resolve_notify_bind,
    resolve_service_binds_for_launch, resolve_shell_ui_bind, to_service_binds,
};
use entrypoint_runtime::preflight_entrypoint_runtime;
use seccomp_fds::SeccompFds;

mod entrypoint_runtime;
mod seccomp_fds;
mod verified_installation;
pub(crate) use verified_installation::{verified_launch_installation, verified_launch_publisher};
pub mod tilolang_process;
pub mod tilolang_window;

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error(transparent)]
    Manifest(#[from] crate::manifest::ManifestReadError),
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
         user data directory to put this app's per-workspace state in."
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
         Install it with `tilo-install runtime install --version <v> --from-host`, \
         or pass --allow-missing-runtime to fall back to --minimal-libs / no runtime binds."
    )]
    RuntimeBundleMissing {
        runtime: String,
        paths: Vec<PathBuf>,
    },
    #[error(
        "`{package}` was built for a different Tilo runtime than the one installed, so it cannot \
         start: it needs the library `{library}`, which the installed runtime `{runtime}` does \
         not provide (looked in {searched}). Update it with `app update {package}` or reinstall \
         it with `app install {package}`, which also fetches the runtime it needs, or remove it \
         with `app remove {package}` to go back to the system build.",
        searched = display_paths(.searched)
    )]
    RuntimeLibraryMissing {
        package: String,
        runtime: String,
        library: String,
        searched: Vec<PathBuf>,
    },
    #[error("manifest declares runtime `{0}` but the value is not in `<name>/<version>` form")]
    RuntimeReferenceMalformed(String),
    #[error(
        "`{package}` needs the runtime `{runtime}`, which is not installed (looked in \
         {searched}). Installing or updating the app brings it; reinstall it with \
         `app install {package}`.",
        searched = display_paths(.searched)
    )]
    RequiredRuntimeMissing {
        package: String,
        runtime: String,
        searched: Vec<PathBuf>,
    },
    #[error(
        "`{package}` changed while it was opening: an update replaced the build at {path}. \
         Open it again to start the new one."
    )]
    PackageChanged { package: String, path: PathBuf },
    #[error(
        "could not hold {path} for this launch, so an update could remove it under the app: {source}"
    )]
    LaunchLease {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
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
        "the tilolang runtime `{runtime}` at {path} carries no standard library ({file}), which \
         every tilolang app on it runs beside: bake the runtime again with \
         `tilolang_bake_runtime`, or install it again with an app that names it"
    )]
    RuntimeStdMissing {
        runtime: String,
        path: PathBuf,
        file: &'static str,
    },
    #[error(
        "`{desktop_id}` is a host application, not a Tilo package: it has no manifest, so \
         there is no sandbox to build for it. Launch it with `tilo-launcher machine-app \
         --desktop-id {desktop_id}`, which runs it unconfined and says so."
    )]
    HostAppNotAPackage { desktop_id: String },
    #[error(
        "refusing to launch: this app was granted access to {hosts:?} and that can only be \
         enforced by brokering its traffic, but the egress proxy could not start ({source}). \
         Launching anyway would mean giving it unrestricted outbound access, which is not \
         what was granted. Set TILO_LAUNCHER_EGRESS_PROXY=off to do that deliberately."
    )]
    EgressProxy {
        hosts: Vec<String>,
        #[source]
        source: crate::egress_proxy::ProxyError,
    },
    #[error("refusing URI open: {0}")]
    InvalidOpenUri(String),
    #[error(
        "refusing to launch {package}: it needs system interfaces this Tilo does not provide: \
         {unmet}. It was built for another version of Tilo. `app remove {package}` goes back to \
         the copy the system ships, if it ships one."
    )]
    InterfacesUnmet { package: String, unmet: String },
    #[error("refusing managed-service launch: {0}")]
    InvalidServiceLaunch(String),
    #[error(
        "refusing to launch `{package}` as `{app_id}`: an app has exactly one id, the package id \
         of its signed manifest"
    )]
    AppIdMismatch { app_id: String, package: String },
    #[error(
        "internal error: the sandbox argv lost its trailing entrypoint token before a splice \
         step could run. This should never happen — refusing to launch rather than risk a \
         command line silently missing one of its sandboxing directives."
    )]
    ArgvInvariant,
    #[error("captured worker resource plan is missing or exceeds launcher bounds")]
    InvalidCapturePlan,
}

pub use crate::capture::{
    CaptureCancellation, CapturedLaunchOutput, CapturedResourcePlan, CapturedTermination,
};
impl From<crate::capture::InvalidCapturePlan> for LaunchError {
    fn from(_: crate::capture::InvalidCapturePlan) -> Self {
        Self::InvalidCapturePlan
    }
}

impl LaunchError {
    pub fn to_exit_code(&self) -> i32 {
        match self {
            LaunchError::Service(ServiceError::RequiredMissing { .. }) => {
                EXIT_REQUIRED_SERVICE_MISSING
            }
            _ => 1,
        }
    }
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent CLI-exposed launch knob (no_services, minimal_libs, \
              landlock_enforce, allow_missing_runtime, allow_unsigned_runtime), not combinatorial \
              state — grouping them into an enum would not make any combination less valid"
)]
pub struct LaunchOptions {
    /// A host launch's signed package root; another installation fails the launch.
    pub expected_package_hash: Option<String>,
    pub no_services: bool,
    /// Wins over the manifest's `runtime` field.
    pub runtime_root: Option<PathBuf>,
    /// Override the bwrap binary (defaults to `bwrap`).
    pub bwrap_bin: Option<PathBuf>,
    /// Override the in-namespace launcher executable.
    pub sandbox_shim_bin: Option<PathBuf>,
    /// The minimum host paths a dynamically-linked ELF needs to `execve()`, read-only.
    pub minimal_libs: bool,
    /// Route exec through `__sandbox-exec` so Landlock installs inside bwrap's mount namespace.
    pub landlock_enforce: bool,
    /// Accept Landlock not, or only partially, enforced by the kernel.
    pub landlock_best_effort: bool,
    pub mode: LaunchMode,
    /// Debug escape hatch: fall back to `minimal_libs` when no bundle resolves.
    pub allow_missing_runtime: bool,
    /// Accept an unsigned runtime bundle.
    pub allow_unsigned_runtime: bool,
    pub require_runtime_publisher: Option<String>,
    /// Host path of the Wayland socket, bound into the sandbox at `/run/user/<uid>/wayland-0`.
    pub wayland_socket: Option<PathBuf>,
    /// The compositor's DRM render node.
    pub render_node: Option<PathBuf>,
    /// Host data home of the per-workspace app directories; `None` resolves `$XDG_DATA_HOME`.
    pub data_home: Option<PathBuf>,
    /// Host path of the session's bounded, non-secret human identity projection.
    pub trusted_identities_host_path: Option<PathBuf>,
    /// Separate process action policy projection; never a sandbox capability grant.
    pub action_authority_host_path: Option<PathBuf>,
    /// Workspace this launch belongs to — the CRDT cluster id ([`crate::sandbox::WORKSPACE_ID_ENV`]).
    pub workspace_id: Option<String>,
    /// Host path of the compositor-written `Vec<CapOverride>` JSON; anything unusable leaves the
    /// manifest's sandbox as-is.
    pub capabilities_path: Option<PathBuf>,
    /// The app id the caller launched; any other id than the manifest's refuses the launch.
    pub app_id: Option<String>,
    /// A document to open, forwarded as `argv[1]`.
    pub open_document: Option<PathBuf>,
    /// A URI to open, forwarded as one opaque argv value.
    pub open_uri: Option<String>,
    /// A GUI launch's own arguments, after `--`, forwarded verbatim as `argv[1..]`.
    pub gui_argv: Vec<String>,
    /// Open [`Self::open_uri`] in a window that browses as a visitor.
    pub open_as_visitor: bool,
    /// The app's connector for one outside origin (`imap`), not a window: the session's instances of
    /// the app reach it over the socket the launcher hands it.
    pub tilolang_connector: Option<String>,
    /// The connection to the app's origin connector the device's daemon made, handed to the
    /// process that runs the app's instance here as the service
    /// [`runtime_portal::origin_connector::READER`] names.
    pub tilolang_origin_reader: bool,
    /// The instance a capture's process joins its workspace as, and the person at it
    /// (`<instance> <workspace> <person>`, [`runtime_portal::joined_capture::ENV`]): its collaboration and
    /// identity connections, which the device's daemon made, come with the release after the
    /// origin connection.
    pub tilolang_joined: Option<String>,
    /// What the person gave this app a choice on, bound into its sandbox at the reference each was
    /// answered with: exactly these, read and written as each choice allows, and nothing else.
    pub granted: Vec<GrantedPath>,
}

/// One path the person chose for an app: the host file or folder behind it, the reference the app
/// reaches it by, and whether the choice lets the app change it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct GrantedPath {
    /// The host file or folder the person chose.
    pub source: PathBuf,
    /// The path it is bound at inside the sandbox: the reference the chooser answered.
    pub target: PathBuf,
    /// Whether a save or a writable folder lets the app change it.
    #[serde(default)]
    pub write: bool,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            expected_package_hash: None,
            no_services: false,
            runtime_root: None,
            bwrap_bin: None,
            sandbox_shim_bin: None,
            minimal_libs: false,
            // tapp applications are untrusted by default.
            landlock_enforce: true,
            landlock_best_effort: false,
            mode: LaunchMode::default(),
            allow_missing_runtime: false,
            allow_unsigned_runtime: false,
            require_runtime_publisher: None,
            wayland_socket: None,
            render_node: None,
            data_home: None,
            trusted_identities_host_path: None,
            action_authority_host_path: None,
            workspace_id: None,
            capabilities_path: None,
            app_id: None,
            open_document: None,
            open_uri: None,
            gui_argv: Vec::new(),
            open_as_visitor: false,
            tilolang_connector: None,
            tilolang_origin_reader: false,
            tilolang_joined: None,
            granted: Vec::new(),
        }
    }
}

fn gui_open_args(opts: &LaunchOptions) -> Vec<String> {
    let mut args = if let Some(uri) = &opts.open_uri {
        let mut args = vec![uri.clone()];
        if opts.open_as_visitor {
            args.push(runtime_portal::process_link::VISITOR_WINDOW_FLAG.to_owned());
        }
        args
    } else {
        opts.open_document
            .as_ref()
            .map(|path| vec![path.display().to_string()])
            .unwrap_or_default()
    };
    // The app's arguments ride the tail after the entrypoint token that bwrap forwards as `argv[1..]`.
    args.extend(opts.gui_argv.iter().cloned());
    args
}

fn validate_open_uri(manifest: &ManifestView, opts: &LaunchOptions) -> Result<(), LaunchError> {
    let Some(uri) = opts.open_uri.as_deref() else {
        return Ok(());
    };
    if uri.is_empty()
        || uri.len() > 8 * 1024
        || uri.trim() != uri
        || uri.chars().any(char::is_control)
    {
        return Err(LaunchError::InvalidOpenUri("malformed URI".into()));
    }
    let (scheme, rest) = uri
        .split_once(':')
        .ok_or_else(|| LaunchError::InvalidOpenUri("URI has no scheme".into()))?;
    let scheme = scheme.to_ascii_lowercase();
    // Every app opens its own records; the record names nothing outside them.
    let own_document = scheme == collab_types::DOCUMENT_URI_SCHEME;
    if rest.is_empty()
        || !(own_document || manifest.opens_schemes.iter().any(|claim| claim == &scheme))
    {
        return Err(LaunchError::InvalidOpenUri(format!(
            "{} does not declare scheme {scheme}",
            manifest.package_id
        )));
    }
    Ok(())
}

/// The interface versions `manifest` speaks that `provided` does not serve.
fn interfaces_unmet(
    manifest: &ManifestView,
    provided: Option<&tilo_app_package::interfaces::ProvidedInterfaces>,
) -> Vec<tilo_app_package::interfaces::Unmet> {
    let Some(provided) = provided else {
        return Vec::new();
    };
    let spoken: Vec<_> = manifest
        .sandbox
        .services
        .iter()
        .map(
            |(name, req)| tilo_app_package::interfaces::InterfaceRequirement {
                name: name.clone(),
                version: req.version,
            },
        )
        .collect();
    provided.unmet(&spoken)
}

/// Refuse a package built against an interface version this OS does not serve.
fn validate_interfaces(manifest: &ManifestView) -> Result<(), LaunchError> {
    let provided = package_store::interfaces::system_interfaces();
    let unmet = interfaces_unmet(manifest, provided.as_ref());
    if unmet.is_empty() {
        return Ok(());
    }
    Err(LaunchError::InterfacesUnmet {
        package: manifest.package_id.clone(),
        unmet: tilo_app_package::interfaces::describe_unmet(&unmet),
    })
}

fn validate_launch_mode(manifest: &ManifestView, opts: &LaunchOptions) -> Result<(), LaunchError> {
    if let LaunchMode::Host { desktop_id } = &opts.mode {
        return Err(LaunchError::HostAppNotAPackage {
            desktop_id: desktop_id.clone(),
        });
    }
    if let Some(app_id) = opts
        .app_id
        .as_deref()
        .filter(|app_id| *app_id != manifest.package_id)
    {
        return Err(LaunchError::AppIdMismatch {
            app_id: app_id.to_owned(),
            package: manifest.package_id.clone(),
        });
    }
    if !matches!(&opts.mode, LaunchMode::Service { .. }) {
        return Ok(());
    }
    if manifest.kind != crate::manifest::KIND_SERVICE {
        return Err(LaunchError::InvalidServiceLaunch(format!(
            "{} is `{}`, not a service package",
            manifest.package_id, manifest.kind
        )));
    }
    if manifest.entrypoint.cli_bin().is_none() {
        return Err(LaunchError::InvalidServiceLaunch(format!(
            "{} has no headless entrypoint",
            manifest.package_id
        )));
    }
    if opts.wayland_socket.is_some() || opts.open_document.is_some() || opts.open_uri.is_some() {
        return Err(LaunchError::InvalidServiceLaunch(
            "managed services cannot receive a display, document, or URI".into(),
        ));
    }
    Ok(())
}

fn is_native_game_launch(manifest: &ManifestView, opts: &LaunchOptions) -> bool {
    matches!(&opts.mode, LaunchMode::Gui)
        && manifest.category.as_deref().is_some_and(|category| {
            category.eq_ignore_ascii_case("game") || category.eq_ignore_ascii_case("games")
        })
}

#[cfg(test)]
mod open_uri_launch_tests {
    use super::*;

    fn browser_manifest() -> ManifestView {
        crate::manifest::test_manifest(
            "id = \"so.tilo.browser\"\nversion = \"1\"\nkind = \"app\"",
            "[entrypoint]\nbin = \"bin/browser\"\n\
             [providers]\nschemes = [\"https\"]\n",
        )
    }

    tilo_test_env::scenario! {
        fn a_launch_follows_its_manifest;

        phase launch_claims_and_device_hints_follow_the_manifest;
        {
            {
                let manifest = browser_manifest();
                let accepted = LaunchOptions {
                    open_uri: Some("https://example.test/a --not-an-argument".into()),
                    ..Default::default()
                };
                validate_open_uri(&manifest, &accepted).unwrap();
                let own_document = LaunchOptions {
                    open_uri: Some("tilo-document:3f0c9a1e-2b4d-4e8f-9a6b-7c5d1e2f3a4b".into()),
                    ..Default::default()
                };
                validate_open_uri(&manifest, &own_document)
                    .expect("every app opens its own records unclaimed");

                for uri in [
                    "http://example.test",
                    "file:///etc/passwd",
                    "https://x\n--flag",
                    "tilo-document:",
                ] {
                    let opts = LaunchOptions {
                        open_uri: Some(uri.into()),
                        ..Default::default()
                    };
                    assert!(
                        validate_open_uri(&manifest, &opts).is_err(),
                        "accepted {uri:?}"
                    );
                }
            }
            {
                let game: ManifestView = crate::manifest::test_manifest(
                    "id = \"so.tilo.game\"\nversion = \"1\"\nkind = \"app\"\ncategory = \"Games\"",
                    "[entrypoint]\nbin = \"bin/game\"\n",
                );
                assert!(is_native_game_launch(&game, &LaunchOptions::default()));
                let service = LaunchOptions {
                    mode: LaunchMode::Service {
                        passthrough_argv: Vec::new(),
                    },
                    ..LaunchOptions::default()
                };
                assert!(!is_native_game_launch(&game, &service));
                assert!(!is_native_game_launch(
                    &browser_manifest(),
                    &LaunchOptions::default()
                ));
            }
            {
                let mut game = vec!["/app/game".to_string()];
                splice_game_ntsync(&mut game, true).unwrap();
                assert!(
                    game.windows(3)
                        .any(|window| { window == ["--dev-bind-try", "/dev/ntsync", "/dev/ntsync"] })
                );
                assert_eq!(game.last().map(String::as_str), Some("/app/game"));

                let mut ordinary = vec!["/app/browser".to_string()];
                splice_game_ntsync(&mut ordinary, false).unwrap();
                assert_eq!(ordinary, ["/app/browser"]);
            }
        }
    }
}

#[cfg(test)]
mod interface_tests;

#[cfg(test)]
mod managed_service_tests;

/// Resolve the capability plan: read the compositor's file, evaluate it for this app now.
pub fn resolve_capabilities(manifest: &ManifestView, opts: &LaunchOptions) -> CapabilityPlan {
    let app_id = manifest.package_id.clone();
    let grants = opts
        .capabilities_path
        .as_deref()
        .map(load_capability_file)
        .unwrap_or_default();
    let sandbox = crate::capabilities::declared(manifest, grants.sandbox);
    let mut plan = CapabilityPlan::for_app(&app_id, &sandbox, Some(unix_now()));
    // A workspace launch may change the space when its person may.
    if grants.author {
        plan.participation = crate::capabilities::Participation::Author;
    } else if opts.workspace_id.is_some() {
        plan.participation = crate::capabilities::Participation::ReadOnly;
    }
    tracing::info!(
        target: "tilo_launcher::capabilities",
        app = %app_id,
        ro = plan.ro_paths.len(),
        rw = plan.rw_paths.len(),
        devices = ?plan.devices,
        network = plan.allow_network,
        "workspace capabilities resolved",
    );
    plan
}

pub fn resolve_runtime_test_helper(
    manifest: &crate::manifest::ManifestView,
    opts: &LaunchOptions,
) -> Result<Option<(PathBuf, String)>, LaunchError> {
    resolve_runtime(manifest, opts)
}

/// The launch argv after `splice_required_runtimes` ran on a bare `entry` token, and the mounts it
/// returned for Landlock.
pub fn required_runtimes_test_helper(
    manifest: &crate::manifest::ManifestView,
    opts: &LaunchOptions,
) -> Result<(Vec<String>, Vec<String>), LaunchError> {
    let mut argv = vec!["entry".to_string()];
    let mounts = splice_required_runtimes(&mut argv, manifest, opts)?;
    Ok((argv, mounts))
}

/// The one directory this launch may write to, keyed by `opts.workspace_id`.
fn resolve_app_dir(manifest: &ManifestView, opts: &LaunchOptions) -> Result<PathBuf, LaunchError> {
    let data_home = opts
        .data_home
        .clone()
        .or_else(crate::appdir::host_data_home)
        .ok_or_else(|| LaunchError::NoDataHome(manifest.package_id.clone()))?;
    crate::appdir::prepare_host_app_dir(
        &data_home,
        opts.workspace_id.as_deref(),
        &manifest.package_id,
    )
    .map_err(|source| LaunchError::AppDir {
        package: manifest.package_id.clone(),
        path: crate::appdir::host_app_dir(
            &data_home,
            &crate::appdir::workspace_slug(opts.workspace_id.as_deref()),
            &manifest.package_id,
        ),
        source,
    })
}

/// The directory the person's own records of this app are kept in, one per workspace.
fn resolve_person_dir(
    manifest: &ManifestView,
    opts: &LaunchOptions,
) -> Result<PathBuf, LaunchError> {
    let data_home = opts
        .data_home
        .clone()
        .or_else(crate::appdir::host_data_home)
        .ok_or_else(|| LaunchError::NoDataHome(manifest.package_id.clone()))?;
    crate::appdir::prepare_local_person_dir(&data_home, &manifest.package_id).map_err(|source| {
        LaunchError::AppDir {
            package: manifest.package_id.clone(),
            path: crate::appdir::local_person_dir(&data_home, &manifest.package_id),
            source,
        }
    })
}

fn resolve_launch_dirs(
    manifest: &ManifestView,
    opts: &LaunchOptions,
) -> Result<(PathBuf, PathBuf), LaunchError> {
    Ok((
        resolve_app_dir(manifest, opts)?,
        resolve_person_dir(manifest, opts)?,
    ))
}

/// Say which loader a launch is pinned to, and every part of the pin that could not be made.
fn log_selection(manifest: &crate::manifest::ManifestView, selection: &crate::gpu::GpuSelection) {
    for note in &selection.notes {
        tracing::warn!(target: "tilo_launcher::gpu", package = %manifest.package_id, "{note}");
    }
    tracing::info!(
        target: "tilo_launcher::gpu",
        package = %manifest.package_id,
        node = %selection.node.display(),
        driver = selection.driver.as_deref().unwrap_or("<unknown>"),
        egl = selection.egl_vendor_json.as_ref().map_or_else(|| "<loader default>".into(), |p| p.display().to_string()),
        vulkan = selection.vulkan_icd_json.as_ref().map_or_else(|| "<loader default>".into(), |p| p.display().to_string()),
        "pinning the app to the compositor's render node",
    );
}

/// Priority: explicit override > manifest declaration (per-user, then system; system first for an
/// ABI-bound version) > no bind.
pub fn resolve_runtime(
    manifest: &crate::manifest::ManifestView,
    opts: &LaunchOptions,
) -> Result<Option<(PathBuf, String)>, LaunchError> {
    if let Some(explicit) = &opts.runtime_root {
        let version = manifest
            .runtime_ref()
            .map_or_else(|| "1".to_string(), |(_, v)| v.to_string());
        enforce_bundle_policy(explicit, opts, false)?;
        return Ok(Some((explicit.clone(), version)));
    }
    let Some(raw) = manifest.runtime.as_deref() else {
        return Ok(None);
    };
    let (found, candidates) = find_runtime_bundle(raw, opts)?;
    if found.is_some() {
        return Ok(found);
    }
    if opts.allow_missing_runtime {
        tracing::warn!(
            "manifest declares runtime `{raw}` but no bundle found; \
             continuing with --minimal-libs={} (allow_missing_runtime=on)",
            opts.minimal_libs
        );
        return Ok(None);
    }
    Err(LaunchError::RuntimeBundleMissing {
        runtime: raw.to_string(),
        paths: candidates,
    })
}

/// Look `raw` (`<name>/<version>`) up where runtime bundles are installed and verify the first one
/// found.
#[allow(
    clippy::type_complexity,
    reason = "the found bundle and the searched directories travel together"
)]
fn find_runtime_bundle(
    raw: &str,
    opts: &LaunchOptions,
) -> Result<(Option<(PathBuf, String)>, Vec<PathBuf>), LaunchError> {
    let (version, candidates) = runtime_lookup(raw)?;
    let system = runtime_bundle::system_bundle_path(version);
    for cand in &candidates {
        if cand.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME).exists() {
            enforce_bundle_policy(cand, opts, cand == &system)?;
            return Ok((Some((cand.clone(), version.to_string())), candidates));
        }
    }
    Ok((None, candidates))
}

/// The version `raw` (`<name>/<version>`) names and where a launch looks for its bundle, in order.
fn runtime_lookup(raw: &str) -> Result<(&str, Vec<PathBuf>), LaunchError> {
    let (_, version) = raw
        .split_once('/')
        .filter(|(name, version)| !name.is_empty() && valid_runtime_version(version))
        .ok_or_else(|| LaunchError::RuntimeReferenceMalformed(raw.to_string()))?;
    let per_user = runtime_bundle::default_bundle_path_from(
        package_store::dev_env::dev_override(runtime_bundle::RUNTIME_DIR_ENV),
        version,
    )
    .ok();
    let system = runtime_bundle::system_bundle_path(version);
    Ok((version, runtime_candidates(version, per_user, system)))
}

/// Whether the runtime `raw` names is installed where a launch looks for it.
#[must_use]
pub fn runtime_installed(raw: &str) -> bool {
    runtime_lookup(raw).is_ok_and(|(_, candidates)| {
        candidates
            .iter()
            .any(|cand| cand.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME).exists())
    })
}

/// The standard library the tilolang runtime `raw` (`tilolang/<version>`) carries
/// ([`runtime_portal::process_link::STD_FILE`]), read from its bundle where a launch finds it and
/// proven as a launch proves that bundle: its signed manifest, and the library's bytes against
/// the hash the manifest lists. `None` when the runtime is not installed here.
///
/// # Errors
/// A malformed reference, a bundle that does not verify, or one that carries no library.
pub fn runtime_std(raw: &str) -> Result<Option<Vec<u8>>, LaunchError> {
    let (version, candidates) = runtime_lookup(raw)?;
    let system = runtime_bundle::system_bundle_path(version);
    let Some(dir) = candidates
        .into_iter()
        .find(|cand| cand.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME).exists())
    else {
        return Ok(None);
    };
    let manifest = verified_bundle(&dir, &LaunchOptions::default(), dir == system)?;
    let file = runtime_portal::process_link::STD_FILE;
    let missing = || LaunchError::RuntimeStdMissing {
        runtime: raw.to_owned(),
        path: dir.clone(),
        file,
    };
    let listed = manifest
        .files
        .iter()
        .find(|listed| listed.path == file)
        .ok_or_else(missing)?;
    let bytes = std::fs::read(dir.join(file)).map_err(|_| missing())?;
    if blake3::hash(&bytes).to_hex().as_str() != listed.hash {
        return Err(LaunchError::RuntimeBundleSignature {
            path: dir.clone(),
            reason: format!("{file} is not the file its manifest lists"),
        });
    }
    Ok(Some(bytes))
}

/// Where to look for runtime `version`, in order.
fn runtime_candidates(version: &str, per_user: Option<PathBuf>, system: PathBuf) -> Vec<PathBuf> {
    if runtime_bundle::split_abi_version(version).is_some() {
        [system].into_iter().chain(per_user).collect()
    } else {
        per_user.into_iter().chain([system]).collect()
    }
}

/// A version is one plain path component under the runtime roots and `/run/tilo-runtime`.
pub(crate) fn valid_runtime_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 64
        && version != "."
        && version != ".."
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'_'))
}

/// The minimal-libs stopgap and the manifest's required runtimes, spliced before the entrypoint.
fn splice_runtime_mounts(
    argv: &mut Vec<String>,
    manifest: &crate::manifest::ManifestView,
    opts: &LaunchOptions,
    runtime: Option<(&Path, &str)>,
) -> Result<Vec<String>, LaunchError> {
    let required = resolve_required_runtimes(manifest, opts)?;
    preflight_entrypoint_runtime(manifest, opts, runtime, &required)?;
    splice_runtime_mounts_with(argv, manifest, opts, runtime.is_some(), &required)
}

/// [`splice_runtime_mounts`] for required runtimes already resolved.
fn splice_runtime_mounts_with(
    argv: &mut Vec<String>,
    manifest: &ManifestView,
    opts: &LaunchOptions,
    resolved_runtime_present: bool,
    required: &[(PathBuf, String)],
) -> Result<Vec<String>, LaunchError> {
    let mut paths = splice_minimal_libs(argv, opts, resolved_runtime_present)?;
    paths.extend(splice_resolved_required_runtimes(argv, manifest, required)?);
    Ok(paths)
}

/// Every bundle the manifest requires, installed and verified, with its version.
fn resolve_required_runtimes(
    manifest: &crate::manifest::ManifestView,
    opts: &LaunchOptions,
) -> Result<Vec<(PathBuf, String)>, LaunchError> {
    manifest
        .required_runtimes
        .iter()
        .map(|raw| match find_runtime_bundle(raw, opts)? {
            (Some(bundle), _) => Ok(bundle),
            (None, searched) => Err(LaunchError::RequiredRuntimeMissing {
                package: manifest.package_id.clone(),
                runtime: raw.clone(),
                searched,
            }),
        })
        .collect()
}

/// Mount each bundle read-only at its runtime mount, before the trailing entrypoint token, and name
/// the entrypoint runtime's modules where a script finds them.
fn splice_resolved_required_runtimes(
    argv: &mut Vec<String>,
    manifest: &ManifestView,
    bundles: &[(PathBuf, String)],
) -> Result<Vec<String>, LaunchError> {
    let mut mounts = Vec::new();
    for (root, version) in bundles {
        let entry = pop_entry(argv)?;
        let mount = crate::sandbox::push_required_runtime(argv, root, version);
        argv.push(entry);
        mounts.push(mount.display().to_string());
    }
    splice_runtime_node_path(argv, manifest, bundles)?;
    splice_runtime_trust(argv, manifest)?;
    Ok(mounts)
}

/// Whether a script's runtime trusts the system's certificate authorities, as bun and Node name it.
const SYSTEM_CA_ENV: &str = "NODE_USE_SYSTEM_CA";

/// A script's runtime trusts the certificate authorities this device trusts, the ones the
/// sandbox shows it (`/etc/pki`) and the daemon's own connections are checked against, rather
/// than the list its interpreter was built with: an authority the device's owner added is the
/// app's too, and one they removed is not. A compiled entrypoint reads its own.
fn splice_runtime_trust(
    argv: &mut Vec<String>,
    manifest: &ManifestView,
) -> Result<(), LaunchError> {
    if manifest.entrypoint.script.is_none() {
        return Ok(());
    }
    splice_cli_setenv(argv, Some((SYSTEM_CA_ENV.to_string(), "1".to_string())))
}

/// `NODE_PATH` names the module directories bun resolves a bare import from.
const NODE_PATH_ENV: &str = "NODE_PATH";

/// A script entrypoint imports the packages its runtime provides (the SDK's native addon) rather
/// than carrying them in its payload. bun resolves a bare `import` through `NODE_PATH` — Node
/// honours it for `require` alone — so a script whose runtime bundle carries `lib/node_modules`
/// gets the mount's directory, read-only like the rest of the runtime. A compiled entrypoint
/// resolves no package and gets none.
fn splice_runtime_node_path(
    argv: &mut Vec<String>,
    manifest: &ManifestView,
    bundles: &[(PathBuf, String)],
) -> Result<(), LaunchError> {
    if manifest.entrypoint.script.is_none() {
        return Ok(());
    }
    let Some(raw) = manifest.entrypoint.runtime.as_deref() else {
        return Ok(());
    };
    let Some((_, version)) = raw.split_once('/') else {
        return Ok(());
    };
    let Some((root, _)) = bundles.iter().find(|(_, held)| held == version) else {
        return Ok(());
    };
    if !root.join("lib/node_modules").is_dir() {
        return Ok(());
    }
    splice_cli_setenv(
        argv,
        Some((
            NODE_PATH_ENV.to_string(),
            crate::sandbox::runtime_node_path(version)
                .display()
                .to_string(),
        )),
    )
}

/// [`resolve_required_runtimes`] then [`splice_resolved_required_runtimes`].
fn splice_required_runtimes(
    argv: &mut Vec<String>,
    manifest: &crate::manifest::ManifestView,
    opts: &LaunchOptions,
) -> Result<Vec<String>, LaunchError> {
    splice_resolved_required_runtimes(argv, manifest, &resolve_required_runtimes(manifest, opts)?)
}

/// Verify the bundle at `path`: `require_runtime_publisher`, else the trust list, else any signed
/// publisher.
fn enforce_bundle_policy(
    path: &Path,
    opts: &LaunchOptions,
    system_bundle: bool,
) -> Result<(), LaunchError> {
    verified_bundle(path, opts, system_bundle).map(drop)
}

/// A bundle a person installed into their own store: every payload byte proven and signed by a trusted
/// publisher.
pub fn verified_user_bundle(path: &Path) -> Result<runtime_bundle::BundleManifest, LaunchError> {
    verified_bundle(path, &LaunchOptions::default(), false)
}

/// [`enforce_bundle_policy`], returning the verified manifest.
fn verified_bundle(
    path: &Path,
    opts: &LaunchOptions,
    system_bundle: bool,
) -> Result<runtime_bundle::BundleManifest, LaunchError> {
    let trusted = load_trusted_publishers_for_bundle(path).map_err(|error| {
        LaunchError::RuntimeBundleSignature {
            path: path.to_path_buf(),
            reason: error.to_string(),
        }
    })?;
    let require_publisher = opts.require_runtime_publisher.clone();
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
        // A per-user bundle is writable by unconfined processes, so every byte is proven before it is
        // used.
        let records = runtime_bundle::record::directory();
        runtime_bundle::verify_bundle_recorded_with(path, &verify_opts, records.as_deref()).map(
            |(manifest, check)| {
                tracing::debug!(
                    bundle = %path.display(),
                    by_verity = check.by_verity,
                    by_record = check.by_record,
                    hashed = check.hashed,
                    hashed_bytes = check.hashed_bytes,
                    "per-user runtime verified"
                );
                manifest
            },
        )
    };
    match verified {
        Ok(manifest) => {
            trusted
                .check(&manifest)
                .map_err(|error| LaunchError::RuntimeBundleSignature {
                    path: path.to_path_buf(),
                    reason: error.to_string(),
                })?;
            Ok(manifest)
        }
        Err(runtime_bundle::BundleError::NotSigned) => Err(LaunchError::RuntimeBundleUnsigned {
            path: path.to_path_buf(),
        }),
        Err(e) => Err(LaunchError::RuntimeBundleSignature {
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

    let Ok(root) = root.canonicalize() else {
        return false;
    };

    // A writable ancestor could replace the version directory, so every signed file and its parents
    // are checked.
    let mut ancestor = Some(root.as_path());
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
        while let Some(dir) =
            parent.filter(|dir| dir.starts_with(root.as_path()) && *dir != root.as_path())
        {
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

#[cfg(test)]
#[cfg(unix)]
mod system_bundle_tree_policy_tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    fn manifest_for(path: &str) -> runtime_bundle::BundleManifest {
        runtime_bundle::BundleManifest {
            version: "test".to_string(),
            arch: std::env::consts::ARCH.to_string(),
            root_hash: "00".repeat(32),
            files: vec![runtime_bundle::BundleFile {
                path: path.to_string(),
                hash: "00".repeat(32),
                size: 1,
                verity: None,
            }],
            produced_at: 0,
            publisher_hex: None,
            signature_hex: None,
            verity_signature_hex: None,
        }
    }

    #[test]
    fn immutable_system_bundle_accepts_a_root_owned_symlinked_prefix() {
        if rustix::process::geteuid().as_raw() != 0 {
            return;
        }
        // The policy walks every ancestor, so the fixture has to live below root-owned directories
        // nobody else may write.
        let parent = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .unwrap_or_else(std::env::temp_dir);
        let ancestors_secure = parent.canonicalize().is_ok_and(|parent| {
            parent.ancestors().all(|dir| {
                std::fs::symlink_metadata(dir)
                    .is_ok_and(|metadata| metadata.uid() == 0 && metadata.mode() & 0o022 == 0)
            })
        });
        if !ancestors_secure {
            return;
        }
        let temp = tempfile::tempdir_in(&parent).expect("temporary root");
        let deployment = temp.path().join("deployment/usr/lib/tilo-runtime/test");
        std::fs::create_dir_all(&deployment).expect("deployment tree");
        // tempfile deliberately creates a private 0700 parent, whereas the real rpm-ostree deployment
        // is rooted below traversable, immutable root-owned directories.
        for directory in [
            temp.path(),
            &temp.path().join("deployment"),
            &temp.path().join("deployment/usr"),
            &temp.path().join("deployment/usr/lib"),
            &temp.path().join("deployment/usr/lib/tilo-runtime"),
            &deployment,
        ] {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755))
                .expect("immutable deployment directory permissions");
        }
        std::fs::write(deployment.join("bundle.toml"), "manifest").expect("manifest");
        std::fs::write(deployment.join("payload"), b"x").expect("payload");
        symlink(temp.path().join("deployment/usr"), temp.path().join("usr"))
            .expect("atomic-style usr link");

        let root = temp.path().join("usr/lib/tilo-runtime/test");
        assert!(system_bundle_tree_is_root_owned(
            &root,
            &manifest_for("payload")
        ));
        assert_eq!(
            std::fs::symlink_metadata(temp.path().join("usr"))
                .expect("link metadata")
                .uid(),
            0
        );
    }
}

/// A per-user runtime is writable by unconfined processes, so a launch proves every byte.
#[cfg(test)]
#[cfg(unix)]
mod per_user_runtime_verification_tests {
    use super::*;
    use std::path::Path;

    const DYLIB: &str = "lib/libruntime_dylib.so";

    /// A signed per-user runtime under `parent` whose toolkit dylib is `dylib_size` bytes, and launch
    /// options that pin its publisher.
    fn per_user_runtime(
        parent: &Path,
        dylib_size: usize,
    ) -> (tempfile::TempDir, PathBuf, LaunchOptions) {
        let work = tempfile::tempdir_in(parent).unwrap();
        let host = work.path().join("host");
        let loader = runtime_bundle::host_minimal_paths_for(std::env::consts::ARCH).unwrap()[0]
            .trim_start_matches('/');
        std::fs::create_dir_all(host.join(loader).parent().unwrap()).unwrap();
        std::fs::write(host.join(loader), b"loader").unwrap();
        let dylib = work.path().join("libruntime_dylib.so");
        let bytes: Vec<u8> = (0..=250u8).cycle().take(dylib_size).collect();
        std::fs::write(&dylib, bytes).unwrap();
        let identity = runtime_bundle::Identity::generate();
        let identity_path = work.path().join("identity.json");
        std::fs::write(&identity_path, identity.to_json_bytes()).unwrap();
        let bundle = work.path().join("tilo-runtime/2026.10");
        runtime_bundle::build_bundle_with(
            &bundle,
            "2026.10",
            &runtime_bundle::BuildOptions {
                from: Some(&host),
                arch: Some(std::env::consts::ARCH),
                identity: Some(&identity_path),
                extra_libs: &[dylib],
                extra_bins: &[],
            },
        )
        .unwrap();
        let opts = LaunchOptions {
            require_runtime_publisher: Some(identity.public_key_hex()),
            ..Default::default()
        };
        (work, bundle, opts)
    }

    /// Replace `path` with its bytes but one, as anyone who can write the per-user runtime directory
    /// can, sealed file or not.
    fn replace_with_one_byte_changed(path: &Path) {
        let mut bytes = std::fs::read(path).unwrap();
        bytes[0] ^= 1;
        std::fs::remove_file(path).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn a_modified_byte_in_an_unsealed_per_user_runtime_is_refused() {
        let (_work, bundle, opts) = per_user_runtime(&std::env::temp_dir(), 3 * 4096);
        enforce_bundle_policy(&bundle, &opts, false).unwrap();
        replace_with_one_byte_changed(&bundle.join(DYLIB));
        assert!(matches!(
            enforce_bundle_policy(&bundle, &opts, false),
            Err(LaunchError::RuntimeBundleSignature { .. })
        ));
    }

    /// Bytes the calling thread has read: the other tests in this binary run on other threads.
    #[cfg(target_os = "linux")]
    fn thread_bytes_read() -> u64 {
        std::fs::read_to_string("/proc/thread-self/io")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("rchar: "))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    /// A temporary directory on a filesystem with fs-verity: `TILO_FSVERITY_DIR`, else the system
    /// temporary directory.
    fn verity_dir() -> Result<tempfile::TempDir, String> {
        let base = std::env::var_os("TILO_FSVERITY_DIR")
            .filter(|dir| !dir.is_empty())
            .map_or_else(std::env::temp_dir, PathBuf::from);
        let dir = tempfile::tempdir_in(&base).map_err(|e| format!("{}: {e}", base.display()))?;
        let probe = dir.path().join("probe");
        std::fs::write(&probe, b"probe").map_err(|e| e.to_string())?;
        runtime_bundle::verity::enable(&probe).map_err(|e| format!("{}: {e}", base.display()))?;
        Ok(dir)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn per_user_runtime_launch_does_not_hash_payload() {
        let dir = tilo_test_env::require_capability!(
            FsVerity,
            verity_dir(),
            "no fs-verity filesystem to install a runtime on"
        );
        let (_work, bundle, opts) = per_user_runtime(dir.path(), 64 << 20);
        let manifest =
            runtime_bundle::verify_bundle_with(&bundle, &runtime_bundle::VerifyOptions::default())
                .unwrap();
        runtime_bundle::enable_bundle_verity(&bundle, &manifest).unwrap();

        let before = thread_bytes_read();
        let started = std::time::Instant::now();
        enforce_bundle_policy(&bundle, &opts, false).unwrap();
        let elapsed = started.elapsed();
        let read = thread_bytes_read().saturating_sub(before);
        eprintln!("per-user 64 MiB runtime verified in {elapsed:?}, {read} bytes read");
        assert!(read < 1 << 20, "the launch read {read} bytes");

        replace_with_one_byte_changed(&bundle.join(DYLIB));
        assert!(matches!(
            enforce_bundle_policy(&bundle, &opts, false),
            Err(LaunchError::RuntimeBundleSignature { .. })
        ));
    }
}

/// A single-entry trust list is the pin handed to `verify_bundle_with`; longer lists are checked after
/// it returns.
pub(crate) fn load_trusted_publishers_for_bundle(
    bundle: &Path,
) -> std::io::Result<runtime_bundle::component_pins::BundleTrust> {
    let path = trusted_publishers_path_from(package_store::dev_env::dev_override(
        crate::dev_env::TRUSTED_PUBLISHERS_FILE_ENV,
    ));
    runtime_bundle::component_pins::load_for_bundle(&path, bundle)
}

/// Where the trusted-publisher list is read from, given the override the build's check let through.
fn trusted_publishers_path_from(allowed_override: Option<std::ffi::OsString>) -> PathBuf {
    allowed_override.map_or_else(tilo_paths::runtime_publishers, PathBuf::from)
}

#[cfg(test)]
mod runtime_candidate_tests {
    use super::*;

    #[test]
    fn an_abi_bound_runtime_runs_on_the_system_copy_of_its_identity() {
        let user = PathBuf::from("/home/ana/.local/share/tilo-runtime");
        let system = PathBuf::from("/usr/lib/tilo-runtime");
        let bound = "2026.07+native.0123456789abcdef0123456789abcdef";
        assert_eq!(
            runtime_candidates(bound, Some(user.join(bound)), system.join(bound)),
            [system.join(bound), user.join(bound)],
            "the version names its payload, so the system's bundle is the same bytes"
        );
        // A release-only version names no payload: the person's own install stands in for the
        // system's.
        assert_eq!(
            runtime_candidates("1", Some(user.join("1")), system.join("1")),
            [user.join("1"), system.join("1")]
        );
        assert_eq!(
            runtime_candidates(bound, None, system.join(bound)),
            [system.join(bound)]
        );
    }
}

#[cfg(test)]
mod dev_override_tests {
    use super::*;
    use crate::dev_env::TRUSTED_PUBLISHERS_FILE_ENV;
    use package_store::dev_env::dev_override_from;

    #[test]
    fn trust_roots_follow_the_build_profile() {
        {
            let path = trusted_publishers_path_from(dev_override_from(
                false,
                TRUSTED_PUBLISHERS_FILE_ENV,
                Some("/tmp/attacker-publishers.txt".into()),
            ));
            assert_eq!(path, tilo_paths::runtime_publishers());
        }
        {
            let path = trusted_publishers_path_from(dev_override_from(
                true,
                TRUSTED_PUBLISHERS_FILE_ENV,
                Some("/dev/runtime/trusted-publishers.txt".into()),
            ));
            assert_eq!(path, Path::new("/dev/runtime/trusted-publishers.txt"));
        }
        {
            let guarded = dev_override_from(
                false,
                runtime_bundle::RUNTIME_DIR_ENV,
                Some("/tmp/attacker-runtime".into()),
            );
            // Resolution may fail with no data dir; it must never land under the override.
            if let Ok(path) = runtime_bundle::default_bundle_path_from(guarded, "1") {
                assert!(!path.starts_with("/tmp/attacker-runtime"), "{path:?}");
            }
            let honoured = dev_override_from(
                true,
                runtime_bundle::RUNTIME_DIR_ENV,
                Some("/dev/runtime".into()),
            );
            assert_eq!(
                runtime_bundle::default_bundle_path_from(honoured, "1").ok(),
                Some(PathBuf::from("/dev/runtime/1"))
            );
        }
    }
}

/// Reads the manifest, builds the sandbox and execs; it does not call `process::exit`.
pub fn launch(
    manifest_path: &Path,
    payload_root: &Path,
    opts: &LaunchOptions,
) -> Result<ExitStatus, LaunchError> {
    let manifest = launch_manifest(manifest_path, payload_root, opts)?;
    launch_app(&manifest, payload_root, opts)
}

/// The first half of [`launch`]: reads the manifest, verified against the signed installation.
pub fn launch_manifest(
    manifest_path: &Path,
    payload_root: &Path,
    opts: &LaunchOptions,
) -> Result<ManifestView, LaunchError> {
    let manifest = if let Some(expected) = opts.expected_package_hash.as_deref() {
        verified_original_manifest(manifest_path, payload_root, opts, expected)?
    } else {
        read_manifest(manifest_path)?
    };
    tilo_startup_metrics::handoff_mark("package-verified");
    Ok(manifest)
}

/// The second half of [`launch`]: builds the sandbox for a manifest and runs the app in it.
pub fn launch_app(
    manifest: &ManifestView,
    payload_root: &Path,
    opts: &LaunchOptions,
) -> Result<ExitStatus, LaunchError> {
    launch_with_context(manifest, payload_root, opts, None)
}

pub(crate) fn verified_original_manifest(
    manifest_path: &Path,
    payload_root: &Path,
    opts: &LaunchOptions,
    expected: &str,
) -> Result<ManifestView, LaunchError> {
    let refusal =
        || LaunchError::InvalidServiceLaunch("original signed package launch changed".into());
    let app = opts.app_id.as_deref().ok_or_else(refusal)?;
    let installed = package_store::find_installed(app)
        .map_err(|_| refusal())?
        .ok_or_else(refusal)?;
    verified_launch_installation(&installed, manifest_path, payload_root, expected)
}

/// Which entrypoint binary the launch targets, and the arguments it forwards.
#[derive(Debug, Clone, Default)]
pub enum LaunchMode {
    #[default]
    Gui,
    Cli {
        command: String,
        passthrough_argv: Vec<String>,
    },
    /// A desktop-managed, headless process declared as `kind = "service"`.
    Service { passthrough_argv: Vec<String> },
    /// An application Tilo did not package (a host `.desktop` entry, Flatpak or otherwise).
    Host {
        /// Desktop-file id of the entry, e.g. `com.spotify.Client`.
        desktop_id: String,
    },
}

/// Whether a launch opens a window, which gets the shell-UI connection: a command, a headless service
/// or an origin connector has no window to describe.
fn opens_window(mode: &LaunchMode, connector: Option<&str>) -> bool {
    matches!(mode, LaunchMode::Gui) && connector.is_none()
}

/// Env override that turns the egress proxy off, downgrading a host-scoped grant to unrestricted access.
pub const EGRESS_PROXY_ENV: &str = crate::dev_env::EGRESS_PROXY_ENV;

/// Start the egress proxy if this launch needs one; a check that cannot start refuses the launch.
fn start_egress_proxy(
    access: &crate::egress_proxy::EgressAccess,
    app: &str,
) -> Result<
    (
        Option<crate::sandbox::EgressProxyBind>,
        Option<crate::egress_proxy::EgressProxy>,
    ),
    LaunchError,
> {
    if !access.needs_proxy() {
        return Ok((None, None));
    }
    // Calls need the proxy even with no TCP host: TCP is then denied and only the call's UDP
    // passes, while a call is live.
    let policy = access
        .decision
        .policy()
        .cloned()
        .unwrap_or_else(crate::egress_proxy::EgressPolicy::none);

    let host_socket = crate::egress_proxy::proxy_socket_path(
        &crate::dbus_proxy::session_runtime_dir(),
        std::process::id(),
    );
    match crate::egress_proxy::EgressProxy::start_for_app(
        &host_socket,
        policy.clone(),
        app,
        access.calls,
    ) {
        Ok(mut check) => {
            let bind = crate::sandbox::EgressProxyBind {
                host_socket,
                sandbox_socket: PathBuf::from(crate::egress_proxy::SANDBOX_PROXY_PATH),
                port: crate::egress_proxy::RELAY_PORT,
                udp_client: check.take_udp_client(),
            };
            Ok((Some(bind), Some(check)))
        }
        Err(source) => Err(LaunchError::EgressProxy {
            hosts: policy.describe(),
            source,
        }),
    }
}

/// Put service sockets, seccomp programs and account data on their child fds, in the child, before `exec`.
///
/// bwrap forwards fds it was not asked about untouched, so a placement here survives namespace setup.
/// Every source moves above the target window first, so no `dup2` clobbers one still waiting; not
/// bwrap's `--file`, which copies a socket's contents and blocks forever.
///
/// # Safety
///
/// Call only from `Command::pre_exec` (post-fork, pre-exec). Uses only `fcntl`, `dup2` and `close`, all
/// async-signal-safe; allocates nothing.
unsafe fn place_child_fds(pairs: &mut [(RawFd, RawFd)]) -> std::io::Result<()> {
    let Some(highest) = pairs.iter().map(|(_, dst)| *dst).max() else {
        return Ok(());
    };
    let floor = highest.checked_add(1).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "service fd relocation floor overflowed RawFd",
        )
    })?;
    // Capture descriptors sit above 1024, and F_DUPFD refuses a floor at or above the soft limit.
    unsafe { raise_soft_nofile(floor, pairs.len())? };
    for (src, _) in pairs.iter_mut() {
        // F_DUPFD returns the lowest free fd at or above the floor, clear of every target.
        let moved = unsafe { libc::fcntl(*src, libc::F_DUPFD, floor) };
        if moved < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // A second live descriptor at an unnamed number is one the app cannot close.
        unsafe { libc::close(*src) };
        *src = moved;
    }
    for (src, dst) in pairs.iter() {
        if unsafe { libc::dup2(*src, *dst) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
        // dup2 leaves the target without FD_CLOEXEC, so it must survive bwrap's and the app's exec.
        unsafe {
            let flags = libc::fcntl(*dst, libc::F_GETFD);
            if flags != -1 {
                let _ = libc::fcntl(*dst, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
        }
    }
    // The app gets exactly one descriptor per service; skip numbers that are themselves a target.
    for (src, dst) in pairs.iter() {
        if *src != *dst && !pairs.iter().any(|(_, other)| other == src) {
            unsafe { libc::close(*src) };
        }
    }
    Ok(())
}

/// The rlimits bwrap and the app inherit: no core dumps, a bounded descriptor table and CPU time.
unsafe fn raise_soft_nofile(floor: RawFd, count: usize) -> std::io::Result<()> {
    let needed = u64::try_from(floor)
        .ok()
        .and_then(|floor| floor.checked_add(u64::try_from(count).ok()?))
        .ok_or_else(|| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: the pointer refers to an initialized rlimit for the call.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if limit.rlim_cur >= needed {
        return Ok(());
    }
    if limit.rlim_max != libc::RLIM_INFINITY && limit.rlim_max < needed {
        // No allocation after fork: the errno says it, the doc says why.
        return Err(std::io::Error::from_raw_os_error(libc::EMFILE));
    }
    limit.rlim_cur = needed;
    // SAFETY: as above.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

unsafe fn set_sandbox_resource_limits() -> std::io::Result<()> {
    let core = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // 1024 leaves ample room for renderers while bounding fd-exhaustion attacks.
    let nofile = libc::rlimit {
        rlim_cur: 1024,
        rlim_max: 1024,
    };
    // SAFETY: both pointers refer to initialized rlimit values for the duration of these syscalls.
    if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &raw const core) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const nofile) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

unsafe fn set_captured_resource_limits(plan: CapturedResourcePlan) -> std::io::Result<()> {
    let cpu = libc::rlimit {
        rlim_cur: plan.cpu_seconds,
        rlim_max: plan.cpu_seconds,
    };
    let address_space = libc::rlimit {
        rlim_cur: plan.memory_bytes,
        rlim_max: plan.memory_bytes,
    };
    // SAFETY: both pointers remain valid for the duration of the syscalls.
    if unsafe { libc::setrlimit(libc::RLIMIT_CPU, &raw const cpu) } != 0
        || unsafe { libc::setrlimit(libc::RLIMIT_AS, &raw const address_space) } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Pop the trailing entrypoint token so a splice function can insert flags before it.
fn pop_entry(argv: &mut Vec<String>) -> Result<String, LaunchError> {
    argv.pop().ok_or(LaunchError::ArgvInvariant)
}

/// Splice in the `TILO_CLI_COMMAND` setenv before the trailing entrypoint token so bwrap still parses
/// it as a flag.
fn splice_cli_setenv(
    argv: &mut Vec<String>,
    extra_setenv: Option<(String, String)>,
) -> Result<(), LaunchError> {
    if let Some((k, v)) = extra_setenv {
        let entry = pop_entry(argv)?;
        argv.push("--setenv".to_string());
        argv.push(k);
        argv.push(v);
        argv.push(entry);
    }
    Ok(())
}

/// Hand this launch's phase marks to the app, only when the shell asked for startup metrics.
fn splice_startup_marks(argv: &mut Vec<String>) -> Result<(), LaunchError> {
    tilo_startup_metrics::handoff_mark("sandbox-argv-ready");
    splice_cli_setenv(
        argv,
        std::env::var_os("TILO_STARTUP_METRICS_PATH").map(|path| {
            (
                "TILO_STARTUP_METRICS_PATH".to_string(),
                path.to_string_lossy().into_owned(),
            )
        }),
    )?;
    splice_cli_setenv(
        argv,
        tilo_startup_metrics::handoff_env_value()
            .map(|marks| (tilo_startup_metrics::LAUNCHER_MARKS_ENV.to_string(), marks)),
    )
}

/// Expose the upstream NT synchronization device only to GUI packages marked as games, where the
/// kernel has one: a kernel without it still starts the game, which then synchronizes without it.
fn splice_game_ntsync(argv: &mut Vec<String>, latency_sensitive: bool) -> Result<(), LaunchError> {
    if latency_sensitive {
        let entry = pop_entry(argv)?;
        argv.extend([
            "--dev-bind-try".to_string(),
            "/dev/ntsync".to_string(),
            "/dev/ntsync".to_string(),
            entry,
        ]);
    }
    Ok(())
}

/// Narrow dynamic-loader binds only (not all of `/usr`), for a plain dynamically-linked `execve()`.
fn splice_minimal_libs(
    argv: &mut Vec<String>,
    opts: &LaunchOptions,
    resolved_runtime_present: bool,
) -> Result<Vec<String>, LaunchError> {
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
            // The fixed system profile already binds these parents read-only.
            let covered = crate::profile::SYSTEM_READ_PATHS
                .iter()
                .any(|root| std::path::Path::new(path).starts_with(root));
            if !covered && std::path::Path::new(path).exists() {
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

/// Workspace id, spliced before the entrypoint token, absent when the launch has no session.
fn splice_workspace_id_bind(
    argv: &mut Vec<String>,
    opts: &LaunchOptions,
) -> Result<(), LaunchError> {
    if let Some(workspace_id) = opts.workspace_id.as_deref() {
        let entry = pop_entry(argv)?;
        crate::sandbox::push_workspace_id(argv, workspace_id);
        argv.push(entry);
    }
    Ok(())
}

/// A trusted shell launch's small typed context for the app; it carries no authority.
fn splice_tool_context(argv: &mut Vec<String>) -> Result<(), LaunchError> {
    if let Some(context) = validated_tool_context_from_env() {
        let entry = pop_entry(argv)?;
        argv.push("--setenv".to_string());
        argv.push("TILO_TOOL_CONTEXT".to_string());
        argv.push(context);
        argv.push(entry);
    }
    Ok(())
}

/// App directory: bound read-write at a fixed sandbox path, HOME/XDG vars pointed inside it.
fn splice_app_dir(
    argv: &mut Vec<String>,
    app_dir: &Path,
    person_dir: &Path,
    capabilities: &CapabilityPlan,
) -> Result<(), LaunchError> {
    let entry = pop_entry(argv)?;
    crate::appdir::push_app_dir(argv, app_dir);
    crate::appdir::push_person_dir(argv, person_dir);
    // The font index every app of this build shares, so a launch after the first reads a database.
    if let Some(index) = crate::appdir::prepare_host_font_index() {
        crate::appdir::push_font_index(argv, &index);
    }
    // After `push_app_dir` repoints `HOME`: the user's home, only when a grant bound it.
    if let Some(user_home) = crate::sandbox::push_user_home(argv, capabilities) {
        tracing::debug!(
            user_home = %user_home.display(),
            "a granted bind covers the user's home; telling the app where it is"
        );
    }
    crate::sandbox::push_user_data(argv, capabilities);
    // The workspace's shared mounts, on the same terms: only ones the app can read.
    if let Some(roots) = crate::sandbox::push_workspace_roots(argv, capabilities) {
        tracing::debug!(
            workspace_roots = %roots,
            "granted binds cover workspace roots; telling the app where they are"
        );
    }
    argv.push(entry);
    Ok(())
}

/// Settings remaps XDG into the app dir.
fn splice_session_settings(argv: &mut Vec<String>, package_id: &str) -> Result<(), LaunchError> {
    use std::os::unix::fs::DirBuilderExt as _;

    if package_id != tilo_app_id::apps::SETTINGS {
        return Ok(());
    }
    let Some(host) = tilo_paths::xdg::config_home() else {
        return Ok(());
    };
    let host = host.join("tilo");
    for (relative, bind) in crate::appdir::SESSION_SETTINGS_BINDS {
        if *bind == crate::appdir::SessionBind::Writable {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(host.join(relative))?;
        }
    }
    let entry = pop_entry(argv)?;
    crate::appdir::push_session_settings_bind(argv, &host);
    argv.push(entry);
    Ok(())
}

/// The image-owned system authority grant: the control panel's host binaries, configuration and
/// sockets.
fn splice_system_authority(
    argv: &mut Vec<String>,
    manifest: &ManifestView,
) -> Result<
    (
        Option<crate::system_authority::SystemAuthorityPlan>,
        Option<DBusProxyGuard>,
    ),
    LaunchError,
> {
    let Some((plan, guard)) = crate::system_authority::resolve(
        &manifest.package_id,
        &manifest.sandbox.services,
        current_uid(),
    ) else {
        return Ok((None, None));
    };
    let entry = pop_entry(argv)?;
    crate::system_authority::push_binds(argv, &plan);
    argv.push(entry);
    Ok((Some(plan), guard))
}

/// Mount Files' live workspace-governance identity directory read-only.
fn splice_trusted_identities(
    argv: &mut Vec<String>,
    manifest: &ManifestView,
    opts: &LaunchOptions,
) -> Result<(), LaunchError> {
    if manifest.package_id == "so.tilo.file-explorer"
        && let Some(host_path) = opts.trusted_identities_host_path.as_deref()
    {
        let entry = pop_entry(argv)?;
        crate::sandbox::push_trusted_identities_bind(argv, host_path);
        argv.push(entry);
    }
    Ok(())
}

/// What the person gave this app a choice on, bound into its sandbox at each reference: exactly
/// this app's choices, and nothing another app was given.
///
/// # Errors
/// A target outside the references a choice answers with, or a source that is not there.
fn splice_granted(argv: &mut Vec<String>, opts: &LaunchOptions) -> Result<(), LaunchError> {
    if opts.granted.is_empty() {
        return Ok(());
    }
    let entry = pop_entry(argv)?;
    for granted in &opts.granted {
        let reference = granted.target.to_str().ok_or_else(|| {
            LaunchError::InvalidServiceLaunch("a granted reference is not a path".into())
        })?;
        if !reference.starts_with(GRANTED_ROOT)
            || granted.target.components().any(|part| {
                !matches!(
                    part,
                    std::path::Component::Normal(_) | std::path::Component::RootDir
                )
            })
            || !granted.source.is_absolute()
            || !granted.source.exists()
        {
            return Err(LaunchError::InvalidServiceLaunch(
                "a granted path is not a choice the person made".into(),
            ));
        }
        argv.push(if granted.write {
            "--bind".to_owned()
        } else {
            "--ro-bind".to_owned()
        });
        argv.push(granted.source.display().to_string());
        argv.push(reference.to_owned());
    }
    argv.push(entry);
    Ok(())
}

/// Where a choice's reference is bound: the root every answer names, never a host path.
const GRANTED_ROOT: &str = "/run/tilo/chosen/";

/// One app/person projection directory, so atomic replacement and revocation stay visible.
fn splice_action_authority(
    argv: &mut Vec<String>,
    manifest: &ManifestView,
    opts: &LaunchOptions,
) -> Result<(), LaunchError> {
    use collab_types::action_authority::{AppActionAuthorityProjection, MAX_PROJECTION_BYTES};
    use std::io::Read as _;
    let Some(path) = opts.action_authority_host_path.as_deref() else {
        return Ok(());
    };
    let invalid =
        || LaunchError::InvalidServiceLaunch("invalid host action authority binding".into());
    if path.file_name().and_then(|name| name.to_str()) != Some("current.json") {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((MAX_PROJECTION_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_PROJECTION_BYTES {
        return Err(invalid());
    }
    let policy: AppActionAuthorityProjection = serde_json::from_slice(&bytes).map_err(|error| {
        LaunchError::InvalidServiceLaunch(format!(
            "invalid host action authority binding: unreadable projection: {error}"
        ))
    })?;
    policy.validate().map_err(|error| {
        LaunchError::InvalidServiceLaunch(format!("invalid host action authority binding: {error}"))
    })?;
    if policy.app_id != manifest.package_id
        || opts.workspace_id.as_deref() != Some(&policy.workspace)
    {
        return Err(LaunchError::InvalidServiceLaunch(format!(
            "invalid host action authority binding: it is for {} in workspace {}, the launch for {} \
             in workspace {}",
            policy.app_id,
            policy.workspace,
            manifest.package_id,
            opts.workspace_id.as_deref().unwrap_or("(none)")
        )));
    }
    let entry = pop_entry(argv)?;
    crate::sandbox::push_action_authority_bind(argv, path);
    argv.push(entry);
    Ok(())
}

/// Wayland display, spliced before the entrypoint token like the minimal-libs binds.
fn splice_wayland(
    argv: &mut Vec<String>,
    opts: &LaunchOptions,
) -> Result<Option<crate::sandbox::WaylandBind>, LaunchError> {
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

/// The egress proxy socket: the app's only route out of an empty network namespace.
fn splice_egress_proxy_bind(
    argv: &mut Vec<String>,
    egress_proxy: Option<&crate::sandbox::EgressProxyBind>,
) -> Result<(), LaunchError> {
    if let Some(bind) = egress_proxy {
        let entry = pop_entry(argv)?;
        crate::sandbox::push_egress_proxy(argv, bind);
        argv.push(entry);
    }
    Ok(())
}

/// GPU: the compositor's render node decides EGL/Vulkan visibility, narrowing the `/dev/dri` grant.
fn splice_gpu(
    argv: &mut Vec<String>,
    manifest: &ManifestView,
    opts: &LaunchOptions,
    gpu_granted: bool,
) -> Result<(Option<crate::gpu::GpuSelection>, Option<PathBuf>), LaunchError> {
    // An app without `gpu` draws through its workspace's render service: no node, no pin, no ICD
    // probe. Where it draws itself all the same it has no device for Mesa's device-select layer to
    // prefer, and the layer, which crashed such apps at a session's start, is off.
    if !gpu_granted {
        let entry = pop_entry(argv)?;
        for (key, value) in crate::gpu::LoaderPin::Unpinned.env() {
            argv.extend(["--setenv".to_string(), key.to_string(), value]);
        }
        argv.push(entry);
        return Ok((None, None));
    }
    let entry = pop_entry(argv)?;
    crate::profile::push_gpu_devices(argv);
    argv.push(entry);
    // The same pin the session's own surface apps run with ([`crate::gpu::LoaderPin`]), set here
    // inside the sandbox, with the device nodes narrowed to it.
    let pin = crate::gpu::LoaderPin::for_render_node(opts.render_node.as_deref());
    let entry = pop_entry(argv)?;
    let gpu = match pin {
        crate::gpu::LoaderPin::Node {
            selection,
            software,
        } => {
            log_selection(manifest, &selection);
            // The device nodes were just bound: narrow them to the node.
            crate::sandbox::push_gpu_selection(argv, &selection, true);
            if let Some(pin) = software {
                tracing::info!(
                    target: "tilo_launcher::gpu",
                    package = %manifest.package_id,
                    vulkan = %pin.vulkan_icd_json.display(),
                    "no driver claims the compositor's render node: the app renders with lavapipe",
                );
                crate::sandbox::push_software_pin(argv, &pin);
            }
            Some(selection)
        }
        crate::gpu::LoaderPin::Software(pin) => {
            crate::sandbox::push_software_pin(argv, &pin);
            crate::profile::hide_gpu_devices(argv);
            tracing::info!(
                target: "tilo_launcher::gpu",
                package = %manifest.package_id,
                vulkan = %pin.vulkan_icd_json.display(),
                "no GPU: the app renders with the software renderer and sees no render node",
            );
            None
        }
        unpinned @ crate::gpu::LoaderPin::Unpinned => {
            for (key, value) in unpinned.env() {
                argv.extend(["--setenv".to_string(), key.to_string(), value]);
            }
            None
        }
    };
    argv.push(entry);
    let seed = gpu
        .as_ref()
        .and_then(|selection| selection.pipeline_seed.clone())
        .or_else(crate::gpu::host_pipeline_seed);
    if gpu.is_none()
        && let Some(seed) = &seed
    {
        let entry = pop_entry(argv)?;
        crate::sandbox::push_pipeline_seed(argv, seed);
        argv.push(entry);
    }
    if seed.is_some() && matches!(&opts.mode, LaunchMode::Gui) {
        let entry = pop_entry(argv)?;
        argv.extend([
            "--setenv".to_string(),
            crate::mesa_seed::CONSUME_ENV.to_string(),
            "1".to_string(),
        ]);
        argv.push(entry);
    }
    Ok((gpu, seed))
}

/// Read-only rules match the host projection mounts; file permissions stay in the capability plan.
pub(crate) fn push_projection_landlock_rules(argv: &mut Vec<String>, opts: &LaunchOptions) {
    if opts.action_authority_host_path.is_some() {
        argv.push("--ro".into());
        argv.push(crate::sandbox::SANDBOX_ACTION_AUTHORITY_DIR.into());
    }
}

/// Everything [`splice_sandbox_shim`] needs beyond `argv` and `opts`.
struct ShimSpliceCtx<'a> {
    manifest: &'a ManifestView,
    egress_proxy: &'a Option<crate::sandbox::EgressProxyBind>,
    runtime_arg: Option<(&'a Path, &'a str)>,
    extra_ro_paths: &'a [String],
    egress: &'a crate::egress_proxy::EgressDecision,
    capabilities: &'a CapabilityPlan,
    dbus_plan: &'a Option<DBusBindPlan>,
    system_authority: &'a Option<crate::system_authority::SystemAuthorityPlan>,
    wayland_bind: &'a Option<crate::sandbox::WaylandBind>,
    gpu: &'a Option<crate::gpu::GpuSelection>,
    /// The app was granted `gpu`: its device nodes are bound.
    gpu_devices: bool,
    pipeline_seed: &'a Option<PathBuf>,
    keep_fds: &'a [RawFd],
}

/// Bind this launcher in and rewrite the entrypoint token into `<shim> __sandbox-exec [rules] --
/// <entry>`.
fn splice_sandbox_shim(
    argv: &mut Vec<String>,
    opts: &LaunchOptions,
    ctx: &ShimSpliceCtx<'_>,
) -> Result<(), LaunchError> {
    let need_shim = opts.landlock_enforce || ctx.egress_proxy.is_some();
    if !need_shim {
        return Ok(());
    }
    let entry = pop_entry(argv)?;
    let launcher = if let Some(path) = &opts.sandbox_shim_bin {
        path.clone()
    } else {
        std::env::current_exe()
            .map_err(|e| LaunchError::Io(std::io::Error::other(format!("current_exe: {e}"))))?
    };
    argv.push("--ro-bind".to_string());
    argv.push(launcher.display().to_string());
    argv.push(SHIM_MOUNT_PATH.to_string());
    argv.push(SHIM_MOUNT_PATH.to_string());
    argv.push(crate::landlock_shim::SANDBOX_EXEC_SUBCOMMAND.to_string());
    if !opts.landlock_enforce {
        argv.push("--no-landlock".to_string());
    } else if opts.landlock_best_effort {
        argv.push("--landlock-best-effort".to_string());
    }
    for fd in ctx.keep_fds {
        argv.push("--keep-fd".to_string());
        argv.push(fd.to_string());
    }
    // The relay's endpoints.
    if let Some(bind) = ctx.egress_proxy {
        argv.push("--net-proxy".to_string());
        argv.push(bind.sandbox_socket.display().to_string());
        argv.push("--net-proxy-port".to_string());
        argv.push(bind.port.to_string());
        if let Some(client) = &bind.udp_client {
            use std::os::fd::AsRawFd as _;
            argv.push("--net-udp-fd".to_string());
            argv.push(client.as_raw_fd().to_string());
        }
    }
    // Sandbox-side read paths for Landlock: payload, runtime, minimal-libs binds and the manifest's
    // reads.
    let payload_mount = crate::sandbox::payload_mount_for(&ctx.manifest.package_id);
    argv.push("--ro".to_string());
    argv.push(payload_mount.display().to_string());
    push_runtime_landlock_rules(argv, ctx.runtime_arg);
    for p in ctx.extra_ro_paths {
        argv.push("--ro".to_string());
        argv.push(p.clone());
    }
    // The staged theme bundles and the compiled painter sets a window reads its theme from: both
    // were bound above, and an unnamed bind is unreadable under Landlock.
    crate::sandbox::push_theme_bundles_landlock(argv);
    // The same list the binds came from: an unnamed bind is visible but returns EACCES on every open.
    for path in crate::profile::landlock_ro_paths(ctx.egress.shares_host_network()) {
        argv.push("--ro".to_string());
        argv.push(path.display().to_string());
    }
    // This is generated sandbox data, never the host account database.
    argv.extend(["--ro".to_string(), "/etc/passwd".to_string()]);
    push_projection_landlock_rules(argv, opts);
    // Every grant needs a matching rule, or the bind is visible and every open returns EACCES.
    push_grant_landlock_rules(argv, ctx);
    // Wayland needs read and write: `connect(2)` on a unix socket is write-flavoured.
    if let Some(bind) = ctx.wayland_bind {
        argv.push("--rw".to_string());
        argv.push(bind.sandbox_socket.display().to_string());
    }
    if let Some(sel) = ctx.gpu {
        crate::sandbox::push_gpu_landlock_rules(argv, sel);
    }
    if ctx.gpu.is_none() && ctx.pipeline_seed.is_some() {
        argv.extend([
            "--ro".to_string(),
            crate::gpu::SANDBOX_PIPELINE_SEED_DIR.to_string(),
        ]);
    }
    // The app's directory by its in-sandbox path: the host path is not reachable from inside.
    argv.push("--rw".to_string());
    argv.push(crate::appdir::SANDBOX_APP_DIR.to_string());
    argv.push("--rw".to_string());
    argv.push(crate::appdir::SANDBOX_PERSON_DIR.to_string());
    // The shared font index is read and written inside the sandbox.
    crate::appdir::push_font_index_landlock(argv);
    // Each choice bound for it, read or written as its own grant allows and never executed.
    for granted in &opts.granted {
        argv.push(if granted.write { "--rw-noexec" } else { "--ro" }.to_string());
        argv.push(granted.target.display().to_string());
    }
    // The GPU device nodes, on the same read-write terms.
    if ctx.gpu_devices {
        for path in crate::profile::landlock_rw_device_paths() {
            argv.push("--rw".to_string());
            argv.push(path.display().to_string());
        }
    }
    push_scratch_and_engine_rules(argv, ctx.manifest);
    // Service sockets need no rule: they arrive connected, never opened by path.
    if let Some(bind) = ctx.egress_proxy {
        argv.push("--rw".to_string());
        argv.push(bind.sandbox_socket.display().to_string());
    }
    // Landlock's network rule mirrors the namespace: `Blocked` denies TCP, `Filtered` pins the relay
    // port.
    if matches!(ctx.egress, crate::egress_proxy::EgressDecision::Blocked) {
        argv.push("--net-restricted".to_string());
    }
    argv.push("--".to_string());
    argv.push(entry);
    Ok(())
}

/// Landlock rules for every grant the launch carries.
fn push_grant_landlock_rules(argv: &mut Vec<String>, ctx: &ShimSpliceCtx<'_>) {
    for path in &ctx.capabilities.ro_paths {
        argv.push("--ro".to_string());
        argv.push(path.display().to_string());
    }
    for path in &ctx.capabilities.rw_paths {
        argv.push("--rw".to_string());
        argv.push(path.display().to_string());
    }
    // A bound D-Bus socket needs a read rule on its sandbox path, or ashpd's `connect()` gets EACCES.
    if let Some(plan) = ctx.dbus_plan {
        argv.push("--ro".to_string());
        argv.push(plan.sandbox_path.display().to_string());
    }
    // The control panel's binaries, configuration and sockets from the plan its binds came from.
    if let Some(plan) = ctx.system_authority {
        crate::system_authority::push_landlock_rules(argv, plan);
    }
}

fn push_runtime_landlock_rules(argv: &mut Vec<String>, runtime: Option<(&Path, &str)>) {
    if let Some((rt_root, ver)) = runtime {
        let rt = crate::sandbox::runtime_mount_for(ver);
        argv.push("--ro".to_string());
        argv.push(rt.display().to_string());
        if rt_root.join("lib/fonts").is_dir() {
            argv.extend([
                "--ro".to_string(),
                crate::sandbox::RUNTIME_FONT_CATALOG_PATH.to_string(),
            ]);
        }
        // The canonical loader paths re-bound out of the bundle, or the entry's INTERP is blocked.
        for canonical in crate::sandbox::CANONICAL_LOADER_PATHS {
            let no_slash = canonical.trim_start_matches('/');
            if rt_root.join("lib").join(no_slash).exists() {
                argv.push("--ro".to_string());
                argv.push((*canonical).to_string());
            }
        }
    }
}

fn push_scratch_and_engine_rules(argv: &mut Vec<String>, manifest: &ManifestView) {
    // Scratch mounts the app expects writable; both vanish with the process.
    for p in ["/tmp", "/dev"] {
        argv.push("--rw".to_string());
        argv.push(p.to_string());
    }
    // A script run by a runtime's interpreter reads a little of its own procfs: bun 1.4.5 aborts
    // without `/proc/self/maps`, and reads its own `statm`, `cgroup` and `fd/` as it starts;
    // CPython 3.13 reads none of it. Nothing else of /proc is granted — not another process's,
    // not the kernel's tunables or sockets. bwrap mounts a fresh procfs in the private PID
    // namespace (`--proc /proc`), so these are the app's own process alone.
    if manifest.entrypoint.script.is_some() {
        for path in crate::sandbox::PROC_SCRIPT_READ_PATHS {
            argv.push("--ro".to_string());
            argv.push((*path).to_string());
        }
    }
    let profile = crate::seccomp::manifest_profile(&manifest.sandbox, &manifest.package_id);
    for path in crate::profile::landlock_rw_nested_sandbox_paths(profile) {
        argv.push("--rw".to_string());
        argv.push(path.display().to_string());
    }
}

/// A libc-compatible account record without disclosing the host account database.
fn sandbox_account(uid: u32, gid: u32) -> String {
    format!("app:x:{uid}:{gid}:Sandbox application:/run/tilo/app:/sbin/nologin\n")
}

/// Give bwrap a private, read-only account file.
fn stage_sandbox_account(argv: &mut Vec<String>) -> Result<OwnedFd, LaunchError> {
    let account = sandbox_account(
        rustix::process::geteuid().as_raw(),
        rustix::process::getegid().as_raw(),
    );
    let data = stage_seccomp_blob(account.as_bytes())?;
    // The source sits above the fixed child slots, so no earlier `dup2` overwrites it.
    let fd = unsafe { libc::fcntl(data.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 128) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let owned = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    argv.splice(
        0..0,
        [
            "--ro-bind-data".into(),
            ACCOUNT_CHILD_FD.to_string(),
            "/etc/passwd".into(),
        ],
    );
    Ok(owned)
}

#[cfg(test)]
mod scratch_rule_tests {
    use super::*;

    /// A script run by a runtime's interpreter reads a measured little of its own procfs, read-only;
    /// every other app gets no rule for `/proc` at all, and nothing ever gets it writable. bwrap
    /// mounts a fresh procfs in the private PID namespace, so `/proc/self` is the app's own process.
    #[test]
    fn only_a_script_runtime_reads_procfs_and_only_its_own() {
        let mut script = crate::manifest::test_manifest(
            "id = \"com.example.app\"\nversion = \"1.0.0\"\nkind = \"app\"",
            "[entrypoint]\nbin = \"bin/bun\"\n",
        );
        script.entrypoint.script = Some("main.js".to_string());
        let mut argv: Vec<String> = Vec::new();
        push_scratch_and_engine_rules(&mut argv, &script);
        for path in crate::sandbox::PROC_SCRIPT_READ_PATHS {
            assert!(
                argv.windows(2)
                    .any(|pair| pair[0] == "--ro" && pair[1] == *path),
                "{path} is not readable: {argv:?}"
            );
        }
        assert!(
            !argv
                .windows(2)
                .any(|pair| pair[0] == "--ro" && pair[1] == "/proc"),
            "no app reads all of procfs: {argv:?}"
        );
        assert!(
            !argv
                .windows(2)
                .any(|pair| pair[0] == "--rw" && pair[1].starts_with("/proc")),
            "procfs is never writable: {argv:?}"
        );

        let ordinary = crate::manifest::test_manifest(
            "id = \"com.example.app\"\nversion = \"1.0.0\"\nkind = \"app\"",
            "[entrypoint]\nbin = \"bin/app\"\n",
        );
        let mut argv: Vec<String> = Vec::new();
        push_scratch_and_engine_rules(&mut argv, &ordinary);
        assert!(
            !argv.iter().any(|arg| arg.starts_with("/proc")),
            "an app that runs no script reads no procfs: {argv:?}"
        );
    }
}

#[cfg(test)]
mod sandbox_account_tests {
    use super::*;
    use tilo_test_env::SpawnUnderEnvLock as _;

    tilo_test_env::scenario! {
        fn the_sandbox_account_is_private_data_in_its_own_slot;

        phase handoff_preserves_a_service_source_in_the_account_child_slot;
        {
            let service = stage_seccomp_blob(b"service").expect("service fixture");
            let filters = SeccompFds {
                clone3_compat: stage_seccomp_blob(b"clone3").expect("compat fixture"),
                baseline: stage_seccomp_blob(b"baseline").expect("baseline fixture"),
            };
            let account = stage_sandbox_account(&mut Vec::new()).expect("account");
            let expected = format!(
                "serviceclone3baseline{}",
                sandbox_account(
                    rustix::process::geteuid().as_raw(),
                    rustix::process::getegid().as_raw()
                )
            );
            let source = service.as_raw_fd();
            let mut pairs = vec![(ACCOUNT_CHILD_FD, 17)];
            pairs.extend(filters.child_fds(account.as_raw_fd()));
            let mut command = Command::new("/bin/cat");
            command.args([
                "/proc/self/fd/17",
                "/proc/self/fd/40",
                "/proc/self/fd/41",
                "/proc/self/fd/42",
            ]);
            // Move only the child's fixture onto the colliding source slot.
            unsafe {
                command.pre_exec(move || {
                    if libc::dup2(source, ACCOUNT_CHILD_FD) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    place_child_fds(&mut pairs)
                });
            }
            let output = command.output_locked().expect("fixture child");
            assert!(output.status.success(), "{:?}", output.stderr);
            assert_eq!(output.stdout, expected.as_bytes());
        }

        phase staged_account_is_private_read_only_data_and_reserves_its_descriptor;
        {
            use std::io::Read as _;
            let mut argv = vec!["--unshare-all".to_string(), "/app/bin".to_string()];
            let fd = stage_sandbox_account(&mut argv).expect("stage account");
            assert!(fd.as_raw_fd() >= 128);
            let mut contents = String::new();
            std::fs::File::from(fd)
                .read_to_string(&mut contents)
                .expect("account bytes");
            assert_eq!(
                contents,
                sandbox_account(
                    rustix::process::geteuid().as_raw(),
                    rustix::process::getegid().as_raw()
                )
            );
            assert_eq!(
                argv,
                [
                    "--ro-bind-data",
                    "42",
                    "/etc/passwd",
                    "--unshare-all",
                    "/app/bin"
                ]
            );
        }

        phase account_contains_only_the_sandbox_identity_and_home;
        {
            assert_eq!(
                sandbox_account(1000, 1001),
                "app:x:1000:1001:Sandbox application:/run/tilo/app:/sbin/nologin\n"
            );
            assert!(crate::services::RESERVED_CHILD_FDS.contains(&ACCOUNT_CHILD_FD));
        }
    }
}

/// What [`spawn_and_wait`] wires into the child in `pre_exec`: the staged seccomp filters, the resolved
/// service sockets and whether to apply game scheduling hints.
struct ChildSetup {
    seccomp_fds: SeccompFds,
    services: Vec<crate::services::ResolvedService>,
    latency_sensitive: bool,
}

/// Finish building `argv` (seccomp prefix + passthrough tail), spawn `bwrap` with the seccomp fd and
/// service sockets wired into the child via `pre_exec`, then wait for it.
#[expect(
    clippy::too_many_lines,
    reason = "one ordered spawn sequence: descriptor ownership must stay visible from preparation to reaping"
)]
fn spawn_and_wait(
    mut argv: Vec<String>,
    cli_passthrough: Vec<String>,
    opts: &LaunchOptions,
    child_setup: ChildSetup,
    egress: &crate::egress_proxy::EgressDecision,
    egress_proxy: Option<&crate::sandbox::EgressProxyBind>,
    observer: Option<&dyn EnrolledProcessObserver>,
    debug_output: Option<PathBuf>,
) -> Result<ExitStatus, LaunchError> {
    let ChildSetup {
        seccomp_fds,
        services: resolved,
        latency_sensitive,
    } = child_setup;
    let debug_output = debug_output.and_then(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .map_err(|error| {
                tracing::warn!(path = %path.display(), %error, "debug output log unavailable");
            })
            .ok()
            .map(|file| (path, file))
    });
    SeccompFds::prepend_bwrap_args(&mut argv);
    let account_fd = stage_sandbox_account(&mut argv)?;
    let host_account_fd = account_fd.as_raw_fd();
    // The system read profile from the host's pre-built view, when it can carry this launch's mounts
    // unchanged.
    let system_view = if observer.is_none() {
        crate::sandbox_view::attach(&mut argv)
    } else {
        None
    };

    if observer
        .and_then(EnrolledProcessObserver::namespace_init_fd)
        .is_some()
    {
        // Only the stricter owned-service path asks for PID 1 supervision.
        if !argv.iter().any(|arg| arg == "--unshare-all")
            || opts.bwrap_bin.as_deref() != Some(Path::new("/usr/bin/bwrap"))
        {
            return Err(LaunchError::InvalidServiceLaunch(
                "private service namespace missing".into(),
            ));
        }
        // These are bwrap options, before its command (which may already be the Landlock shim plus
        // opaque arguments).
        argv.splice(
            0..0,
            [
                "--as-pid-1".into(),
                "--info-fd".into(),
                crate::services::SERVICE_NAMESPACE_CHILD_FD.to_string(),
            ],
        );
    }

    // Passthrough: everything after the entrypoint token is opaque to bwrap, forwarded as the
    // sandboxed program's argv[1..].
    argv.extend(cli_passthrough);

    // 4.
    let bwrap = opts
        .bwrap_bin
        .clone()
        .unwrap_or_else(|| PathBuf::from("bwrap"));
    let mut cmd = Command::new(&bwrap);
    cmd.args(&argv);
    clear_bwrap_environment(&mut cmd);
    if observer.is_some_and(EnrolledProcessObserver::owns_process_group) {
        cmd.process_group(0);
    }
    if observer.is_some() {
        // What the app writes reaches whoever started the launcher: the session reads both
        // streams into its log, labelled with the app, and that is the only account of an app
        // that fails inside itself. A terminal is never handed to the sandbox, though: one on
        // the app's streams would let it push input into the shell that started it.
        cmd.stdin(std::process::Stdio::null())
            .stdout(app_stream(std::io::IsTerminal::is_terminal(
                &std::io::stdout(),
            )))
            .stderr(app_stream(std::io::IsTerminal::is_terminal(
                &std::io::stderr(),
            )));
    }
    if let Some((path, _)) = &debug_output {
        tracing::warn!(
            path = %path.display(),
            "{DEBUG_STDERR_ENV}=1: capturing the app's output (debug only)",
        );
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
    }

    // pre_exec: place both seccomp blobs on fixed child fds, and put each service socket on the fd
    // number the app was told to read.
    let mut service_fds: Vec<(RawFd, RawFd)> = resolved
        .iter()
        .map(|r| (r.host_fd(), r.child_fd as RawFd))
        .collect();
    service_fds.extend(seccomp_fds.child_fds(host_account_fd));
    if let Some(fd) = observer.and_then(EnrolledProcessObserver::namespace_init_fd) {
        service_fds.push((fd, crate::services::SERVICE_NAMESPACE_CHILD_FD));
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
        &[
            PathBuf::from(crate::appdir::SANDBOX_APP_DIR),
            PathBuf::from(crate::appdir::SANDBOX_PERSON_DIR),
        ],
    );
    let net_restricted = matches!(egress, crate::egress_proxy::EgressDecision::Blocked);
    // The shim enforces Landlock via opts.landlock_enforce (argv splice above); this just logs intent
    // — the shim's own line has the real outcome.
    tracing::info!(
        target: "tilo_launcher::landlock",
        rules = landlock_plan.len(),
        net_restricted,
        enforce = opts.landlock_enforce,
        "landlock plan derived",
    );
    tracing::info!(
        target: "tilo_launcher::egress_proxy",
        guarded = egress_proxy.is_some(),
        hosts = ?egress.policy().map(super::egress_proxy::EgressPolicy::describe),
        shares_host_network = egress.shares_host_network(),
        "egress resolved",
    );
    let _ = landlock_plan;

    // SAFETY: pre_exec runs in the child after fork, before exec.
    let private_service = observer
        .and_then(EnrolledProcessObserver::namespace_init_fd)
        .is_some();
    unsafe {
        cmd.pre_exec(move || {
            place_child_fds(&mut service_fds)?;
            set_sandbox_resource_limits()?;
            if private_service {
                libc::umask(0o077);
            }
            if latency_sensitive {
                crate::performance::apply_game_performance_hints()?;
            }
            // Landlock is NOT installed here — bwrap needs broad FS access to set up the namespace;
            // the `__sandbox-exec` shim installs it after.
            Ok(())
        });
    }

    // Last parent-side policy/ownership check after preparation, immediately before spawn.
    if let Some(observer) = observer {
        observer.before_spawn().map_err(LaunchError::Spawn)?;
    }
    // 5.
    let mut child = cmd.spawn().map_err(LaunchError::Spawn)?;
    let debug_path = debug_output.as_ref().map(|(path, _)| path.clone());
    let mut readers = Vec::new();
    if let Some((_, file)) = debug_output {
        let sink = Arc::new(std::sync::Mutex::new(file));
        let budget = Arc::new(AtomicUsize::new(DEBUG_OUTPUT_LIMIT));
        if let Some(out) = child.stdout.take() {
            let (sink, budget) = (sink.clone(), budget.clone());
            if let Ok(reader) = std::thread::Builder::new()
                .name("tilo-debug-stdout".into())
                .spawn(move || copy_bounded_output(out, &sink, &budget))
            {
                readers.push(reader);
            }
        }
        if let Some(err) = child.stderr.take()
            && let Ok(reader) = std::thread::Builder::new()
                .name("tilo-debug-stderr".into())
                .spawn(move || copy_bounded_output(err, &sink, &budget))
        {
            readers.push(reader);
        }
    }
    // Keep both filter fds alive until spawn has duplicated them in the child.
    drop(seccomp_fds);
    drop(account_fd);
    // Same rule: the child holds its own copies, so the launcher's are dead weight, dropped before the
    // wait so the far end learns of the exit.
    drop(resolved);
    let status = if let Some(observer) = observer {
        observer.child_created();
        observer.supervise(child)?
    } else {
        child.wait()?
    };
    // The app's own streams are read out before its end is said, so what it wrote is whole.
    for reader in readers {
        let _ = reader.join();
    }
    // A sandbox that ended badly names what it ran, and what the app said before it went: an empty
    // stream leaves this as the only record of the program and the arguments it was given.
    if !status.success() {
        let said = debug_path
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| last_lines(&text))
            .filter(|said| !said.is_empty())
            .map(|said| format!("; it said: {said}"))
            .unwrap_or_default();
        eprintln!(
            "tilo-launcher: {} {status} ran {} {}{said}",
            opts.app_id.as_deref().unwrap_or("the app"),
            bwrap.display(),
            argv.join(" ")
        );
    }
    drop(system_view);
    Ok(status)
}

/// What a managed app's stdout or stderr is: the launcher's own stream, unless that is a terminal.
fn app_stream(terminal: bool) -> std::process::Stdio {
    if terminal {
        std::process::Stdio::null()
    } else {
        std::process::Stdio::inherit()
    }
}

/// Seccomp baseline → tmpfile → fd.
fn stage_seccomp(
    manifest: &ManifestView,
    grants: crate::seccomp::SyscallGrants,
) -> Result<SeccompFds, LaunchError> {
    let decision = crate::seccomp::resolve_profile(&manifest.sandbox.seccomp, &manifest.package_id);
    let profile = decision.applied;
    if decision.refused {
        eprintln!(
            "tilo-launcher: {} asked for seccomp profile {:?}, which the image does not \
             entitle it to; applying {} (write-xor-execute enforced). Entitlement is \
             decided by {}, not by the package.",
            manifest.package_id,
            decision.requested,
            profile.as_str(),
            crate::seccomp::JIT_POLICY_PATH,
        );
    } else if profile.as_str() != decision.requested {
        eprintln!(
            "tilo-launcher: manifest asked for seccomp profile {:?}, which is not one we know; \
             applying {} (write-xor-execute enforced)",
            decision.requested,
            profile.as_str()
        );
    }
    Ok(SeccompFds {
        clone3_compat: stage_seccomp_blob(&crate::seccomp::compile_clone3_compat()?)?,
        baseline: stage_seccomp_blob(&compile_app_baseline(profile, grants)?)?,
    })
}

/// What an argv builder produces: the bwrap command line, an optional `(name, value)` environment pair
/// the caller must splice in, and the trailing arguments passed through to the app.
type BuiltArgv = (Vec<String>, Option<(String, String)>, Vec<String>);

/// Pick the entrypoint bin per launch mode: GUI uses `bin`; CLI uses `cli_bin()` (falling back to
/// `bin`) and splices `TILO_CLI_COMMAND` / forwards passthrough argv.
fn launch_entry_bin<'a>(manifest: &'a ManifestView, mode: &LaunchMode) -> Option<&'a str> {
    match mode {
        LaunchMode::Gui => Some(
            manifest
                .entrypoint
                .gui_bin()
                .unwrap_or(&manifest.entrypoint.bin),
        ),
        LaunchMode::Cli { .. } | LaunchMode::Service { .. } => Some(
            manifest
                .entrypoint
                .cli_bin()
                .unwrap_or(&manifest.entrypoint.bin),
        ),
        LaunchMode::Host { .. } => None,
    }
}

/// Refuse a launch whose entrypoint links against a library the sandboxed loader will not find in the
/// runtime bundle, the payload or the host's library directories.
fn preflight_runtime_libraries(
    manifest: &ManifestView,
    payload_root: &Path,
    opts: &LaunchOptions,
    runtime: Option<(&Path, &str)>,
) -> Result<(), LaunchError> {
    let Some((runtime_root, version)) = runtime else {
        return Ok(());
    };
    let Some(entry_bin) = launch_entry_bin(manifest, &opts.mode) else {
        return Ok(());
    };
    if manifest.entrypoint.runtime.is_some() {
        // The binary is the entrypoint runtime's, checked once that runtime is resolved
        // (`preflight_entrypoint_runtime`).
        return Ok(());
    }
    let payload_mount = crate::sandbox::payload_mount_for(&manifest.package_id);
    let runtime_mount = crate::sandbox::runtime_mount_for(version);
    let view = crate::loader_preflight::LoaderView::for_sandbox(
        (&payload_mount, payload_root),
        (&runtime_mount, runtime_root),
        vec![crate::sandbox::runtime_library_path(version)],
    );
    view.check(&payload_mount.join(entry_bin))
        .map_err(|missing| {
            tracing::error!(
                package = %manifest.package_id,
                library = %missing.library,
                needed_by = %missing.needed_by.display(),
                runtime = %runtime_root.display(),
                "app links against a library its runtime does not provide"
            );
            LaunchError::RuntimeLibraryMissing {
                package: manifest.package_id.clone(),
                runtime: manifest
                    .runtime
                    .clone()
                    .unwrap_or_else(|| format!("tilo-runtime/{version}")),
                library: missing.library,
                searched: missing.searched,
            }
        })
}

/// The entrypoint's arguments before any the launch adds: the script its runtime's binary runs
/// (`[entrypoint] script`), where the sandbox mounts the package's payload.
fn script_args(manifest: &ManifestView) -> Vec<String> {
    manifest
        .entrypoint
        .script
        .iter()
        .map(|script| {
            crate::sandbox::payload_mount_for(&manifest.package_id)
                .join(script)
                .display()
                .to_string()
        })
        .collect()
}

fn build_initial_argv(
    manifest: &ManifestView,
    payload_root: &Path,
    opts: &LaunchOptions,
    runtime_arg: Option<(&Path, &str)>,
    binds: &[crate::sandbox::ServiceBind],
    dbus_plan: Option<&DBusBindPlan>,
    capabilities: &CapabilityPlan,
) -> Result<BuiltArgv, LaunchError> {
    let (command, setenv, entry_tail) = build_entry_argv(
        manifest,
        payload_root,
        opts,
        runtime_arg,
        binds,
        dbus_plan,
        capabilities,
    )?;
    let mut args = script_args(manifest);
    args.extend(entry_tail);
    Ok((command, setenv, args))
}

fn build_entry_argv(
    manifest: &ManifestView,
    payload_root: &Path,
    opts: &LaunchOptions,
    runtime_arg: Option<(&Path, &str)>,
    binds: &[crate::sandbox::ServiceBind],
    dbus_plan: Option<&DBusBindPlan>,
    capabilities: &CapabilityPlan,
) -> Result<BuiltArgv, LaunchError> {
    match &opts.mode {
        LaunchMode::Gui => Ok((
            build_bwrap_argv(
                manifest,
                payload_root,
                runtime_arg,
                binds,
                dbus_plan,
                capabilities,
            ),
            None,
            // A GUI launch carries at most one argument — the document to open — riding the same tail
            // as CLI passthrough (opaque bytes after entrypoint).
            gui_open_args(opts),
        )),
        LaunchMode::Cli {
            command,
            passthrough_argv,
        } => {
            let entry_bin_owned = launch_entry_bin(manifest, &opts.mode)
                .unwrap_or(&manifest.entrypoint.bin)
                .to_string();
            let v = crate::sandbox::build_bwrap_argv_for_entry(
                manifest,
                payload_root,
                runtime_arg,
                binds,
                dbus_plan,
                &entry_bin_owned,
                capabilities,
            );
            Ok((
                v,
                Some(("TILO_CLI_COMMAND".to_string(), command.clone())),
                passthrough_argv.clone(),
            ))
        }
        LaunchMode::Service { passthrough_argv } => {
            let entry_bin_owned = launch_entry_bin(manifest, &opts.mode)
                .unwrap_or(&manifest.entrypoint.bin)
                .to_string();
            let argv = crate::sandbox::build_bwrap_argv_for_entry(
                manifest,
                payload_root,
                runtime_arg,
                binds,
                dbus_plan,
                &entry_bin_owned,
                capabilities,
            );
            Ok((argv, None, passthrough_argv.clone()))
        }
        // Refused at the top of `launch_with`, before any of the setup above ran.
        LaunchMode::Host { desktop_id } => Err(LaunchError::HostAppNotAPackage {
            desktop_id: desktop_id.clone(),
        }),
    }
}

/// Build and exec the sandbox for an already-parsed `manifest`: validates the launch-mode/fs policy,
/// resolves service/identity binds, the runtime bundle, the app's writable directory and its network
/// egress proxy, then spawns `bwrap` and waits for it.
pub fn launch_with(
    manifest: &ManifestView,
    payload_root: &Path,
    opts: &LaunchOptions,
) -> Result<ExitStatus, LaunchError> {
    if opts.expected_package_hash.is_some() {
        return Err(LaunchError::InvalidServiceLaunch(
            "original signed package requires manifest-backed run".into(),
        ));
    }
    launch_with_context(manifest, payload_root, opts, None)
}

pub(crate) fn validate_owned_service_options(opts: &LaunchOptions) -> Result<(), LaunchError> {
    if !matches!(opts.mode, LaunchMode::Service { .. })
        || opts.expected_package_hash.is_none()
        || !opts.landlock_enforce
        || opts.landlock_best_effort
        || opts.allow_missing_runtime
        || opts.allow_unsigned_runtime
    {
        return Err(LaunchError::InvalidServiceLaunch(
            "owned service requires signed, confined, identity-free launch".into(),
        ));
    }
    Ok(())
}

/// Start `work` on its own thread of `scope`, beside the launch's main line.
fn spawn_beside<'scope, T: Send + 'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    name: &str,
    work: impl FnOnce() -> T + Send + 'scope,
) -> Result<std::thread::ScopedJoinHandle<'scope, T>, LaunchError> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn_scoped(scope, work)
        .map_err(|error| LaunchError::Io(std::io::Error::other(format!("{name} thread: {error}"))))
}

/// The result of [`spawn_beside`]'s work; its panic continues here.
fn join_beside<T>(handle: std::thread::ScopedJoinHandle<'_, T>) -> T {
    handle
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// A process owner that admits, spawns and supervises one launch on the launcher's behalf: the
/// owned-service path implements it so the launch pipeline can check its authority before the child
/// exists and hand the child back for supervision.
pub(crate) trait EnrolledProcessObserver {
    fn reviewed_network_hosts(&self) -> std::io::Result<Option<Vec<String>>> {
        Ok(None)
    }

    fn namespace_init_fd(&self) -> Option<std::os::fd::RawFd> {
        None
    }
    fn owns_process_group(&self) -> bool {
        false
    }
    fn before_spawn(&self) -> std::io::Result<()>;
    fn child_created(&self);
    fn supervise(&self, child: std::process::Child) -> std::io::Result<std::process::ExitStatus>;
}

/// What the host prepares before a sandbox can be described: the verified runtime bundle, if any, and
/// the app's writable directories.
struct LaunchHost {
    runtime: Option<(PathBuf, String)>,
    app_dir: PathBuf,
    person_dir: PathBuf,
}

/// Verify the runtime bundle, check the entrypoint's libraries against it, and create the app's
/// directories (bwrap can only bind what exists).
fn prepare_launch_host(
    manifest: &ManifestView,
    payload_root: &Path,
    opts: &LaunchOptions,
) -> Result<LaunchHost, LaunchError> {
    // A resolved bundle wins over --minimal-libs and skips the loader-bind stopgap entirely.
    let runtime = resolve_runtime(manifest, opts)?;
    tilo_startup_metrics::handoff_mark("runtime-verified");
    let runtime_arg = runtime.as_ref().map(|(p, v)| (p.as_path(), v.as_str()));
    preflight_runtime_libraries(manifest, payload_root, opts, runtime_arg)?;
    tilo_startup_metrics::handoff_mark("loader-preflight-done");
    // The app's one writable directory, for the workspace it's opened into.
    let (app_dir, person_dir) = resolve_launch_dirs(manifest, opts)?;
    tilo_startup_metrics::handoff_mark("app-dirs-ready");
    Ok(LaunchHost {
        runtime,
        app_dir,
        person_dir,
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "one ordered launch pipeline; splitting it would scatter the policy checks that must precede spawn"
)]
pub(crate) fn launch_with_context(
    manifest: &ManifestView,
    payload_root: &Path,
    opts: &LaunchOptions,
    native_owner: Option<&dyn EnrolledProcessObserver>,
) -> Result<ExitStatus, LaunchError> {
    validate_open_uri(manifest, opts)?;
    // 0a.
    validate_launch_mode(manifest, opts)?;
    validate_interfaces(manifest)?;

    // Hold the payload and runtime trees before they are looked at, until the sandbox that mounts them
    // has exited: an update or collection meanwhile leaves them alone (`crate::lease`).
    let _leases = crate::lease::LaunchLeases::take(manifest, payload_root, opts)?;

    // The user's capability decision.
    let mut capabilities = resolve_capabilities(manifest, opts);

    // Host preflight precedes connected service ownership.
    let seccomp_fds = stage_seccomp(
        manifest,
        crate::seccomp::syscall_grants(
            manifest,
            &capabilities,
            package_store::layout::image_shipped(payload_root),
        ),
    )?;
    // Whether the app opens the GPU itself, or its workspace's render service draws it.
    let gpu_granted = crate::gpu::granted(
        manifest,
        &capabilities,
        package_store::layout::image_shipped(payload_root),
    );
    tilo_startup_metrics::handoff_mark("seccomp-staged");

    // D-Bus proxy decision.
    let (portals, graphical) = (&manifest.sandbox.portals, opts.wayland_socket.is_some());
    let media_player = dbus_proxy::media_player_for(manifest);
    let media_player = media_player.as_deref();
    let ((dbus_plan, _dbus_guards), host, required_runtimes) = std::thread::scope(|scope| {
        let dbus = spawn_beside(scope, "launch-dbus", move || {
            let bind = resolve_dbus_bind(portals, graphical, media_player);
            tilo_startup_metrics::handoff_mark("dbus-proxies-ready");
            bind
        })?;
        // Verifying a required runtime reads its whole payload (a browser engine is hundreds of MB);
        // nothing else here needs the answer before the sandbox argv is built.
        let required = spawn_beside(scope, "launch-required-runtimes", move || {
            let bundles = resolve_required_runtimes(manifest, opts);
            tilo_startup_metrics::handoff_mark("required-runtimes-verified");
            bundles
        })?;
        let host = prepare_launch_host(manifest, payload_root, opts);
        Ok::<_, LaunchError>((join_beside(dbus), host, join_beside(required)))
    })?;
    tilo_startup_metrics::handoff_mark("dbus-joined");
    let LaunchHost {
        runtime: resolved_runtime,
        app_dir,
        person_dir,
    } = host?;
    let runtime_arg = resolved_runtime
        .as_ref()
        .map(|(p, v)| (p.as_path(), v.as_str()));
    let required_runtimes = required_runtimes?;
    preflight_entrypoint_runtime(manifest, opts, runtime_arg, &required_runtimes)?;

    if let Some(owner) = native_owner
        && let Some(hosts) = owner.reviewed_network_hosts()?
    {
        if hosts.iter().any(|host| {
            host.is_empty() || host.contains('*') || host.parse::<std::net::IpAddr>().is_ok()
        }) {
            return Err(LaunchError::InvalidServiceLaunch(
                "owned service requires named reviewed hosts".into(),
            ));
        }
        capabilities.allow_network = !hosts.is_empty();
        capabilities.net_hosts = hosts;
    }
    // Connect only after cold runtime verification, loader preflight and host preparation.
    let egress = crate::egress_proxy::egress_access(manifest, &capabilities, payload_root);
    let package_id = manifest.package_id.as_str();
    let resolved = {
        // A connector holds none of the app's data: it gets no collaboration connection (the
        // instances in the app's tilolang process have the app's own).
        let mut services = manifest.sandbox.services.clone();
        if opts.tilolang_connector.is_some() {
            services.remove(crate::services::COLLAB_SERVICE);
        }
        let mut resolved = resolve_service_binds_for_launch(
            &services,
            opts.no_services,
            ServiceLaunchContext {
                app_id: package_id,
                workspace_id: opts.workspace_id.as_deref(),
            },
        )?;
        if opens_window(&opts.mode, opts.tilolang_connector.as_deref())
            && let Some(shell_ui) =
                resolve_shell_ui_bind(package_id, &egress.decision, opts.no_services)?
        {
            resolved.push(shell_ui);
        }
        if let Some(identity) = crate::services::resolve_identity_bind_for_contract(
            package_id,
            opts.workspace_id.as_deref(),
            opts.no_services,
            manifest
                .sandbox
                .services
                .get(crate::services::IDENTITY_SERVICE),
        )? {
            resolved.push(identity);
        }
        if let Some(blobs) =
            resolve_blob_bind(package_id, opts.workspace_id.as_deref(), opts.no_services)?
        {
            resolved.push(blobs);
        }
        if let Some(chooser) =
            resolve_chooser_bind(package_id, opts.workspace_id.as_deref(), opts.no_services)?
        {
            resolved.push(chooser);
        }
        // The person's clipboard, the same service a tilolang instance reaches: a copy in either
        // language is the person's copy. A launch without services, or a session that serves none,
        // hands none, and the app's copies and pastes are refused.
        if let Some(clipboard) = resolve_clipboard_bind(opts.no_services) {
            resolved.push(clipboard);
        }
        // Notifications, over the session's notifier: a window is what an app has to call the
        // person to, as the notification portal was granted to a graphical app. What it shows
        // reaches the same notification centre the desktop's own notices do.
        if opens_window(&opts.mode, opts.tilolang_connector.as_deref())
            && let Some(notify) = resolve_notify_bind(package_id, opts.no_services)?
        {
            resolved.push(notify);
        }
        #[cfg(target_os = "linux")]
        if matches!(opts.mode, LaunchMode::Gui)
            && !gpu_granted
            && let Some(render) = crate::services::resolve_render_bind(
                package_id,
                opts.workspace_id.as_deref(),
                opts.no_services,
            )?
        {
            resolved.push(render);
        }
        if let Some(stats) = crate::system_stats::resolve_bind(
            manifest,
            &capabilities,
            payload_root,
            opts.workspace_id.as_deref(),
            opts.no_services,
        )? {
            resolved.push(stats);
        }
        // An origin connector's socket, which the session released it with as a window's: the instances
        // of the app attach over it.
        #[cfg(target_os = "linux")]
        if opts.tilolang_connector.is_some()
            && let Some(link) = crate::launch_hold::take_window_link()
        {
            resolved.push(crate::services::ResolvedService {
                name: runtime_portal::origin_connector::SERVICE.to_owned(),
                host: link,
                child_fd: runtime_portal::origin_connector::CHILD_FD,
            });
        }
        // The connection to the app's origin connector, which the device's daemon made and
        // released with this launch: the process runs the instance here, so it reaches the
        // service over this rather than starting a connector of its own.
        #[cfg(target_os = "linux")]
        if opts.tilolang_origin_reader
            && let Some(link) = crate::launch_hold::take_window_link()
        {
            resolved.push(crate::services::ResolvedService {
                name: runtime_portal::origin_connector::READER.to_owned(),
                host: link,
                child_fd: runtime_portal::origin_connector::CHILD_FD,
            });
        }
        // The connections a capture's instance joins its workspace over, which the device's
        // daemon made and released with this launch after the origin connection.
        #[cfg(target_os = "linux")]
        if opts.tilolang_joined.is_some() {
            use runtime_portal::joined_capture as joined;
            let (Some(collaboration), Some(identity)) = (
                crate::launch_hold::take_window_link(),
                crate::launch_hold::take_window_link(),
            ) else {
                return Err(LaunchError::InvalidServiceLaunch(
                    "the launch names a joined instance, but its release handed no connections \
                     for it"
                        .into(),
                ));
            };
            resolved.push(crate::services::ResolvedService {
                name: joined::COLLABORATION.to_owned(),
                host: collaboration,
                child_fd: joined::COLLABORATION_CHILD_FD,
            });
            resolved.push(crate::services::ResolvedService {
                name: joined::IDENTITY.to_owned(),
                host: identity,
                child_fd: joined::IDENTITY_CHILD_FD,
            });
        }
        // The shell opens the app's later windows in this process through the link it released this
        // launch with (`runtime_portal::process_link`).
        #[cfg(target_os = "linux")]
        if matches!(opts.mode, LaunchMode::Gui)
            && opts.tilolang_connector.is_none()
            && let Some(link) = crate::launch_hold::take_window_link()
        {
            resolved.push(crate::services::ResolvedService {
                name: runtime_portal::process_link::SERVICE.to_owned(),
                host: link,
                child_fd: runtime_portal::process_link::CHILD_FD,
            });
        }
        resolved
    };
    let binds = to_service_binds(&resolved);
    tilo_startup_metrics::handoff_mark("services-resolved");

    let keep_fds: Vec<RawFd> = resolved.iter().map(|r| r.child_fd as RawFd).collect();

    // Egress: a grant naming specific hosts runs a check; `*` or bare `host` mode shares the host
    // network with no allowlist to enforce, so neither needs a check.
    let (egress_proxy, _proxy_guard) = start_egress_proxy(&egress, manifest.package_id.as_str())?;
    tilo_startup_metrics::handoff_mark("egress-ready");

    // Pick the entrypoint bin per launch mode and build the initial argv.
    let (mut argv, extra_setenv, cli_passthrough) = build_initial_argv(
        manifest,
        payload_root,
        opts,
        runtime_arg,
        &binds,
        dbus_plan.as_ref(),
        &capabilities,
    )?;
    let latency_sensitive = is_native_game_launch(manifest, opts);
    splice_cli_setenv(&mut argv, extra_setenv)?;
    if binds
        .iter()
        .any(|bind| bind.name == crate::services::COLLAB_SERVICE)
    {
        splice_cli_setenv(
            &mut argv,
            Some((runtime_portal::process_binding::ENV.into(), "1".into())),
        )?;
    }
    if let Some(joined) = &opts.tilolang_joined {
        // What the process joins its workspace as: the instance its connections were made for,
        // and the person at it.
        if runtime_portal::joined_capture::named(joined).is_none() {
            return Err(LaunchError::InvalidServiceLaunch(format!(
                "a joined instance is named `<instance> <workspace> <person>`, not {joined:?}"
            )));
        }
        splice_cli_setenv(
            &mut argv,
            Some((runtime_portal::joined_capture::ENV.into(), joined.clone())),
        )?;
    }
    if let Some(origin) = &opts.tilolang_connector {
        if !manifest
            .entrypoint
            .origins
            .iter()
            .any(|declared| declared == origin)
        {
            return Err(LaunchError::InvalidServiceLaunch(format!(
                "{} declares no `{origin}` origin to connect",
                manifest.package_id
            )));
        }
        splice_cli_setenv(
            &mut argv,
            Some((
                runtime_portal::origin_connector::ORIGIN_ENV.into(),
                origin.clone(),
            )),
        )?;
    }
    splice_game_ntsync(&mut argv, latency_sensitive)?;
    let ro_paths = splice_runtime_mounts_with(
        &mut argv,
        manifest,
        opts,
        resolved_runtime.is_some(),
        &required_runtimes,
    )?;
    splice_workspace_id_bind(&mut argv, opts)?;
    splice_tool_context(&mut argv)?;
    splice_app_dir(&mut argv, &app_dir, &person_dir, &capabilities)?;
    splice_session_settings(&mut argv, &manifest.package_id)?;
    let (system_authority, _system_bus_guard) = splice_system_authority(&mut argv, manifest)?;
    splice_trusted_identities(&mut argv, manifest, opts)?;
    splice_granted(&mut argv, opts)?;
    splice_action_authority(&mut argv, manifest, opts)?;
    let wayland_bind = splice_wayland(&mut argv, opts)?;
    splice_egress_proxy_bind(&mut argv, egress_proxy.as_ref())?;
    let (gpu, pipeline_seed) = splice_gpu(&mut argv, manifest, opts, gpu_granted)?;
    splice_startup_marks(&mut argv)?;
    splice_sandbox_shim(
        &mut argv,
        opts,
        &ShimSpliceCtx {
            manifest,
            egress_proxy: &egress_proxy,
            runtime_arg,
            extra_ro_paths: &ro_paths,
            egress: &egress.decision,
            capabilities: &capabilities,
            dbus_plan: &dbus_plan,
            system_authority: &system_authority,
            wayland_bind: &wayland_bind,
            gpu: &gpu,
            gpu_devices: gpu_granted,
            pipeline_seed: &pipeline_seed,
            keep_fds: &keep_fds,
        },
    )?;

    spawn_and_wait(
        argv,
        cli_passthrough,
        opts,
        ChildSetup {
            seccomp_fds,
            services: resolved,
            latency_sensitive,
        },
        &egress.decision,
        egress_proxy.as_ref(),
        native_owner,
        opts.data_home.as_deref().and_then(debug_output_log),
    )
}

/// Debug-only, off by default: when the launching host sets `TILO_LAUNCHER_DEBUG_STDERR=1`, a
/// supervised child's stdout and stderr go to a bounded log file in its data home instead of
/// `/dev/null`.
pub const DEBUG_STDERR_ENV: &str = "TILO_LAUNCHER_DEBUG_STDERR";
/// Upper bound of one launch's captured output.
const DEBUG_OUTPUT_LIMIT: usize = 1024 * 1024;

fn debug_output_log(data_home: &Path) -> Option<PathBuf> {
    (std::env::var_os(DEBUG_STDERR_ENV).as_deref() == Some(std::ffi::OsStr::new("1")))
        .then(|| data_home.join("launcher-debug-output.log"))
}

/// The last of what an app wrote, as one line of a launcher report: its most recent non-empty
/// lines, within a length a log reads.
fn last_lines(text: &str) -> String {
    const KEPT_LINES: usize = 20;
    const KEPT_BYTES: usize = 2000;
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let said = lines
        .get(lines.len().saturating_sub(KEPT_LINES)..)
        .unwrap_or_default()
        .join(" | ");
    if said.len() <= KEPT_BYTES {
        return said;
    }
    // The end is the part that says why it stopped: keep that, cut on a character boundary.
    let mut cut = said.len().saturating_sub(KEPT_BYTES);
    while !said.is_char_boundary(cut) {
        cut = cut.saturating_add(1);
    }
    match said.get(cut..) {
        Some(tail) => format!("…{tail}"),
        None => said,
    }
}

/// Copy `source` into `sink` until EOF or until `budget` bytes were written in total across all copiers
/// sharing it; the rest is drained and dropped so the child never blocks on a full pipe.
fn copy_bounded_output(
    mut source: impl std::io::Read,
    sink: &std::sync::Mutex<std::fs::File>,
    budget: &AtomicUsize,
) {
    use std::io::Write as _;
    let mut buf = [0u8; 8192];
    loop {
        let n = match source.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let granted = budget
            .fetch_update(AtomicOrdering::AcqRel, AtomicOrdering::Acquire, |left| {
                Some(left.saturating_sub(n))
            })
            .unwrap_or(0)
            .min(n);
        if granted > 0
            && let Ok(mut file) = sink.lock()
        {
            let _ = file.write_all(buf.get(..granted).unwrap_or_default());
        }
    }
}

fn validated_tool_context_from_env() -> Option<String> {
    let context = std::env::var("TILO_TOOL_CONTEXT").ok()?;
    if context.len() > 4 * 1024 {
        tracing::warn!("dropping oversized TILO_TOOL_CONTEXT");
        return None;
    }
    if let Ok(serde_json::Value::Object(_)) = serde_json::from_str::<serde_json::Value>(&context) {
        Some(context)
    } else {
        tracing::warn!("dropping malformed TILO_TOOL_CONTEXT");
        None
    }
}

/// Finish building `argv` (seccomp prefix + passthrough tail), spawn `bwrap` with its stdout piped,
/// capture it, and wait.
fn spawn_and_capture(
    mut argv: Vec<String>,
    passthrough: Vec<String>,
    opts: &LaunchOptions,
    seccomp_fds: SeccompFds,
    resolved: Vec<crate::services::ResolvedService>,
) -> Result<(ExitStatus, Vec<u8>), LaunchError> {
    SeccompFds::prepend_bwrap_args(&mut argv);
    let account_fd = stage_sandbox_account(&mut argv)?;
    let host_account_fd = account_fd.as_raw_fd();
    // Everything after the entrypoint token is opaque to bwrap and becomes the child's argv[1..].
    argv.extend(passthrough);

    let bwrap = opts
        .bwrap_bin
        .clone()
        .unwrap_or_else(|| PathBuf::from("bwrap"));
    let mut cmd = Command::new(&bwrap);
    cmd.args(&argv);
    clear_bwrap_environment(&mut cmd);
    cmd.stdout(std::process::Stdio::piped());

    let mut service_fds: Vec<(RawFd, RawFd)> = resolved
        .iter()
        .map(|r| (r.host_fd(), r.child_fd as RawFd))
        .collect();
    service_fds.extend(seccomp_fds.child_fds(host_account_fd));
    unsafe {
        cmd.pre_exec(move || {
            place_child_fds(&mut service_fds)?;
            set_sandbox_resource_limits()?;
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(LaunchError::Spawn)?;
    drop(seccomp_fds);
    drop(account_fd);
    // As in `launch_with`: the child has its own copies, so holding these open only keeps the service
    // from seeing the app end.
    drop(resolved);
    let mut buf = Vec::new();
    if let Some(mut so) = child.stdout.take() {
        use std::io::Read;
        so.read_to_end(&mut buf)?;
    }
    let status = child.wait()?;
    Ok((status, buf))
}

fn reserve_output(counter: &AtomicUsize, requested: usize, limit: usize) -> usize {
    let mut current = counter.load(AtomicOrdering::Acquire);
    loop {
        let available = limit.saturating_sub(current);
        let accepted = requested.min(available);
        let next = current.saturating_add(accepted);
        match counter.compare_exchange_weak(
            current,
            next,
            AtomicOrdering::AcqRel,
            AtomicOrdering::Acquire,
        ) {
            Ok(_) => return accepted,
            Err(observed) => current = observed,
        }
    }
}

fn drain_bounded(
    mut reader: impl std::io::Read,
    used: &AtomicUsize,
    exceeded: &AtomicBool,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    let mut captured = Vec::new();
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let accepted = reserve_output(used, read, limit);
        if let Some(bytes) = chunk.get(..accepted) {
            captured.extend_from_slice(bytes);
        }
        if accepted != read {
            exceeded.store(true, AtomicOrdering::Release);
        }
    }
    Ok(captured)
}

#[cfg(test)]
mod captured_resource_tests {
    use super::*;

    fn plan() -> CapturedResourcePlan {
        CapturedResourcePlan {
            cpu_seconds: 2,
            memory_bytes: 64 * 1024 * 1024,
            output_bytes: 8,
            wall_time: Duration::from_secs(3),
        }
    }

    #[test]
    fn captured_output_is_bounded_and_cancellable() {
        {
            assert!(plan().validate().is_ok());
            for invalid in [
                CapturedResourcePlan {
                    cpu_seconds: 0,
                    ..plan()
                },
                CapturedResourcePlan {
                    memory_bytes: CapturedResourcePlan::MAX_MEMORY_BYTES.saturating_add(1),
                    ..plan()
                },
                CapturedResourcePlan {
                    output_bytes: 0,
                    ..plan()
                },
                CapturedResourcePlan {
                    wall_time: CapturedResourcePlan::MAX_WALL_TIME + Duration::from_millis(1),
                    ..plan()
                },
            ] {
                assert!(matches!(
                    invalid.validate(),
                    Err(crate::capture::InvalidCapturePlan)
                ));
            }
        }
        {
            let used = Arc::new(AtomicUsize::new(0));
            let exceeded = Arc::new(AtomicBool::new(false));
            let first = drain_bounded(
                std::io::Cursor::new(b"123456"),
                used.as_ref(),
                exceeded.as_ref(),
                8,
            )
            .unwrap();
            let second = drain_bounded(
                std::io::Cursor::new(b"abcdef"),
                used.as_ref(),
                exceeded.as_ref(),
                8,
            )
            .unwrap();
            assert_eq!(first, b"123456");
            assert_eq!(second, b"ab");
            assert_eq!(used.load(AtomicOrdering::Acquire), 8);
            assert!(exceeded.load(AtomicOrdering::Acquire));
        }
        {
            let cancellation = CaptureCancellation::default();
            let observer = cancellation.clone();
            assert!(!observer.is_cancelled());
            cancellation.cancel();
            assert!(observer.is_cancelled());
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the bounded supervisor keeps pipe ownership, process-group teardown, and every terminal reason in one auditable transaction"
)]
fn spawn_and_capture_bounded(
    mut argv: Vec<String>,
    passthrough: Vec<String>,
    opts: &LaunchOptions,
    seccomp_fds: SeccompFds,
    resolved: Vec<crate::services::ResolvedService>,
    plan: CapturedResourcePlan,
    cancellation: &CaptureCancellation,
) -> Result<CapturedLaunchOutput, LaunchError> {
    let plan = plan.validate()?;
    SeccompFds::prepend_bwrap_args(&mut argv);
    let account_fd = stage_sandbox_account(&mut argv)?;
    let host_account_fd = account_fd.as_raw_fd();
    argv.extend(passthrough);

    let bwrap = opts
        .bwrap_bin
        .clone()
        .unwrap_or_else(|| PathBuf::from("bwrap"));
    let mut cmd = Command::new(&bwrap);
    cmd.args(&argv);
    clear_bwrap_environment(&mut cmd);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.stdin(std::process::Stdio::null());

    let mut service_fds: Vec<(RawFd, RawFd)> = resolved
        .iter()
        .map(|service| (service.host_fd(), service.child_fd as RawFd))
        .collect();
    service_fds.extend(seccomp_fds.child_fds(host_account_fd));
    unsafe {
        cmd.pre_exec(move || {
            place_child_fds(&mut service_fds)?;
            set_sandbox_resource_limits()?;
            set_captured_resource_limits(plan)?;
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(LaunchError::Spawn)?;
    drop(seccomp_fds);
    drop(account_fd);
    drop(resolved);
    let used = Arc::new(AtomicUsize::new(0));
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout = child.stdout.take().ok_or_else(|| {
        LaunchError::Io(std::io::Error::other(
            "captured worker stdout was not piped",
        ))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        LaunchError::Io(std::io::Error::other(
            "captured worker stderr was not piped",
        ))
    })?;
    let stdout_used = Arc::clone(&used);
    let stdout_exceeded = Arc::clone(&exceeded);
    let stdout_thread = std::thread::spawn(move || {
        drain_bounded(
            stdout,
            stdout_used.as_ref(),
            stdout_exceeded.as_ref(),
            plan.output_bytes,
        )
    });
    let stderr_used = Arc::clone(&used);
    let stderr_exceeded = Arc::clone(&exceeded);
    let stderr_thread = std::thread::spawn(move || {
        drain_bounded(
            stderr,
            stderr_used.as_ref(),
            stderr_exceeded.as_ref(),
            plan.output_bytes,
        )
    });

    let deadline = Instant::now()
        .checked_add(plan.wall_time)
        .ok_or(LaunchError::InvalidCapturePlan)?;
    let termination = loop {
        if let Some(status) = child.try_wait()? {
            break CapturedTermination::Exited(status);
        }
        let reason = if cancellation.is_cancelled() {
            Some(CapturedTermination::Cancelled)
        } else if exceeded.load(AtomicOrdering::Acquire) {
            Some(CapturedTermination::OutputLimit)
        } else if Instant::now() >= deadline {
            Some(CapturedTermination::TimedOut)
        } else {
            None
        };
        if let Some(reason) = reason {
            bounded_command::terminate_tree(&mut child, Duration::ZERO);
            break reason;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_thread
        .join()
        .map_err(|_| LaunchError::Io(std::io::Error::other("stdout capture thread panicked")))??;
    let stderr = stderr_thread
        .join()
        .map_err(|_| LaunchError::Io(std::io::Error::other("stderr capture thread panicked")))??;
    Ok(CapturedLaunchOutput {
        termination,
        stdout,
        stderr,
    })
}

/// [`launch_with`] sibling that captures the child's stdout instead of inheriting it.
pub fn launch_with_captured(
    manifest: &ManifestView,
    payload_root: &Path,
    opts: &LaunchOptions,
) -> Result<(ExitStatus, Vec<u8>), LaunchError> {
    launch_with_capture_spawn(manifest, payload_root, opts, spawn_and_capture)
}

/// Captured sandbox launch with launcher-enforced resource limits and cancellation.
pub fn launch_with_captured_bounded(
    manifest: &ManifestView,
    payload_root: &Path,
    opts: &LaunchOptions,
    plan: CapturedResourcePlan,
    cancellation: CaptureCancellation,
) -> Result<CapturedLaunchOutput, LaunchError> {
    plan.validate()?;
    launch_with_capture_spawn(
        manifest,
        payload_root,
        opts,
        move |argv, passthrough, opts, seccomp, services| {
            spawn_and_capture_bounded(
                argv,
                passthrough,
                opts,
                seccomp,
                services,
                plan,
                &cancellation,
            )
        },
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "the captured launch mirrors `launch_with_context`'s ordered pipeline step for step"
)]
fn launch_with_capture_spawn<T>(
    manifest: &ManifestView,
    payload_root: &Path,
    opts: &LaunchOptions,
    spawn: impl FnOnce(
        Vec<String>,
        Vec<String>,
        &LaunchOptions,
        SeccompFds,
        Vec<crate::services::ResolvedService>,
    ) -> Result<T, LaunchError>,
) -> Result<T, LaunchError> {
    if opts.expected_package_hash.is_some() {
        return Err(LaunchError::InvalidServiceLaunch(
            "original signed package requires manifest-backed run".into(),
        ));
    }
    validate_open_uri(manifest, opts)?;
    // Same refusals as `launch_with`: this is a second entry into the sandbox builder, and a policy with
    // one enforced route is a route beside it.
    validate_launch_mode(manifest, opts)?;
    validate_interfaces(manifest)?;

    // Seccomp baseline resolved identically to `launch_with` — two paths reaching different conclusions
    // would make capture a way around the filter.
    let capabilities = resolve_capabilities(manifest, opts);
    let seccomp_fds = stage_seccomp(
        manifest,
        crate::seccomp::syscall_grants(
            manifest,
            &capabilities,
            package_store::layout::image_shipped(payload_root),
        ),
    )?;
    // Whether the app opens the GPU itself, or its workspace's render service draws it.
    let gpu_granted = crate::gpu::granted(
        manifest,
        &capabilities,
        package_store::layout::image_shipped(payload_root),
    );

    // As in `launch_with_context`: held from before the lookup until the sandbox has exited.
    let _leases = crate::lease::LaunchLeases::take(manifest, payload_root, opts)?;
    // Build the exact production sandbox shape.
    let resolved_runtime = resolve_runtime(manifest, opts)?;
    let runtime_arg = resolved_runtime
        .as_ref()
        .map(|(p, v)| (p.as_path(), v.as_str()));
    preflight_runtime_libraries(manifest, payload_root, opts, runtime_arg)?;
    // D-Bus plan (same decision path as `launch_with`).
    let (dbus_plan, _dbus_guards) = resolve_dbus_bind(
        &manifest.sandbox.portals,
        opts.wayland_socket.is_some(),
        dbus_proxy::media_player_for(manifest).as_deref(),
    );
    // As in `launch_with_context`, cold runtime checks precede service handoff.
    let egress = crate::egress_proxy::egress_access(manifest, &capabilities, payload_root);
    let package_id = manifest.package_id.as_str();
    let mut resolved = resolve_service_binds_for_launch(
        &manifest.sandbox.services,
        opts.no_services,
        ServiceLaunchContext {
            app_id: package_id,
            workspace_id: opts.workspace_id.as_deref(),
        },
    )?;
    if opens_window(&opts.mode, opts.tilolang_connector.as_deref())
        && let Some(shell_ui) =
            resolve_shell_ui_bind(package_id, &egress.decision, opts.no_services)?
    {
        resolved.push(shell_ui);
    }
    if let Some(identity) = crate::services::resolve_identity_bind_for_contract(
        package_id,
        opts.workspace_id.as_deref(),
        opts.no_services,
        manifest
            .sandbox
            .services
            .get(crate::services::IDENTITY_SERVICE),
    )? {
        resolved.push(identity);
    }
    if let Some(blobs) =
        resolve_blob_bind(package_id, opts.workspace_id.as_deref(), opts.no_services)?
    {
        resolved.push(blobs);
    }
    if let Some(chooser) =
        resolve_chooser_bind(package_id, opts.workspace_id.as_deref(), opts.no_services)?
    {
        resolved.push(chooser);
    }
    // The person's clipboard, the same service a tilolang instance reaches: a copy in either
    // language is the person's copy. A launch without services, or a session that serves none,
    // hands none, and the app's copies and pastes are refused.
    if let Some(clipboard) = resolve_clipboard_bind(opts.no_services) {
        resolved.push(clipboard);
    }
    if opens_window(&opts.mode, opts.tilolang_connector.as_deref())
        && let Some(notify) = resolve_notify_bind(package_id, opts.no_services)?
    {
        resolved.push(notify);
    }
    #[cfg(target_os = "linux")]
    if matches!(opts.mode, LaunchMode::Gui)
        && !gpu_granted
        && let Some(render) = crate::services::resolve_render_bind(
            package_id,
            opts.workspace_id.as_deref(),
            opts.no_services,
        )?
    {
        resolved.push(render);
    }
    if let Some(stats) = crate::system_stats::resolve_bind(
        manifest,
        &capabilities,
        payload_root,
        opts.workspace_id.as_deref(),
        opts.no_services,
    )? {
        resolved.push(stats);
    }
    let binds = to_service_binds(&resolved);

    let keep_fds: Vec<RawFd> = resolved.iter().map(|r| r.child_fd as RawFd).collect();
    // Same egress decision as `launch_with`: without the check spliced in here too, a host-scoped grant
    // would be silently offline.
    let (egress_proxy, _proxy_guard) = start_egress_proxy(&egress, manifest.package_id.as_str())?;
    // Entrypoint + trailing arguments, resolved the same way `launch_with` does — both entries must agree
    // on binary and arguments.
    let (mut argv, extra_setenv, passthrough) = build_initial_argv(
        manifest,
        payload_root,
        opts,
        runtime_arg,
        &binds,
        dbus_plan.as_ref(),
        &capabilities,
    )?;
    splice_cli_setenv(&mut argv, extra_setenv)?;
    if binds
        .iter()
        .any(|bind| bind.name == crate::services::COLLAB_SERVICE)
    {
        splice_cli_setenv(
            &mut argv,
            Some((runtime_portal::process_binding::ENV.into(), "1".into())),
        )?;
    }
    let ro_paths = splice_runtime_mounts(&mut argv, manifest, opts, runtime_arg)?;
    splice_tool_context(&mut argv)?;
    splice_egress_proxy_bind(&mut argv, egress_proxy.as_ref())?;
    splice_workspace_id_bind(&mut argv, opts)?;
    let (app_dir, person_dir) = resolve_launch_dirs(manifest, opts)?;
    splice_app_dir(&mut argv, &app_dir, &person_dir, &capabilities)?;
    splice_session_settings(&mut argv, &manifest.package_id)?;
    let (system_authority, _system_bus_guard) = splice_system_authority(&mut argv, manifest)?;
    splice_trusted_identities(&mut argv, manifest, opts)?;
    splice_granted(&mut argv, opts)?;
    splice_action_authority(&mut argv, manifest, opts)?;
    let wayland_bind = splice_wayland(&mut argv, opts)?;
    let (gpu, pipeline_seed) = splice_gpu(&mut argv, manifest, opts, gpu_granted)?;
    splice_sandbox_shim(
        &mut argv,
        opts,
        &ShimSpliceCtx {
            manifest,
            egress_proxy: &egress_proxy,
            runtime_arg,
            extra_ro_paths: &ro_paths,
            egress: &egress.decision,
            capabilities: &capabilities,
            dbus_plan: &dbus_plan,
            system_authority: &system_authority,
            wayland_bind: &wayland_bind,
            gpu: &gpu,
            gpu_devices: gpu_granted,
            pipeline_seed: &pipeline_seed,
            keep_fds: &keep_fds,
        },
    )?;

    spawn(argv, passthrough, opts, seccomp_fds, resolved)
}

/// Give `bwrap` itself an environment worth inheriting, so a dropped `--clearenv` cannot silently
/// become a leak.
fn clear_bwrap_environment(cmd: &mut Command) {
    cmd.env_clear();
    cmd.env("PATH", crate::sandbox::SANDBOX_PATH);
}

/// Write the seccomp blob to a `memfd` so its fd outlives the parent's tempdir teardown and the bwrap
/// exec; falls back to a real tempfile.
fn stage_seccomp_blob(bytes: &[u8]) -> std::io::Result<OwnedFd> {
    // Try memfd first.
    let name = b"tilo-launcher-seccomp\0";
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

    // Fallback: O_TMPFILE under /tmp.
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
        // `written < bytes.len()` holds here, so this cannot run past the buffer in practice, but a
        // real file offset is not the place to trust that silently: an error beats a payload written
        // to the wrong offset.
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
        // Dynamic linker's cache.
        "/etc/ld.so.cache",
    ]
}

/// Path inside the bwrap sandbox where the launcher binary is bind-mounted to be exec'd as the Landlock
/// shim.
pub const SHIM_MOUNT_PATH: &str = "/tilo-launcher";

/// Decide how to expose D-Bus: the manifest's portal declaration and, for a graphical app, the
/// notification portal.
fn resolve_dbus_bind(
    portals: &std::collections::BTreeMap<String, PortalPolicy>,
    graphical: bool,
    media_player: Option<&str>,
) -> (Option<DBusBindPlan>, Vec<DBusProxyGuard>) {
    let (runtime, pid) = (dbus_proxy::session_runtime_dir(), std::process::id());
    let sockets = ProxySockets {
        session: dbus_proxy::proxy_socket_path(&runtime, pid),
        ready_within: dbus_proxy::PROXY_READY_TIMEOUT,
    };
    resolve_dbus_bind_at(portals, graphical, media_player, &sockets)
}

/// Where one launch's D-Bus proxy listens on the host, and how long it may take to report its
/// listener ready.
struct ProxySockets {
    session: PathBuf,
    ready_within: std::time::Duration,
}

fn resolve_dbus_bind_at(
    portals: &std::collections::BTreeMap<String, PortalPolicy>,
    graphical: bool,
    media_player: Option<&str>,
    sockets: &ProxySockets,
) -> (Option<DBusBindPlan>, Vec<DBusProxyGuard>) {
    if dbus_proxy::portal_filter_args(portals, graphical).is_empty() && media_player.is_none() {
        return (None, Vec::new());
    }
    let Ok(bus_addr) = std::env::var("DBUS_SESSION_BUS_ADDRESS") else {
        tracing::warn!(
            "DBUS_SESSION_BUS_ADDRESS is unset; portals and accessibility will be \
             unavailable to the sandboxed app"
        );
        return (None, Vec::new());
    };
    if bus_addr.is_empty() {
        return (None, Vec::new());
    }
    let uid = current_uid();
    let Some(binary) = dbus_proxy::find_proxy_binary() else {
        tracing::warn!(
            "xdg-dbus-proxy unavailable; portals and accessibility stay unavailable. \
             Check TILO_LAUNCHER_DBUS_PROXY_BIN or install `xdg-dbus-proxy` \
             to enable filtered bus traffic."
        );
        return (None, Vec::new());
    };

    // The accessibility bus is never proxied: the session publishes each window's tree itself,
    // so nothing of an app process reaches it.
    let filter = dbus_proxy::session_filter_args(portals, graphical, media_player);
    let filter = filter.iter().map(String::as_str).collect::<Vec<_>>();
    let session = match dbus_proxy::spawn_filtered_proxy_within(
        &binary,
        &bus_addr,
        &sockets.session,
        &filter,
        sockets.ready_within,
    ) {
        Ok(guard) => guard,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "xdg-dbus-proxy failed to spawn; the session bus stays unavailable"
            );
            return (None, Vec::new());
        }
    };
    let plan = DBusBindPlan {
        host_socket: session.socket_path().to_path_buf(),
        sandbox_path: dbus_proxy::sandbox_bus_path(uid),
    };
    tracing::info!(
        proxy_binary = %binary.display(),
        socket = %plan.host_socket.display(),
        "filtered D-Bus proxy spawned"
    );
    (Some(plan), vec![session])
}

#[cfg(test)]
mod dbus_resolution_tests {
    use super::*;

    tilo_test_env::scenario! {
        fn the_session_bus_reaches_a_launch_only_through_its_proxies;

        phase a_missing_filter_proxy_never_falls_back_to_the_raw_session_bus;
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(
                crate::dbus_proxy::PROXY_BIN_ENV,
                "/definitely/missing/xdg-dbus-proxy",
            )
            .set("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1234/bus");
            let portals = std::collections::BTreeMap::from([(
                "file-chooser".to_string(),
                PortalPolicy::ViaHost,
            )]);

            let (plan, guards) = resolve_dbus_bind(&portals, true, None);

            assert!(plan.is_none(), "a raw D-Bus bind must never be planned");
            assert!(guards.is_empty(), "no proxy process should have started");
        }

        phase only_the_session_proxy_starts_for_a_graphical_launch;
        {
            use std::os::unix::fs::PermissionsExt;
            let tmp = tempfile::tempdir().unwrap();
            let proxy = tmp.path().join("xdg-dbus-proxy");
            std::fs::write(
                &proxy,
                r#"#!/bin/sh
notify=0
if [ "$1" = --fd=0 ]; then notify=1; shift; fi
: >"$2"
if [ "$notify" = 1 ]; then printf x >&0; fi
exec sleep infinity
"#,
            )
            .unwrap();
            std::fs::set_permissions(&proxy, std::fs::Permissions::from_mode(0o755)).unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(crate::dbus_proxy::PROXY_BIN_ENV, &proxy)
                .set("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1234/bus")
                .set("AT_SPI_BUS_ADDRESS", "unix:path=/run/user/1234/at-spi/bus");
            let sockets = ProxySockets {
                session: tmp.path().join("sockets/1.sock"),
                ready_within: tilo_test_env::hang_bound(),
            };

            let (plan, guards) =
                resolve_dbus_bind_at(&std::collections::BTreeMap::new(), true, None, &sockets);

            let plan = plan.expect("the session proxy became ready");
            assert_eq!(plan.host_socket, sockets.session);
            assert_eq!(guards.len(), 1, "no accessibility proxy is spawned");
            // Nothing of the launch points an app at an accessibility bus.
            assert!(
                !tmp.path().join("sockets/1-atspi.sock").exists(),
                "the launcher starts no AT-SPI proxy"
            );
            drop(guards);
            assert!(!sockets.session.exists());
        }
    }
}

/// Current process UID via libc, avoiding a `users`/`nix` dependency.
fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

#[cfg(test)]
mod app_dir_tests {
    use super::*;

    fn manifest(id: &str) -> crate::manifest::ManifestView {
        crate::manifest::test_manifest(
            &format!("id = {id:?}\nversion = \"0.1.0\"\nkind = \"app\""),
            "[entrypoint]\nbin = \"bin/x\"\n",
        )
    }

    fn opts_in(workspace: Option<&str>) -> LaunchOptions {
        LaunchOptions {
            workspace_id: workspace.map(str::to_string),
            ..LaunchOptions::default()
        }
    }

    fn opts_at(data_home: &Path, workspace: Option<&str>) -> LaunchOptions {
        LaunchOptions {
            data_home: Some(data_home.to_path_buf()),
            ..opts_in(workspace)
        }
    }

    tilo_test_env::scenario! {
        fn the_app_directory_and_settings_binds_come_from_the_workspace;

        phase the_app_directory_is_resolved_per_workspace_and_never_from_the_manifest;
        {
            {
                let m = manifest("so.tilo.fixture");
                let workspace = resolve_capabilities(&m, &opts_in(Some("workspace-a")));
                assert_eq!(
                    workspace.participation,
                    crate::capabilities::Participation::ReadOnly
                );

                let personal = resolve_capabilities(&m, &opts_in(None));
                assert_eq!(
                    personal.participation,
                    crate::capabilities::Participation::Unscoped
                );

                let dir = tempfile::tempdir().expect("tempdir");
                let file = dir.path().join("grants.json");
                std::fs::write(
                    &file,
                    serde_json::to_vec(&tilo_permissions::caps::LaunchGrants {
                        author: true,
                        sandbox: Vec::new(),
                    })
                    .expect("encodes"),
                )
                .expect("written");
                let authored = resolve_capabilities(
                    &m,
                    &LaunchOptions {
                        capabilities_path: Some(file),
                        ..opts_in(Some("workspace-a"))
                    },
                );
                assert_eq!(
                    authored.participation,
                    crate::capabilities::Participation::Author
                );
            }
            {
                let data = tempfile::tempdir().expect("tempdir");
                let m = manifest("so.tilo.fixture");
                let a = resolve_person_dir(&m, &opts_at(data.path(), Some("workspace-a")))
                    .expect("workspace a");
                let personal = resolve_person_dir(&m, &opts_at(data.path(), None)).expect("personal");
                assert!(a.is_dir());
                assert_eq!(a, personal);
                assert!(a.starts_with(data.path().join("tilo-person")));
                let workspace =
                    resolve_app_dir(&m, &opts_at(data.path(), Some("workspace-a"))).expect("workspace");
                assert!(!a.starts_with(&workspace) && !workspace.starts_with(&a));
                let other = resolve_person_dir(&manifest("so.tilo.other"), &opts_at(data.path(), None))
                    .expect("another app");
                assert_ne!(a, other, "an app keeps only its own records of the person");
            }
        }

        phase settings_binds_its_accounts_but_never_the_session_settings_and_other_apps_bind_nothing;
        {
            let config = tempfile::tempdir().expect("session config");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", config.path());

            let mut argv = vec!["/entry".to_string()];
            splice_session_settings(&mut argv, "so.tilo.notes").expect("notes");
            assert_eq!(argv, vec!["/entry".to_string()]);

            splice_session_settings(&mut argv, "so.tilo.settings").expect("settings");
            let host = config.path().join("tilo");
            let accounts = host.join("accounts");
            assert!(accounts.is_dir(), "accounts must exist to bind read-write");
            assert!(
                argv.windows(3).any(|w| w[0] == "--bind"
                    && w[1] == accounts.to_string_lossy()
                    && w[2] == "/run/tilo/app/.config/tilo/accounts"),
                "Settings must bind accounts over remapped XDG: {argv:?}"
            );
            assert!(
                !argv.iter().any(|arg| arg == host.to_string_lossy().as_ref()
                    || arg == crate::appdir::SANDBOX_SESSION_CONFIG),
                "the whole session configuration root must never be bound: {argv:?}"
            );
            for private in [
                "identity",
                "trusted-attestors.toml",
                "revoked-keys.toml",
                "registries.toml",
                "records",
            ] {
                assert!(
                    !argv.iter().any(|arg| arg.ends_with(private)),
                    "{private} must stay out of the Settings sandbox: {argv:?}"
                );
            }
            // The settings document is the shell's: Settings reaches it through the settings service on
            // its channel, like every app, not a bind.
            assert!(
                !argv
                    .iter()
                    .any(|arg| arg.ends_with("/so.tilo.settings-device")
                        || arg.ends_with("/so.tilo.settings-device/state.automerge")),
                "Settings must not bind or name the session document: {argv:?}"
            );
            assert_eq!(argv.last().map(String::as_str), Some("/entry"));
        }
    }
}

#[cfg(test)]
mod managed_spawn_policy_tests {
    use super::*;
    struct CurrentPolicy {
        allowed: bool,
        checked: AtomicUsize,
        spawned: AtomicUsize,
    }
    impl EnrolledProcessObserver for CurrentPolicy {
        fn before_spawn(&self) -> std::io::Result<()> {
            self.checked.fetch_add(1, AtomicOrdering::SeqCst);
            if self.allowed {
                Ok(())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "revoked current policy",
                ))
            }
        }
        fn child_created(&self) {
            self.spawned.fetch_add(1, AtomicOrdering::SeqCst);
        }
        fn supervise(&self, mut child: std::process::Child) -> std::io::Result<ExitStatus> {
            child.wait()
        }
    }
    struct NativeOwner {
        allowed: bool,
        checked: AtomicUsize,
        supervised: AtomicUsize,
        own_group: AtomicBool,
    }
    impl EnrolledProcessObserver for NativeOwner {
        fn owns_process_group(&self) -> bool {
            true
        }
        fn child_created(&self) {}
        fn before_spawn(&self) -> std::io::Result<()> {
            self.checked.fetch_add(1, AtomicOrdering::SeqCst);
            if self.allowed {
                Ok(())
            } else {
                Err(std::io::ErrorKind::PermissionDenied.into())
            }
        }
        fn supervise(&self, mut child: std::process::Child) -> std::io::Result<ExitStatus> {
            self.supervised.fetch_add(1, AtomicOrdering::SeqCst);
            // The actual unreaped child owns its group; no ambient session group can be handed to
            // cancellation logic as though it were this service.
            let pid = i32::try_from(child.id()).map_err(std::io::Error::other)?;
            self.own_group
                .store(unsafe { libc::getpgid(pid) } == pid, AtomicOrdering::SeqCst);
            child.wait()
        }
    }
    struct OrdinaryPrivateService {
        info: OwnedFd,
        checked: AtomicUsize,
        spawned: AtomicUsize,
    }
    impl EnrolledProcessObserver for OrdinaryPrivateService {
        fn namespace_init_fd(&self) -> Option<RawFd> {
            Some(self.info.as_raw_fd())
        }
        fn before_spawn(&self) -> std::io::Result<()> {
            self.checked.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        }
        fn child_created(&self) {
            self.spawned.fetch_add(1, AtomicOrdering::SeqCst);
        }
        fn supervise(&self, mut child: std::process::Child) -> std::io::Result<ExitStatus> {
            child.wait()
        }
    }
    tilo_test_env::scenario! {
        fn a_prepared_spawn_rechecks_policy_before_any_child;

        phase owned_service_prepared_spawn_rechecks_source_and_hands_over_own_child;
        {
            // The real final spawn boundary, not runtime/package qualification.
            for allowed in [false, true] {
                let owner = NativeOwner {
                    allowed,
                    checked: AtomicUsize::new(0),
                    supervised: AtomicUsize::new(0),
                    own_group: AtomicBool::new(false),
                };
                let result = spawn_and_wait(
                    vec![],
                    vec![],
                    &LaunchOptions {
                        bwrap_bin: Some("/usr/bin/true".into()),
                        ..Default::default()
                    },
                    ChildSetup {
                        seccomp_fds: SeccompFds {
                            clone3_compat: tempfile::tempfile().unwrap().into(),
                            baseline: tempfile::tempfile().unwrap().into(),
                        },
                        services: vec![],
                        latency_sensitive: false,
                    },
                    &crate::egress_proxy::EgressDecision::Blocked,
                    None,
                    Some(&owner),
                    None,
                );
                assert_eq!(owner.checked.load(AtomicOrdering::SeqCst), 1);
                assert_eq!(
                    owner.supervised.load(AtomicOrdering::SeqCst),
                    usize::from(allowed)
                );
                assert_eq!(owner.own_group.load(AtomicOrdering::SeqCst), allowed);
                if allowed {
                    assert!(result.unwrap().success());
                } else {
                    assert!(matches!(result, Err(LaunchError::Spawn(error))
                        if error.kind() == std::io::ErrorKind::PermissionDenied));
                }
            }
        }

        phase ordinary_private_service_missing_full_namespace_plan_refuses_before_spawn;
        {
            // Exercise the actual final command boundary with no capture token.
            for flags in [
                Vec::<String>::new(),
                [
                    "--unshare-pid",
                    "--unshare-net",
                    "--unshare-ipc",
                    "--unshare-uts",
                ]
                .map(String::from)
                .to_vec(),
            ] {
                let owner = OrdinaryPrivateService {
                    info: tempfile::tempfile().unwrap().into(),
                    checked: AtomicUsize::new(0),
                    spawned: AtomicUsize::new(0),
                };
                let result = spawn_and_wait(
                    flags,
                    vec![],
                    &LaunchOptions {
                        bwrap_bin: Some("/usr/bin/bwrap".into()),
                        ..Default::default()
                    },
                    ChildSetup {
                        seccomp_fds: SeccompFds {
                            clone3_compat: tempfile::tempfile().unwrap().into(),
                            baseline: tempfile::tempfile().unwrap().into(),
                        },
                        services: vec![],
                        latency_sensitive: false,
                    },
                    &crate::egress_proxy::EgressDecision::Blocked,
                    None,
                    Some(&owner),
                    None,
                );
                assert!(matches!(result, Err(LaunchError::InvalidServiceLaunch(reason))
                    if reason == "private service namespace missing"));
                assert_eq!(owner.checked.load(AtomicOrdering::SeqCst), 0);
                assert_eq!(owner.spawned.load(AtomicOrdering::SeqCst), 0);
            }
        }

        phase managed_prepared_spawn_checks_current_policy_before_child_creation;
        {
            // This exercises the real prepared-command spawn boundary, not a package, sandbox or live
            // managed process proof.
            for allowed in [false, true] {
                let policy = CurrentPolicy {
                    allowed,
                    checked: AtomicUsize::new(0),
                    spawned: AtomicUsize::new(0),
                };
                let options = LaunchOptions {
                    bwrap_bin: Some("/usr/bin/true".into()),
                    ..Default::default()
                };
                let result = spawn_and_wait(
                    vec![],
                    vec![],
                    &options,
                    ChildSetup {
                        seccomp_fds: SeccompFds {
                            clone3_compat: tempfile::tempfile().unwrap().into(),
                            baseline: tempfile::tempfile().unwrap().into(),
                        },
                        services: vec![],
                        latency_sensitive: false,
                    },
                    &crate::egress_proxy::EgressDecision::Blocked,
                    None,
                    Some(&policy),
                    None,
                );
                assert_eq!(policy.checked.load(AtomicOrdering::SeqCst), 1);
                assert_eq!(
                    policy.spawned.load(AtomicOrdering::SeqCst),
                    usize::from(allowed)
                );
                if allowed {
                    assert!(
                        result
                            .expect("real Linux prepared-command spawn required")
                            .success()
                    );
                } else {
                    assert!(
                        matches!(result, Err(LaunchError::Spawn(ref error)) if error.kind() == std::io::ErrorKind::PermissionDenied)
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod debug_output_tests {
    use super::*;

    tilo_test_env::scenario! {
        fn debug_output_is_bounded_and_off_by_default;

        phase debug_output_copy_is_bounded_and_drains;
        {
            let path = tempfile::NamedTempFile::new().unwrap();
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(path.path())
                .unwrap();
            let sink = Arc::new(std::sync::Mutex::new(file));
            let budget = Arc::new(AtomicUsize::new(10));
            copy_bounded_output(&b"0123456"[..], &sink, &budget);
            copy_bounded_output(&b"789abcdef"[..], &sink, &budget);
            assert_eq!(std::fs::read(path.path()).unwrap(), b"0123456789");
            assert_eq!(budget.load(AtomicOrdering::SeqCst), 0);
        }

        phase debug_output_is_off_by_default;
        {
            if std::env::var_os(DEBUG_STDERR_ENV).is_none() {
                assert!(debug_output_log(Path::new("/nonexistent")).is_none());
            }
        }

        phase a_failed_launch_says_the_last_of_what_the_app_wrote;
        {
            assert_eq!(last_lines(""), "");
            assert_eq!(
                last_lines("loading\n\n  out of memory  \n"),
                "loading | out of memory"
            );
            // Only the most recent lines are kept, and the end is what a person needs.
            let many: String = (0..30)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n");
            let said = last_lines(&many);
            assert!(said.starts_with("line 10"), "{said}");
            assert!(said.ends_with("line 29"), "{said}");
            // A very long line is cut from the front, never in the middle of a character.
            let long = format!("{}\néchec", "x".repeat(4000));
            let said = last_lines(&long);
            assert!(said.starts_with('…'), "{said}");
            assert!(said.ends_with("échec"), "{said}");
        }
    }
}

#[cfg(test)]
mod action_authority_tests {
    use super::*;
    use collab_types::action_authority::AppActionAuthorityProjection;
    #[test]
    fn action_authority_binding_is_readonly_scoped_and_separate_from_sandbox_grants() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("current.json");
        let manifest: ManifestView = crate::manifest::test_manifest(
            "id = \"so.tilo.store\"
version = \"1.0\"
kind = \"app\"",
            "[entrypoint]
bin = \"store\"
",
        );
        let policy = AppActionAuthorityProjection::project(
            &tilo_action_policy::WorkspacePolicy::new("workspace-a", "person-a"),
            &manifest.package_id,
            "person-a",
            false,
        );
        std::fs::write(&path, serde_json::to_vec(&policy).unwrap()).unwrap();
        let opts = LaunchOptions {
            workspace_id: Some("workspace-a".into()),
            action_authority_host_path: Some(path),
            ..LaunchOptions::default()
        };
        let before = resolve_capabilities(&manifest, &LaunchOptions::default());
        let after = resolve_capabilities(&manifest, &opts);
        assert_eq!(before.ro_paths, after.ro_paths);
        assert_eq!(before.rw_paths, after.rw_paths);
        let mut argv = vec!["bwrap".into(), "store".into()];
        splice_action_authority(&mut argv, &manifest, &opts).unwrap();
        assert_eq!(
            argv,
            vec![
                "bwrap",
                "--ro-bind",
                temp.path().to_str().unwrap(),
                crate::sandbox::SANDBOX_ACTION_AUTHORITY_DIR,
                "store"
            ]
        );
        for workspace in [None, Some("workspace-b".into())] {
            let mut wrong = opts.clone();
            wrong.workspace_id = workspace;
            assert!(splice_action_authority(&mut vec!["store".into()], &manifest, &wrong).is_err());
        }
        let mut wrong_app = policy;
        wrong_app.app_id = "so.tilo.settings".into();
        std::fs::write(
            opts.action_authority_host_path.as_ref().unwrap(),
            serde_json::to_vec(&wrong_app).unwrap(),
        )
        .unwrap();
        assert!(splice_action_authority(&mut vec!["store".into()], &manifest, &opts).is_err());
        std::fs::write(
            opts.action_authority_host_path.as_ref().unwrap(),
            vec![b' '; collab_types::action_authority::MAX_PROJECTION_BYTES + 1],
        )
        .unwrap();
        assert!(splice_action_authority(&mut vec!["store".into()], &manifest, &opts).is_err());
    }
}

#[cfg(test)]
mod mesa_seed_software_tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_software_launch_follows_the_compositor_onto_lavapipe;

        phase a_launch_without_a_render_node_follows_a_gpu_less_compositor_onto_lavapipe;
        {
            let mut home = tilo_test_env::TestHome::new().unwrap();
            let icds = home.root().join("vulkan/icd.d");
            std::fs::create_dir_all(&icds).unwrap();
            let lavapipe = icds.join("lvp_icd.json");
            std::fs::write(
                &lavapipe,
                r#"{"file_format_version":"1.0.1","ICD":{"library_path":"libvulkan_lvp.so"}}"#,
            )
            .unwrap();
            let radeon = icds.join("radeon_icd.json");
            std::fs::write(
                &radeon,
                r#"{"file_format_version":"1.0.1","ICD":{"library_path":"libvulkan_radeon.so"}}"#,
            )
            .unwrap();
            home.env()
                .set(crate::gpu::VK_DRIVER_FILES_ENV, &lavapipe)
                .remove(crate::gpu::VK_ICD_FILENAMES_ENV)
                .remove(crate::gpu::EGL_VENDOR_ENV);
            let manifest: ManifestView = crate::manifest::test_manifest(
                "id = \"io.tilo.software-pin\"\nversion = \"0.1.0\"\nkind = \"app\"",
                "[entrypoint]\nbin = \"bin/probe\"\n\n[capabilities]\noptional = [\"gpu\"]\n",
            );
            let opts = LaunchOptions::default();
            let mut argv = vec!["/app/probe".to_owned()];
            let (gpu, _) = splice_gpu(
                &mut argv,
                &manifest,
                &opts,
                crate::gpu::granted(&manifest, &CapabilityPlan::empty(), true),
            )
            .unwrap();
            assert!(gpu.is_none(), "no render node, no device pin");
            let path = lavapipe.to_str().unwrap();
            assert!(
                argv.windows(3)
                    .any(|args| args == ["--ro-bind-try", path, path])
            );
            for key in [
                crate::gpu::VK_DRIVER_FILES_ENV,
                crate::gpu::VK_ICD_FILENAMES_ENV,
            ] {
                assert!(argv.windows(3).any(|args| args == ["--setenv", key, path]));
            }
            assert!(
                !argv.iter().any(|arg| arg.starts_with("/dev/dri/renderD")),
                "no render node, no node pin: {argv:?}"
            );
            let bound = argv
                .windows(3)
                .rposition(|args| args == ["--dev-bind-try", "/dev/dri", "/dev/dri"])
                .expect("the GPU devices were bound first");
            let hidden = argv
                .windows(2)
                .rposition(|args| args == ["--tmpfs", "/dev/dri"])
                .expect("and then hidden: the app sees no render node of a machine without a GPU");
            assert!(hidden > bound, "{argv:?}");
            assert_eq!(argv.last().map(String::as_str), Some("/app/probe"));

            // A GPU's driver in the environment is not the software pin.
            home.env().set(crate::gpu::VK_DRIVER_FILES_ENV, &radeon);
            let mut argv = vec!["/app/probe".to_owned()];
            splice_gpu(
                &mut argv,
                &manifest,
                &opts,
                crate::gpu::granted(&manifest, &CapabilityPlan::empty(), true),
            )
            .unwrap();
            assert!(
                !argv
                    .iter()
                    .any(|arg| arg == crate::gpu::VK_DRIVER_FILES_ENV)
            );
            // Nor did the compositor name a GPU: Mesa's device-select layer, which asks the
            // compositor for its device and crashed in `native-render-prepare` where it named
            // none (a session's first launches, on a machine without 3D), is off.
            assert!(
                argv.windows(3)
                    .any(|args| args == ["--setenv", crate::gpu::NO_DEVICE_SELECT_ENV, "1"]),
                "{argv:?}"
            );
            assert_eq!(argv.last().map(String::as_str), Some("/app/probe"));
        }

        phase a_software_launch_gets_only_the_read_only_session_seed_without_a_device_pin;
        {
            let mut home = tilo_test_env::TestHome::new().unwrap();
            // A compositor's software pin is its own case below; CI pins lavapipe in the test environment
            // itself.
            home.env()
                .remove(crate::gpu::VK_DRIVER_FILES_ENV)
                .remove(crate::gpu::VK_ICD_FILENAMES_ENV)
                .remove(crate::gpu::EGL_VENDOR_ENV);
            let source = home.runtime().join("tilo/pipeline-seed");
            std::fs::create_dir_all(&source).unwrap();
            let manifest: ManifestView = crate::manifest::test_manifest(
                "id = \"io.tilo.seed-test\"\nversion = \"0.1.0\"\nkind = \"app\"",
                "[entrypoint]\nbin = \"bin/probe\"\n\n[capabilities]\noptional = [\"gpu\"]\n",
            );
            let opts = LaunchOptions::default();
            let mut argv = vec!["/app/probe".to_owned()];
            let (gpu, seed) = splice_gpu(
                &mut argv,
                &manifest,
                &opts,
                crate::gpu::granted(&manifest, &CapabilityPlan::empty(), true),
            )
            .unwrap();
            assert!(gpu.is_none());
            assert_eq!(seed.as_deref(), Some(source.as_path()));
            assert!(argv.windows(3).any(|args| args
                == [
                    "--ro-bind-try",
                    source.to_str().unwrap(),
                    crate::gpu::SANDBOX_PIPELINE_SEED_DIR
                ]));
            assert!(!argv.iter().any(
                |arg| arg.starts_with("/dev/dri/renderD") || arg == crate::gpu::VK_DRIVER_FILES_ENV
            ));
            assert!(
                argv.windows(3)
                    .any(|args| args == ["--setenv", crate::mesa_seed::CONSUME_ENV, "1"])
            );
            for mode in [
                LaunchMode::Cli {
                    command: "probe".into(),
                    passthrough_argv: Vec::new(),
                },
                LaunchMode::Service {
                    passthrough_argv: Vec::new(),
                },
            ] {
                let opts = LaunchOptions {
                    mode,
                    ..LaunchOptions::default()
                };
                let mut helper = vec!["/app/helper".to_owned()];
                splice_gpu(
                    &mut helper,
                    &manifest,
                    &opts,
                    crate::gpu::granted(&manifest, &CapabilityPlan::empty(), true),
                )
                .unwrap();
                assert!(
                    !helper
                        .iter()
                        .any(|arg| arg == crate::mesa_seed::CONSUME_ENV),
                    "non-painting helpers must not initialize graphics caches"
                );
            }
            assert_eq!(argv.last().map(String::as_str), Some("/app/probe"));
        }
    }
}

#[cfg(test)]
mod render_service_launch_tests {
    use super::*;

    tilo_test_env::scenario! {
        fn an_app_gets_a_gpu_only_when_it_declares_one;

        phase an_app_without_gpu_gets_no_device_and_no_graphics_setup_but_the_layer_switch;
        // An app that draws through its workspace's render service: no GPU node, no loader pin, no
        // shader-cache seed, and none of the probing they cost, even with a render node and a session seed
        // to hand out.
        {
            let mut home = tilo_test_env::TestHome::new().unwrap();
            home.env()
                .set(
                    crate::gpu::VK_DRIVER_FILES_ENV,
                    "/usr/share/vulkan/icd.d/x.json",
                )
                .remove(crate::gpu::EGL_VENDOR_ENV);
            std::fs::create_dir_all(home.runtime().join("tilo/pipeline-seed")).unwrap();
            let manifest: ManifestView = crate::manifest::test_manifest(
                "id = \"so.tilo.calculator\"\nversion = \"0.1.0\"\nkind = \"app\"",
                "[entrypoint]\nbin = \"bin/calculator\"\n",
            );
            assert!(!crate::gpu::granted(
                &manifest,
                &CapabilityPlan::empty(),
                true
            ));
            let opts = LaunchOptions {
                render_node: Some(PathBuf::from("/dev/dri/renderD128")),
                ..LaunchOptions::default()
            };
            let mut argv = vec!["/app/calculator".to_owned()];
            let (gpu, seed) = splice_gpu(
                &mut argv,
                &manifest,
                &opts,
                crate::gpu::granted(&manifest, &CapabilityPlan::empty(), true),
            )
            .unwrap();
            assert!(gpu.is_none() && seed.is_none());
            // No GPU reaches it: no device node, no grant, no loader pin, no seed. The one thing
            // set is loader hygiene: Mesa's implicit device-select layer is off, which an app
            // drawing itself without a device crashed in (`native-render-prepare`, at a session's
            // start on a machine without 3D).
            assert_eq!(
                argv,
                ["--setenv", crate::gpu::NO_DEVICE_SELECT_ENV, "1", "/app/calculator"],
                "nothing but the layer switch spliced"
            );
        }

        phase an_app_declaring_gpu_gets_its_device;
        // An app that declares `gpu` keeps what every app had before: the device nodes, narrowed to the
        // compositor's render node.
        {
            let manifest: ManifestView = crate::manifest::test_manifest(
                "id = \"so.tilo.browser\"\nversion = \"0.1.0\"\nkind = \"app\"",
                "[entrypoint]\nbin = \"bin/browser\"\n\n[capabilities]\noptional = [\"gpu\"]\n",
            );
            assert!(crate::gpu::granted(
                &manifest,
                &CapabilityPlan::empty(),
                true
            ));
            let mut argv = vec!["/app/browser".to_owned()];
            splice_gpu(&mut argv, &manifest, &LaunchOptions::default(), true).unwrap();
            assert!(
                argv.windows(3)
                    .any(|args| args == ["--dev-bind-try", "/dev/dri", "/dev/dri"]),
                "{argv:?}"
            );
            assert_eq!(argv.last().map(String::as_str), Some("/app/browser"));
        }
    }
}

#[cfg(test)]
mod shell_ui_tests;

#[cfg(test)]
mod gui_argv_tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_gui_launch_passes_the_apps_arguments_through;

        phase a_gui_launch_forwards_the_apps_own_arguments;
        // A GUI launch carries the app's own arguments after the entrypoint, as a CLI launch carries its
        // passthrough, and alongside the one a person opens with rather than instead of it.
        {
            {
                let opts = LaunchOptions {
                    gui_argv: vec!["--index".to_owned()],
                    ..LaunchOptions::default()
                };
                assert_eq!(gui_open_args(&opts), ["--index"]);
            }
            {
                // A document the person opened with comes first, then the app's own arguments; neither
                // displaces the other.
                let opts = LaunchOptions {
                    open_document: Some(PathBuf::from("/workspace/a.note")),
                    gui_argv: vec!["--index".to_owned()],
                    ..LaunchOptions::default()
                };
                assert_eq!(gui_open_args(&opts), ["/workspace/a.note", "--index"]);
            }
            {
                // No argv at all is the ordinary launch, unchanged.
                let opts = LaunchOptions::default();
                assert!(gui_open_args(&opts).is_empty());
            }
        }

        phase the_arguments_reach_the_sandboxed_program_after_its_entrypoint;
        // The argv a GUI launch builds keeps those arguments after the entrypoint token, where bwrap
        // forwards them to the child as `argv[1..]` — the same place a CLI launch's passthrough lands.
        {
            let manifest: ManifestView = crate::manifest::test_manifest(
                "id = \"so.tilo.chat\"\nversion = \"0.1.0\"\nkind = \"app\"\nruntime = \"tilo-runtime/2026.07\"",
                "[entrypoint]\nbin = \"bin/chat\"\n",
            );
            let opts = LaunchOptions {
                gui_argv: vec!["--index".to_owned()],
                ..LaunchOptions::default()
            };
            let (argv, _, tail) = build_initial_argv(
                &manifest,
                Path::new("/host/pkg"),
                &opts,
                None,
                &[],
                None,
                &CapabilityPlan::empty(),
            )
            .unwrap();
            assert_eq!(tail, ["--index"], "the app's argument rides the tail");
            // The last token is the program bwrap execs; everything after it is that program's own argv,
            // which the spawn appends.
            assert_eq!(
                argv.last().map(String::as_str),
                Some("/var/tilo-apps/so.tilo.chat/bin/chat")
            );
        }
    }
}

#[cfg(test)]
mod launch_preflight_tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn assert_runtime_refusal_precedes_handoff(captured: bool) {
        let root = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", root.path());
        let socket = crate::services::collab_socket();
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let manifest: ManifestView = crate::manifest::test_manifest(
            "id = \"so.tilo.preflight\"\nversion = \"1\"\nkind = \"app\"",
            r#"[entrypoint]
bin = "bin/preflight"
type = "gui"
[sandbox.services]
collab = "required@1"
"#,
        );
        let missing_runtime = root.path().join("missing-runtime");
        let options = LaunchOptions {
            runtime_root: Some(missing_runtime.clone()),
            workspace_id: Some("preflight-workspace".into()),
            ..LaunchOptions::default()
        };
        let error = if captured {
            launch_with_captured(&manifest, root.path(), &options).unwrap_err()
        } else {
            launch_with(&manifest, root.path(), &options).unwrap_err()
        };
        assert!(
            matches!(error, LaunchError::RuntimeBundleSignature { ref path, .. } if path == &missing_runtime),
            "runtime verification must refuse the actual launch: {error}"
        );
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "runtime preflight contacted the live broker and started its launch deadline"
        );
    }

    tilo_test_env::scenario! {
        fn runtime_preflight_precedes_service_handoff;

        phase native_runtime_preflight_precedes_service_handoff;
        {
            assert_runtime_refusal_precedes_handoff(false);
        }

        phase captured_runtime_preflight_precedes_service_handoff;
        {
            assert_runtime_refusal_precedes_handoff(true);
        }
    }
}
