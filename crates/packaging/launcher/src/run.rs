//! Top-level `run` orchestration, matching the CLI sequence: read the
//! manifest; resolve service sockets (per `--no-services`); stage the
//! seccomp blob to a tempfile; build the bwrap argv; spawn `bwrap` with
//! `pre_exec` wiring the mandatory seccomp fd; the in-namespace shim installs
//! mandatory Landlock when requested, failing the launch if it cannot; wait
//! and relay the exit code.

use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::time::{Duration, Instant};

use tilo_clock::unix_now;

use crate::capabilities::{CapabilityPlan, load_capability_file};
use crate::dbus_proxy::{self, DBusProxyGuard};
use crate::manifest::{MinimalManifest, PortalPolicy, read_manifest};
use crate::sandbox::{DBusBindPlan, build_bwrap_argv};
use crate::seccomp::compile_baseline;
use crate::services::{
    BASELINE_SECCOMP_CHILD_FD, CLONE3_COMPAT_SECCOMP_CHILD_FD, EXIT_REQUIRED_SERVICE_MISSING,
    ServiceError, ServiceLaunchContext, resolve_blob_bind, resolve_chooser_bind,
    resolve_identity_bind, resolve_service_binds_for_launch, to_service_binds,
};

struct SeccompFds {
    clone3_compat: OwnedFd,
    baseline: OwnedFd,
}

impl SeccompFds {
    fn prepend_bwrap_args(argv: &mut Vec<String>) {
        let mut prefix = vec![
            "--add-seccomp-fd".to_string(),
            CLONE3_COMPAT_SECCOMP_CHILD_FD.to_string(),
            "--add-seccomp-fd".to_string(),
            BASELINE_SECCOMP_CHILD_FD.to_string(),
        ];
        prefix.append(argv);
        *argv = prefix;
    }

    fn host_fds(&self) -> (RawFd, RawFd) {
        (self.clone3_compat.as_raw_fd(), self.baseline.as_raw_fd())
    }
}

#[cfg(test)]
mod seccomp_fd_tests {
    use super::*;

    #[test]
    fn both_seccomp_filters_precede_the_sandbox_arguments() {
        let compat = stage_seccomp_blob(&[1, 2, 3]).expect("compat filter fd");
        let baseline = stage_seccomp_blob(&[4, 5, 6]).expect("baseline filter fd");
        let _fds = SeccompFds {
            clone3_compat: compat,
            baseline,
        };
        let mut argv = vec!["--unshare-all".to_string(), "/app/bin".to_string()];

        SeccompFds::prepend_bwrap_args(&mut argv);

        assert_eq!(
            argv,
            [
                "--add-seccomp-fd",
                "40",
                "--add-seccomp-fd",
                "41",
                "--unshare-all",
                "/app/bin",
            ]
        );
    }

    #[test]
    fn no_handed_over_descriptor_can_replace_a_seccomp_program() {
        use crate::services::RESERVED_CHILD_FDS;
        // Declared services are numbered from 17 upwards around the reserved
        // slots; a filter sharing a slot with one would make bwrap read its
        // program from that service's socket and never start the app.
        for fd in [CLONE3_COMPAT_SECCOMP_CHILD_FD, BASELINE_SECCOMP_CHILD_FD] {
            assert!(RESERVED_CHILD_FDS.contains(&fd), "{fd} must be reserved");
            assert!(fd > 31, "{fd} must stay above bwrap's own descriptors");
        }
        let mut unique = RESERVED_CHILD_FDS.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            RESERVED_CHILD_FDS.len(),
            "reserved slots overlap"
        );
    }
}

unsafe fn place_fd(host: RawFd, child: RawFd) -> std::io::Result<()> {
    if unsafe { libc::dup2(host, child) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let flags = unsafe { libc::fcntl(child, libc::F_GETFD) };
    if flags != -1 {
        let _ = unsafe { libc::fcntl(child, libc::F_SETFD, flags & !libc::FD_CLOEXEC) };
    }
    Ok(())
}

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
         Install it with `tilo-install runtime install --version <v> --from-host`, \
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
        "`{desktop_id}` is a host application, not a Tilo package: it has no manifest, so \
         there is no sandbox to build for it. Launch it with `tilo-launcher run-host \
         --desktop-id {desktop_id}`, which runs it unconfined and says so."
    )]
    HostAppNotAPackage { desktop_id: String },
    #[error(
        "refusing to launch: this app was granted access to {hosts:?} and that can only be \
         enforced by brokering its traffic, but the egress gate could not start ({source}). \
         Launching anyway would mean giving it unrestricted outbound access, which is not \
         what was granted. Set TILO_LAUNCHER_NET_GATE=off to do that deliberately."
    )]
    EgressGate {
        hosts: Vec<String>,
        #[source]
        source: crate::netgate::GateError,
    },
    #[error("refusing URI open: {0}")]
    InvalidOpenUri(String),
    #[error("refusing managed-service launch: {0}")]
    InvalidServiceLaunch(String),
    #[error(
        "refusing to launch `{package}` as `{app_id}`: an app has exactly one id, the package id \
         of its signed manifest"
    )]
    AppIdMismatch { app_id: String, package: String },
    #[error("refusing authored launch: identity signing broker could not start ({0})")]
    IdentityBroker(String),
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
    CaptureCancellation, CapturedResourcePlan, CapturedRunOutput, CapturedTermination,
};
impl From<crate::capture::InvalidCapturePlan> for RunError {
    fn from(_: crate::capture::InvalidCapturePlan) -> Self {
        Self::InvalidCapturePlan
    }
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

/// Where a launch finds the identity record it brokers to the app.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum IdentitySource {
    /// The active identity in the invoking user's store, located through
    /// `$XDG_CONFIG_HOME`, else `$HOME/.config`.
    #[default]
    UserStore,
    /// The active identity in this identity store directory.
    Store(PathBuf),
    /// This exact record, used verbatim with no existence check. It is
    /// migrated only when it lies in the user's store.
    Record(PathBuf),
    /// No identity: the app runs without an identity broker.
    None,
}

/// Optional knobs for `run_with`.
#[derive(Debug, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent CLI-exposed launch knob (no_services, minimal_libs, \
              landlock_enforce, allow_missing_runtime, allow_unsigned_runtime), not combinatorial \
              state — grouping them into an enum would not make any combination less valid"
)]
pub struct RunOptions {
    /// Skip all service-socket resolution.
    pub no_services: bool,
    /// Explicit override for the tilo-runtime bundle directory; wins over
    /// the manifest's `runtime` field.
    pub runtime_root: Option<PathBuf>,
    /// Override the bwrap binary (defaults to `bwrap`).
    pub bwrap_bin: Option<PathBuf>,
    /// Override the in-namespace launcher executable. Production leaves this
    /// unset and re-enters the currently running `tilo-launcher`; integration
    /// tests use the separately-built binary rather than their test harness.
    pub sandbox_shim_bin: Option<PathBuf>,
    /// Bind-mount the minimum host paths a dynamically-linked ELF needs to
    /// `execve()`, read-only. Ignored (silently) when a runtime bundle resolves.
    pub minimal_libs: bool,
    /// Route exec through the `__sandbox-exec` shim so Landlock installs
    /// inside bwrap's mount namespace. Requires this binary bind-mounted at
    /// [`SHIM_MOUNT_PATH`]. Default on; off is a logged debug escape hatch.
    pub landlock_enforce: bool,
    /// Accept Landlock not, or only partially, enforced by the kernel. Set
    /// only for a system-store app on a provider-sandboxed machine
    /// ([`crate::provider_sandbox`]).
    pub landlock_best_effort: bool,
    /// Launch mode — GUI (default), an interactive CLI command, or a managed
    /// service. CLI and service modes pick `cli_bin()` (falling back to
    /// `bin`) and forward argv without requiring a Wayland surface.
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
    /// Where the identity brokered to the app comes from. It is read only by
    /// the launcher to create a broker capability and is never bound into
    /// the sandbox.
    pub identity: IdentitySource,
    /// Host data home holding the per-room app directories; `None` resolves
    /// `$XDG_DATA_HOME`, else `$HOME/.local/share`.
    pub data_home: Option<PathBuf>,
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
    /// The app id the caller launched. An app has exactly one id, so a value
    /// other than the manifest's package id refuses the launch.
    pub app_id: Option<String>,
    /// A document to open, forwarded as `argv[1]`. Only an argument — binds
    /// and unlocks nothing on its own. `None` launches with no arguments.
    pub open_document: Option<PathBuf>,
    /// A URI to open, forwarded as one opaque argv value. The caller must
    /// validate its scheme against the freshly-read installed manifest.
    pub open_uri: Option<String>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            no_services: false,
            runtime_root: None,
            bwrap_bin: None,
            sandbox_shim_bin: None,
            minimal_libs: false,
            // tapp applications are untrusted by default.  A caller using the
            // convenience constructor must not accidentally remove one of the
            // kernel-enforced confinement layers.
            landlock_enforce: true,
            landlock_best_effort: false,
            mode: LaunchMode::default(),
            allow_missing_runtime: false,
            allow_unsigned_runtime: false,
            require_runtime_publisher: None,
            wayland_socket: None,
            render_node: None,
            identity: IdentitySource::UserStore,
            data_home: None,
            trusted_identities_host_path: None,
            workspace_id: None,
            capabilities_path: None,
            app_id: None,
            open_document: None,
            open_uri: None,
        }
    }
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

fn validate_launch_mode(manifest: &MinimalManifest, opts: &RunOptions) -> Result<(), RunError> {
    if let LaunchMode::Host { desktop_id } = &opts.mode {
        return Err(RunError::HostAppNotAPackage {
            desktop_id: desktop_id.clone(),
        });
    }
    if let Some(app_id) = opts
        .app_id
        .as_deref()
        .filter(|app_id| *app_id != manifest.package_id)
    {
        return Err(RunError::AppIdMismatch {
            app_id: app_id.to_owned(),
            package: manifest.package_id.clone(),
        });
    }
    if !matches!(&opts.mode, LaunchMode::Service { .. }) {
        return Ok(());
    }
    if manifest.kind != crate::manifest::KIND_SERVICE {
        return Err(RunError::InvalidServiceLaunch(format!(
            "{} is `{}`, not a service package",
            manifest.package_id, manifest.kind
        )));
    }
    if manifest.entrypoint.cli_bin().is_none() {
        return Err(RunError::InvalidServiceLaunch(format!(
            "{} has no headless entrypoint",
            manifest.package_id
        )));
    }
    if opts.wayland_socket.is_some() || opts.open_document.is_some() || opts.open_uri.is_some() {
        return Err(RunError::InvalidServiceLaunch(
            "managed services cannot receive a display, document, or URI".into(),
        ));
    }
    Ok(())
}

fn is_native_game_launch(manifest: &MinimalManifest, opts: &RunOptions) -> bool {
    matches!(&opts.mode, LaunchMode::Gui)
        && manifest.category.as_deref().is_some_and(|category| {
            category.eq_ignore_ascii_case("game") || category.eq_ignore_ascii_case("games")
        })
}

#[cfg(test)]
mod resource_claim_tests {
    use super::*;

    fn browser_manifest() -> MinimalManifest {
        toml::from_str(
            "package_id = \"so.tilo.browser\"\nversion = \"1\"\n\
             [entrypoint]\nbin = \"bin/browser\"\n\
             [providers]\nschemes = [\"https\"]\n",
        )
        .unwrap()
    }

    #[test]
    fn launch_claims_and_device_hints_follow_the_manifest() {
        // Phase `uri_is_revalidated_against_the_launch_manifest`.
        {
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
        // Phase `only_gui_game_packages_receive_the_scheduler_hint`.
        {
            let game: MinimalManifest = toml::from_str(
                "[package]\nid = \"so.tilo.game\"\nversion = \"1\"\ncategory = \"Games\"\n\
                 [entrypoint]\nbin = \"bin/game\"\n",
            )
            .unwrap();
            assert!(is_native_game_launch(&game, &RunOptions::default()));
            let service = RunOptions {
                mode: LaunchMode::Service {
                    passthrough_argv: Vec::new(),
                },
                ..RunOptions::default()
            };
            assert!(!is_native_game_launch(&game, &service));
            assert!(!is_native_game_launch(
                &browser_manifest(),
                &RunOptions::default()
            ));
        }
        // Phase `only_game_launches_receive_the_ntsync_device`.
        {
            let mut game = vec!["/app/game".to_string()];
            splice_game_ntsync(&mut game, true).unwrap();
            assert!(
                game.windows(3)
                    .any(|window| { window == ["--dev-bind", "/dev/ntsync", "/dev/ntsync"] })
            );
            assert_eq!(game.last().map(String::as_str), Some("/app/game"));

            let mut ordinary = vec!["/app/browser".to_string()];
            splice_game_ntsync(&mut ordinary, false).unwrap();
            assert_eq!(ordinary, ["/app/browser"]);
        }
    }
}

#[cfg(test)]
mod managed_service_tests {
    use super::*;

    fn manifest(kind: &str, entrypoint_type: &str) -> MinimalManifest {
        toml::from_str(&format!(
            "[package]\nid = \"so.tilo.worker\"\nversion = \"1\"\nkind = \"{kind}\"\n\
             [entrypoint]\nbin = \"bin/worker\"\ntype = \"{entrypoint_type}\"\n"
        ))
        .unwrap()
    }

    fn service_options() -> RunOptions {
        RunOptions {
            mode: LaunchMode::Service {
                passthrough_argv: vec!["--serve".into()],
            },
            ..RunOptions::default()
        }
    }

    #[test]
    fn managed_service_launches_are_confined_to_headless_service_packages() {
        // Phase `managed_service_requires_service_kind_and_headless_entrypoint`.
        {
            let service = manifest("service", "cli");
            assert!(validate_launch_mode(&service, &service_options()).is_ok());
            assert!(matches!(
                validate_launch_mode(&manifest("app", "cli"), &service_options()),
                Err(RunError::InvalidServiceLaunch(_))
            ));
            assert!(matches!(
                validate_launch_mode(&manifest("service", "gui"), &service_options()),
                Err(RunError::InvalidServiceLaunch(_))
            ));
        }
        // Phase `managed_service_cannot_receive_a_display_or_open_resource`.
        {
            let manifest = manifest("service", "cli");
            let display = RunOptions {
                wayland_socket: Some("/run/user/1000/wayland-0".into()),
                ..service_options()
            };
            assert!(validate_launch_mode(&manifest, &display).is_err());
            let document = RunOptions {
                open_document: Some("/tmp/document".into()),
                ..service_options()
            };
            assert!(validate_launch_mode(&manifest, &document).is_err());
        }
        // Phase `no_launch_can_readdress_its_signed_package_id`.
        {
            let service = RunOptions {
                app_id: Some("so.tilo.other".into()),
                ..service_options()
            };
            assert!(matches!(
                validate_launch_mode(&manifest("service", "cli"), &service),
                Err(RunError::AppIdMismatch { .. })
            ));
            let gui = RunOptions {
                app_id: Some("so.tilo.other".into()),
                ..RunOptions::default()
            };
            assert!(matches!(
                validate_launch_mode(&manifest("app", "gui"), &gui),
                Err(RunError::AppIdMismatch { .. })
            ));
            let exact = RunOptions {
                app_id: Some("so.tilo.worker".into()),
                ..RunOptions::default()
            };
            assert!(validate_launch_mode(&manifest("app", "gui"), &exact).is_ok());
        }
    }
}

/// Resolve the capability plan: read the compositor's file, evaluate it
/// for this app now. Split out so [`run_with_captured`] and tests share it.
pub fn resolve_capabilities(manifest: &MinimalManifest, opts: &RunOptions) -> CapabilityPlan {
    let app_id = manifest.package_id.clone();
    let records = opts
        .capabilities_path
        .as_deref()
        .map(load_capability_file)
        .unwrap_or_default();
    let mut plan = CapabilityPlan::for_app(&app_id, &records, Some(unix_now()));
    // A workspace launch with no explicit author grant is a viewer. Treating
    // missing/malformed policy as authorship made every failure permissive.
    if opts.workspace_id.is_some()
        && plan.participation == crate::capabilities::Participation::Unscoped
    {
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
    let data_home = opts
        .data_home
        .clone()
        .or_else(crate::appdir::host_data_home)
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

/// The identity store records are migrated in, if any: the named store, or
/// the user's store for [`IdentitySource::UserStore`] and
/// [`IdentitySource::Record`].
fn identity_store(source: &IdentitySource) -> Option<PathBuf> {
    match source {
        IdentitySource::UserStore | IdentitySource::Record(_) => {
            runtime_identity::identity_dir_from_env().ok()
        }
        IdentitySource::Store(dir) => Some(dir.clone()),
        IdentitySource::None => None,
    }
}

/// The identity record this launch brokers: an explicit record verbatim (no
/// existence check), else the active identity selected in the store, else
/// `None`.
pub fn resolve_identity_host_path(opts: &RunOptions) -> Option<PathBuf> {
    match &opts.identity {
        IdentitySource::Record(path) => Some(path.clone()),
        IdentitySource::None => None,
        source @ (IdentitySource::UserStore | IdentitySource::Store(_)) => {
            runtime_identity::resolve_active_identity(&identity_store(source)?)
        }
    }
}

fn start_identity_broker(
    path: &Path,
    store: Option<&Path>,
) -> Result<(UnixStream, UnixStream, std::thread::JoinHandle<()>), RunError> {
    // Standard store records are migrated before loading. Explicit debug
    // paths outside the store still stay host-side, but are not rewritten in
    // an unexpected directory.
    if let Some(store) = store
        && path.parent() == Some(store)
        && let Some(id) = path.file_stem().and_then(|value| value.to_str())
        && path.extension().and_then(|value| value.to_str()) == Some("json")
        && path == store.join(format!("{id}.json"))
    {
        runtime_identity::secure_identity(store, id)
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
        .name("tilo-identity-broker".into())
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
    // Look up: the configured/user root first, then the immutable system
    // fallback. A developer build honours TILO_RUNTIME_BUNDLE_DIR; developer
    // sessions use it to keep the runtime matched to an overlaid
    // registry/payload set instead of silently binding the installed `/usr`
    // runtime. A release build ignores it (see `crate::dev_env`).
    let per_user = runtime_bundle::default_bundle_path_from(
        crate::dev_env::dev_override(runtime_bundle::RUNTIME_DIR_ENV),
        version,
    )
    .ok();
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
///      (`/etc/tilo/runtime-bundle-trusted-publishers.txt`, or
///      `TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE` in a developer build).
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

    let Ok(root) = root.canonicalize() else {
        return false;
    };

    // Protect against replacing the otherwise-secure version directory through
    // a writable ancestor, then check every signed file and its subdirectories.
    // Canonicalise first: Fedora Atomic deliberately exposes `/usr` through a
    // root-owned symlink into its immutable deployment. Rejecting every
    // symlink component made a normal rpm-ostree installation impossible to
    // launch even though the resolved tree and all of its ancestors were
    // root-owned and non-writable.
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

#[cfg(all(test, unix))]
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
            }],
            produced_at: 0,
            publisher_hex: None,
            signature_hex: None,
        }
    }

    #[test]
    fn immutable_system_bundle_accepts_a_root_owned_symlinked_prefix() {
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
        // The policy walks every ancestor, so the fixture has to live below
        // root-owned directories nobody else may write. The build output
        // tree usually is one when tests run as root; a sticky, world-writable
        // temporary directory never is, and there the check cannot be modelled.
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
        // tempfile deliberately creates a private 0700 parent, whereas the
        // real rpm-ostree deployment is rooted below traversable, immutable
        // root-owned directories. Model that production ancestry exactly.
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
/// `None` when neither the (developer-build) env override nor the default
/// file exists.
fn load_trusted_publisher_list() -> Option<Vec<String>> {
    let path = trusted_publishers_path_from(crate::dev_env::dev_override(
        crate::dev_env::TRUSTED_PUBLISHERS_FILE_ENV,
    ));
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

/// The image's runtime trusted-publisher list.
pub const TRUSTED_PUBLISHERS_FILE: &str = "/etc/tilo/runtime-bundle-trusted-publishers.txt";

/// Where the trusted-publisher list is read from, given the override the
/// build's gate let through ([`crate::dev_env::dev_override`]).
fn trusted_publishers_path_from(allowed_override: Option<std::ffi::OsString>) -> PathBuf {
    allowed_override.map_or_else(|| PathBuf::from(TRUSTED_PUBLISHERS_FILE), PathBuf::from)
}

#[cfg(test)]
mod dev_override_tests {
    use super::*;
    use crate::dev_env::{TRUSTED_PUBLISHERS_FILE_ENV, dev_override_from};

    #[test]
    fn trust_roots_follow_the_build_profile() {
        // Phase `a_release_build_reads_the_images_trusted_publisher_list`.
        {
            let path = trusted_publishers_path_from(dev_override_from(
                false,
                TRUSTED_PUBLISHERS_FILE_ENV,
                Some("/tmp/attacker-publishers.txt".into()),
            ));
            assert_eq!(path, Path::new(TRUSTED_PUBLISHERS_FILE));
        }
        // Phase `a_developer_build_honours_the_trusted_publisher_override`.
        {
            let path = trusted_publishers_path_from(dev_override_from(
                true,
                TRUSTED_PUBLISHERS_FILE_ENV,
                Some("/dev/runtime/trusted-publishers.txt".into()),
            ));
            assert_eq!(path, Path::new("/dev/runtime/trusted-publishers.txt"));
        }
        // Phase `a_release_build_ignores_the_runtime_bundle_root_override`.
        {
            let gated = dev_override_from(
                false,
                runtime_bundle::RUNTIME_DIR_ENV,
                Some("/tmp/attacker-runtime".into()),
            );
            // Resolution may fail on a machine with no data dir; what matters is
            // that it never lands under the override.
            if let Ok(path) = runtime_bundle::default_bundle_path_from(gated, "1") {
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
    /// A desktop-managed, headless process declared as `kind = "service"`.
    /// Unlike [`Self::Cli`], this is not a user shell command and therefore
    /// does not mint a `TILO_CLI_COMMAND`; its exact argv comes from trusted
    /// desktop code.
    Service { passthrough_argv: Vec<String> },
    /// An application Tilo did not package (a host `.desktop` entry, Flatpak
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
/// grant to unrestricted access. Explicit only — never a silent default —
/// and honoured only by developer builds (see [`crate::dev_env`]).
pub const NET_GATE_ENV: &str = crate::dev_env::NET_GATE_ENV;

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
/// bounded: only a *narrowing* grant needs the gate. `TILO_LAUNCHER_NET_GATE=off`
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
    // `Filtered` is the only decision needing a gate; `TILO_LAUNCHER_NET_GATE=off`
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

/// Bound per-process resources inherited by bwrap and the tapp. Core dumps may
/// contain workspace content or broker responses; an untrusted app also does not
/// need an unbounded descriptor table. Both soft and hard limits are set so the
/// app cannot raise them again.
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
    // SAFETY: both pointers refer to initialized rlimit values for the duration
    // of these syscalls.
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

/// Splice in the `TILO_CLI_COMMAND` setenv before the trailing entrypoint
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

/// Expose the upstream NT synchronization device only to GUI packages marked
/// as games. The fresh bwrap `/dev` hides host devices by default; without this
/// bind Wine/Proton silently falls back to slower userspace synchronization.
fn splice_game_ntsync(argv: &mut Vec<String>, latency_sensitive: bool) -> Result<(), RunError> {
    if latency_sensitive {
        let entry = pop_entry(argv)?;
        argv.extend([
            "--dev-bind".to_string(),
            "/dev/ntsync".to_string(),
            "/dev/ntsync".to_string(),
            entry,
        ]);
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
        argv.push("TILO_TOOL_CONTEXT".to_string());
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
    if manifest.package_id == "so.tilo.file-explorer"
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
    keep_fds: &'a [RawFd],
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
    let launcher = if let Some(path) = &opts.sandbox_shim_bin {
        path.clone()
    } else {
        std::env::current_exe()
            .map_err(|e| RunError::Io(std::io::Error::other(format!("current_exe: {e}"))))?
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
    push_scratch_and_engine_rules(argv, ctx.manifest);
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

fn push_scratch_and_engine_rules(argv: &mut Vec<String>, manifest: &MinimalManifest) {
    // Standard tmpfs mounts the app expects writable; --rw grants full FS
    // access, and both vanish with the process (scratch, not hidden state).
    for p in ["/tmp", "/dev"] {
        argv.push("--rw".to_string());
        argv.push(p.to_string());
    }
    let profile = manifest.sandbox.seccomp_profile(&manifest.package_id);
    for path in crate::profile::landlock_rw_nested_sandbox_paths(profile) {
        argv.push("--rw".to_string());
        argv.push(path.display().to_string());
    }
}

/// What [`spawn_and_wait`] wires into the child in `pre_exec`: the staged
/// seccomp filters, the resolved service sockets, the identity broker (app end,
/// control end, serving thread) and whether to apply game scheduling hints.
/// The launcher's copies are released at fixed points around `spawn`.
struct ChildSetup {
    seccomp_fds: SeccompFds,
    services: Vec<crate::services::ResolvedService>,
    identity_broker: (
        Option<UnixStream>,
        Option<UnixStream>,
        Option<std::thread::JoinHandle<()>>,
    ),
    latency_sensitive: bool,
}

/// Finish building `argv` (seccomp prefix + passthrough tail), spawn `bwrap`
/// with the seccomp fd and service sockets wired into the child via
/// `pre_exec`, then wait for it. Logs the derived Landlock/egress plan from
/// the parent for observability (the shim enforces Landlock; this only
/// records intent).
#[expect(
    clippy::too_many_lines,
    reason = "one ordered spawn sequence: descriptor custody must stay visible from preparation to reaping"
)]
fn spawn_and_wait(
    mut argv: Vec<String>,
    cli_passthrough: Vec<String>,
    opts: &RunOptions,
    child_setup: ChildSetup,
    egress: &crate::netgate::EgressDecision,
    net_gate: Option<&crate::sandbox::NetGateBind>,
    observer: Option<(
        &dyn crate::managed::EnrolledProcessObserver,
        Option<PathBuf>,
    )>,
) -> Result<ExitStatus, RunError> {
    let ChildSetup {
        seccomp_fds,
        services: resolved,
        identity_broker,
        latency_sensitive,
    } = child_setup;
    let (observer, debug_output) = match observer {
        Some((observer, debug_output)) => (Some(observer), debug_output),
        None => (None, None),
    };
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
    let (identity_broker_app, identity_broker_control, identity_broker_thread) = identity_broker;
    SeccompFds::prepend_bwrap_args(&mut argv);

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
    if observer.is_some() {
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Some((path, _)) = &debug_output {
            tracing::warn!(
                path = %path.display(),
                "{DEBUG_STDERR_ENV}=1: capturing the managed child's output (debug only)",
            );
            cmd.stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
        }
    }

    // pre_exec: place both seccomp blobs on fixed child fds, and put
    // each service socket on the fd number the app was told to read.
    let (host_clone3_compat_fd, host_baseline_seccomp_fd) = seccomp_fds.host_fds();
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
        target: "tilo_launcher::landlock",
        rules = landlock_plan.len(),
        net_restricted,
        enforce = opts.landlock_enforce,
        "landlock plan derived",
    );
    tracing::info!(
        target: "tilo_launcher::netgate",
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
            place_fd(host_clone3_compat_fd, CLONE3_COMPAT_SECCOMP_CHILD_FD)?;
            place_fd(host_baseline_seccomp_fd, BASELINE_SECCOMP_CHILD_FD)?;
            place_service_fds(&mut service_fds)?;
            set_sandbox_resource_limits()?;
            if latency_sensitive {
                crate::performance::apply_game_performance_hints()?;
            }
            // Landlock is NOT installed here — bwrap needs broad FS access to set up
            // the namespace; the `__sandbox-exec` shim installs it after. See `landlock_shim`.
            Ok(())
        });
    }

    // Last parent-side policy/custody check after preparation, immediately before spawn.
    if let Some(observer) = observer {
        observer.before_spawn().map_err(RunError::Spawn)?;
    }
    // 5. Spawn + wait.
    let mut child = cmd.spawn().map_err(RunError::Spawn)?;
    if let Some((_, file)) = debug_output {
        let sink = Arc::new(std::sync::Mutex::new(file));
        let budget = Arc::new(AtomicUsize::new(DEBUG_OUTPUT_LIMIT));
        if let Some(out) = child.stdout.take() {
            let (sink, budget) = (sink.clone(), budget.clone());
            let _ = std::thread::Builder::new()
                .name("tilo-debug-stdout".into())
                .spawn(move || copy_bounded_output(out, &sink, &budget));
        }
        if let Some(err) = child.stderr.take() {
            let _ = std::thread::Builder::new()
                .name("tilo-debug-stderr".into())
                .spawn(move || copy_bounded_output(err, &sink, &budget));
        }
    }
    // Keep both filter fds alive until spawn has duplicated them in the child.
    drop(seccomp_fds);
    // Same rule: the child holds its own copies, so the launcher's are dead
    // weight, dropped before the wait so the far end learns of the exit.
    drop(resolved);
    drop(identity_broker_app);
    let status = if let Some(observer) = observer {
        observer.child_created();
        observer.supervise(child)?
    } else {
        child.wait()?
    };
    stop_identity_broker(identity_broker_control, identity_broker_thread);
    Ok(status)
}

/// Seccomp baseline → tmpfile → fd. The manifest *asks* for a profile; the
/// image decides whether this package may have it. An unrecognised name is a
/// packaging mistake; a recognised-but-unentitled one is an app trying to
/// lift a protection off itself — logged distinctly.
fn stage_seccomp(manifest: &MinimalManifest) -> Result<SeccompFds, RunError> {
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
        baseline: stage_seccomp_blob(&compile_baseline(profile)?)?,
    })
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
                let store = identity_store(&opts.identity);
                let (app, control, thread) = start_identity_broker(path, store.as_deref())?;
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
/// `cli_bin()` (falling back to `bin`) and splices `TILO_CLI_COMMAND` /
/// forwards passthrough argv. Returns `(argv, extra_setenv, passthrough)`.
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
                Some(("TILO_CLI_COMMAND".to_string(), command.clone())),
                passthrough_argv.clone(),
            ))
        }
        LaunchMode::Service { passthrough_argv } => {
            let entry_bin_owned = manifest
                .entrypoint
                .cli_bin()
                .unwrap_or(&manifest.entrypoint.bin)
                .to_string();
            let argv = crate::sandbox::build_bwrap_argv_for_entry(
                manifest,
                payload_root,
                runtime_arg,
                binds,
                dbus_plan,
                identity_arg,
                &entry_bin_owned,
                capabilities,
            );
            Ok((argv, None, passthrough_argv.clone()))
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
    run_with_context(manifest, payload_root, opts, None)
}

pub(crate) fn run_enrolled(
    manifest: &MinimalManifest,
    payload_root: &Path,
    opts: &RunOptions,
    services: &mut crate::services::EnrolledServiceContext,
    observer: &dyn crate::managed::EnrolledProcessObserver,
) -> Result<ExitStatus, RunError> {
    run_with_context(manifest, payload_root, opts, Some((services, observer)))
}

#[expect(
    clippy::too_many_lines,
    reason = "one ordered launch pipeline; splitting it would scatter the policy checks that must precede spawn"
)]
fn run_with_context(
    manifest: &MinimalManifest,
    payload_root: &Path,
    opts: &RunOptions,
    mut enrolled: Option<(
        &mut crate::services::EnrolledServiceContext,
        &dyn crate::managed::EnrolledProcessObserver,
    )>,
) -> Result<ExitStatus, RunError> {
    validate_open_uri(manifest, opts)?;
    // 0a. A host application has no manifest and no sandbox. Refusing here,
    // before any socket/directory/argv exists, keeps the two launch kinds
    // from quietly converging. Managed-service constraints are shared with
    // the captured path below: capture must never become a bypass.
    validate_launch_mode(manifest, opts)?;

    // 0. Policy: a manifest naming a filesystem path is refused before
    // anything else exists — silently dropping it would surface as an app
    // bug, not a policy one. Checked at launch too, not just install.
    crate::manifest::enforce_fs_policy(manifest)?;

    // 1. Service sockets.
    let package_id = manifest.package_id.as_str();
    let resolved = if let Some((services, _)) = enrolled.as_mut() {
        let binding = services.binding().clone();
        services
            .take_for_launch(&binding, &manifest.sandbox.services)
            .map_err(RunError::InvalidServiceLaunch)?
    } else {
        let mut resolved = resolve_service_binds_for_launch(
            &manifest.sandbox.services,
            opts.no_services,
            ServiceLaunchContext {
                app_id: package_id,
                workspace_id: opts.workspace_id.as_deref(),
            },
        )?;
        crate::services::authenticate_shell_ui_bind(&resolved, package_id)?;
        if let Some(identity) =
            resolve_identity_bind(package_id, opts.workspace_id.as_deref(), opts.no_services)?
        {
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
        resolved
    };
    let binds = to_service_binds(&resolved);

    // 2. Seccomp baseline → tmpfile → fd.
    let seccomp_fds = stage_seccomp(manifest)?;

    // 2b. D-Bus proxy decision. Failure leaves the bus unavailable; there is
    // no raw-session-bus fallback. `_dbus_guard` stays alive until bwrap is
    // waited on so Drop can SIGTERM the proxy.
    let (dbus_plan, _dbus_guard) = resolve_dbus_bind(&manifest.sandbox.portals);

    // 3. Argv. Runtime bundle resolution: a resolved bundle wins over
    // --minimal-libs and skips the loader-bind stopgap entirely.
    let resolved_runtime = if enrolled.is_some() {
        Some(
            crate::managed::verify_runtime(manifest, opts)
                .map_err(RunError::InvalidServiceLaunch)?,
        )
    } else {
        resolve_runtime(manifest, opts)?
    };
    let runtime_arg = resolved_runtime
        .as_ref()
        .map(|(p, v)| (p.as_path(), v.as_str()));

    // The app's one writable directory, for the room it's opened into.
    // Created here since bwrap can only bind what exists.
    let app_dir = if let Some((services, _)) = enrolled.as_ref() {
        crate::appdir::prepare_host_app_dir(
            services.data_home(),
            Some(&services.binding().room),
            &services.binding().app,
        )?
    } else {
        resolve_app_dir(manifest, opts)?
    };

    // The user's capability decision. Empty unless the compositor wrote a
    // file — an app launched without one is confined to its manifest alone.
    let mut capabilities = resolve_capabilities(manifest, opts);
    if enrolled.is_some() {
        capabilities.participation = crate::capabilities::Participation::Author;
    }

    // Resolve the selected record on the host, migrate legacy storage, then
    // mint one app-scoped broker connection. The path is only a presence
    // marker for the argv builder; it is never mounted.
    let (identity_broker_app, identity_broker_control, identity_broker_thread, identity_host) =
        if enrolled.is_some() {
            (None, None, None, None)
        } else {
            resolve_identity_broker(&capabilities, opts)?
        };
    let identity_arg = identity_broker_app.as_ref().and(identity_host.as_deref());
    let mut keep_fds: Vec<RawFd> = resolved.iter().map(|r| r.child_fd as RawFd).collect();
    if identity_broker_app.is_some() {
        keep_fds.push(runtime_identity::broker::BROKER_CHILD_FD);
    }

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
    let latency_sensitive = is_native_game_launch(manifest, opts);
    splice_cli_setenv(&mut argv, extra_setenv)?;
    if let Some((services, _)) = enrolled.as_ref() {
        splice_cli_setenv(
            &mut argv,
            Some((
                "TILO_MANAGED_AGENT_CONTEXT".into(),
                services
                    .agent_context_json()
                    .map_err(RunError::InvalidServiceLaunch)?,
            )),
        )?;
    }
    splice_game_ntsync(&mut argv, latency_sensitive)?;
    let extra_ro_paths = splice_minimal_libs(&mut argv, opts, resolved_runtime.is_some())?;
    splice_workspace_id_bind(&mut argv, opts)?;
    if enrolled.is_none() {
        splice_tool_context(&mut argv)?;
    }
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
            identity_broker: (
                identity_broker_app,
                identity_broker_control,
                identity_broker_thread,
            ),
            latency_sensitive,
        },
        &egress,
        net_gate.as_ref(),
        enrolled.map(|(services, observer)| (observer, debug_output_log(services.data_home()))),
    )
}

/// Debug-only, off by default: when the launching host sets
/// `TILO_LAUNCHER_DEBUG_STDERR=1`, a managed (enrolled) child's stdout and
/// stderr go to a bounded log file in its data home instead of `/dev/null`.
/// This changes no sandbox, policy or custody decision; it only keeps the
/// diagnostic output a supervised app would otherwise lose.
pub const DEBUG_STDERR_ENV: &str = "TILO_LAUNCHER_DEBUG_STDERR";
/// Upper bound of one launch's captured output.
const DEBUG_OUTPUT_LIMIT: usize = 1024 * 1024;

fn debug_output_log(data_home: &Path) -> Option<PathBuf> {
    (std::env::var_os(DEBUG_STDERR_ENV).as_deref() == Some(std::ffi::OsStr::new("1")))
        .then(|| data_home.join("launcher-debug-output.log"))
}

/// Copy `source` into `sink` until EOF or until `budget` bytes were written
/// in total across all copiers sharing it; the rest is drained and dropped so
/// the child never blocks on a full pipe.
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

/// Finish building `argv` (seccomp prefix + passthrough tail), spawn `bwrap`
/// with its stdout piped, capture it, and wait. Confinement is prepared by
/// the same path as [`spawn_and_wait`]; capturing fd 1 is not a weaker launch.
fn spawn_and_capture(
    mut argv: Vec<String>,
    passthrough: Vec<String>,
    opts: &RunOptions,
    seccomp_fds: SeccompFds,
    resolved: Vec<crate::services::ResolvedService>,
    identity_broker: (
        Option<UnixStream>,
        Option<UnixStream>,
        Option<std::thread::JoinHandle<()>>,
    ),
) -> Result<(ExitStatus, Vec<u8>), RunError> {
    let (identity_broker_app, identity_broker_control, identity_broker_thread) = identity_broker;
    SeccompFds::prepend_bwrap_args(&mut argv);
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

    let (host_clone3_compat_fd, host_baseline_seccomp_fd) = seccomp_fds.host_fds();
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
            place_fd(host_clone3_compat_fd, CLONE3_COMPAT_SECCOMP_CHILD_FD)?;
            place_fd(host_baseline_seccomp_fd, BASELINE_SECCOMP_CHILD_FD)?;
            place_service_fds(&mut service_fds)?;
            set_sandbox_resource_limits()?;
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(RunError::Spawn)?;
    drop(seccomp_fds);
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
        // Phase `limits_are_nonzero_and_launcher_bounded`.
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
        // Phase `stdout_and_stderr_share_one_hard_memory_budget`.
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
        // Phase `cancellation_is_target_local_and_sticky`.
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
    opts: &RunOptions,
    seccomp_fds: SeccompFds,
    resolved: Vec<crate::services::ResolvedService>,
    identity_broker: (
        Option<UnixStream>,
        Option<UnixStream>,
        Option<std::thread::JoinHandle<()>>,
    ),
    plan: CapturedResourcePlan,
    cancellation: &CaptureCancellation,
) -> Result<CapturedRunOutput, RunError> {
    let plan = plan.validate()?;
    let (identity_broker_app, identity_broker_control, identity_broker_thread) = identity_broker;
    SeccompFds::prepend_bwrap_args(&mut argv);
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

    let (host_clone3_compat_fd, host_baseline_seccomp_fd) = seccomp_fds.host_fds();
    let mut service_fds: Vec<(RawFd, RawFd)> = resolved
        .iter()
        .map(|service| (service.host_fd(), service.child_fd as RawFd))
        .collect();
    if let Some(stream) = &identity_broker_app {
        service_fds.push((
            stream.as_raw_fd(),
            runtime_identity::broker::BROKER_CHILD_FD,
        ));
    }
    unsafe {
        cmd.pre_exec(move || {
            place_fd(host_clone3_compat_fd, CLONE3_COMPAT_SECCOMP_CHILD_FD)?;
            place_fd(host_baseline_seccomp_fd, BASELINE_SECCOMP_CHILD_FD)?;
            place_service_fds(&mut service_fds)?;
            set_sandbox_resource_limits()?;
            set_captured_resource_limits(plan)?;
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(RunError::Spawn)?;
    drop(seccomp_fds);
    drop(resolved);
    drop(identity_broker_app);
    let used = Arc::new(AtomicUsize::new(0));
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout = child.stdout.take().ok_or_else(|| {
        RunError::Io(std::io::Error::other(
            "captured worker stdout was not piped",
        ))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        RunError::Io(std::io::Error::other(
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
        .ok_or(RunError::InvalidCapturePlan)?;
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
            let pid = i32::try_from(child.id()).unwrap_or(i32::MAX);
            // SAFETY: the child created its own process group before exec;
            // a negative pid addresses only that exact group.
            let process_group = pid.saturating_neg();
            let killed = unsafe { libc::kill(process_group, libc::SIGKILL) } == 0;
            if !killed {
                let _ = child.kill();
            }
            let _ = child.wait();
            break reason;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_thread
        .join()
        .map_err(|_| RunError::Io(std::io::Error::other("stdout capture thread panicked")))??;
    let stderr = stderr_thread
        .join()
        .map_err(|_| RunError::Io(std::io::Error::other("stderr capture thread panicked")))??;
    stop_identity_broker(identity_broker_control, identity_broker_thread);
    Ok(CapturedRunOutput {
        termination,
        stdout,
        stderr,
    })
}

type IdentityBrokerHandles = (
    Option<UnixStream>,
    Option<UnixStream>,
    Option<std::thread::JoinHandle<()>>,
);

/// [`run_with`] sibling that captures the child's stdout instead of inheriting
/// it. It deliberately uses the same Landlock, seccomp, runtime, portal,
/// identity, filesystem and egress decisions as a normal tapp launch.
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
    run_with_capture_spawn(manifest, payload_root, opts, spawn_and_capture)
}

/// Captured sandbox launch with launcher-enforced resource limits and
/// cancellation. Sandbox construction is exactly the same as
/// [`run_with_captured`]; only process supervision differs.
///
/// # Errors
///
/// Returns [`RunError`] when the plan is invalid, sandbox construction fails,
/// or the bounded child cannot be spawned, supervised, or collected.
pub fn run_with_captured_bounded(
    manifest: &MinimalManifest,
    payload_root: &Path,
    opts: &RunOptions,
    plan: CapturedResourcePlan,
    cancellation: CaptureCancellation,
) -> Result<CapturedRunOutput, RunError> {
    plan.validate()?;
    run_with_capture_spawn(
        manifest,
        payload_root,
        opts,
        move |argv, passthrough, opts, seccomp, services, identity| {
            spawn_and_capture_bounded(
                argv,
                passthrough,
                opts,
                seccomp,
                services,
                identity,
                plan,
                &cancellation,
            )
        },
    )
}

fn run_with_capture_spawn<T>(
    manifest: &MinimalManifest,
    payload_root: &Path,
    opts: &RunOptions,
    spawn: impl FnOnce(
        Vec<String>,
        Vec<String>,
        &RunOptions,
        SeccompFds,
        Vec<crate::services::ResolvedService>,
        IdentityBrokerHandles,
    ) -> Result<T, RunError>,
) -> Result<T, RunError> {
    validate_open_uri(manifest, opts)?;
    // Same refusals as `run_with`: this is a second entry into the sandbox
    // builder, and a policy with one enforced door is a door beside it.
    validate_launch_mode(manifest, opts)?;
    crate::manifest::enforce_fs_policy(manifest)?;

    // Service sockets.
    let package_id = manifest.package_id.as_str();
    let mut resolved = resolve_service_binds_for_launch(
        &manifest.sandbox.services,
        opts.no_services,
        ServiceLaunchContext {
            app_id: package_id,
            workspace_id: opts.workspace_id.as_deref(),
        },
    )?;
    crate::services::authenticate_shell_ui_bind(&resolved, package_id)?;
    if let Some(identity) =
        resolve_identity_bind(package_id, opts.workspace_id.as_deref(), opts.no_services)?
    {
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
    let binds = to_service_binds(&resolved);

    // Seccomp baseline resolved identically to `run_with` — two paths
    // reaching different conclusions would make capture a way around the filter.
    let seccomp_fds = SeccompFds {
        clone3_compat: stage_seccomp_blob(&crate::seccomp::compile_clone3_compat()?)?,
        baseline: stage_seccomp_blob(&compile_baseline(
            manifest.sandbox.seccomp_profile(&manifest.package_id),
        )?)?,
    };

    // Build the exact production sandbox shape. Capture changes stdout only.
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
    let mut keep_fds: Vec<RawFd> = resolved.iter().map(|r| r.child_fd as RawFd).collect();
    if identity_broker_app.is_some() {
        keep_fds.push(runtime_identity::broker::BROKER_CHILD_FD);
    }
    // Same egress decision as `run_with`: without the gate spliced in here
    // too, a host-scoped grant would be silently offline. No shim on this
    // path, so no loopback relay — ALL_PROXY has nothing listening behind it.
    let egress = crate::sandbox::egress_decision(manifest, &capabilities);
    let (net_gate, _gate_guard) = start_egress_gate(&egress)?;
    // Entrypoint + trailing arguments, resolved the same way `run_with`
    // does — both entries must agree on binary and arguments.
    let (mut argv, extra_setenv, passthrough) = build_initial_argv(
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
    splice_tool_context(&mut argv)?;
    splice_net_gate_bind(&mut argv, net_gate.as_ref())?;
    splice_workspace_id_bind(&mut argv, opts)?;
    let app_dir = resolve_app_dir(manifest, opts)?;
    splice_app_dir(&mut argv, &app_dir, &capabilities)?;
    splice_trusted_identities(&mut argv, manifest, opts)?;
    let wayland_bind = splice_wayland(&mut argv, opts)?;
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
            keep_fds: &keep_fds,
        },
    )?;

    spawn(
        argv,
        passthrough,
        opts,
        seccomp_fds,
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
/// Excludes `/usr/bin` and other non-runtime paths.
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
pub const SHIM_MOUNT_PATH: &str = "/tilo-launcher";

/// Decide how to expose D-Bus per the manifest's portal declaration.
/// Spawn `xdg-dbus-proxy` filtered to portals/notifications/MPRIS.
///
/// There is deliberately no raw-session-bus fallback. Giving an untrusted
/// sandbox the complete user bus because a filtering helper is missing turns
/// an availability problem into a privilege-escalation path (and lets the app
/// call portal *implementation* services directly). A failed proxy therefore
/// means portals are unavailable for that launch.
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

    // The only path: xdg-dbus-proxy filtered to portal-adjacent names.
    if let Some(binary) = dbus_proxy::find_proxy_binary() {
        let sock = dbus_proxy::proxy_socket_path(uid, std::process::id());
        match dbus_proxy::spawn_dbus_proxy(&binary, &bus_addr, &sock) {
            Ok(guard) => {
                let plan = DBusBindPlan {
                    host_socket: guard.socket_path().to_path_buf(),
                    sandbox_path: dbus_proxy::sandbox_bus_path(uid),
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
                    "xdg-dbus-proxy failed to spawn; portal access stays unavailable"
                );
            }
        }
    } else {
        tracing::warn!(
            "xdg-dbus-proxy unavailable; portal access stays unavailable. \
             Check TILO_LAUNCHER_DBUS_PROXY_BIN or install `flatpak-xdg-utils` \
             to enable filtered portal traffic."
        );
    }
    (None, None)
}

#[cfg(test)]
mod dbus_resolution_tests {
    use super::*;

    #[test]
    fn a_missing_filter_proxy_never_falls_back_to_the_raw_session_bus() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(
            crate::dbus_proxy::PROXY_BIN_ENV,
            "/definitely/missing/xdg-dbus-proxy",
        )
        .set("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1234/bus");
        let portals = std::collections::BTreeMap::from([(
            "notifications".to_string(),
            PortalPolicy::ViaHost,
        )]);

        let (plan, guard) = resolve_dbus_bind(&portals);

        assert!(plan.is_none(), "a raw D-Bus bind must never be planned");
        assert!(guard.is_none(), "no proxy process should have started");
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

    /// A store directory holding a `primary` record. The resolver only checks
    /// that the record is a regular file, so the content is irrelevant here.
    fn store_with_primary(root: &Path) -> PathBuf {
        let store = root.join("tilo").join("identity");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("primary.json"), b"{}").unwrap();
        store
    }

    #[test]
    fn identity_record_resolution_honours_the_requested_source() {
        // Phase `explicit_record_wins_and_skips_existence_check`.
        {
            let opts = RunOptions {
                identity: IdentitySource::Record(PathBuf::from("/nowhere/does/not/exist.json")),
                ..RunOptions::default()
            };
            assert_eq!(
                resolve_identity_host_path(&opts),
                Some(PathBuf::from("/nowhere/does/not/exist.json")),
                "explicit path is used verbatim, no exists() check"
            );
        }
        // Phase `a_named_store_resolves_its_active_identity`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let store = store_with_primary(tmp.path());
            let opts = RunOptions {
                identity: IdentitySource::Store(store.clone()),
                ..RunOptions::default()
            };
            assert_eq!(
                resolve_identity_host_path(&opts),
                Some(store.join("primary.json"))
            );
        }
        // Phase `an_empty_store_resolves_nothing`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let opts = RunOptions {
                identity: IdentitySource::Store(tmp.path().to_path_buf()),
                ..RunOptions::default()
            };
            assert_eq!(resolve_identity_host_path(&opts), None);
        }
        // Phase `no_identity_ignores_the_users_store`.
        // `None` means no identity even when the user's store holds one.
        {
            let home = tilo_test_env::TestHome::new().unwrap();
            store_with_primary(home.config());
            let opts = RunOptions {
                identity: IdentitySource::None,
                ..RunOptions::default()
            };
            assert_eq!(resolve_identity_host_path(&opts), None);
        }
        // Phase `explicit_identity_outside_the_managed_store_is_never_rewritten`.
        {
            use std::os::unix::fs::PermissionsExt;

            let dir = tempfile::tempdir().unwrap();
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
            let store = tempfile::tempdir().unwrap();
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

            let (app, control, thread) = start_identity_broker(&path, Some(store.path())).unwrap();
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
            assert!(!store.path().join("secrets").exists());
        }
    }

    // The user's store is located through the environment; these pin that
    // resolution itself.

    #[test]
    fn the_user_identity_store_is_located_via_xdg_then_home() {
        // Phase `the_user_store_prefers_xdg_config_home`.
        {
            let mut home = tilo_test_env::TestHome::new().unwrap();
            // A record under `$HOME/.config` must lose to `$XDG_CONFIG_HOME`.
            store_with_primary(&home.home().join(".config"));
            let xdg = home.root().join("xdg-config");
            let ident = store_with_primary(&xdg).join("primary.json");
            home.env().set("XDG_CONFIG_HOME", &xdg);
            assert_eq!(
                resolve_identity_host_path(&RunOptions::default()),
                Some(ident)
            );
        }
        // Phase `the_user_store_falls_back_to_home_without_xdg`.
        {
            let mut home = tilo_test_env::TestHome::new().unwrap();
            let ident = store_with_primary(&home.home().join(".config")).join("primary.json");
            home.env().remove("XDG_CONFIG_HOME");
            assert_eq!(
                resolve_identity_host_path(&RunOptions::default()),
                Some(ident)
            );
        }
        // Phase `the_user_store_resolves_nothing_without_a_home`.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove("XDG_CONFIG_HOME").remove("HOME");
            assert_eq!(resolve_identity_host_path(&RunOptions::default()), None);
        }
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
            identity: IdentitySource::None,
            ..RunOptions::default()
        }
    }

    fn opts_at(data_home: &Path, room: Option<&str>) -> RunOptions {
        RunOptions {
            data_home: Some(data_home.to_path_buf()),
            ..opts_in(room)
        }
    }

    #[test]
    fn the_app_directory_is_resolved_per_room_and_never_from_the_manifest() {
        // Phase `workspace_launch_without_an_author_grant_is_read_only`.
        {
            let m = manifest("so.tilo.fixture");
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
        // Phase `the_launch_resolves_one_directory_per_room`.
        // The launch path resolves the directory rather than the manifest
        // naming it — the room launched into decides which one.
        {
            let data = tempfile::tempdir().expect("tempdir");

            let m = manifest("so.tilo.fixture");
            let a = resolve_app_dir(&m, &opts_at(data.path(), Some("room-a"))).expect("room a");
            let b = resolve_app_dir(&m, &opts_at(data.path(), Some("room-b"))).expect("room b");
            let personal = resolve_app_dir(&m, &opts_at(data.path(), None)).expect("personal");

            assert!(a.is_dir() && b.is_dir() && personal.is_dir());
            assert_ne!(a, b);
            assert_ne!(a, personal);
            assert!(
                !a.starts_with(&b) && !b.starts_with(&a),
                "neither room may contain the other: {a:?} {b:?}"
            );
            for dir in [&a, &b, &personal] {
                assert!(
                    dir.starts_with(data.path().join("tilo-rooms")),
                    "{dir:?} is outside the rooms tree"
                );
            }
        }
        // Phase `a_declared_write_path_is_never_created`.
        // A package cannot make the launcher create a directory it named: the
        // retired `sandbox.fs.write` block is refused before the directory step.
        {
            let data = tempfile::tempdir().expect("tempdir");

            let mut m = manifest("so.tilo.fixture");
            let victim = data.path().join("so.tilo.totp");
            m.sandbox.fs.write.push(victim.display().to_string());
            let refused = run_with(&m, Path::new("/nonexistent"), &opts_at(data.path(), None));

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
}

#[cfg(test)]
mod managed_spawn_policy_tests {
    use super::*;
    struct CurrentPolicy {
        allowed: bool,
        checked: AtomicUsize,
        spawned: AtomicUsize,
    }
    impl crate::managed::EnrolledProcessObserver for CurrentPolicy {
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
    #[test]
    fn managed_prepared_spawn_checks_current_policy_before_child_creation() {
        // This exercises the real prepared-command spawn boundary, not a package,
        // sandbox or live managed process proof. The permit's policy check is
        // covered separately by the consuming-authority unit test.
        for allowed in [false, true] {
            let policy = CurrentPolicy {
                allowed,
                checked: AtomicUsize::new(0),
                spawned: AtomicUsize::new(0),
            };
            let options = RunOptions {
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
                    identity_broker: (None, None, None),
                    latency_sensitive: false,
                },
                &crate::netgate::EgressDecision::Blocked,
                None,
                Some((&policy, None)),
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
                    matches!(result, Err(RunError::Spawn(ref error)) if error.kind() == std::io::ErrorKind::PermissionDenied)
                );
            }
        }
    }
}

#[cfg(test)]
mod debug_output_tests {
    use super::*;

    #[test]
    fn debug_output_copy_is_bounded_and_drains() {
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

    #[test]
    fn debug_output_is_off_by_default() {
        if std::env::var_os(DEBUG_STDERR_ENV).is_none() {
            assert!(debug_output_log(Path::new("/nonexistent")).is_none());
        }
    }
}
