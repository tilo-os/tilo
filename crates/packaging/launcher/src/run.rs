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
use crate::manifest::{ManifestView, PortalPolicy, read_manifest};
use crate::sandbox::{DBusBindPlan, build_bwrap_argv};
use crate::seccomp::compile_app_baseline;
use crate::services::{
    ACCOUNT_CHILD_FD, BASELINE_SECCOMP_CHILD_FD, CLONE3_COMPAT_SECCOMP_CHILD_FD,
    EXIT_REQUIRED_SERVICE_MISSING, ServiceError, ServiceLaunchContext, resolve_blob_bind,
    resolve_chooser_bind, resolve_service_binds_for_launch, to_service_binds,
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

    fn child_fds(&self, account: RawFd) -> [(RawFd, RawFd); 3] {
        [
            (
                self.clone3_compat.as_raw_fd(),
                CLONE3_COMPAT_SECCOMP_CHILD_FD,
            ),
            (self.baseline.as_raw_fd(), BASELINE_SECCOMP_CHILD_FD),
            (account, ACCOUNT_CHILD_FD),
        ]
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

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Manifest(#[from] crate::manifest::ManifestReadError),
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

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
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
    /// Original signed package root selected by a host launch. A different
    /// installation must fail before any services or sandbox are constructed.
    pub expected_package_hash: Option<String>,
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
    /// Device-local location metadata for Files; filesystem authority remains
    /// with separately reviewed workspace grants.
    pub retained_files_host_path: Option<PathBuf>,
    /// Verified, selected browser data; projected read-only to Browser GUI only.
    pub browser_continuity_host_path: Option<PathBuf>,
    /// Separate host action policy projection; never a sandbox capability grant.
    pub action_authority_host_path: Option<PathBuf>,
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
    /// Shell instance id selecting the fixed private standard console namespace.
    pub headless_console: Option<u64>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            expected_package_hash: None,
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
            retained_files_host_path: None,
            browser_continuity_host_path: None,
            action_authority_host_path: None,
            workspace_id: None,
            capabilities_path: None,
            app_id: None,
            open_document: None,
            open_uri: None,
            headless_console: None,
        }
    }
}

fn gui_open_args(opts: &RunOptions) -> Vec<String> {
    let mut args = if let Some(uri) = &opts.open_uri {
        vec![uri.clone()]
    } else {
        opts.open_document
            .as_ref()
            .map(|path| vec![path.display().to_string()])
            .unwrap_or_default()
    };
    if let Some(instance) = opts.headless_console {
        args.extend([
            "--headless-drive".into(),
            serde_json::json!({
                "serve": crate::headless_console::guest_socket(instance),
            })
            .to_string(),
        ]);
    }
    args
}

fn validate_open_uri(manifest: &ManifestView, opts: &RunOptions) -> Result<(), RunError> {
    if let Some(instance) = opts.headless_console
        && (instance == 0
            || opts.wayland_socket.is_some()
            || !matches!(opts.mode, LaunchMode::Gui)
            || manifest.entrypoint.gui_bin().is_none())
    {
        return Err(RunError::InvalidServiceLaunch(
            "invalid headless console launch".into(),
        ));
    }
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
    // Every app opens its own records; the record names nothing outside them.
    let own_record = scheme == collab_types::RECORD_URI_SCHEME;
    if rest.is_empty()
        || !(own_record || manifest.opens_schemes.iter().any(|claim| claim == &scheme))
    {
        return Err(RunError::InvalidOpenUri(format!(
            "{} does not declare scheme {scheme}",
            manifest.package_id
        )));
    }
    Ok(())
}

/// The interface versions `manifest` speaks that `provided` does not serve:
/// every service it names, required or optional, since where an optional
/// one is present the app speaks only its own version of it.
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

/// Refuse a package built against interface versions this OS does not
/// serve: the launcher sets up the app's connections, and it sets up none at
/// a version the other side does not speak. The catalog already runs the
/// image's copy instead of such an update, and the shell updates the package
/// first (docs/architecture/compatibility.md); this is the same rule for
/// every other way into a launch.
fn validate_interfaces(manifest: &ManifestView) -> Result<(), RunError> {
    let provided = package_store::interfaces::system_interfaces();
    let unmet = interfaces_unmet(manifest, provided.as_ref());
    if unmet.is_empty() {
        return Ok(());
    }
    Err(RunError::InterfacesUnmet {
        package: manifest.package_id.clone(),
        unmet: tilo_app_package::interfaces::describe_unmet(&unmet),
    })
}

fn validate_launch_mode(manifest: &ManifestView, opts: &RunOptions) -> Result<(), RunError> {
    if let Some(protected) = crate::registration_gate::protected_model_launch() {
        protected
            .validate(&manifest.package_id, opts.workspace_id.as_deref())
            .map_err(|error| RunError::InvalidServiceLaunch(error.to_string()))?;
        if opts.no_services || !matches!(&opts.mode, LaunchMode::Service { .. }) {
            return Err(RunError::InvalidServiceLaunch(
                "protected model launch requires its registered service descriptors".into(),
            ));
        }
    }
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

fn is_native_game_launch(manifest: &ManifestView, opts: &RunOptions) -> bool {
    matches!(&opts.mode, LaunchMode::Gui)
        && manifest.category.as_deref().is_some_and(|category| {
            category.eq_ignore_ascii_case("game") || category.eq_ignore_ascii_case("games")
        })
}

#[cfg(test)]
mod headless_console_launch_tests {
    use super::*;

    fn browser_manifest() -> ManifestView {
        crate::manifest::test_manifest(
            "id = \"so.tilo.browser\"\nversion = \"1\"\nkind = \"app\"",
            "[entrypoint]\nbin = \"bin/browser\"\n\
             [providers]\nschemes = [\"https\"]\n",
        )
    }

    #[test]
    fn typed_headless_console_uses_fixed_standard_wire_and_refuses_other_modes() {
        let mut opts = RunOptions {
            headless_console: Some(72),
            ..Default::default()
        };
        validate_open_uri(&browser_manifest(), &opts).unwrap();
        let args = gui_open_args(&opts);
        assert_eq!(args[0], "--headless-drive");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&args[1]).unwrap(),
            serde_json::json!({"serve":"/run/tilo/app/.headless-72/control.sock"})
        );
        opts.wayland_socket = Some("/not/a/display".into());
        assert!(validate_open_uri(&browser_manifest(), &opts).is_err());
        opts.wayland_socket = None;
        opts.headless_console = Some(0);
        assert!(validate_open_uri(&browser_manifest(), &opts).is_err());
        opts.headless_console = Some(72);
        opts.mode = LaunchMode::Service {
            passthrough_argv: Vec::new(),
        };
        assert!(validate_open_uri(&browser_manifest(), &opts).is_err());
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
            let own_record = RunOptions {
                open_uri: Some("tilo-record:3f0c9a1e-2b4d-4e8f-9a6b-7c5d1e2f3a4b".into()),
                ..Default::default()
            };
            validate_open_uri(&manifest, &own_record)
                .expect("every app opens its own records unclaimed");

            for uri in [
                "http://example.test",
                "file:///etc/passwd",
                "https://x\n--flag",
                "tilo-record:",
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
            let game: ManifestView = crate::manifest::test_manifest(
                "id = \"so.tilo.game\"\nversion = \"1\"\nkind = \"app\"\ncategory = \"Games\"",
                "[entrypoint]\nbin = \"bin/game\"\n",
            );
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
mod interface_tests {
    use super::*;
    use tilo_app_package::interfaces::ProvidedInterfaces;

    #[test]
    fn a_build_speaking_another_interface_version_is_refused() {
        let settings: ManifestView = crate::manifest::test_manifest(
            "id = \"so.tilo.settings\"\nversion = \"2\"\nkind = \"app\"",
            "[entrypoint]\nbin = \"bin/settings\"\n\
             [sandbox.services]\nsystem = \"required@2\"\ncollab = \"optional@3\"\n",
        );
        let os = |system: u32, runtime: u32| {
            ProvidedInterfaces::new([("system", system), ("runtime", runtime)], ["collab"])
        };
        let unmet = interfaces_unmet(&settings, Some(&os(1, 3)));
        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].requirement, "system@2");
        let optional = interfaces_unmet(&settings, Some(&os(2, 4)));
        assert_eq!(
            optional[0].requirement, "collab@3",
            "an optional service is spoken at its own version where it is there"
        );
        assert!(interfaces_unmet(&settings, Some(&os(2, 3))).is_empty());
        assert!(interfaces_unmet(&settings, None).is_empty());

        let message = RunError::InterfacesUnmet {
            package: "so.tilo.settings".into(),
            unmet: tilo_app_package::interfaces::describe_unmet(&unmet),
        }
        .to_string();
        assert!(message.contains("system@2 (this system provides system@1)"));
        assert!(message.contains("app remove so.tilo.settings"));
    }
}

#[cfg(test)]
mod managed_service_tests {
    use super::*;

    fn manifest(kind: &str, entrypoint_type: &str) -> ManifestView {
        crate::manifest::test_manifest(
            &format!("id = \"so.tilo.worker\"\nversion = \"1\"\nkind = \"{kind}\""),
            &format!("[entrypoint]\nbin = \"bin/worker\"\ntype = \"{entrypoint_type}\"\n"),
        )
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
    fn owned_service_refuses_policy_relaxation_before_touching_installation() {
        let strict = RunOptions {
            mode: LaunchMode::Service {
                passthrough_argv: vec![],
            },
            expected_package_hash: Some("not-an-installed-package".into()),
            identity: IdentitySource::None,
            ..RunOptions::default()
        };
        let mut variants = Vec::new();
        let mut opts = strict.clone();
        opts.mode = LaunchMode::Gui;
        variants.push(opts);
        let mut opts = strict.clone();
        opts.expected_package_hash = None;
        variants.push(opts);
        let mut opts = strict.clone();
        opts.identity = IdentitySource::UserStore;
        variants.push(opts);
        let mut opts = strict.clone();
        opts.landlock_enforce = false;
        variants.push(opts);
        let mut opts = strict.clone();
        opts.landlock_best_effort = true;
        variants.push(opts);
        let mut opts = strict.clone();
        opts.allow_missing_runtime = true;
        variants.push(opts);
        let mut opts = strict;
        opts.allow_unsigned_runtime = true;
        variants.push(opts);
        for opts in variants {
            let result = validate_owned_service_options(&opts);
            assert!(
                matches!(result, Err(RunError::InvalidServiceLaunch(message))
                if message == "owned service requires signed, confined, identity-free launch")
            );
        }
    }

    #[test]
    fn owned_service_requires_original_installation() {
        let opts = RunOptions {
            mode: LaunchMode::Service {
                passthrough_argv: vec![],
            },
            expected_package_hash: Some("not-an-installed-package".into()),
            identity: IdentitySource::None,
            // Omitted app id means no installation lookup can touch user state.
            app_id: None,
            ..RunOptions::default()
        };
        validate_owned_service_options(&opts).unwrap();
        let result = verified_original_manifest(
            Path::new("/missing/owned-service.toml"),
            Path::new("/missing/payload"),
            &opts,
            "not-an-installed-package",
        );
        assert!(
            matches!(result, Err(RunError::InvalidServiceLaunch(message))
            if message == "original signed package launch changed")
        );
    }

    #[test]
    fn signal_source_manifest_is_admitted_as_a_headless_managed_service() {
        let manifest: ManifestView =
            toml::from_str(include_str!("../signal-provider.toml")).unwrap();
        let options = RunOptions {
            app_id: Some("so.tilo.signal-provider".into()),
            mode: LaunchMode::Service {
                passthrough_argv: vec![],
            },
            no_services: true,
            identity: IdentitySource::None,
            ..RunOptions::default()
        };
        assert_eq!(
            manifest.entrypoint.cli_bin(),
            Some("bin/tilo-signal-provider")
        );
        validate_launch_mode(&manifest, &options).unwrap();
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
pub fn resolve_capabilities(manifest: &ManifestView, opts: &RunOptions) -> CapabilityPlan {
    let app_id = manifest.package_id.clone();
    let grants = opts
        .capabilities_path
        .as_deref()
        .map(load_capability_file)
        .unwrap_or_default();
    let mut plan = CapabilityPlan::for_app(&app_id, &grants.sandbox, Some(unix_now()));
    // An app the room's grants make an author changes it; a workspace launch
    // of any other app is a viewer. Treating missing or malformed policy as
    // authorship made every failure permissive.
    if grants.authors(&app_id) {
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

/// Priority: explicit override > manifest declaration (per-user, then
/// system; system first for an ABI-bound version) > no bind. `Ok(None)` (no declaration) is distinct from `Err`.
///
/// # Errors
///
/// Returns [`RunError`] if the manifest's runtime reference is malformed, if
/// the resolved bundle fails signature/publisher verification, or if no
/// bundle is found and `opts.allow_missing_runtime` is unset.
pub fn resolve_runtime_test_helper(
    manifest: &crate::manifest::ManifestView,
    opts: &RunOptions,
) -> Result<Option<(PathBuf, String)>, RunError> {
    resolve_runtime(manifest, opts)
}

/// The launch argv after [`splice_optional_runtimes`] ran on a bare `entry`
/// token, and the mounts it returned for Landlock.
///
/// # Errors
///
/// As [`splice_optional_runtimes`].
pub fn optional_runtimes_test_helper(
    manifest: &crate::manifest::ManifestView,
    opts: &RunOptions,
) -> Result<(Vec<String>, Vec<String>), RunError> {
    let mut argv = vec!["entry".to_string()];
    let mounts = splice_optional_runtimes(&mut argv, manifest, opts)?;
    Ok((argv, mounts))
}

/// Resolve/create/migrate the one directory this launch may write to,
/// keyed by `opts.workspace_id` — no list to walk.
fn resolve_app_dir(manifest: &ManifestView, opts: &RunOptions) -> Result<PathBuf, RunError> {
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

/// Resolve/create the directory the person's own records of this app are
/// kept in: one for every room it is launched into.
fn resolve_person_dir(manifest: &ManifestView, opts: &RunOptions) -> Result<PathBuf, RunError> {
    let data_home = opts
        .data_home
        .clone()
        .or_else(crate::appdir::host_data_home)
        .ok_or_else(|| RunError::NoDataHome(manifest.package_id.clone()))?;
    crate::appdir::prepare_host_person_dir(&data_home, &manifest.package_id).map_err(|source| {
        RunError::AppDir {
            package: manifest.package_id.clone(),
            path: crate::appdir::host_person_dir(&data_home, &manifest.package_id),
            source,
        }
    })
}

/// The room's app directory and the person directory a launch binds.
fn resolve_launch_dirs(
    manifest: &ManifestView,
    opts: &RunOptions,
) -> Result<(PathBuf, PathBuf), RunError> {
    Ok((
        resolve_app_dir(manifest, opts)?,
        resolve_person_dir(manifest, opts)?,
    ))
}

/// The identity store whose records a launch requires in provisioned form,
/// if any: the named store, or
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
    // A launch only reads the identity store, and only a record's public
    // fields: the broker answers metadata and refuses every signature, so it
    // never needs the person's key and never asks the secret provider for it.
    // A store record must still be in the form provisioning leaves it; one
    // that is not fails the launch and names the step that secures it. An
    // explicit debug record outside the store is read as written.
    let metadata = if store.is_some_and(|store| path.parent() == Some(store)) {
        runtime_identity::load_metadata_for_launch(path)
    } else {
        runtime_identity::load_metadata_from_path(path)
    }
    .map_err(|error| RunError::IdentityBroker(error.to_string()))?;
    let (host, app) =
        UnixStream::pair().map_err(|error| RunError::IdentityBroker(error.to_string()))?;
    let control = host
        .try_clone()
        .map_err(|error| RunError::IdentityBroker(error.to_string()))?;
    let thread = std::thread::Builder::new()
        .name("tilo-identity-broker".into())
        .spawn(move || {
            if let Err(error) = runtime_identity::broker::serve_metadata(host, &metadata) {
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
    manifest: &crate::manifest::ManifestView,
    opts: &RunOptions,
) -> Option<crate::gpu::GpuSelection> {
    let node = opts.render_node.as_ref()?;
    // The compositor resolved its node's drivers once and pinned its own
    // loaders to them; a launch it started inherits that pin. Asking every
    // installed ICD again here would load each driver on every app launch.
    let mut selection = crate::gpu::inherited_selection(node)
        .unwrap_or_else(|| crate::gpu::select_for_node(node, &crate::gpu::IcdSearch::default()));
    selection.pipeline_seed = crate::gpu::host_pipeline_seed();
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
/// system; system first for an ABI-bound version) > no bind. `Ok(None)` (no declaration) is distinct from `Err`.
///
/// # Errors
///
/// Returns [`RunError`] if the manifest's runtime reference is malformed, if
/// the resolved bundle fails signature/publisher verification, or if no
/// bundle is found and `opts.allow_missing_runtime` is unset.
pub fn resolve_runtime(
    manifest: &crate::manifest::ManifestView,
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
    Err(RunError::RuntimeBundleMissing {
        runtime: raw.to_string(),
        paths: candidates,
    })
}

/// Look `raw` (`<name>/<version>`) up where runtime bundles are installed —
/// the configured/user root first, then the immutable system fallback, and
/// the other way round for an ABI-bound version ([`runtime_candidates`]) —
/// and verify the first bundle found. Returns the bundle and its version, plus
/// every directory checked.
///
/// A developer build honours `TILO_RUNTIME_BUNDLE_DIR`; developer sessions
/// use it to keep the runtime matched to an overlaid registry/payload set
/// instead of silently binding the installed `/usr` runtime. A release build
/// ignores it (see `crate::dev_env`).
#[allow(
    clippy::type_complexity,
    reason = "the found bundle and the searched directories travel together"
)]
fn find_runtime_bundle(
    raw: &str,
    opts: &RunOptions,
) -> Result<(Option<(PathBuf, String)>, Vec<PathBuf>), RunError> {
    let (_, version) = raw
        .split_once('/')
        .filter(|(name, version)| !name.is_empty() && valid_runtime_version(version))
        .ok_or_else(|| RunError::RuntimeReferenceMalformed(raw.to_string()))?;
    let per_user = runtime_bundle::default_bundle_path_from(
        package_store::dev_env::dev_override(runtime_bundle::RUNTIME_DIR_ENV),
        version,
    )
    .ok();
    let system = runtime_bundle::system_bundle_path(version);
    let candidates = runtime_candidates(version, per_user, system.clone());
    for cand in &candidates {
        if cand.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME).exists() {
            enforce_bundle_policy(cand, opts, cand == &system)?;
            return Ok((Some((cand.clone(), version.to_string())), candidates));
        }
    }
    Ok((None, candidates))
}

/// Where to look for runtime `version`, in order.
///
/// A person's own store comes first, so a runtime they installed stands in
/// for the system's of the same name. An ABI-bound version
/// (`<release>+native.<digest>`) is the exception: its name is its payload's
/// identity (the digest of the root hash every launch verifies), so the
/// system's bundle of it holds the same bytes as the person's. Apps then run
/// on the system's copy: proven once by its immutable root-owned tree rather
/// than hashed at each launch, and the copy `tilo-runtime-resident.service`
/// keeps in memory for every person, instead of a second copy of the same
/// file in each person's store.
fn runtime_candidates(version: &str, per_user: Option<PathBuf>, system: PathBuf) -> Vec<PathBuf> {
    if runtime_bundle::split_abi_version(version).is_some() {
        [system].into_iter().chain(per_user).collect()
    } else {
        per_user.into_iter().chain([system]).collect()
    }
}

/// A version names a directory under the runtime roots and the sandbox's
/// `/run/tilo-runtime`, so it is one plain path component.
pub(crate) fn valid_runtime_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 64
        && version != "."
        && version != ".."
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'_'))
}

/// The minimal-libs stopgap and the manifest's optional runtimes, spliced
/// before the entrypoint. Returns the sandbox paths Landlock must let the app
/// read.
fn splice_runtime_mounts(
    argv: &mut Vec<String>,
    manifest: &crate::manifest::ManifestView,
    opts: &RunOptions,
    resolved_runtime_present: bool,
) -> Result<Vec<String>, RunError> {
    let optional = resolve_optional_runtimes(manifest, opts);
    splice_runtime_mounts_with(argv, opts, resolved_runtime_present, &optional)
}

/// [`splice_runtime_mounts`] for optional runtimes already resolved.
fn splice_runtime_mounts_with(
    argv: &mut Vec<String>,
    opts: &RunOptions,
    resolved_runtime_present: bool,
    optional: &[(PathBuf, String)],
) -> Result<Vec<String>, RunError> {
    let mut paths = splice_minimal_libs(argv, opts, resolved_runtime_present)?;
    paths.extend(splice_resolved_optional_runtimes(argv, optional)?);
    Ok(paths)
}

/// Every installed, verified bundle the manifest lists in
/// `[package].optional-runtimes`, with its version. A missing bundle is
/// skipped and one that fails verification is refused, so the app sees either
/// as not installed and offers to install it.
///
/// Verifying a per-user bundle reads the whole payload, so the launch runs
/// this beside the rest of host preparation rather than after it.
fn resolve_optional_runtimes(
    manifest: &crate::manifest::ManifestView,
    opts: &RunOptions,
) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    for raw in &manifest.optional_runtimes {
        match find_runtime_bundle(raw, opts) {
            Ok((Some(bundle), _)) => found.push(bundle),
            Ok((None, searched)) => tracing::info!(
                runtime = %raw,
                searched = ?searched,
                "optional runtime is not installed"
            ),
            Err(error) => tracing::warn!(
                runtime = %raw,
                %error,
                "optional runtime refused; launching without it"
            ),
        }
    }
    found
}

/// Mount each of `bundles` (from [`resolve_optional_runtimes`]) read-only at
/// its runtime mount, before the trailing entrypoint token. Returns the
/// sandbox paths Landlock must let the app read.
fn splice_resolved_optional_runtimes(
    argv: &mut Vec<String>,
    bundles: &[(PathBuf, String)],
) -> Result<Vec<String>, RunError> {
    let mut mounts = Vec::new();
    for (root, version) in bundles {
        let entry = pop_entry(argv)?;
        let mount = crate::sandbox::push_optional_runtime(argv, root, version);
        argv.push(entry);
        mounts.push(mount.display().to_string());
    }
    Ok(mounts)
}

/// [`resolve_optional_runtimes`] then [`splice_resolved_optional_runtimes`].
fn splice_optional_runtimes(
    argv: &mut Vec<String>,
    manifest: &crate::manifest::ManifestView,
    opts: &RunOptions,
) -> Result<Vec<String>, RunError> {
    splice_resolved_optional_runtimes(argv, &resolve_optional_runtimes(manifest, opts))
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
    verified_bundle(path, opts, system_bundle).map(drop)
}

/// A runtime bundle a person installed into their own store, verified by the
/// policy a launch applies to it ([`enforce_bundle_policy`]): every payload
/// byte proven (by its signed fs-verity digest, else hashed), signed by a
/// trusted publisher. Returns its manifest, whose root hash is the bundle's
/// identity.
///
/// # Errors
///
/// The [`RunError`] a launch of an app on this bundle would refuse with.
pub fn verified_user_bundle(path: &Path) -> Result<runtime_bundle::BundleManifest, RunError> {
    verified_bundle(path, &RunOptions::default(), false)
}

/// [`enforce_bundle_policy`], returning the verified manifest.
fn verified_bundle(
    path: &Path,
    opts: &RunOptions,
    system_bundle: bool,
) -> Result<runtime_bundle::BundleManifest, RunError> {
    let trusted = load_trusted_publishers_for_bundle(path).map_err(|error| {
        RunError::RuntimeBundleSignature {
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
        // A per-user bundle is writable by the user's unconfined processes,
        // so every byte must be proven, not assumed: files sealed with
        // fs-verity at install by their publisher-signed digest are proven
        // by the kernel without being read, the rest are hashed in full.
        runtime_bundle::verify_bundle_verity_with(path, &verify_opts).map(|(manifest, check)| {
            tracing::debug!(
                bundle = %path.display(),
                by_verity = check.by_verity,
                hashed = check.hashed,
                hashed_bytes = check.hashed_bytes,
                "per-user runtime verified"
            );
            manifest
        })
    };
    match verified {
        Ok(manifest) => {
            trusted
                .check(&manifest)
                .map_err(|error| RunError::RuntimeBundleSignature {
                    path: path.to_path_buf(),
                    reason: error.to_string(),
                })?;
            Ok(manifest)
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

/// A per-user runtime is writable by the user's unconfined processes, so a
/// launch must prove every byte: sealed files by their signed
/// fs-verity digest without reading them, every other file by its hash.
#[cfg(test)]
#[cfg(unix)]
mod per_user_runtime_verification_tests {
    use super::*;
    use std::path::Path;

    const DYLIB: &str = "lib/libruntime_dylib.so";

    /// A signed per-user runtime under `parent` whose toolkit dylib is
    /// `dylib_size` bytes, and launch options that pin its publisher.
    fn per_user_runtime(
        parent: &Path,
        dylib_size: usize,
    ) -> (tempfile::TempDir, PathBuf, RunOptions) {
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
        let opts = RunOptions {
            require_runtime_publisher: Some(identity.public_key_hex()),
            ..Default::default()
        };
        (work, bundle, opts)
    }

    /// Replace `path` with its bytes but one, as anyone who can write the
    /// per-user runtime directory can, sealed file or not.
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
            Err(RunError::RuntimeBundleSignature { .. })
        ));
    }

    /// Bytes the calling thread has read: the other tests in this binary run
    /// on other threads.
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

    /// A temporary directory on a filesystem with fs-verity:
    /// `TILO_FSVERITY_DIR`, else the system temporary directory.
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

        // phase: the_launch_reads_none_of_the_payload
        let before = thread_bytes_read();
        let started = std::time::Instant::now();
        enforce_bundle_policy(&bundle, &opts, false).unwrap();
        let elapsed = started.elapsed();
        let read = thread_bytes_read().saturating_sub(before);
        eprintln!("per-user 64 MiB runtime verified in {elapsed:?}, {read} bytes read");
        assert!(read < 1 << 20, "the launch read {read} bytes");

        // phase: a_replaced_file_is_refused_before_the_sandbox_is_built
        replace_with_one_byte_changed(&bundle.join(DYLIB));
        assert!(matches!(
            enforce_bundle_policy(&bundle, &opts, false),
            Err(RunError::RuntimeBundleSignature { .. })
        ));
    }
}

/// A single-entry trust list is the pin handed to
/// `verify_bundle_with`; multi-entry lists are checked after it returns.
/// Read operator policy and canonical immutable component publisher pins.
/// A present invalid component pin fails closed, including when another key
/// would otherwise validate the bundle.
pub(crate) fn load_trusted_publishers_for_bundle(
    bundle: &Path,
) -> std::io::Result<runtime_bundle::component_pins::BundleTrust> {
    let path = trusted_publishers_path_from(package_store::dev_env::dev_override(
        crate::dev_env::TRUSTED_PUBLISHERS_FILE_ENV,
    ));
    runtime_bundle::component_pins::load_for_bundle(&path, bundle)
}

/// The image's runtime trusted-publisher list.
pub const TRUSTED_PUBLISHERS_FILE: &str = "/etc/tilo/runtime-bundle-trusted-publishers.txt";

/// Where the trusted-publisher list is read from, given the override the
/// build's gate let through ([`package_store::dev_env::dev_override`]).
fn trusted_publishers_path_from(allowed_override: Option<std::ffi::OsString>) -> PathBuf {
    allowed_override.map_or_else(|| PathBuf::from(TRUSTED_PUBLISHERS_FILE), PathBuf::from)
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
        // A release-only version (a development bundle, an engine runtime)
        // names no payload: the person's own install stands in for the
        // system's of that name.
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
    let manifest = launch_manifest(manifest_path, payload_root, opts)?;
    run_manifest(&manifest, payload_root, opts)
}

/// The first half of [`run`]: the manifest to launch, verified against the
/// signed installation when `opts` names one (`--expected-package-hash`).
/// Reads and hashes files only; it starts no process.
///
/// # Errors
///
/// As [`run`], for a manifest that can't be read or an installation that
/// changed.
pub fn launch_manifest(
    manifest_path: &Path,
    payload_root: &Path,
    opts: &RunOptions,
) -> Result<ManifestView, RunError> {
    let manifest = if let Some(expected) = opts.expected_package_hash.as_deref() {
        verified_original_manifest(manifest_path, payload_root, opts, expected)?
    } else {
        read_manifest(manifest_path)?
    };
    tilo_startup_metrics::handoff_mark("package-verified");
    Ok(manifest)
}

/// The second half of [`run`]: build the sandbox for a manifest from
/// [`launch_manifest`] and run the app in it.
///
/// # Errors
///
/// As [`run`].
pub fn run_manifest(
    manifest: &ManifestView,
    payload_root: &Path,
    opts: &RunOptions,
) -> Result<ExitStatus, RunError> {
    run_with_context(manifest, payload_root, opts, None, None)
}

/// Run an ordinary signed GUI application with canonical namespace custody.
/// This changes supervision only: services, egress, private state and sandbox
/// grants still come from the ordinary application pipeline. The owner does
/// not confer retained file authority.
///
/// # Errors
/// Refuses unsigned/unpinned launches, non-GUI modes, changed installation,
/// failed namespace observation or retired owner authority.
pub fn run_supervised_gui(
    manifest_path: &Path,
    payload_root: &Path,
    opts: &RunOptions,
    owner: &crate::ordinary_namespace::OrdinaryNamespaceOwner,
) -> Result<ExitStatus, RunError> {
    if !matches!(opts.mode, LaunchMode::Gui) {
        return Err(RunError::InvalidServiceLaunch(
            "ordinary namespace owner requires GUI mode".into(),
        ));
    }
    let expected = opts.expected_package_hash.as_deref().ok_or_else(|| {
        RunError::InvalidServiceLaunch(
            "ordinary namespace owner requires signed package pin".into(),
        )
    })?;
    let manifest = verified_original_manifest(manifest_path, payload_root, opts, expected)?;
    let mut pinned = opts.clone();
    if pinned.bwrap_bin.is_none() {
        pinned.bwrap_bin = Some("/usr/bin/bwrap".into());
    }
    run_with_context(&manifest, payload_root, &pinned, None, Some(owner))
}

pub(crate) fn verified_original_manifest(
    manifest_path: &Path,
    payload_root: &Path,
    opts: &RunOptions,
    expected: &str,
) -> Result<ManifestView, RunError> {
    let refusal =
        || RunError::InvalidServiceLaunch("original signed package launch changed".into());
    let app = opts.app_id.as_deref().ok_or_else(refusal)?;
    let installed = package_store::find_installed(app)
        .map_err(|_| refusal())?
        .ok_or_else(refusal)?;
    verified_launch_installation(&installed, manifest_path, payload_root, expected)
}

pub(crate) fn verified_launch_installation(
    installed: &package_store::InstalledPackage,
    manifest_path: &Path,
    payload_root: &Path,
    expected: &str,
) -> Result<ManifestView, RunError> {
    // Re-read on every launch, as `tilo-install run` does: a publisher
    // revoked after install must not keep launching from the shell. An
    // unreadable list refuses rather than reading as "nothing revoked".
    use tilo_app_package::TrustFile as _;
    let revoked = match tilo_app_package::default_revoked_keys_path() {
        Some(path) => tilo_app_package::RevocationList::load(&path).map_err(|error| {
            RunError::InvalidServiceLaunch(format!(
                "the publisher revocation list is unreadable ({error})"
            ))
        })?,
        None => tilo_app_package::RevocationList::default(),
    };
    verified_launch_installation_against(installed, manifest_path, payload_root, expected, &revoked)
}

pub(crate) fn verified_launch_installation_against(
    installed: &package_store::InstalledPackage,
    manifest_path: &Path,
    payload_root: &Path,
    expected: &str,
    revoked: &tilo_app_package::RevocationList,
) -> Result<ManifestView, RunError> {
    let refusal =
        || RunError::InvalidServiceLaunch("original signed package launch changed".into());
    if installed.package_id_hex != expected
        || installed.manifest_path != manifest_path
        || installed.payload_root != payload_root
    {
        return Err(refusal());
    }
    let report =
        package_store::integrity::verify(installed, payload_root).map_err(|_| refusal())?;
    if !report.is_intact() {
        return Err(refusal());
    }
    let record = package_store::integrity::read_record(
        &package_store::integrity::record_dir(installed),
        &installed.id,
        expected,
    )
    .map_err(|_| refusal())?;
    if let Some(entry) = revoked.find(&record.root.publisher_identity) {
        return Err(RunError::InvalidServiceLaunch(format!(
            "the package's publisher key is revoked ({})",
            entry.reason
        )));
    }
    let manifest: ManifestView =
        toml::from_str(std::str::from_utf8(&record.manifest_bytes).map_err(|_| refusal())?)
            .map_err(|_| refusal())?;
    if manifest.package_id != installed.id {
        return Err(refusal());
    }
    Ok(manifest)
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
            hosts: policy.describe(),
            source,
        }),
    }
}

/// Put service sockets, seccomp programs and account data on their child fds, in the
/// child, before `exec`. bwrap forwards fds it wasn't asked about
/// untouched, so a placement here survives namespace setup.
///
/// Not bwrap's `--file FD DEST`: that copies the descriptor's *contents*
/// into a regular file, so a connected socket blocks reading for an EOF
/// that never comes — the launch hangs before `exec` with no symptom.
///
/// Two passes: a source can already sit on
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
    for (src, _) in pairs.iter_mut() {
        // F_DUPFD returns the lowest free fd >= floor, i.e. clear of
        // every target.
        let moved = unsafe { libc::fcntl(*src, libc::F_DUPFD, floor) };
        if moved < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // The original number is done with — a second live descriptor at an
        // unnamed number is one the app cannot close.
        unsafe { libc::close(*src) };
        *src = moved;
    }
    for (src, dst) in pairs.iter() {
        if unsafe { libc::dup2(*src, *dst) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
        // dup2 leaves the target without FD_CLOEXEC — it must survive bwrap's and
        // the app's exec.
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

/// Hand this launch's phase marks to the app
/// ([`tilo_startup_metrics::LAUNCHER_MARKS_ENV`]), so its startup file also
/// covers the launcher. Only when the shell asked for startup metrics; the
/// value is the launcher's own fixed mark names and wall-clock times. The
/// last mark is taken here, after every check and before the sandbox spawns.
fn splice_startup_marks(argv: &mut Vec<String>) -> Result<(), RunError> {
    tilo_startup_metrics::handoff_mark("sandbox-argv-ready");
    splice_cli_setenv(
        argv,
        tilo_startup_metrics::handoff_env_value()
            .map(|marks| (tilo_startup_metrics::LAUNCHER_MARKS_ENV.to_string(), marks)),
    )
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
            // The fixed system profile already binds these parents read-only.
            // Rebinding a child can land on a distro symlink (libgcc_s.so.1),
            // which bubblewrap rightly refuses as a mount destination.
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
    person_dir: &Path,
    capabilities: &CapabilityPlan,
) -> Result<(), RunError> {
    let entry = pop_entry(argv)?;
    crate::appdir::push_app_dir(argv, app_dir);
    crate::appdir::push_person_dir(argv, person_dir);
    // After `push_app_dir` (which repoints `HOME`): this says where the
    // *user's* home is, only when a grant actually bound it.
    if let Some(user_home) = crate::sandbox::push_user_home(argv, capabilities) {
        tracing::debug!(
            user_home = %user_home.display(),
            "a granted bind covers the user's home; telling the app where it is"
        );
    }
    crate::sandbox::push_user_data(argv, capabilities);
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

/// Settings remaps XDG into the app dir. Bind the parts of the session's
/// `~/.config/tilo` it edits over that path
/// ([`crate::appdir::SESSION_SETTINGS_BINDS`]), so a write to the settings
/// document is the one the compositor watches and the open Settings window
/// paints, while the rest of that directory (trust lists, registries,
/// records) stays out of the sandbox.
fn splice_session_settings(argv: &mut Vec<String>, package_id: &str) -> Result<(), RunError> {
    use std::os::unix::fs::DirBuilderExt as _;

    if package_id != "so.tilo.settings" {
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

/// The image-owned system authority grant ([`crate::system_authority`]):
/// bind the control panel's host binaries, configuration and authority
/// sockets. A no-op for every other package. The returned guard keeps the
/// system-bus proxy alive and must outlive the child.
fn splice_system_authority(
    argv: &mut Vec<String>,
    manifest: &ManifestView,
) -> Result<
    (
        Option<crate::system_authority::SystemAuthorityPlan>,
        Option<DBusProxyGuard>,
    ),
    RunError,
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

/// Mount Files' live room-governance identity directory read-only. The
/// projection is package-scoped and must remain visible across atomic host
/// replacement, so the containing directory—not one inode—is bound.
fn splice_trusted_identities(
    argv: &mut Vec<String>,
    manifest: &ManifestView,
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

/// Expose only host-owned retained-location metadata, never a file grant.
fn splice_retained_files(
    argv: &mut Vec<String>,
    manifest: &ManifestView,
    opts: &RunOptions,
) -> Result<(), RunError> {
    if manifest.package_id != "so.tilo.file-explorer" {
        return Ok(());
    }
    let Some(path) = opts.retained_files_host_path.as_deref() else {
        return Ok(());
    };
    validate_retained_files_projection(path)?;
    let entry = pop_entry(argv)?;
    crate::sandbox::push_retained_files_bind(argv, path);
    argv.push(entry);
    Ok(())
}

fn validate_retained_files_projection(path: &Path) -> Result<(), RunError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let invalid =
        || RunError::InvalidServiceLaunch("invalid retained file location projection".into());
    if !path.is_absolute()
        || path.file_name().and_then(|name| name.to_str()) != Some("locations.json")
    {
        return Err(invalid());
    }
    let parent = path.parent().ok_or_else(invalid)?;
    let directory = std::fs::symlink_metadata(parent)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !directory.is_dir()
        || directory.uid() != current_uid()
        || directory.mode() & 0o077 != 0
        || !metadata.is_file()
        || metadata.uid() != current_uid()
        || metadata.mode() & 0o077 != 0
        || metadata.len() > 16 * 1024
    {
        return Err(invalid());
    }
    Ok(())
}

/// Bind only one app/person projection directory so atomic replacement and
/// revocation are visible without exposing the host capability store.
fn splice_action_authority(
    argv: &mut Vec<String>,
    manifest: &ManifestView,
    opts: &RunOptions,
) -> Result<(), RunError> {
    use collab_types::action_authority::{AppActionAuthorityProjection, MAX_PROJECTION_BYTES};
    use std::io::Read as _;
    let Some(path) = opts.action_authority_host_path.as_deref() else {
        return Ok(());
    };
    let invalid = || RunError::InvalidServiceLaunch("invalid host action authority binding".into());
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
    let policy: AppActionAuthorityProjection =
        serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    policy.validate().map_err(|_| invalid())?;
    if policy.app_id != manifest.package_id || opts.workspace_id.as_deref() != Some(&policy.room) {
        return Err(invalid());
    }
    let entry = pop_entry(argv)?;
    crate::sandbox::push_action_authority_bind(argv, path);
    argv.push(entry);
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
    manifest: &ManifestView,
    opts: &RunOptions,
    gpu_granted: bool,
) -> Result<(Option<crate::gpu::GpuSelection>, Option<PathBuf>), RunError> {
    // An app without `gpu` draws through its room's render service: no
    // device node, no loader pin, no shader-cache seed, and none of the ICD
    // probing they cost.
    if !gpu_granted {
        return Ok((None, None));
    }
    let entry = pop_entry(argv)?;
    crate::profile::push_gpu_devices(argv);
    argv.push(entry);
    let gpu = resolve_gpu_selection(manifest, opts);
    if let Some(sel) = &gpu {
        let entry = pop_entry(argv)?;
        // The device nodes were just bound: narrow them to the node.
        crate::sandbox::push_gpu_selection(argv, sel, true);
        argv.push(entry);
    } else if opts.render_node.is_none()
        && let Some(pin) = crate::gpu::inherited_software_pin()
    {
        // A compositor with no GPU renders with lavapipe and pinned its own
        // loaders to it; its apps follow, as they follow a GPU's pin.
        let entry = pop_entry(argv)?;
        crate::sandbox::push_software_pin(argv, &pin);
        argv.push(entry);
    }
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

/// Read-only metadata rules match the host projection mounts; original file
/// permissions remain in the independently resolved capability plan.
pub(crate) fn push_projection_landlock_rules(
    argv: &mut Vec<String>,
    opts: &RunOptions,
    manifest: &ManifestView,
) {
    if opts.action_authority_host_path.is_some() {
        argv.push("--ro".into());
        argv.push(crate::sandbox::SANDBOX_ACTION_AUTHORITY_DIR.into());
    }
    if crate::browser_projection::eligible(manifest, opts) {
        argv.extend([
            "--ro".to_owned(),
            browser_continuity::PROJECTION_DIRECTORY.to_owned(),
        ]);
    }
    if manifest.package_id == "so.tilo.file-explorer" && opts.retained_files_host_path.is_some() {
        argv.extend([
            "--ro".to_owned(),
            crate::sandbox::SANDBOX_RETAINED_FILES_DIR.to_owned(),
        ]);
    }
}

/// Everything [`splice_sandbox_shim`] needs beyond `argv` and `opts`.
struct ShimSpliceCtx<'a> {
    manifest: &'a ManifestView,
    net_gate: &'a Option<crate::sandbox::NetGateBind>,
    runtime_arg: Option<(&'a Path, &'a str)>,
    extra_ro_paths: &'a [String],
    egress: &'a crate::netgate::EgressDecision,
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
    push_runtime_landlock_rules(argv, ctx.runtime_arg);
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
    // This is generated sandbox data, never the host account database.
    argv.extend(["--ro".to_string(), "/etc/passwd".to_string()]);
    push_projection_landlock_rules(argv, opts, ctx.manifest);
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
    // The control panel's host binaries, configuration and authority
    // sockets, from the same plan its binds came from.
    if let Some(plan) = ctx.system_authority {
        crate::system_authority::push_landlock_rules(argv, plan);
    }
    // Wayland needs read+write in Landlock: connect(2) on a unix socket is
    // write-flavoured, so read-only surfaces as EACCES at `wl_display_connect`.
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
    // The app's own directory, named by its in-sandbox path — the host path
    // isn't reachable from inside, so a rule on it would match nothing.
    argv.push("--rw".to_string());
    argv.push(crate::appdir::SANDBOX_APP_DIR.to_string());
    // And the person's records of it, bound the same way.
    argv.push("--rw".to_string());
    argv.push(crate::appdir::SANDBOX_PERSON_DIR.to_string());
    // The GPU device nodes a `gpu` app was bound — same O_RDWR reasoning
    // as above.
    if ctx.gpu_devices {
        for path in crate::profile::landlock_rw_device_paths() {
            argv.push("--rw".to_string());
            argv.push(path.display().to_string());
        }
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
}

fn push_scratch_and_engine_rules(argv: &mut Vec<String>, manifest: &ManifestView) {
    // Standard tmpfs mounts the app expects writable; --rw grants full FS
    // access, and both vanish with the process (scratch, not hidden state).
    for p in ["/tmp", "/dev"] {
        argv.push("--rw".to_string());
        argv.push(p.to_string());
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

/// Give bwrap a private, read-only account file. No host `/etc/passwd` bind.
fn stage_sandbox_account(
    argv: &mut Vec<String>,
    original_identity: Option<(u32, u32)>,
) -> Result<OwnedFd, RunError> {
    let (uid, gid) = original_identity.unwrap_or_else(|| {
        (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw(),
        )
    });
    let account = sandbox_account(uid, gid);
    let data = stage_seccomp_blob(account.as_bytes())?;
    // Keep the source above the fixed child slots: earlier dup2 calls must
    // not overwrite it before the account descriptor is handed to bwrap.
    let fd = unsafe { libc::fcntl(data.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 128) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let owned = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    let mut binding = Vec::new();
    if original_identity.is_some() {
        // One fixed non-secret synthetic account, readable after credential
        // drop; this mode applies to this exact next data operation only.
        binding.extend(["--perms".into(), "0444".into()]);
    }
    binding.extend([
        "--ro-bind-data".into(),
        ACCOUNT_CHILD_FD.to_string(),
        "/etc/passwd".into(),
    ]);
    argv.splice(0..0, binding);
    Ok(owned)
}

#[cfg(test)]
mod sandbox_account_tests {
    use super::*;

    #[test]
    fn handoff_preserves_a_service_source_in_the_account_child_slot() {
        let service = stage_seccomp_blob(b"service").expect("service fixture");
        let filters = SeccompFds {
            clone3_compat: stage_seccomp_blob(b"clone3").expect("compat fixture"),
            baseline: stage_seccomp_blob(b"baseline").expect("baseline fixture"),
        };
        let account = stage_sandbox_account(&mut Vec::new(), None).expect("account");
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
        // The production handoff must preserve it before assigning account 42.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(source, ACCOUNT_CHILD_FD) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                place_child_fds(&mut pairs)
            });
        }
        let output = command.output().expect("fixture child");
        assert!(output.status.success(), "{:?}", output.stderr);
        assert_eq!(output.stdout, expected.as_bytes());
    }

    #[test]
    fn staged_account_is_private_read_only_data_and_reserves_its_descriptor() {
        use std::io::Read as _;
        let mut argv = vec!["--unshare-all".to_string(), "/app/bin".to_string()];
        let fd = stage_sandbox_account(&mut argv, None).expect("stage account");
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

    #[test]
    fn account_contains_only_the_sandbox_identity_and_home() {
        assert_eq!(
            sandbox_account(1000, 1001),
            "app:x:1000:1001:Sandbox application:/run/tilo/app:/sbin/nologin\n"
        );
        assert!(crate::services::RESERVED_CHILD_FDS.contains(&ACCOUNT_CHILD_FD));
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
    let target_projection =
        observer.and_then(crate::managed::EnrolledProcessObserver::target_projection);
    if let Some(target) = target_projection {
        target.prepare_filter(&seccomp_fds.baseline)?;
        target.splice_baseline_argv(&mut argv)?;
    }
    let original_baseline = observer
        .and_then(crate::managed::EnrolledProcessObserver::capture_projection)
        .map(|capture| capture.original_baseline_filter(&seccomp_fds.baseline))
        .transpose()?
        .flatten();
    if let Some(lease) = &original_baseline {
        lease.splice_argv(&mut argv)?;
    }
    let original_identity = observer
        .and_then(crate::managed::EnrolledProcessObserver::capture_projection)
        .and_then(crate::owned_service::CaptureAdmission::original_sandbox_account_identity)
        .or_else(|| {
            observer
                .and_then(crate::managed::EnrolledProcessObserver::retained_io_projection)
                .map(crate::owned_service::RetainedIoAdmission::identity)
        })
        .or_else(|| {
            target_projection.map(crate::owned_service::TargetProjectionAdmission::identity)
        });
    let account_fd = stage_sandbox_account(&mut argv, original_identity)?;
    let host_account_fd = account_fd.as_raw_fd();
    // The system read profile from the host's pre-built view, when it can
    // carry this launch's mounts unchanged. Held until the child is reaped:
    // a superseded view is retired only once no sandbox built from it runs.
    // Supervised launches keep their audited per-path argv.
    let system_view = if observer.is_none() {
        crate::sandbox_view::attach(&mut argv)
    } else {
        None
    };

    if observer
        .and_then(crate::managed::EnrolledProcessObserver::namespace_init_fd)
        .is_some()
    {
        // Only the stricter owned-service path asks for PID 1 supervision.
        if !argv.iter().any(|arg| arg == "--unshare-all")
            || (opts.bwrap_bin.as_deref() != Some(Path::new("/usr/bin/bwrap"))
                && observer
                    .and_then(crate::managed::EnrolledProcessObserver::capture_projection)
                    .is_none()
                && observer
                    .and_then(crate::managed::EnrolledProcessObserver::retained_io_projection)
                    .is_none()
                && target_projection.is_none())
        {
            return Err(RunError::InvalidServiceLaunch(
                "private service namespace missing".into(),
            ));
        }
        // These are bwrap options, before its command (which may already
        // be the Landlock shim plus opaque arguments). Never append them
        // next to the provider's trailing entrypoint.
        argv.splice(
            0..0,
            [
                "--as-pid-1".into(),
                "--info-fd".into(),
                crate::services::SERVICE_NAMESPACE_CHILD_FD.to_string(),
            ],
        );
    }

    // Validate the canonical full namespace plan above BEFORE the root-only
    // original-source token replaces its user-namespace component. Bootstrap
    // admission emits a fixed private PID/net/IPC/UTS plan; the signed shim
    // verifies actual isolation before saved source credentials become active.
    if let Some(capture) =
        observer.and_then(crate::managed::EnrolledProcessObserver::capture_projection)
    {
        capture.splice_original_bootstrap(&mut argv)?;
    }

    // Passthrough: everything after the entrypoint token is opaque to bwrap,
    // forwarded as the sandboxed program's argv[1..].
    argv.extend(cli_passthrough);

    // 4. Build the bwrap Command.
    let bwrap = opts
        .bwrap_bin
        .clone()
        .unwrap_or_else(|| PathBuf::from("bwrap"));
    if let Some(io) =
        observer.and_then(crate::managed::EnrolledProcessObserver::retained_io_projection)
    {
        io.splice_credentials(&mut argv)?;
    }
    if let Some(target) = target_projection {
        target.splice_credentials(&mut argv)?;
    }
    let mut cmd = if let Some(capture) =
        observer.and_then(crate::managed::EnrolledProcessObserver::capture_projection)
    {
        capture.command()
    } else if let Some(io) =
        observer.and_then(crate::managed::EnrolledProcessObserver::retained_io_projection)
    {
        io.native()?.command()
    } else if let Some(target) = target_projection {
        target.native()?.command()
    } else {
        Command::new(&bwrap)
    };
    cmd.args(&argv);
    clear_bwrap_environment(&mut cmd);
    if observer.is_some_and(crate::managed::EnrolledProcessObserver::owns_process_group) {
        cmd.process_group(0);
    }
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
    service_fds.extend(seccomp_fds.child_fds(host_account_fd));
    if let Some(fd) = observer.and_then(crate::managed::EnrolledProcessObserver::namespace_init_fd)
    {
        service_fds.push((fd, crate::services::SERVICE_NAMESPACE_CHILD_FD));
    }
    if let Some(capture) =
        observer.and_then(crate::managed::EnrolledProcessObserver::capture_projection)
    {
        service_fds.extend(capture.descriptor_pairs());
    }
    if let Some(lease) = &original_baseline {
        lease.splice_pairs(&mut service_fds)?;
    }
    if let Some(io) =
        observer.and_then(crate::managed::EnrolledProcessObserver::retained_io_projection)
    {
        service_fds.extend(io.pairs()?);
    }
    if let Some(retained) =
        observer.and_then(crate::managed::EnrolledProcessObserver::retained_view)
    {
        service_fds.extend(retained.descriptor_pairs()?);
    }
    if let Some(parent) =
        observer.and_then(crate::managed::EnrolledProcessObserver::selected_parent)
    {
        service_fds.push(parent.descriptor_pair()?);
    }
    if let Some(target) = target_projection {
        service_fds.extend(target.pairs()?);
        target.replace_baseline(&mut service_fds)?;
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
        hosts = ?egress.policy().map(super::netgate::EgressPolicy::describe),
        shares_host_network = egress.shares_host_network(),
        "egress resolved",
    );
    let _ = landlock_plan;

    // SAFETY: pre_exec runs in the child after fork, before exec. We
    // perform only async-signal-safe operations: dup2, fcntl, plus the
    // landlock crate's syscall wrappers. No allocator calls, no locks.
    let private_service = observer
        .and_then(crate::managed::EnrolledProcessObserver::namespace_init_fd)
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
    drop(account_fd);
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
    drop(system_view);
    Ok(status)
}

/// Seccomp baseline → tmpfile → fd. The manifest *asks* for a profile; the
/// image decides whether this package may have it. An unrecognised name is a
/// packaging mistake; a recognised-but-unentitled one is an app trying to
/// lift a protection off itself — logged distinctly.
fn stage_seccomp(
    manifest: &ManifestView,
    grants: crate::seccomp::SyscallGrants,
) -> Result<SeccompFds, RunError> {
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
/// The payload-relative binary a launch in `mode` executes; `None` for a host
/// application, which has no payload.
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

/// Refuse a launch whose entrypoint links against a library the sandboxed
/// loader will not find in the runtime bundle, the payload or the host's
/// library directories. Without this the app dies in the loader before
/// `main`, and the only trace is one loader line on stderr. Only a launch on
/// a runtime bundle is checked: the host-library modes have no bundle the app
/// could have been built against.
fn preflight_runtime_libraries(
    manifest: &ManifestView,
    payload_root: &Path,
    opts: &RunOptions,
    runtime: Option<(&Path, &str)>,
) -> Result<(), RunError> {
    let Some((runtime_root, version)) = runtime else {
        return Ok(());
    };
    let Some(entry_bin) = launch_entry_bin(manifest, &opts.mode) else {
        return Ok(());
    };
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
            RunError::RuntimeLibraryMissing {
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

fn build_initial_argv(
    manifest: &ManifestView,
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
            let entry_bin_owned = launch_entry_bin(manifest, &opts.mode)
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
            let entry_bin_owned = launch_entry_bin(manifest, &opts.mode)
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
    manifest: &ManifestView,
    payload_root: &Path,
    opts: &RunOptions,
) -> Result<ExitStatus, RunError> {
    if opts.expected_package_hash.is_some() {
        return Err(RunError::InvalidServiceLaunch(
            "original signed package requires manifest-backed run".into(),
        ));
    }
    run_with_context(manifest, payload_root, opts, None, None)
}

pub(crate) fn validate_owned_service_options(opts: &RunOptions) -> Result<(), RunError> {
    if !matches!(opts.mode, LaunchMode::Service { .. })
        || opts.expected_package_hash.is_none()
        || opts.identity != IdentitySource::None
        || !opts.landlock_enforce
        || opts.landlock_best_effort
        || opts.allow_missing_runtime
        || opts.allow_unsigned_runtime
    {
        return Err(RunError::InvalidServiceLaunch(
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
) -> Result<std::thread::ScopedJoinHandle<'scope, T>, RunError> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn_scoped(scope, work)
        .map_err(|error| RunError::Io(std::io::Error::other(format!("{name} thread: {error}"))))
}

/// The result of [`spawn_beside`]'s work; its panic continues here.
fn join_beside<T>(handle: std::thread::ScopedJoinHandle<'_, T>) -> T {
    handle
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// What the host prepares before a sandbox can be described: the verified
/// runtime bundle, if any, and the app's writable directories.
struct LaunchHost {
    runtime: Option<(PathBuf, String)>,
    app_dir: PathBuf,
    person_dir: PathBuf,
}

/// Verify the runtime bundle, check the entrypoint's libraries against it,
/// and create the app's directories (bwrap can only bind what exists).
fn prepare_launch_host(
    manifest: &ManifestView,
    payload_root: &Path,
    opts: &RunOptions,
    enrolled: Option<&crate::services::EnrolledServiceContext>,
    owner: Option<&dyn crate::managed::EnrolledProcessObserver>,
) -> Result<LaunchHost, RunError> {
    // A resolved bundle wins over --minimal-libs and skips the loader-bind
    // stopgap entirely.
    let retained_io =
        owner.and_then(crate::managed::EnrolledProcessObserver::retained_io_projection);
    let target_projection =
        owner.and_then(crate::managed::EnrolledProcessObserver::target_projection);
    let runtime = if enrolled.is_some() {
        Some(
            crate::managed::verify_runtime(manifest, opts)
                .map_err(RunError::InvalidServiceLaunch)?,
        )
    } else if let Some(capture) =
        owner.and_then(crate::managed::EnrolledProcessObserver::capture_projection)
    {
        Some(capture.resolve_runtime(manifest)?)
    } else if let Some(io) = retained_io {
        Some(io.native()?.resolve_runtime(manifest)?)
    } else if let Some(target) = target_projection {
        Some(target.native()?.resolve_runtime(manifest)?)
    } else {
        resolve_runtime(manifest, opts)?
    };
    if let (Some(io), Some((root, _))) = (retained_io, runtime.as_ref()) {
        io.exclude_path(root)?;
    }
    tilo_startup_metrics::handoff_mark("runtime-verified");
    let runtime_arg = runtime.as_ref().map(|(p, v)| (p.as_path(), v.as_str()));
    preflight_runtime_libraries(manifest, payload_root, opts, runtime_arg)?;
    tilo_startup_metrics::handoff_mark("loader-preflight-done");
    // The app's one writable directory, for the room it's opened into.
    let (app_dir, person_dir) = if let Some(io) = retained_io {
        io.launch_dirs()?
    } else if let Some(target) = target_projection {
        target.launch_dirs()?
    } else if let Some(services) = enrolled {
        (
            crate::appdir::prepare_host_app_dir(
                services.data_home(),
                Some(&services.binding().room),
                &services.binding().app,
            )?,
            crate::appdir::prepare_host_person_dir(services.data_home(), &services.binding().app)?,
        )
    } else {
        resolve_launch_dirs(manifest, opts)?
    };
    tilo_startup_metrics::handoff_mark("app-dirs-ready");
    Ok(LaunchHost {
        runtime,
        app_dir,
        person_dir,
    })
}

pub(crate) fn run_enrolled(
    manifest: &ManifestView,
    payload_root: &Path,
    opts: &RunOptions,
    services: &mut crate::services::EnrolledServiceContext,
    observer: &dyn crate::managed::EnrolledProcessObserver,
) -> Result<ExitStatus, RunError> {
    run_with_context(
        manifest,
        payload_root,
        opts,
        Some((services, observer)),
        None,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "one ordered launch pipeline; splitting it would scatter the policy checks that must precede spawn"
)]
pub(crate) fn run_with_context(
    manifest: &ManifestView,
    payload_root: &Path,
    opts: &RunOptions,
    mut enrolled: Option<(
        &mut crate::services::EnrolledServiceContext,
        &dyn crate::managed::EnrolledProcessObserver,
    )>,
    native_owner: Option<&dyn crate::managed::EnrolledProcessObserver>,
) -> Result<ExitStatus, RunError> {
    let retained_io =
        native_owner.and_then(crate::managed::EnrolledProcessObserver::retained_io_projection);
    let target_projection =
        native_owner.and_then(crate::managed::EnrolledProcessObserver::target_projection);
    if (retained_io.is_some() || target_projection.is_some()) && enrolled.is_some()
        || retained_io.is_some() && target_projection.is_some()
    {
        return Err(RunError::InvalidServiceLaunch(
            "Mixed retained/enrolled authority".into(),
        ));
    }
    let retained_view =
        native_owner.and_then(crate::managed::EnrolledProcessObserver::retained_view);
    let selected_parent =
        native_owner.and_then(crate::managed::EnrolledProcessObserver::selected_parent);
    if selected_parent.is_some() && retained_view.is_some() {
        return Err(RunError::InvalidServiceLaunch(
            "Mixed ordinary view grants".into(),
        ));
    }
    if (retained_view.is_some() || selected_parent.is_some())
        && (enrolled.is_some()
            || retained_io.is_some()
            || target_projection.is_some()
            || native_owner
                .and_then(crate::managed::EnrolledProcessObserver::capture_projection)
                .is_some())
    {
        return Err(RunError::InvalidServiceLaunch(
            "Mixed ordinary retained authority".into(),
        ));
    }
    validate_open_uri(manifest, opts)?;
    // 0a. A host application has no manifest and no sandbox. Refusing here,
    // before any socket/directory/argv exists, keeps the two launch kinds
    // from quietly converging. Managed-service constraints are shared with
    // the captured path below: capture must never become a bypass.
    validate_launch_mode(manifest, opts)?;
    validate_interfaces(manifest)?;

    // 0. Policy: a manifest naming a filesystem path is refused before
    // anything else exists — silently dropping it would surface as an app
    // bug, not a policy one. Checked at launch too, not just install.
    crate::manifest::enforce_fs_policy(manifest)?;

    // Hold the payload and runtime trees before they are looked at, until
    // the sandbox that mounts them has exited: an update or collection
    // meanwhile leaves them alone (`crate::lease`).
    let _leases = crate::lease::LaunchLeases::take(manifest, payload_root, opts)?;

    // The user's capability decision. Empty unless the compositor wrote a
    // file — an app launched without one is confined to its manifest alone.
    // Resolved before the seccomp baseline, which a grant can narrow.
    let mut capabilities = resolve_capabilities(manifest, opts);

    // Host preflight precedes connected service custody.
    // Seccomp baseline → tmpfile → fd.
    let seccomp_fds = stage_seccomp(
        manifest,
        crate::seccomp::syscall_grants(
            manifest,
            &capabilities,
            crate::system_stats::image_shipped(payload_root),
        ),
    )?;
    // Whether the app opens the GPU itself, or its room's render service
    // draws it.
    let gpu_granted = crate::gpu::granted(
        manifest,
        &capabilities,
        crate::system_stats::image_shipped(payload_root),
    );
    tilo_startup_metrics::handoff_mark("seccomp-staged");

    // D-Bus proxy decision. Failure leaves the bus unavailable; there is
    // no raw-session-bus fallback. `_dbus_guards` stay alive until bwrap is
    // waited on so Drop can SIGTERM the proxies.
    //
    // The proxies' start-up and readiness handshakes need nothing from the
    // runtime, loader and directory preparation, and those need nothing
    // from the proxies, so the two run side by side. Every check still
    // completes before anything below; a failed one returns after the
    // join, and dropping the guards stops the proxies before any sandbox
    // exists.
    let (portals, graphical) = (&manifest.sandbox.portals, opts.wayland_socket.is_some());
    let ((dbus_plan, _dbus_guards), host, optional_runtimes) = std::thread::scope(|scope| {
        let dbus = spawn_beside(scope, "launch-dbus", move || {
            let bind = resolve_dbus_bind(portals, graphical);
            tilo_startup_metrics::handoff_mark("dbus-proxies-ready");
            bind
        })?;
        // Verifying an optional runtime reads its whole payload (a browser
        // engine is hundreds of MB); nothing else here needs the answer
        // before the sandbox argv is built.
        let optional = spawn_beside(scope, "launch-optional-runtimes", move || {
            let bundles = resolve_optional_runtimes(manifest, opts);
            tilo_startup_metrics::handoff_mark("optional-runtimes-verified");
            bundles
        })?;
        let host = prepare_launch_host(
            manifest,
            payload_root,
            opts,
            enrolled.as_ref().map(|(services, _)| &**services),
            native_owner,
        );
        Ok::<_, RunError>((join_beside(dbus), host, join_beside(optional)))
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

    if let Some(owner) = native_owner
        && let Some(hosts) = owner.reviewed_network_hosts()?
    {
        if hosts.iter().any(|host| {
            host.is_empty() || host.contains('*') || host.parse::<std::net::IpAddr>().is_ok()
        }) {
            return Err(RunError::InvalidServiceLaunch(
                "owned service requires named reviewed hosts".into(),
            ));
        }
        capabilities.allow_network = !hosts.is_empty();
        capabilities.net_hosts = hosts;
    }
    if enrolled.is_some() {
        capabilities.participation = crate::capabilities::Participation::Author;
    }

    // Connect only after cold runtime verification, loader preflight and host
    // preparation. The broker starts its executing-task deadline at handoff;
    // those checks must not consume the sandboxed app's response window.
    let egress = crate::sandbox::egress_decision(manifest, &capabilities);
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
        crate::services::authenticate_shell_ui_bind(&resolved, package_id, &egress)?;
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
        // Every app may start independent jobs; it declares nothing for it.
        if let Some(door) = crate::job_door::open(opts.no_services) {
            resolved.push(door);
        }
        resolved
    };
    let binds = to_service_binds(&resolved);
    tilo_startup_metrics::handoff_mark("services-resolved");
    let mut headless_console = opts
        .headless_console
        .map(|instance| crate::headless_console::Namespace::prepare(&app_dir, instance))
        .transpose()?;

    // Resolve the selected record on the host, migrate legacy storage, then
    // mint one app-scoped broker connection. The path is only a presence
    // marker for the argv builder; it is never mounted.
    let (identity_broker_app, identity_broker_control, identity_broker_thread, identity_host) =
        if enrolled.is_some() {
            (None, None, None, None)
        } else {
            resolve_identity_broker(&capabilities, opts)?
        };
    tilo_startup_metrics::handoff_mark("identity-ready");
    let identity_arg = identity_broker_app.as_ref().and(identity_host.as_deref());
    let mut keep_fds: Vec<RawFd> = resolved.iter().map(|r| r.child_fd as RawFd).collect();
    if identity_broker_app.is_some() {
        keep_fds.push(runtime_identity::broker::BROKER_CHILD_FD);
    }

    // Egress: a grant naming specific hosts runs a gate; `*` or bare `host`
    // mode shares the host network with no allowlist to enforce, so neither
    // needs a gate. Guard bound here (not in the `if let`) so it outlives the child.
    let (net_gate, _gate_guard) = start_egress_gate(&egress)?;
    tilo_startup_metrics::handoff_mark("egress-ready");

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
    if enrolled.is_none()
        && binds
            .iter()
            .any(|bind| bind.name == crate::services::COLLAB_SERVICE)
    {
        splice_cli_setenv(
            &mut argv,
            Some((runtime_portal::process_binding::ENV.into(), "1".into())),
        )?;
    }
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
    let mut ro_paths = splice_runtime_mounts_with(
        &mut argv,
        opts,
        resolved_runtime.is_some(),
        &optional_runtimes,
    )?;
    if native_owner
        .and_then(crate::managed::EnrolledProcessObserver::capture_projection)
        .is_some()
    {
        crate::owned_service::capture::splice(&mut argv, &mut ro_paths);
        if let Some(capture) =
            native_owner.and_then(crate::managed::EnrolledProcessObserver::capture_projection)
        {
            capture.splice_archive(&mut argv, &mut ro_paths);
        }
    }
    if native_owner
        .and_then(crate::managed::EnrolledProcessObserver::capture_projection)
        .is_some_and(crate::owned_service::CaptureAdmission::has_output)
    {
        crate::owned_service::capture::splice_output(&mut argv);
    }
    splice_workspace_id_bind(&mut argv, opts)?;
    if enrolled.is_none() && retained_io.is_none() && target_projection.is_none() {
        splice_tool_context(&mut argv)?;
    }
    splice_app_dir(&mut argv, &app_dir, &person_dir, &capabilities)?;
    if retained_io.is_none() && target_projection.is_none() {
        splice_session_settings(&mut argv, &manifest.package_id)?;
    }
    let (system_authority, _system_bus_guard) = splice_system_authority(&mut argv, manifest)?;
    splice_trusted_identities(&mut argv, manifest, opts)?;
    splice_retained_files(&mut argv, manifest, opts)?;
    let _browser_projection = crate::browser_projection::splice(&mut argv, manifest, opts)?;
    splice_action_authority(&mut argv, manifest, opts)?;
    let wayland_bind = splice_wayland(&mut argv, opts)?;
    splice_net_gate_bind(&mut argv, net_gate.as_ref())?;
    let (gpu, pipeline_seed) = if retained_io.is_some() || target_projection.is_some() {
        (None, None)
    } else {
        splice_gpu(&mut argv, manifest, opts, gpu_granted)?
    };
    splice_startup_marks(&mut argv)?;
    splice_sandbox_shim(
        &mut argv,
        opts,
        &ShimSpliceCtx {
            manifest,
            net_gate: &net_gate,
            runtime_arg,
            extra_ro_paths: &ro_paths,
            egress: &egress,
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

    if native_owner
        .and_then(crate::managed::EnrolledProcessObserver::capture_projection)
        .is_some()
    {
        crate::owned_service::capture::splice_shim_data(&mut argv)?;
        if let Some(capture) =
            native_owner.and_then(crate::managed::EnrolledProcessObserver::capture_projection)
        {
            capture.splice_inventory_scratch_policy(&mut argv)?;
        }
    }

    if native_owner
        .and_then(crate::managed::EnrolledProcessObserver::capture_projection)
        .is_some_and(crate::owned_service::CaptureAdmission::has_output)
    {
        let position = argv
            .iter()
            .position(|arg| arg == "__sandbox-exec")
            .ok_or_else(|| RunError::InvalidServiceLaunch("Capture shim unavailable".into()))?;
        let insertion = position.saturating_add(1);
        argv.splice(
            insertion..insertion,
            [
                "--rw-noexec".into(),
                crate::owned_service::capture::OUTPUT_PATH.into(),
            ],
        );
    }

    if let Some(namespace) = &mut headless_console {
        namespace.before_spawn();
    }
    if let Some(io) = retained_io {
        crate::owned_service::capture::splice_shim_data(&mut argv)?;
        io.splice_source(&mut argv)?;
    }
    if target_projection.is_some() {
        crate::owned_service::capture::splice_shim_data(&mut argv)?;
    }
    if let Some(retained) =
        native_owner.and_then(crate::managed::EnrolledProcessObserver::retained_view)
    {
        retained.splice(&mut argv, capabilities.participation)?;
    }
    if let Some(parent) = selected_parent {
        parent.splice(&mut argv, capabilities.participation)?;
    }
    let result = spawn_and_wait(
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
        native_owner
            .map(|observer| {
                (
                    observer,
                    opts.data_home.as_deref().and_then(debug_output_log),
                )
            })
            .or_else(|| {
                enrolled
                    .map(|(services, observer)| (observer, debug_output_log(services.data_home())))
            }),
    );
    if result.is_ok()
        && let Some(namespace) = &mut headless_console
    {
        namespace.after_exit()?;
    }
    result
}

/// Debug-only, off by default: when the launching host sets
/// `TILO_LAUNCHER_DEBUG_STDERR=1`, a managed child's stdout and
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
    let account_fd = stage_sandbox_account(&mut argv, None)?;
    let host_account_fd = account_fd.as_raw_fd();
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
    service_fds.extend(seccomp_fds.child_fds(host_account_fd));
    unsafe {
        cmd.pre_exec(move || {
            place_child_fds(&mut service_fds)?;
            set_sandbox_resource_limits()?;
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(RunError::Spawn)?;
    drop(seccomp_fds);
    drop(account_fd);
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
    let account_fd = stage_sandbox_account(&mut argv, None)?;
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
    if let Some(stream) = &identity_broker_app {
        service_fds.push((
            stream.as_raw_fd(),
            runtime_identity::broker::BROKER_CHILD_FD,
        ));
    }
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

    let mut child = cmd.spawn().map_err(RunError::Spawn)?;
    drop(seccomp_fds);
    drop(account_fd);
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
            bounded_command::terminate_tree(&mut child, Duration::ZERO);
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
    manifest: &ManifestView,
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
    manifest: &ManifestView,
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

#[expect(
    clippy::too_many_lines,
    reason = "the captured launch mirrors `run_with_context`'s ordered pipeline step for step"
)]
fn run_with_capture_spawn<T>(
    manifest: &ManifestView,
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
    if opts.expected_package_hash.is_some() {
        return Err(RunError::InvalidServiceLaunch(
            "original signed package requires manifest-backed run".into(),
        ));
    }
    validate_open_uri(manifest, opts)?;
    // Same refusals as `run_with`: this is a second entry into the sandbox
    // builder, and a policy with one enforced door is a door beside it.
    validate_launch_mode(manifest, opts)?;
    validate_interfaces(manifest)?;
    crate::manifest::enforce_fs_policy(manifest)?;

    // Seccomp baseline resolved identically to `run_with` — two paths
    // reaching different conclusions would make capture a way around the filter.
    let capabilities = resolve_capabilities(manifest, opts);
    let seccomp_fds = stage_seccomp(
        manifest,
        crate::seccomp::syscall_grants(
            manifest,
            &capabilities,
            crate::system_stats::image_shipped(payload_root),
        ),
    )?;
    // Whether the app opens the GPU itself, or its room's render service
    // draws it.
    let gpu_granted = crate::gpu::granted(
        manifest,
        &capabilities,
        crate::system_stats::image_shipped(payload_root),
    );

    // As in `run_with_context`: held from before the lookup until the
    // sandbox has exited.
    let _leases = crate::lease::LaunchLeases::take(manifest, payload_root, opts)?;
    // Build the exact production sandbox shape. Capture changes stdout only.
    let resolved_runtime = resolve_runtime(manifest, opts)?;
    let runtime_arg = resolved_runtime
        .as_ref()
        .map(|(p, v)| (p.as_path(), v.as_str()));
    preflight_runtime_libraries(manifest, payload_root, opts, runtime_arg)?;
    // D-Bus plan (same decision path as `run_with`).
    let (dbus_plan, _dbus_guards) =
        resolve_dbus_bind(&manifest.sandbox.portals, opts.wayland_socket.is_some());
    // As in `run_with_context`, cold runtime checks precede service handoff.
    // Capture must not start the executing-task deadline during verification.
    let egress = crate::sandbox::egress_decision(manifest, &capabilities);
    let package_id = manifest.package_id.as_str();
    let mut resolved = resolve_service_binds_for_launch(
        &manifest.sandbox.services,
        opts.no_services,
        ServiceLaunchContext {
            app_id: package_id,
            workspace_id: opts.workspace_id.as_deref(),
        },
    )?;
    crate::services::authenticate_shell_ui_bind(&resolved, package_id, &egress)?;
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
    if binds
        .iter()
        .any(|bind| bind.name == crate::services::COLLAB_SERVICE)
    {
        splice_cli_setenv(
            &mut argv,
            Some((runtime_portal::process_binding::ENV.into(), "1".into())),
        )?;
    }
    let ro_paths = splice_runtime_mounts(&mut argv, manifest, opts, resolved_runtime.is_some())?;
    splice_tool_context(&mut argv)?;
    splice_net_gate_bind(&mut argv, net_gate.as_ref())?;
    splice_workspace_id_bind(&mut argv, opts)?;
    let (app_dir, person_dir) = resolve_launch_dirs(manifest, opts)?;
    let mut headless_console = opts
        .headless_console
        .map(|instance| crate::headless_console::Namespace::prepare(&app_dir, instance))
        .transpose()?;
    splice_app_dir(&mut argv, &app_dir, &person_dir, &capabilities)?;
    splice_session_settings(&mut argv, &manifest.package_id)?;
    let (system_authority, _system_bus_guard) = splice_system_authority(&mut argv, manifest)?;
    splice_trusted_identities(&mut argv, manifest, opts)?;
    splice_retained_files(&mut argv, manifest, opts)?;
    let _browser_projection = crate::browser_projection::splice(&mut argv, manifest, opts)?;
    splice_action_authority(&mut argv, manifest, opts)?;
    let wayland_bind = splice_wayland(&mut argv, opts)?;
    let (gpu, pipeline_seed) = splice_gpu(&mut argv, manifest, opts, gpu_granted)?;
    splice_sandbox_shim(
        &mut argv,
        opts,
        &ShimSpliceCtx {
            manifest,
            net_gate: &net_gate,
            runtime_arg,
            extra_ro_paths: &ro_paths,
            egress: &egress,
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

    if let Some(namespace) = &mut headless_console {
        namespace.before_spawn();
    }
    let result = spawn(
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
    );
    if result.is_ok()
        && let Some(namespace) = &mut headless_console
    {
        namespace.after_exit()?;
    }
    result
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

/// Decide how to expose D-Bus: the manifest's portal declaration, and for
/// every graphical app the accessibility buses.
///
/// Each bus reaches the app only through its own filtered `xdg-dbus-proxy`
/// ([`dbus_proxy`]). There is deliberately no raw-bus fallback. Giving an
/// untrusted sandbox the complete user bus because a filtering helper is
/// missing turns an availability problem into a privilege-escalation path
/// (and lets the app call portal *implementation* services directly). A failed
/// proxy therefore means that bus is unavailable for that launch.
fn resolve_dbus_bind(
    portals: &std::collections::BTreeMap<String, PortalPolicy>,
    graphical: bool,
) -> (Option<DBusBindPlan>, Vec<DBusProxyGuard>) {
    let (uid, pid) = (current_uid(), std::process::id());
    let sockets = ProxySockets {
        session: dbus_proxy::proxy_socket_path(uid, pid),
        atspi: dbus_proxy::atspi_proxy_socket_path(uid, pid),
    };
    resolve_dbus_bind_at(portals, graphical, &sockets)
}

/// Where one launch's D-Bus proxies listen on the host.
struct ProxySockets {
    session: PathBuf,
    atspi: PathBuf,
}

fn resolve_dbus_bind_at(
    portals: &std::collections::BTreeMap<String, PortalPolicy>,
    graphical: bool,
    sockets: &ProxySockets,
) -> (Option<DBusBindPlan>, Vec<DBusProxyGuard>) {
    let wants_portals = portals.values().any(|p| matches!(p, PortalPolicy::ViaHost));
    if !wants_portals && !graphical {
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

    // The accessibility bus is found and proxied beside the session bus's
    // proxy: two lookups and readiness handshakes, independent of each
    // other. Without a session proxy the AT-SPI one is dropped (stopped).
    let (session, atspi) = std::thread::scope(|scope| {
        let atspi = graphical.then(|| {
            spawn_beside(scope, "launch-atspi", || {
                spawn_atspi_proxy(&binary, &bus_addr, uid, &sockets.atspi)
            })
        });
        let session = dbus_proxy::spawn_filtered_proxy(
            &binary,
            &bus_addr,
            &sockets.session,
            &dbus_proxy::session_filter_args(wants_portals, graphical),
        );
        let atspi = match atspi {
            Some(Ok(handle)) => join_beside(handle),
            Some(Err(error)) => {
                tracing::warn!(%error, "AT-SPI proxy could not start beside the session proxy");
                spawn_atspi_proxy(&binary, &bus_addr, uid, &sockets.atspi)
            }
            None => None,
        };
        (session, atspi)
    });
    let session = match session {
        Ok(guard) => guard,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "xdg-dbus-proxy failed to spawn; the session bus stays unavailable"
            );
            return (None, Vec::new());
        }
    };
    let mut plan = DBusBindPlan {
        host_socket: session.socket_path().to_path_buf(),
        sandbox_path: dbus_proxy::sandbox_bus_path(uid),
        atspi: None,
    };
    let mut guards = vec![session];
    if let Some((atspi, guard)) = atspi {
        plan.atspi = Some(atspi);
        guards.push(guard);
    }
    tracing::info!(
        proxy_binary = %binary.display(),
        socket = %plan.host_socket.display(),
        accessibility = plan.atspi.is_some(),
        "filtered D-Bus proxies spawned"
    );
    (Some(plan), guards)
}

/// The filtered AT-SPI bus proxy for a graphical app, or `None` when the
/// session runs no accessibility bus or the proxy cannot start; the app then
/// stays invisible to assistive technology, never on the raw bus.
fn spawn_atspi_proxy(
    binary: &std::path::Path,
    session_bus: &str,
    uid: u32,
    socket: &Path,
) -> Option<(crate::sandbox::AtspiBindPlan, DBusProxyGuard)> {
    let Some(atspi_addr) = dbus_proxy::host_atspi_address(session_bus) else {
        tracing::info!("the session runs no accessibility bus");
        return None;
    };
    match dbus_proxy::spawn_filtered_proxy(
        binary,
        &atspi_addr,
        socket,
        dbus_proxy::ATSPI_FILTER_ARGS,
    ) {
        Ok(guard) => Some((
            crate::sandbox::AtspiBindPlan {
                host_socket: guard.socket_path().to_path_buf(),
                sandbox_path: dbus_proxy::sandbox_atspi_path(uid),
            },
            guard,
        )),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "AT-SPI proxy failed to spawn; the app stays invisible to assistive technology"
            );
            None
        }
    }
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

        let (plan, guards) = resolve_dbus_bind(&portals, true);

        assert!(plan.is_none(), "a raw D-Bus bind must never be planned");
        assert!(guards.is_empty(), "no proxy process should have started");
    }

    /// The session and AT-SPI proxies come up side by side. Each mock proxy
    /// reports ready only once both have started, so one after the other the
    /// first would never be ready and the session bus would stay unbound.
    #[test]
    fn the_session_and_accessibility_proxies_start_side_by_side() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let proxy = tmp.path().join("xdg-dbus-proxy");
        std::fs::write(
            &proxy,
            r#"#!/bin/sh
notify=0
if [ "$1" = --fd=0 ]; then notify=1; shift; fi
dir=$(dirname "$2")
: >"$2.started"
tries=0
while [ "$(ls "$dir" | grep -c '\.started$')" -lt 2 ]; do
    tries=$((tries + 1))
    [ "$tries" -gt 400 ] && exit 1
    sleep 0.01
done
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
            atspi: tmp.path().join("sockets/1-atspi.sock"),
        };

        let (plan, guards) =
            resolve_dbus_bind_at(&std::collections::BTreeMap::new(), true, &sockets);

        let plan = plan.expect("both proxies became ready together");
        assert_eq!(plan.host_socket, sockets.session);
        assert_eq!(
            plan.atspi.map(|atspi| atspi.host_socket),
            Some(sockets.atspi.clone())
        );
        assert_eq!(guards.len(), 2);
        drop(guards);
        assert!(!sockets.session.exists() && !sockets.atspi.exists());
    }
}

/// Current process UID via libc, avoiding a `users`/`nix` dependency.
/// Falls back to 1000 if the syscall somehow fails.
fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
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
        // Phase `a_joined_devices_public_record_brokers_without_a_secret`.
        // A device joined from a nearby device holds only the identity's
        // public record; its apps sign through the workspace identity service
        // with the certified device key.
        {
            use std::os::unix::fs::PermissionsExt;

            let store = tempfile::tempdir().unwrap();
            std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let path = store.path().join("identity-joined.json");
            let original =
                runtime_identity::public_member_record("Member", [1, 2, 3], [9; 32]).unwrap();
            std::fs::write(&path, &original).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

            for _ in 0..2 {
                let (app, control, thread) =
                    start_identity_broker(&path, Some(store.path())).unwrap();
                drop(app);
                drop(control);
                thread.join().unwrap();
            }

            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert!(!store.path().join("secrets").exists());
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

    /// What any write under `dir` would change: every entry's path, mode,
    /// inode, size, change and modification times, and bytes.
    fn store_snapshot(dir: &Path) -> Vec<(PathBuf, [i64; 8], Vec<u8>)> {
        use std::os::unix::fs::MetadataExt;

        fn walk(path: &Path, out: &mut Vec<(PathBuf, [i64; 8], Vec<u8>)>) {
            let meta = std::fs::symlink_metadata(path).unwrap();
            let stamp = [
                i64::from(meta.mode()),
                meta.ino().cast_signed(),
                meta.size().cast_signed(),
                meta.ctime(),
                meta.ctime_nsec(),
                meta.mtime(),
                meta.mtime_nsec(),
                i64::from(meta.uid()),
            ];
            let bytes = if meta.is_file() {
                std::fs::read(path).unwrap()
            } else {
                Vec::new()
            };
            out.push((path.to_path_buf(), stamp, bytes));
            if meta.is_dir() {
                let mut entries: Vec<_> = std::fs::read_dir(path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .collect();
                entries.sort();
                for entry in entries {
                    walk(&entry, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, &mut out);
        out
    }

    /// Make the store read-only: directories 0500 and files 0400, which the
    /// identity loaders accept as private.
    fn make_read_only(dir: &Path, read_only: bool) {
        use std::os::unix::fs::PermissionsExt;

        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                make_read_only(&path, read_only);
            } else {
                let mode = if read_only { 0o400 } else { 0o600 };
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            }
        }
        let mode = if read_only { 0o500 } else { 0o700 };
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn local_record() -> Vec<u8> {
        let secret = [41u8; 32];
        let key = ed25519_dalek::SigningKey::from_bytes(&secret);
        serde_json::to_vec(&serde_json::json!({
            "display_name": "Local",
            "avatar_color": [1, 2, 3],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": key.verifying_key().to_bytes(),
                "threshold": 1,
                "total_shares": 1,
                "share_index": 1,
                "secret_bytes": secret,
            },
            "created_at": "2026-10-03T00:00:00Z",
            "schema_version": 1,
        }))
        .unwrap()
    }

    /// One share of a 2-of-2 identity, as this device holds it.
    fn threshold_record() -> Vec<u8> {
        use getrandom::{SysRng, rand_core::UnwrapErr};
        use threshold_identity::ThresholdIdentity;

        let mut rng = UnwrapErr(SysRng);
        let mut a = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        let mut b = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        let (mut session_a, round1_a) = a.begin_enrolment(1, 2, 2, &mut rng).unwrap();
        let (mut session_b, round1_b) = b.begin_enrolment(2, 2, 2, &mut rng).unwrap();
        let out_a = session_a.receive_round1(&[round1_b]).unwrap();
        let out_b = session_b.receive_round1(&[round1_a]).unwrap();
        session_a.receive_round2(&[out_b[&1].clone()]).unwrap();
        session_b.receive_round2(&[out_a[&2].clone()]).unwrap();
        a.adopt_dkg_outcome(session_a).unwrap();
        b.adopt_dkg_outcome(session_b).unwrap();
        let secret = a.export_device_secret().unwrap();
        serde_json::to_vec(&serde_json::json!({
            "display_name": "Threshold",
            "avatar_color": [4, 5, 6],
            "threshold_identity": {
                "single_device": false,
                "group_public_key": a.group_public_key().as_bytes(),
                "threshold": 2,
                "total_shares": 2,
                "share_index": 1,
                "secret_bytes": secret.as_slice(),
            },
            "created_at": "2026-10-03T00:00:00Z",
            "schema_version": 1,
        }))
        .unwrap()
    }

    /// Resolve the active record and start its broker, as every authored
    /// launch does.
    fn launch_identity(store: &Path) -> Result<(), RunError> {
        let opts = RunOptions {
            identity: IdentitySource::Store(store.to_path_buf()),
            ..RunOptions::default()
        };
        let path = resolve_identity_host_path(&opts).expect("an active record");
        let (app, control, thread) =
            start_identity_broker(&path, identity_store(&opts.identity).as_deref())?;
        drop(app);
        drop(control);
        thread.join().unwrap();
        Ok(())
    }

    #[test]
    fn launching_never_writes_the_identity_store() {
        // Each record as provisioning leaves it: a local or threshold primary
        // secured into its broker file, and a joined device's public record.
        for (name, id, primary) in [
            ("local", "primary", Some(local_record())),
            ("threshold", "primary", Some(threshold_record())),
            ("public member", "identity-joined", None),
        ] {
            let root = tempfile::tempdir().unwrap();
            let store = root.path().join("identity");
            if let Some(bytes) = primary {
                runtime_identity::import_identity_with_provider(
                    &store,
                    id,
                    &bytes,
                    &runtime_identity::BrokerFileProvider::new(&store),
                )
                .unwrap();
            } else {
                std::fs::create_dir_all(&store).unwrap();
                std::fs::write(
                    store.join(format!("{id}.json")),
                    runtime_identity::public_member_record("Member", [1, 2, 3], [9; 32]).unwrap(),
                )
                .unwrap();
                make_read_only(&store, false);
            }
            runtime_identity::set_active_identity(&store, id).unwrap();
            make_read_only(&store, true);
            let before = store_snapshot(&store);

            for _ in 0..2 {
                launch_identity(&store).unwrap_or_else(|error| panic!("{name}: {error}"));
            }

            assert_eq!(
                store_snapshot(&store),
                before,
                "launching with a {name} record wrote the identity store"
            );
            make_read_only(&store, false);
        }

        // A record whose key was never secured fails the launch, names where
        // to secure it, and stays exactly as it was.
        {
            let root = tempfile::tempdir().unwrap();
            let store = root.path().join("identity");
            std::fs::create_dir_all(&store).unwrap();
            std::fs::write(store.join("primary.json"), local_record()).unwrap();
            make_read_only(&store, true);
            let before = store_snapshot(&store);

            let error = launch_identity(&store).expect_err("an unsecured record must not launch");
            assert!(error.to_string().contains("Settings > Users"), "{error}");
            assert_eq!(store_snapshot(&store), before);
            make_read_only(&store, false);
        }
    }

    /// Ask the app end of a launch's broker for metadata and to sign, over
    /// the broker's pinned frames: an operation or status byte, a big-endian
    /// `u32` length, then the payload.
    fn ask_broker(app: &mut UnixStream, op: u8, payload: &[u8]) -> (u8, Vec<u8>) {
        use std::io::{Read, Write};

        app.write_all(&[op]).unwrap();
        app.write_all(&u32::try_from(payload.len()).unwrap().to_be_bytes())
            .unwrap();
        app.write_all(payload).unwrap();
        let mut head = [0_u8; 5];
        app.read_exact(&mut head).unwrap();
        let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]);
        let mut reply = vec![0_u8; usize::try_from(len).unwrap()];
        app.read_exact(&mut reply).unwrap();
        (head[0], reply)
    }

    #[test]
    fn a_launch_never_reads_the_identity_secret() {
        // Each secured record launches, and hands the app the same public
        // fields its full load reports, after its secret is deleted from the
        // provider: a launch that touched the provider would fail.
        for (name, primary) in [("local", local_record()), ("threshold", threshold_record())] {
            let root = tempfile::tempdir().unwrap();
            let store = root.path().join("identity");
            let path = runtime_identity::import_identity_with_provider(
                &store,
                "primary",
                &primary,
                &runtime_identity::BrokerFileProvider::new(&store),
            )
            .unwrap();
            runtime_identity::set_active_identity(&store, "primary").unwrap();
            let full = runtime_identity::load_from_path(path.clone()).unwrap();
            let expected = serde_json::json!({
                "display_name": full.display_name(),
                "avatar_color": full.avatar_color(),
                "group_public_key": full.group_public_key(),
                "threshold": full.threshold(),
                "total_shares": full.total_shares(),
                "share_index": full.share_index(),
                "single_device": full.is_single_device(),
            });
            drop(full);

            let secrets: Vec<_> = std::fs::read_dir(store.join("secrets"))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            assert_eq!(secrets.len(), 1, "{name}: one secret file");
            for secret in &secrets {
                std::fs::remove_file(secret).unwrap();
            }
            assert!(
                runtime_identity::load_from_path(path.clone()).is_err(),
                "{name}: the secret is gone from its provider"
            );

            let opts = RunOptions {
                identity: IdentitySource::Store(store.clone()),
                ..RunOptions::default()
            };
            let resolved = resolve_identity_host_path(&opts).expect("an active record");
            let (mut app, control, thread) =
                start_identity_broker(&resolved, identity_store(&opts.identity).as_deref())
                    .unwrap_or_else(|error| panic!("{name}: {error}"));

            let (status, metadata) = ask_broker(&mut app, 1, &[]);
            assert_eq!(status, 0, "{name}");
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&metadata).unwrap(),
                expected,
                "{name}: the app's metadata is the record's public fields"
            );
            let (status, refusal) = ask_broker(&mut app, 2, b"sign this");
            assert_eq!(status, 1, "{name}: the broker never signs");
            assert_eq!(
                refusal,
                b"the root signing key is never brokered to an application; \
                  request signatures through the identity service",
                "{name}"
            );
            drop(app);
            drop(control);
            thread.join().unwrap();
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
        // Phase `the_user_store_falls_back_to_the_account_home_without_an_environment`.
        // Only the location is asserted: the account's own store is the
        // developer's real one, and resolving its active identity would read it.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove("XDG_CONFIG_HOME").remove("HOME");
            assert_eq!(
                identity_store(&IdentitySource::UserStore),
                std::env::home_dir().map(|home| home.join(".config/tilo/identity"))
            );
        }
    }
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

            // The room's grants make the app an author.
            let dir = tempfile::tempdir().expect("tempdir");
            let file = dir.path().join("grants.json");
            std::fs::write(
                &file,
                serde_json::to_vec(&collab_types::caps::LaunchGrants {
                    authors: vec!["so.tilo.fixture".into()],
                    sandbox: Vec::new(),
                })
                .expect("encodes"),
            )
            .expect("written");
            let authored = resolve_capabilities(
                &m,
                &RunOptions {
                    capabilities_path: Some(file),
                    ..opts_in(Some("room-a"))
                },
            );
            assert_eq!(
                authored.participation,
                crate::capabilities::Participation::Author
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
        // Phase `the_persons_records_are_one_directory_for_every_room`.
        // The person's own records are the same records whichever room the
        // app is opened in, and in none of the rooms' directories.
        {
            let data = tempfile::tempdir().expect("tempdir");
            let m = manifest("so.tilo.fixture");
            let a = resolve_person_dir(&m, &opts_at(data.path(), Some("room-a"))).expect("room a");
            let personal = resolve_person_dir(&m, &opts_at(data.path(), None)).expect("personal");
            assert!(a.is_dir());
            assert_eq!(a, personal);
            assert!(a.starts_with(data.path().join("tilo-person")));
            let room = resolve_app_dir(&m, &opts_at(data.path(), Some("room-a"))).expect("room");
            assert!(!a.starts_with(&room) && !room.starts_with(&a));
            let other = resolve_person_dir(&manifest("so.tilo.other"), &opts_at(data.path(), None))
                .expect("another app");
            assert_ne!(a, other, "an app keeps only its own records of the person");
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

    #[test]
    fn settings_binds_the_session_store_and_other_apps_do_not() {
        let config = tempfile::tempdir().expect("session config");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", config.path());

        let mut argv = vec!["/entry".to_string()];
        splice_session_settings(&mut argv, "so.tilo.notes").expect("notes");
        assert_eq!(argv, vec!["/entry".to_string()]);

        splice_session_settings(&mut argv, "so.tilo.settings").expect("settings");
        let host = config.path().join("tilo");
        for writable in ["session", "accounts"] {
            assert!(
                host.join(writable).is_dir(),
                "{writable} must exist to bind read-write"
            );
            assert!(
                argv.windows(3).any(|w| w[0] == "--bind"
                    && w[1] == host.join(writable).to_string_lossy()
                    && w[2] == format!("/run/tilo/app/.config/tilo/{writable}")),
                "Settings must bind {writable} over remapped XDG: {argv:?}"
            );
        }
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
        assert!(
            argv.windows(3).any(|w| w[0] == "--setenv"
                && w[1] == "TILO_SESSION_SETTINGS"
                && w[2] == crate::appdir::SANDBOX_SESSION_SETTINGS),
            "Settings must name the session document: {argv:?}"
        );
        assert_eq!(argv.last().map(String::as_str), Some("/entry"));
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
    struct NativeOwner {
        allowed: bool,
        checked: AtomicUsize,
        supervised: AtomicUsize,
        own_group: AtomicBool,
    }
    impl crate::managed::EnrolledProcessObserver for NativeOwner {
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
            // The actual unreaped child owns its group; no ambient session group
            // can be handed to cancellation logic as though it were this service.
            let pid = i32::try_from(child.id()).map_err(std::io::Error::other)?;
            self.own_group
                .store(unsafe { libc::getpgid(pid) } == pid, AtomicOrdering::SeqCst);
            child.wait()
        }
    }
    #[test]
    fn owned_service_prepared_spawn_rechecks_source_and_hands_over_own_child() {
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
                &RunOptions {
                    bwrap_bin: Some("/usr/bin/true".into()),
                    ..Default::default()
                },
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
                Some((&owner, None)),
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
                assert!(matches!(result, Err(RunError::Spawn(error))
                    if error.kind() == std::io::ErrorKind::PermissionDenied));
            }
        }
    }

    struct OrdinaryPrivateService {
        info: OwnedFd,
        checked: AtomicUsize,
        spawned: AtomicUsize,
    }
    impl crate::managed::EnrolledProcessObserver for OrdinaryPrivateService {
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
    #[test]
    fn ordinary_private_service_missing_full_namespace_plan_refuses_before_spawn() {
        // Exercise the actual final command boundary with no capture token.
        // Explicit flags resembling the original-reader plan cannot admit an
        // ordinary service; it still requires the canonical --unshare-all.
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
                &RunOptions {
                    bwrap_bin: Some("/usr/bin/bwrap".into()),
                    ..Default::default()
                },
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
                Some((&owner, None)),
            );
            assert!(matches!(result, Err(RunError::InvalidServiceLaunch(reason))
                if reason == "private service namespace missing"));
            assert_eq!(owner.checked.load(AtomicOrdering::SeqCst), 0);
            assert_eq!(owner.spawned.load(AtomicOrdering::SeqCst), 0);
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
            &tilo_action_policy::RoomPolicy::new("room-a", "person-a"),
            &manifest.package_id,
            "person-a",
            false,
            10_000,
            13_000,
        );
        std::fs::write(&path, serde_json::to_vec(&policy).unwrap()).unwrap();
        let opts = RunOptions {
            workspace_id: Some("room-a".into()),
            action_authority_host_path: Some(path),
            ..RunOptions::default()
        };
        let before = resolve_capabilities(&manifest, &RunOptions::default());
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
        for room in [None, Some("room-b".into())] {
            let mut wrong = opts.clone();
            wrong.workspace_id = room;
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

    #[test]
    fn a_launch_without_a_render_node_follows_a_gpu_less_compositor_onto_lavapipe() {
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
        let opts = RunOptions::default();
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
    }

    #[test]
    fn a_software_launch_gets_only_the_read_only_session_seed_without_a_device_pin() {
        let mut home = tilo_test_env::TestHome::new().unwrap();
        // A compositor's software pin is its own case below; CI pins
        // lavapipe in the test environment itself.
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
        let opts = RunOptions::default();
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
            let opts = RunOptions {
                mode,
                ..RunOptions::default()
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

#[cfg(test)]
mod render_service_launch_tests {
    use super::*;

    /// An app that draws through its room's render service: no GPU node, no
    /// loader pin, no shader-cache seed, and none of the probing they cost,
    /// even with a render node and a session seed to hand out.
    #[test]
    fn an_app_without_gpu_gets_no_device_and_no_graphics_setup() {
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
        let opts = RunOptions {
            render_node: Some(PathBuf::from("/dev/dri/renderD128")),
            ..RunOptions::default()
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
        assert_eq!(argv, vec!["/app/calculator".to_owned()], "nothing spliced");
    }

    /// An app that declares `gpu` keeps what every app had before: the
    /// device nodes, narrowed to the compositor's render node.
    #[test]
    fn an_app_declaring_gpu_gets_its_device() {
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
        splice_gpu(&mut argv, &manifest, &RunOptions::default(), true).unwrap();
        assert!(
            argv.windows(3)
                .any(|args| args == ["--dev-bind-try", "/dev/dri", "/dev/dri"]),
            "{argv:?}"
        );
        assert_eq!(argv.last().map(String::as_str), Some("/app/browser"));
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
        let options = RunOptions {
            runtime_root: Some(missing_runtime.clone()),
            workspace_id: Some("preflight-room".into()),
            identity: IdentitySource::None,
            ..RunOptions::default()
        };
        let error = if captured {
            run_with_captured(&manifest, root.path(), &options).unwrap_err()
        } else {
            run_with(&manifest, root.path(), &options).unwrap_err()
        };
        assert!(
            matches!(error, RunError::RuntimeBundleSignature { ref path, .. } if path == &missing_runtime),
            "runtime verification must refuse the actual launch: {error}"
        );
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "runtime preflight contacted the live broker and started its launch deadline"
        );
    }

    #[test]
    fn native_runtime_preflight_precedes_service_handoff() {
        assert_runtime_refusal_precedes_handoff(false);
    }

    #[test]
    fn captured_runtime_preflight_precedes_service_handoff() {
        assert_runtime_refusal_precedes_handoff(true);
    }
}

#[cfg(test)]
mod retained_files_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn retained_locations_are_live_readonly_metadata_without_filesystem_authority() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().join("locations.json");
        std::fs::write(&path, br#"{"schema":1,"locations":[]}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let manifest: ManifestView = toml::from_str(
            "[package]\nkind = \"app\"\nid = \"so.tilo.file-explorer\"\nversion = \"1.0\"\n[entrypoint]\nbin = \"files\"\n[publisher]\nidentity = \"ed25519:0000000000000000000000000000000000000000000000000000000000000000\"\n[sandbox]\nnamespaces = []\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n[sandbox.net]\nmode = \"off\"\n",
        ).unwrap();
        let opts = RunOptions {
            retained_files_host_path: Some(path),
            ..RunOptions::default()
        };
        let before = resolve_capabilities(&manifest, &RunOptions::default());
        let after = resolve_capabilities(&manifest, &opts);
        assert_eq!(before.ro_paths, after.ro_paths);
        assert_eq!(before.rw_paths, after.rw_paths);
        let mut argv = vec!["bwrap".into(), "files".into()];
        splice_retained_files(&mut argv, &manifest, &opts).unwrap();
        assert_eq!(
            argv,
            vec![
                "bwrap",
                "--ro-bind",
                root.path().to_str().unwrap(),
                crate::sandbox::SANDBOX_RETAINED_FILES_DIR,
                "--setenv",
                "TILO_RETAINED_FILES",
                crate::sandbox::SANDBOX_RETAINED_FILES_PATH,
                "files",
            ]
        );
        let mut shim = vec!["files".into()];
        let enforced = RunOptions {
            landlock_enforce: true,
            ..opts.clone()
        };
        splice_sandbox_shim(
            &mut shim,
            &enforced,
            &ShimSpliceCtx {
                manifest: &manifest,
                net_gate: &None,
                runtime_arg: None,
                extra_ro_paths: &[],
                egress: &crate::netgate::EgressDecision::Blocked,
                capabilities: &after,
                dbus_plan: &None,
                system_authority: &None,
                wayland_bind: &None,
                gpu: &None,
                gpu_devices: false,
                pipeline_seed: &None,
                keep_fds: &[],
            },
        )
        .unwrap();
        assert!(
            shim.windows(2)
                .any(|args| args == ["--ro", crate::sandbox::SANDBOX_RETAINED_FILES_DIR])
        );
        assert!(
            !shim
                .windows(2)
                .any(|args| args == ["--rw", crate::sandbox::SANDBOX_RETAINED_FILES_DIR])
        );
        let mut other = manifest;
        other.package_id = "so.tilo.settings".into();
        let mut argv = vec!["settings".into()];
        splice_retained_files(&mut argv, &other, &opts).unwrap();
        assert_eq!(argv, ["settings"]);
    }

    #[test]
    fn retained_projection_refuses_redirected_or_nonprivate_custody() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().join("locations.json");
        let other = root.path().join("other");
        std::fs::write(&other, b"unchanged").unwrap();
        symlink(&other, &path).unwrap();
        assert!(validate_retained_files_projection(&path).is_err());
        assert_eq!(std::fs::read(&other).unwrap(), b"unchanged");
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(validate_retained_files_projection(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(validate_retained_files_projection(&path).is_ok());
        std::fs::write(&path, vec![0; 16 * 1024 + 1]).unwrap();
        assert!(validate_retained_files_projection(&path).is_err());
    }
}
