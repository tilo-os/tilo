//! Standard command-line lifecycle for Tilo applications.
//!
//! An app is an application definition its component host prepares; this
//! module owns common argument validation, headless driving, captures,
//! logging, and live startup around it.

use std::fmt;
use std::path::PathBuf;

#[cfg(feature = "native")]
use crate::App;
use crate::Theme;
use crate::headless::{self, FrameHarness};

mod shot_frame;
pub use shot_frame::{ShotFrame, ShotLayout};
// Writing a PNG needs the offscreen renderer's blocking GPU readback, which
// the browser has not got; laying out and drawing a frame is portable.
#[cfg(not(target_arch = "wasm32"))]
pub use shot_frame::render_widget_png;

#[cfg(all(target_os = "android", feature = "native"))]
pub mod android;
#[cfg(any(all(target_os = "android", feature = "native"), test))]
mod android_env;

/// Export the Android entry point for this binary's `main`.
///
/// On Android an app is a shared library that `TiloActivity` loads, not an
/// executable, so there is no process `main`. Invoked once at the top level
/// of an app's `main.rs`, this defines the `android_main` the activity calls,
/// which prepares the process and runs that same `main`. On every other
/// target it expands to nothing. `tilo-app build --platform android` builds
/// the file as a `cdylib` example. `android_main!(entry)` runs `entry`
/// instead of `main`, as `dev_reload_entry!` does for an app library.
#[macro_export]
macro_rules! android_main {
    () => {
        $crate::android_main!(main);
    };
    ($entry:expr) => {
        #[cfg(target_os = "android")]
        #[unsafe(no_mangle)]
        fn android_main(app: $crate::launch::android::AndroidApp) {
            $crate::launch::android::run_main(app, $entry);
        }
    };
}

pub const DEFAULT_VIEWPORT: (u32, u32) = headless::DEFAULT_VIEWPORT;

/// Explicit runner authority/presentation preparation; application metadata is never authority.
pub type ComponentHostPreparer =
    fn(
        &crate::application::ApplicationDefinition,
        &LaunchArgs,
    ) -> Result<crate::application::PreparedComponentInputs, LaunchError>;

/// Drive a mounted capture through its normal actions/input before rendering.
/// The callback receives no unbound root or document mutation shortcut.
pub type ComponentCaptureScenario = fn(&mut FrameHarness, &LaunchArgs) -> Result<(), LaunchError>;

type ViewportFactory = fn(&LaunchArgs) -> (u32, u32);
type CommandHandler = fn(&LaunchArgs) -> Result<bool, LaunchError>;

/// Parsed common launch arguments. Extra flags are retained for an app's
/// screenshot factory after being explicitly allow-listed by [`LaunchSpec`].
#[derive(Clone, Debug)]
pub struct LaunchArgs {
    pub shots: Option<PathBuf>,
    /// `--shots-dir DIR`: one capture of every declared scenario, written to
    /// `DIR/NN-name.png` in declaration order.
    pub shots_dir: Option<PathBuf>,
    pub responsive_matrix: Option<PathBuf>,
    pub viewport: Option<(u32, u32)>,
    pub scenario: Option<String>,
    pub headless: Option<headless::Spec>,
    /// Lay out the production root and print its Tilo-native certification.
    pub native_audit: bool,
    pub pseudo_locale: bool,
    pub text_scale: Option<f32>,
    /// `--theme NAME`: the shipped theme a capture renders in
    /// ([`Theme::named`]), over `TILO_SHOTS_THEME`.
    pub theme: Option<String>,
    /// `--workspace-hue DEGREES` and `--your-colour NAME`: what the shell
    /// would tell the captured window about where and for whom it is shown
    /// ([`crate::theme::WindowAppearance`]). Without them a capture is what
    /// a window without a shell shows: neutral, in the default colour.
    pub window: crate::theme::WindowAppearance,
    /// `--history`: a capture shows History open, where the app has
    /// History.
    pub history: bool,
    /// `--search QUERY`: a capture shows this window's search open with
    /// `QUERY` typed, as the app's search launcher opens it.
    pub search: Option<String>,
    /// `--device-scale N`: a capture is drawn at N device pixels per
    /// logical pixel (1 to [`MAX_SHOT_DEVICE_SCALE`]), as a HiDPI display
    /// draws the same window.
    pub device_scale: Option<u32>,
    /// `--distinct-letters on|off`: a capture renders with "Always
    /// distinguish similar letters" on or off, whatever the theme's face
    /// would choose.
    pub distinct_letters: Option<bool>,
    extras: Vec<String>,
    values: Vec<(String, String)>,
    positionals: Vec<String>,
}

impl LaunchArgs {
    /// The arguments of a component mounted by an in-process host: no
    /// screenshot, headless or audit mode and no app-specific arguments.
    pub fn component() -> Self {
        Self {
            shots: None,
            shots_dir: None,
            responsive_matrix: None,
            viewport: None,
            scenario: None,
            headless: None,
            native_audit: false,
            pseudo_locale: false,
            text_scale: None,
            theme: None,
            window: crate::theme::WindowAppearance::default(),
            history: false,
            search: None,
            device_scale: None,
            distinct_letters: None,
            extras: Vec::new(),
            values: Vec::new(),
            positionals: Vec::new(),
        }
    }

    pub fn has(&self, flag: &str) -> bool {
        self.extras.iter().any(|arg| arg == flag)
    }

    /// These arguments naming the app's switch `flag` too (`--private`), as
    /// a command line does: what a host or a test launches with.
    #[must_use]
    pub fn with_flag(mut self, flag: &str) -> Self {
        if !self.has(flag) {
            self.extras.push(flag.to_owned());
        }
        self
    }

    /// Value supplied for an app-specific option allow-listed by the launch
    /// specification.
    pub fn value(&self, option: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(name, _)| name == option)
            .map(|(_, value)| value.as_str())
    }

    /// Positional launch arguments accepted by the specification, in order.
    pub fn positionals(&self) -> &[String] {
        &self.positionals
    }

    /// This process's arguments, for a binary that runs its own live loop
    /// (the greeter, the installer) yet takes the standard capture options,
    /// plus its own capture-only `options`.
    ///
    /// # Errors
    /// The same refusals as [`run`]: an unknown, malformed or duplicate
    /// argument, or a capture option outside a capture.
    #[cfg(feature = "native")]
    pub fn from_env(options: &[&str]) -> Result<Self, LaunchError> {
        let args: Vec<String> = std::env::args().collect();
        parse_args_extended(&args, &[], options, &[], &[], 0)
    }
}

/// How the standard lifecycle launches one application definition: its
/// component host, capture scenarios, commands and accepted arguments.
#[derive(Clone)]
pub struct LaunchSpec {
    definition: &'static crate::application::ApplicationDefinition,
    component_host: Option<ComponentHostPreparer>,
    component_capture_scenario: Option<ComponentCaptureScenario>,
    component_native_services: bool,
    app_id: &'static str,
    shot_viewport: ViewportFactory,
    command_handler: Option<CommandHandler>,
    headless_enabled: bool,
    runtime_attachment: fn(&LaunchArgs) -> bool,
    allowed_flags: &'static [&'static str],
    allowed_options: &'static [&'static str],
    allowed_live_flags: &'static [&'static str],
    allowed_live_options: &'static [&'static str],
    max_positionals: usize,
    scenarios: &'static [&'static str],
}

impl LaunchSpec {
    /// Select automatic runtime attachment for native launches. The default is
    /// enabled. Disabling bootstrap does not revoke inherited service handles;
    /// privacy-sensitive apps must also construct a local-only application.
    #[must_use]
    pub fn runtime_attachment(mut self, policy: fn(&LaunchArgs) -> bool) -> Self {
        self.runtime_attachment = policy;
        self
    }

    /// Whether a native launch with `args` attaches to the session runtime.
    pub fn attaches_runtime(&self, args: &LaunchArgs) -> bool {
        (self.runtime_attachment)(args)
    }
    /// Adapt one common application definition. A runner must install an explicit host.
    pub fn from_definition(definition: &'static crate::application::ApplicationDefinition) -> Self {
        Self {
            definition,
            component_host: None,
            component_capture_scenario: None,
            component_native_services: false,
            app_id: definition.app_id,
            shot_viewport: |_| DEFAULT_VIEWPORT,
            command_handler: None,
            headless_enabled: true,
            runtime_attachment: |_| true,
            allowed_flags: &[],
            allowed_options: &[],
            allowed_live_flags: &[],
            allowed_live_options: &[],
            max_positionals: 0,
            scenarios: &[],
        }
    }

    /// The application this specification launches.
    #[must_use]
    pub fn definition(&self) -> &'static crate::application::ApplicationDefinition {
        self.definition
    }

    /// The id of the app this specification launches.
    #[must_use]
    pub fn app_id(&self) -> &'static str {
        self.app_id
    }

    #[must_use]
    pub fn component_host(mut self, prepare: ComponentHostPreparer) -> Self {
        self.component_host = Some(prepare);
        self
    }

    /// Use the shared native system-service runner for a prepared application.
    /// Service discovery happens only when opening a window, after preparation;
    /// captures, audits and headless driving retain their explicit isolation.
    #[must_use]
    pub fn native_services(mut self) -> Self {
        self.component_native_services = true;
        self
    }

    /// Apply capture-only interactions after the normal factory and restoration.
    /// Never runs for live/headless launches or native certification audits.
    #[must_use]
    pub fn capture_scenario(mut self, scenario: ComponentCaptureScenario) -> Self {
        self.component_capture_scenario = Some(scenario);
        self
    }

    /// What a launch with `args` opens, prepared by the specification's
    /// component host but not instantiated: the record, the profile it
    /// lives in and what the host hands the component.
    ///
    /// # Errors
    /// Returns argument, profile or record failures, or
    /// [`LaunchError::MissingComponentHost`].
    pub fn prepare_hosted(
        &self,
        args: &LaunchArgs,
    ) -> Result<crate::application::PreparedComponentInputs, LaunchError> {
        validate_spec_args(args, self)?;
        let prepare = self
            .component_host
            .ok_or(LaunchError::MissingComponentHost)?;
        prepare(self.definition, args)
    }

    /// Prepare this definition using already authenticated host inputs.
    /// # Errors
    /// Returns argument, capability, binding, factory, or restoration failures.
    pub fn prepare_component(
        &self,
        args: &LaunchArgs,
        context: crate::application::HostContext,
        target: crate::application::LaunchTarget,
        restore: crate::application::RestoreInput,
    ) -> Result<crate::application::AppComponent, LaunchError> {
        validate_spec_args(args, self)?;
        self.definition.instantiate(context, target, restore)
    }

    /// Scenario-specific default viewport. An explicit `--viewport` always
    /// wins over this callback.
    #[must_use]
    pub fn shot_viewport(mut self, factory: ViewportFactory) -> Self {
        self.shot_viewport = factory;
        self
    }

    /// Handle an app-specific, non-windowed command after argument validation.
    /// Return `true` when the command was handled and launch should stop.
    ///
    /// This keeps developer utilities such as fixture generation and benchmark
    /// drivers in the standard parser without making them part of the normal
    /// screenshot or live-window lifecycle. A component's in-process hosts
    /// never run it: commands belong to the app's own process.
    #[must_use]
    pub fn command_handler(mut self, handler: CommandHandler) -> Self {
        self.command_handler = Some(handler);
        self
    }

    /// Reject headless input driving for apps whose widgets have privileged
    /// side effects, such as an on-screen keyboard that injects keystrokes into
    /// another application.
    #[must_use]
    pub fn disable_headless(mut self) -> Self {
        self.headless_enabled = false;
        self
    }

    /// Allow app-specific boolean flags. Value-taking common options belong
    /// in `LaunchArgs`; specialized CLIs should use the lower-level APIs.
    #[must_use]
    pub fn allowed_flags(mut self, flags: &'static [&'static str]) -> Self {
        self.allowed_flags = flags;
        self
    }

    /// Allow app-specific, value-taking options in both `--name value` and
    /// `--name=value` form. Their values are available through
    /// [`LaunchArgs::value`].
    #[must_use]
    pub fn allowed_options(mut self, options: &'static [&'static str]) -> Self {
        self.allowed_options = options;
        self
    }

    /// Allow boolean flags in live/headless mode as well as screenshot mode.
    #[must_use]
    pub fn allowed_live_flags(mut self, flags: &'static [&'static str]) -> Self {
        self.allowed_live_flags = flags;
        self
    }

    /// Allow value-taking options in live/headless mode as well as screenshots.
    #[must_use]
    pub fn allowed_live_options(mut self, options: &'static [&'static str]) -> Self {
        self.allowed_live_options = options;
        self
    }

    /// Accept at most `max` positional document/session arguments.
    #[must_use]
    pub fn max_positionals(mut self, max: usize) -> Self {
        self.max_positionals = max;
        self
    }

    /// Declare every accepted screenshot scenario. With no declaration,
    /// `--scenario` is rejected instead of being silently ignored.
    #[must_use]
    pub fn scenarios(mut self, scenarios: &'static [&'static str]) -> Self {
        self.scenarios = scenarios;
        self
    }
}

pub enum LaunchError {
    Record(crate::record::RecordOpenError),
    Restore(crate::view::ThawError),
    DuplicateApplication(String),
    UnknownApplication(String),
    CapabilityDenied(crate::permissions::PermissionScope),
    BindingMismatch,
    MissingComponentHost,
    IncompatibleComponentSetup,
    /// Setup failed after the record became Ready: `recovery` holds its
    /// store (and any close reports) so the caller can reopen or flush it.
    ComponentSetup {
        source: Box<LaunchError>,
        record: Box<crate::record::RecordCheckout>,
        recovery: LaunchRecovery,
    },
    /// A capture failed or closed dirty: `recovery` holds the close reports.
    ComponentCapture {
        source: Box<LaunchError>,
        recovery: LaunchRecovery,
    },
    MissingValue(String),
    Duplicate(String),
    InvalidViewport(String),
    InvalidTextScale(String),
    UnknownArgument(String),
    ConflictingModes,
    ShotsOnly(String),
    ShotsUnsupported,
    HeadlessUnsupported,
    Scenario(String),
    Operation(String),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Record(error) => write!(f, "record open failed: {error}"),
            Self::Restore(error) => write!(f, "view restore failed: {error}"),
            Self::DuplicateApplication(id) => write!(f, "application {id:?} is already registered"),
            Self::UnknownApplication(id) => write!(f, "application {id:?} is not available"),
            Self::CapabilityDenied(scope) => write!(f, "required capability denied: {scope:?}"),
            Self::MissingComponentHost => {
                f.write_str("application definition requires an explicit component host")
            }
            Self::IncompatibleComponentSetup => {
                f.write_str("application definition cannot use legacy setup overrides")
            }
            Self::BindingMismatch => f.write_str("application binding does not match"),
            Self::ComponentSetup { source, .. } => write!(f, "application setup failed: {source}"),
            Self::ComponentCapture { source, .. } => write!(f, "{source}"),
            Self::MissingValue(flag) => write!(f, "{flag} requires a value"),
            Self::Duplicate(flag) => write!(f, "{flag} was supplied more than once"),
            Self::InvalidViewport(value) => write!(
                f,
                "invalid viewport {value:?}; expected non-zero WIDTHxHEIGHT"
            ),
            Self::InvalidTextScale(value) => write!(
                f,
                "invalid text scale {value:?}; expected a finite value from 0.75 to 2.0"
            ),
            Self::UnknownArgument(arg) => write!(f, "unknown argument {arg:?}"),
            Self::ConflictingModes => {
                write!(
                    f,
                    "choose only one of --shots, --shots-dir, --responsive-matrix, --headless-drive, or --native-audit (--shots-dir captures every scenario, so it takes no --scenario)"
                )
            }
            Self::ShotsOnly(arg) => write!(
                f,
                "{arg} is only meaningful with a capture or native-audit mode"
            ),
            Self::ShotsUnsupported => write!(f, "this application does not support --shots"),
            Self::HeadlessUnsupported => {
                write!(f, "this application does not support --headless-drive")
            }
            Self::Scenario(message) | Self::Operation(message) => f.write_str(message),
        }
    }
}

impl fmt::Debug for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Record(error) => f.debug_tuple("Record").field(error).finish(),
            Self::Restore(error) => f.debug_tuple("Restore").field(error).finish(),
            Self::CapabilityDenied(scope) => {
                f.debug_tuple("CapabilityDenied").field(scope).finish()
            }
            Self::ComponentSetup {
                source,
                record,
                recovery,
            } => f
                .debug_struct("ComponentSetup")
                .field("source", source)
                .field("record", record)
                .field("close_report_count", &recovery.report_count())
                .finish_non_exhaustive(),
            Self::ComponentCapture { source, recovery } => f
                .debug_struct("ComponentCapture")
                .field("source", source)
                .field("close_report_count", &recovery.report_count())
                .finish_non_exhaustive(),
            Self::MissingValue(value) => f.debug_tuple("MissingValue").field(value).finish(),
            Self::Duplicate(value) => f.debug_tuple("Duplicate").field(value).finish(),
            Self::InvalidViewport(value) => f.debug_tuple("InvalidViewport").field(value).finish(),
            Self::InvalidTextScale(value) => {
                f.debug_tuple("InvalidTextScale").field(value).finish()
            }
            Self::UnknownArgument(value) => f.debug_tuple("UnknownArgument").field(value).finish(),
            Self::ShotsOnly(value) => f.debug_tuple("ShotsOnly").field(value).finish(),
            Self::Scenario(value) => f.debug_tuple("Scenario").field(value).finish(),
            Self::Operation(value) => f.debug_tuple("Operation").field(value).finish(),
            Self::DuplicateApplication(value) => {
                f.debug_tuple("DuplicateApplication").field(value).finish()
            }
            Self::UnknownApplication(value) => {
                f.debug_tuple("UnknownApplication").field(value).finish()
            }
            Self::ConflictingModes => f.write_str("ConflictingModes"),
            Self::ShotsUnsupported => f.write_str("ShotsUnsupported"),
            Self::HeadlessUnsupported => f.write_str("HeadlessUnsupported"),
            Self::MissingComponentHost => {
                f.write_str("application definition requires an explicit component host")
            }
            Self::IncompatibleComponentSetup => {
                f.write_str("application definition cannot use legacy setup overrides")
            }
            Self::BindingMismatch => f.write_str("BindingMismatch"),
        }
    }
}

impl std::error::Error for LaunchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Record(error) => Some(error),
            Self::Restore(error) => Some(error),
            Self::ComponentSetup { source, .. } | Self::ComponentCapture { source, .. } => {
                Some(source.as_ref())
            }
            _ => None,
        }
    }
}

impl LaunchError {
    #[cfg(feature = "native")]
    fn exit_code(&self) -> i32 {
        if matches!(
            self,
            Self::Operation(_)
                | Self::Record(_)
                | Self::Restore(_)
                | Self::ComponentSetup { .. }
                | Self::ComponentCapture { .. }
                | Self::CapabilityDenied(_)
                | Self::BindingMismatch
                | Self::MissingComponentHost
                | Self::IncompatibleComponentSetup
        ) {
            1
        } else {
            2
        }
    }
}

// A launch error is plain data that may cross threads (into `anyhow::Error`,
// a worker's result, a `Box<dyn Error + Send + Sync>`); only its recovery
// owners are tied to the launching thread, behind `LaunchRecovery`.
const _: () = {
    const fn assert_send_sync<T: Send + Sync + 'static>() {}
    assert_send_sync::<LaunchError>();
};

/// What a failed launch still owns, handed back to the caller so nothing
/// dirty is lost: the record store holding the canonical record (reopen it,
/// or flush it), and the close reports of components torn down on the way
/// (retry their view or record durability).
///
/// These owners are single-threaded (`Rc` stores). The error carrying them
/// is still `Send + Sync`: the owners are reachable only on the thread that
/// launched, and every accessor returns nothing elsewhere. A recovery dropped
/// on another thread is leaked rather than dropped there — its stores are
/// never touched off their thread — so move the error back to the launching
/// thread (or consume the recovery there) when it must be released.
pub struct LaunchRecovery {
    owner: std::thread::ThreadId,
    report_count: usize,
    parts: std::mem::ManuallyDrop<Box<RecoveryParts>>,
}

struct RecoveryParts {
    records: Option<crate::record::RecordStore>,
    reports: Vec<crate::application::ComponentCloseReport>,
}

// SAFETY: every access to `parts` (shared or exclusive, including drop) is
// gated on the current thread being `owner`, the thread that created them.
// On any other thread the owners are neither read, cloned nor dropped, so
// their non-atomic reference counts are only ever touched by one thread.
#[allow(
    clippy::non_send_fields_in_send_ty,
    reason = "the single-threaded owners are reachable and droppable only on `owner`"
)]
unsafe impl Send for LaunchRecovery {}
// SAFETY: as above; `&LaunchRecovery` exposes the owners only on `owner`.
unsafe impl Sync for LaunchRecovery {}

impl LaunchRecovery {
    /// Recovery owners for the current thread.
    #[must_use]
    pub fn new(
        records: Option<crate::record::RecordStore>,
        reports: Vec<crate::application::ComponentCloseReport>,
    ) -> Self {
        Self {
            owner: std::thread::current().id(),
            report_count: reports.len(),
            parts: std::mem::ManuallyDrop::new(Box::new(RecoveryParts { records, reports })),
        }
    }

    /// Only the store still holding the failed launch's record.
    #[must_use]
    pub fn records(records: crate::record::RecordStore) -> Self {
        Self::new(Some(records), Vec::new())
    }

    /// Only the close reports of torn-down components.
    #[must_use]
    pub fn reports(reports: Vec<crate::application::ComponentCloseReport>) -> Self {
        Self::new(None, reports)
    }

    fn here(&self) -> bool {
        std::thread::current().id() == self.owner
    }

    /// How many close reports this recovery carries, on any thread.
    #[must_use]
    pub fn report_count(&self) -> usize {
        self.report_count
    }

    /// The record store, on the launching thread.
    #[must_use]
    pub fn record_store(&self) -> Option<&crate::record::RecordStore> {
        if self.here() {
            self.parts.records.as_ref()
        } else {
            None
        }
    }

    /// The close reports, on the launching thread (empty elsewhere).
    #[must_use]
    pub fn close_reports(&self) -> &[crate::application::ComponentCloseReport] {
        if self.here() {
            &self.parts.reports
        } else {
            &[]
        }
    }

    /// Take the owners out, on the launching thread.
    ///
    /// # Errors
    /// Returns the recovery unchanged on any other thread.
    pub fn into_parts(
        mut self,
    ) -> Result<
        (
            Option<crate::record::RecordStore>,
            Vec<crate::application::ComponentCloseReport>,
        ),
        Self,
    > {
        if !self.here() {
            return Err(self);
        }
        // SAFETY: on the owning thread; `self` is forgotten right after, so
        // `parts` is taken exactly once and never dropped again.
        let parts = unsafe { std::mem::ManuallyDrop::take(&mut self.parts) };
        std::mem::forget(self);
        Ok((parts.records, parts.reports))
    }

    /// The record store, taken out on the launching thread.
    ///
    /// # Errors
    /// Returns the recovery unchanged on any other thread.
    pub fn into_record_store(self) -> Result<Option<crate::record::RecordStore>, Self> {
        self.into_parts().map(|(records, _)| records)
    }

    /// The close reports, taken out on the launching thread.
    ///
    /// # Errors
    /// Returns the recovery unchanged on any other thread.
    pub fn into_close_reports(self) -> Result<Vec<crate::application::ComponentCloseReport>, Self> {
        self.into_parts().map(|(_, reports)| reports)
    }
}

impl Drop for LaunchRecovery {
    fn drop(&mut self) {
        if self.here() {
            // SAFETY: on the owning thread, and `parts` is dropped only here.
            unsafe { std::mem::ManuallyDrop::drop(&mut self.parts) };
        } else {
            tracing::warn!(
                report_count = self.report_count,
                "launch recovery dropped off its thread; its stores are left unflushed"
            );
        }
    }
}

impl fmt::Debug for LaunchRecovery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LaunchRecovery")
            .field("reachable", &self.here())
            .field("close_report_count", &self.report_count)
            .finish_non_exhaustive()
    }
}

/// Run the standard app lifecycle and report command-line errors using the
/// conventional usage-error exit status (`2`).
#[allow(
    clippy::exit,
    reason = "this *is* the app's CLI entry point (every app's `main` just \
              calls it); reporting a usage error via `std::process::exit` \
              is the documented contract, not an incidental exit deep in \
              library code"
)]
#[cfg(feature = "native")]
pub fn run_or_exit(spec: &LaunchSpec) {
    if let Err(error) = run(spec) {
        eprintln!("error: {error}");
        std::process::exit(error.exit_code());
    }
}

/// Run the standard app lifecycle using `std::env::args`.
///
/// # Errors
///
/// Returns a [`LaunchError`] if argument parsing fails (an unknown flag, a
/// missing or malformed value, a duplicate flag, `--shots` combined with
/// `--headless-drive`, a shots-only flag used outside `--shots`, or an
/// undeclared `--scenario`), or if the app's root factory, shot factory, or
/// shot handler returns one.
#[cfg(feature = "native")]
pub fn run(spec: &LaunchSpec) -> Result<(), LaunchError> {
    tilo_startup_metrics::init();
    let _startup_metrics = tilo_startup_metrics::ScopedDumper;
    tilo_startup_metrics::mark("app-launch-start");
    let _ = tracing_subscriber::fmt::try_init();
    let args: Vec<String> = std::env::args().collect();
    run_with_args(spec, &args)
}

#[cfg(feature = "native")]
fn run_with_args(spec: &LaunchSpec, raw_args: &[String]) -> Result<(), LaunchError> {
    run_with_args_with_attachment(spec, raw_args, runtime_portal::native::ensure_attached)
}

#[cfg(feature = "native")]
fn run_with_args_with_attachment(
    spec: &LaunchSpec,
    raw_args: &[String],
    attach: impl FnOnce(&str) -> std::io::Result<()>,
) -> Result<(), LaunchError> {
    let _detached = crate::wake::WakeScope::enter_detached();
    // Take the content-store capability the launcher handed this process
    // before anything else can, in every mode that was handed one (live,
    // headless drives, captures); the app's store is that device store.
    #[cfg(feature = "native")]
    let _ = tilo_content::ContentStore::device();
    let args = parse_args_extended(
        raw_args,
        spec.allowed_flags,
        spec.allowed_options,
        spec.allowed_live_flags,
        spec.allowed_live_options,
        spec.max_positionals,
    )?;
    validate_spec_args(&args, spec)?;
    // A capture shows the same date and time on every run.
    if args.shots.is_some() || args.responsive_matrix.is_some() {
        crate::clock::freeze_for_capture();
    }
    if let Some(scale) = args.device_scale {
        crate::offscreen::set_capture_device_scale(scale);
    }
    if let Some(on) = args.distinct_letters {
        let _ = CAPTURE_DISTINCT_LETTERS.set(on);
    }
    crate::i18n::I18nManager::set_pseudo_locale(if args.pseudo_locale {
        crate::i18n::PseudoLocale::Expanded
    } else {
        crate::i18n::PseudoLocale::Off
    });
    if let Some(directory) = &args.shots_dir {
        return capture_every_scenario(spec, &args, directory);
    }
    run_parsed(spec, &args, attach)
}

/// `--shots-dir`: each declared scenario (or the plain capture, for an app
/// that declares none) captured on its own, exactly as `--shots --scenario`
/// captures it, into `NN-name.png` so a gallery keeps declaration order.
#[cfg(feature = "native")]
fn capture_every_scenario(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    directory: &std::path::Path,
) -> Result<(), LaunchError> {
    capture_directory(directory, "--shots-dir")?;
    let scenarios: Vec<Option<&str>> = if spec.scenarios.is_empty() {
        vec![None]
    } else {
        spec.scenarios.iter().copied().map(Some).collect()
    };
    for (index, scenario) in scenarios.into_iter().enumerate() {
        let name = scenario.unwrap_or("default");
        let out = directory.join(format!("{:02}-{name}.png", index.saturating_add(1)));
        let mut capture = args.clone();
        capture.shots_dir = None;
        capture.shots = Some(out);
        capture.scenario = scenario.map(str::to_owned);
        // A capture never attaches to the runtime.
        run_parsed(spec, &capture, |_| Ok(()))?;
    }
    Ok(())
}

/// Create the directory a capture mode (`option`) writes its PNGs into.
#[cfg(feature = "native")]
fn capture_directory(directory: &std::path::Path, option: &str) -> Result<(), LaunchError> {
    std::fs::create_dir_all(directory).map_err(|error| {
        LaunchError::Operation(format!(
            "could not create the {option} directory {}: {error}",
            directory.display()
        ))
    })
}

/// Whether these arguments run the app in a live native window, rather than
/// headless, as captures or as an audit.
#[cfg(feature = "native")]
fn opens_native_window(args: &LaunchArgs) -> bool {
    args.headless.is_none()
        && args.shots.is_none()
        && args.responsive_matrix.is_none()
        && !args.native_audit
}

#[allow(
    clippy::exit,
    reason = "the --headless-drive path reports the harness's pass/fail \
              status as this process's exit code, exactly like --shots \
              reports success by returning Ok — there is no caller to \
              hand a status back to"
)]
#[cfg(feature = "native")]
fn run_parsed(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    attach: impl FnOnce(&str) -> std::io::Result<()>,
) -> Result<(), LaunchError> {
    let args = args.clone();
    if let Some(handler) = spec.command_handler
        && handler(&args)?
    {
        return Ok(());
    }
    // Attachment can exec the runtime host in this process's place, and marks
    // the inherited launch guard close-on-exec: it finishes before the
    // renderer preparation below starts a thread.
    if opens_native_window(&args) && (spec.runtime_attachment)(&args) {
        attach(spec.app_id).map_err(|error| {
            LaunchError::Operation(format!("native runtime attachment failed: {error}"))
        })?;
    }
    // Threads may start from here on; startup marks are written from their
    // own, never by the thread that marks.
    tilo_startup_metrics::start_writer();
    // A window's GPU device and vello's pipelines are prepared on their own
    // thread while the app constructs, so its first redraw takes them ready
    // instead of starting them after the compositor's first configure. Never
    // inside the event loop's `resumed`: see `prepare_native_renderer`.
    #[cfg(any(windows, target_os = "linux"))]
    let renderer = if opens_native_window(&args) {
        crate::app::prepare_native_renderer()
    } else {
        None
    };
    // The font database likewise, so the app's first text renderer finds
    // the catalog verified and the fonts read.
    if opens_native_window(&args) {
        prepare_font_database_beside();
    }
    match dispatch_definition(spec, &args)? {
        DefinitionDispatch::Native(app) => {
            #[cfg(any(windows, target_os = "linux"))]
            let app = Box::new((*app).with_startup_renderer(renderer));
            (*app).run();
        }
        DefinitionDispatch::Headless(status) => {
            // process::exit skips the ScopedDumper in run().
            tilo_startup_metrics::mark("headless-exit");
            tilo_startup_metrics::dump();
            std::process::exit(status);
        }
        DefinitionDispatch::Rendered(mut driver) => {
            if let Some(out) = &args.shots {
                driver.prepare_component_output();
                driver.render_png(out);
            } else if let Some(directory) = &args.responsive_matrix {
                for (name, viewport) in RESPONSIVE_VIEWPORTS {
                    driver.resize_component_capture(viewport);
                    driver.render_png(&directory.join(format!("{}-{name}.png", spec.app_id)));
                }
            }
            close_component_capture(&mut driver)?;
        }
        DefinitionDispatch::Audit(mut driver) => {
            finish_component_audit(&mut driver, spec.app_id)?;
        }
    }
    Ok(())
}

/// Start building the process's font database in the background
/// (`text_render::prepare_font_database`): only the database, so it neither
/// shapes text nor touches the GPU the renderer preparation is opening. Where
/// no background work can run, the first text renderer builds it as before.
#[cfg(feature = "native")]
fn prepare_font_database_beside() {
    let _ = crate::background::Background::current().spawn("font-database-prepare", || {
        tilo_startup_metrics::span(
            "font-database-prepare",
            crate::text_render::prepare_font_database,
        );
    });
}

#[cfg(feature = "native")]
fn close_component_capture(driver: &mut FrameHarness) -> Result<(), LaunchError> {
    driver.close_components();
    if driver.component_close_reports().iter().any(|report| {
        report.view_error.is_some()
            || report.record_error.is_some()
            || report.teardown_error.is_some()
    }) {
        return Err(LaunchError::ComponentCapture {
            source: Box::new(LaunchError::Operation(
                "component capture close failed".into(),
            )),
            recovery: LaunchRecovery::reports(driver.component_close_reports().to_vec()),
        });
    }
    Ok(())
}

#[cfg(feature = "native")]
fn finish_component_audit(driver: &mut FrameHarness, app_id: &str) -> Result<(), LaunchError> {
    driver.prepare_component_output();
    let audit = driver.native_app_audit();
    let report = serde_json::to_string_pretty(&audit)
        .map_err(|error| LaunchError::Operation(error.to_string()));
    if let Ok(report) = &report {
        println!("{report}");
    }
    close_component_capture(driver)?;
    report?;
    if !audit.is_certified() {
        return Err(LaunchError::Operation(format!(
            "{app_id} does not satisfy the Tilo-native application contract"
        )));
    }
    Ok(())
}

#[cfg(feature = "native")]
enum DefinitionDispatch {
    Native(Box<App>),
    Headless(i32),
    Rendered(Box<FrameHarness>),
    Audit(Box<FrameHarness>),
}

/// Construction/selection is testable without a process exit, native loop, or GPU.
#[cfg(feature = "native")]
fn dispatch_definition(
    spec: &LaunchSpec,
    args: &LaunchArgs,
) -> Result<DefinitionDispatch, LaunchError> {
    dispatch_definition_with_native(spec, args, |component, inputs, native_services| {
        tilo_startup_metrics::span("app-construct", || {
            if native_services {
                App::from_component_with_native_services(component, inputs)
            } else {
                App::from_component(component, inputs)
            }
        })
    })
}

#[cfg(feature = "native")]
fn dispatch_definition_with_native(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    native: impl FnOnce(
        crate::application::AppComponent,
        crate::application::ComponentRunnerInputs,
        bool,
    ) -> Result<App, LaunchError>,
) -> Result<DefinitionDispatch, LaunchError> {
    dispatch_definition_with_manager(spec, args, native, || native_launch_manager(spec, args))
}

#[cfg(feature = "native")]
fn dispatch_definition_with_manager(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    native: impl FnOnce(
        crate::application::AppComponent,
        crate::application::ComponentRunnerInputs,
        bool,
    ) -> Result<App, LaunchError>,
    manager: impl FnOnce() -> Result<crate::collaboration::CollaborationManager, LaunchError>,
) -> Result<DefinitionDispatch, LaunchError> {
    validate_spec_args(args, spec)?;
    let _selected_local = (args.headless.is_some()
        || args.shots.is_some()
        || args.responsive_matrix.is_some()
        || args.native_audit)
        .then(selected_local_construction)
        .transpose()?;
    let live_native = spec.component_native_services
        && args.headless.is_none()
        && args.shots.is_none()
        && args.responsive_matrix.is_none()
        && !args.native_audit;
    let scope = live_native.then(crate::wake::WakeScope::new);
    let _wake_entry = scope.as_ref().map(crate::wake::WakeScope::enter);
    let construction = scope
        .map(|scope| {
            crate::collaboration::native_initial::NativeConstruction::enter_component(
                manager()?,
                scope,
            )
            .map_err(LaunchError::Operation)
        })
        .transpose()?;
    let prepare = spec
        .component_host
        .ok_or(LaunchError::MissingComponentHost)?;
    let mut prepared =
        match tilo_startup_metrics::span("app-prepare", || prepare(spec.definition, args)) {
            Ok(prepared) => prepared,
            Err(LaunchError::Record(error)) if args.headless.is_some() => {
                eprintln!("headless: state open failed: {error}");
                return Ok(DefinitionDispatch::Headless(3));
            }
            Err(error) => return Err(error),
        };
    if let Some(scale) = args.text_scale {
        prepared.runner_inputs.text_renderer.set_text_scale(scale);
    }
    if let Some(construction) = &construction {
        construction
            .bind_component(&prepared.context.callback_binding())
            .map_err(LaunchError::Operation)?;
    }
    let component = tilo_startup_metrics::span("app-component", || {
        spec.prepare_component(args, prepared.context, prepared.target, prepared.restore)
    })?;
    if let Some(headless) = &args.headless {
        return Ok(DefinitionDispatch::Headless(headless::run_component(
            headless,
            component,
            prepared.runner_inputs,
        )));
    }
    if args.shots.is_some() || args.responsive_matrix.is_some() || args.native_audit {
        let viewport = args.viewport.unwrap_or_else(|| (spec.shot_viewport)(args));
        let mut driver = Box::new(FrameHarness::from_component(
            component,
            viewport,
            prepared.runner_inputs,
        )?);
        // The host prepared the component's presentation; a capture that
        // names a palette is drawn in it, as every other `--shots` path is.
        if args.theme.is_some() || std::env::var_os("TILO_SHOTS_THEME").is_some() {
            driver.set_theme(shot_theme(args));
        }
        if !args.native_audit
            && let Some(scenario) = spec.component_capture_scenario
            && let Err(source) = scenario(&mut driver, args)
        {
            driver.close_components();
            return Err(LaunchError::ComponentCapture {
                source: Box::new(source),
                recovery: LaunchRecovery::reports(driver.take_component_close_reports()),
            });
        }
        driver.enable_document_history_chrome();
        if !args.native_audit
            && let Err(source) = show_history_for_capture(&mut driver, args)
        {
            driver.close_components();
            return Err(LaunchError::ComponentCapture {
                source: Box::new(source),
                recovery: LaunchRecovery::reports(driver.take_component_close_reports()),
            });
        }
        return Ok(if args.native_audit {
            DefinitionDispatch::Audit(driver)
        } else {
            DefinitionDispatch::Rendered(driver)
        });
    }
    let app = crate::collaboration::native_initial::transfer_to_app(|| {
        native(
            component,
            prepared.runner_inputs,
            spec.component_native_services,
        )
    })
    .map_err(LaunchError::Operation)??;
    crate::collaboration::native_initial::check_current().map_err(LaunchError::Operation)?;
    Ok(DefinitionDispatch::Native(Box::new(app)))
}

/// `--history` and `--search`: open History or this window's search before
/// the capture settles, so the shot shows it as a person sees it.
#[cfg(feature = "native")]
fn show_history_for_capture(
    driver: &mut FrameHarness,
    args: &LaunchArgs,
) -> Result<(), LaunchError> {
    if args.history {
        driver
            .open_history()
            .map_err(|error| LaunchError::Operation(format!("--history: {error}")))?;
    }
    // `--search`: the window's search, opened as the app's own launcher
    // opens it.
    if let Some(query) = &args.search {
        driver
            .open_search(&crate::search::SearchRequest::this_app().with_query(query.clone()))
            .map_err(|error| LaunchError::Operation(format!("--search: {error}")))?;
    }
    Ok(())
}

#[cfg(feature = "native")]
fn selected_local_construction()
-> Result<crate::collaboration::native_initial::NativeConstruction, LaunchError> {
    crate::collaboration::native_initial::NativeConstruction::enter(
        crate::collaboration::CollaborationManager::disabled(),
    )
    .map_err(LaunchError::Operation)
}

#[cfg(feature = "native")]
fn native_launch_manager(
    spec: &LaunchSpec,
    args: &LaunchArgs,
) -> Result<crate::collaboration::CollaborationManager, LaunchError> {
    crate::collaboration::native_initial::ensure_unoccupied().map_err(LaunchError::Operation)?;
    if !(spec.runtime_attachment)(args) {
        return Ok(crate::collaboration::CollaborationManager::disabled());
    }
    crate::collaboration::CollaborationManager::for_native_launch(&spec.definition.record_kind)
        .map_err(LaunchError::Operation)
}

fn validate_spec_args(args: &LaunchArgs, spec: &LaunchSpec) -> Result<(), LaunchError> {
    if args.headless.is_some() && !spec.headless_enabled {
        return Err(LaunchError::HeadlessUnsupported);
    }
    if let Some(scenario) = args.scenario.as_deref()
        && !spec.scenarios.contains(&scenario)
    {
        return Err(LaunchError::Scenario(unknown_scenario(
            scenario,
            spec.scenarios,
        )));
    }
    Ok(())
}

/// The refusal of an undeclared `--scenario`, listing the declared ones.
fn unknown_scenario(name: &str, declared: &[&str]) -> String {
    if declared.is_empty() {
        format!("unknown screenshot scenario {name:?}; this app declares no scenarios")
    } else {
        format!(
            "unknown screenshot scenario {name:?}; expected one of {}",
            declared
                .iter()
                .map(|name| format!("{name:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// How long a capture waits for the window to settle (animations finished,
/// thumbnails loaded) before it writes anyway.
pub const CAPTURE_SETTLE_BOUND: crate::time::Duration = crate::time::Duration::from_secs(3);

/// The largest `--device-scale`: a 1280 x 800 window at 4x is 5120 x 3200.
pub const MAX_SHOT_DEVICE_SCALE: u32 = 4;

/// `--distinct-letters` for the capture this process writes: every capture
/// theme ([`shot_theme`], [`shot_theme_from_env`]) takes it.
static CAPTURE_DISTINCT_LETTERS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Pin "Always distinguish similar letters" on a capture theme when the
/// capture asked for it (`--distinct-letters`).
fn with_capture_letters(mut theme: Theme, explicit: Option<bool>) -> Theme {
    if let Some(on) = explicit.or_else(|| CAPTURE_DISTINCT_LETTERS.get().copied()) {
        theme.document.type_.distinct_letters = Some(on);
        theme.tokens.typography.distinct_letters = on;
    }
    theme
}

/// Resolve the palette, text scale, and motion policy used by screenshot
/// capture. An explicit `--text-scale` wins over the
/// `TILO_SHOTS_TEXT_SCALE` environment variable.
#[must_use]
pub fn shot_theme(args: &LaunchArgs) -> Theme {
    let theme = args
        .theme
        .clone()
        .or_else(|| std::env::var("TILO_SHOTS_THEME").ok())
        .and_then(|name| Theme::named(&name))
        .unwrap_or_else(Theme::dark);
    // `TILO_SHOTS_ACCENT=#RRGGBB` stands in for the host accent a standalone
    // app takes as the person's own colour: the presence hue nearest it.
    let theme = match std::env::var("TILO_SHOTS_ACCENT")
        .ok()
        .and_then(|hex| crate::color::parse_hex(&hex))
        .and_then(crate::tokens::PresenceHue::nearest)
    {
        Some(hue) => theme.with_your_colour(hue),
        None => theme,
    };
    let theme = with_capture_letters(theme.in_window(&args.window), args.distinct_letters);
    let environment_scale = std::env::var("TILO_SHOTS_TEXT_SCALE").ok();
    let theme = theme.with_text_scale(shot_text_scale(
        args.text_scale,
        environment_scale.as_deref(),
    ));
    // Screenshot review must use the same motion policy as a real app.  The
    // headless harness does not construct `ui::App`, where this environment
    // preference is normally read, so thread it onto the shot theme here.
    theme.with_reduce_motion(shot_motion_reduced())
}

/// The screenshot theme for capture frontends that have no [`LaunchArgs`]
/// (the `ui-shots` examples): `TILO_SHOTS_THEME`, `TILO_SHOTS_TEXT_SCALE` and
/// the motion preference through the same policy as [`shot_theme`]. An
/// unknown theme name is an error rather than a silent fall back to dark, so
/// a mis-typed light capture cannot masquerade as a dark one.
pub fn shot_theme_from_env() -> Result<Theme, String> {
    let theme = match std::env::var("TILO_SHOTS_THEME") {
        Ok(name) => {
            Theme::named(&name).ok_or_else(|| format!("unknown TILO_SHOTS_THEME `{name}`"))?
        }
        Err(_) => Theme::dark(),
    };
    let environment_scale = std::env::var("TILO_SHOTS_TEXT_SCALE").ok();
    Ok(with_capture_letters(theme, None)
        .with_text_scale(shot_text_scale(None, environment_scale.as_deref()))
        .with_reduce_motion(shot_motion_reduced()))
}

/// Resolve screenshot text-scale precedence without reading process-global
/// state. Custom capture frontends can pass their parsed CLI value and the
/// compatibility environment value through the same policy as [`shot_theme`].
#[must_use]
pub fn shot_text_scale(cli: Option<f32>, environment: Option<&str>) -> f32 {
    cli.or_else(|| {
        environment
            .and_then(|value| value.trim().parse::<f32>().ok())
            .filter(|scale| scale.is_finite())
    })
    .unwrap_or(1.0)
}

/// Read the user motion preference for the headless screenshot path.
///
/// Keep this deliberately in lockstep with `tilo_ui::App`'s startup gate: a test
/// image should not claim reduced-motion support while a launched app animates
/// (or vice versa).
fn shot_motion_reduced() -> bool {
    [
        "TILO_SHOTS_REDUCE_MOTION",
        "TILO_A11Y_REDUCE_MOTION",
        "TILO_REDUCE_MOTION",
    ]
    .into_iter()
    .filter_map(|key| std::env::var(key).ok())
    .any(|value| motion_env_value_enabled(&value))
}

fn motion_env_value_enabled(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
}

/// Product acceptance sizes in logical pixels. They exercise the supported
/// shape classes rather than pretending every device has one canonical size.
pub const RESPONSIVE_VIEWPORTS: [(&str, (u32, u32)); 4] = [
    ("phone", (390, 844)),
    ("tablet", (768, 1024)),
    ("laptop", (1366, 768)),
    ("desktop", (1920, 1080)),
];

#[cfg(test)]
fn parse_args(
    args: &[String],
    allowed_flags: &[&str],
    allowed_options: &[&str],
) -> Result<LaunchArgs, LaunchError> {
    parse_args_extended(args, allowed_flags, allowed_options, &[], &[], 0)
}

#[allow(
    clippy::too_many_lines,
    reason = "one shared argument scan keeps duplicate checks and cross-mode validation together"
)]
#[cfg(feature = "native")]
fn parse_args_extended(
    args: &[String],
    allowed_flags: &[&str],
    allowed_options: &[&str],
    allowed_live_flags: &[&str],
    allowed_live_options: &[&str],
    max_positionals: usize,
) -> Result<LaunchArgs, LaunchError> {
    let mut parsed = LaunchArgs {
        shots: None,
        shots_dir: None,
        responsive_matrix: None,
        viewport: None,
        scenario: None,
        headless: None,
        native_audit: false,
        pseudo_locale: false,
        text_scale: None,
        theme: None,
        window: crate::theme::WindowAppearance::default(),
        history: false,
        search: None,
        device_scale: None,
        distinct_letters: None,
        extras: Vec::new(),
        values: Vec::new(),
        positionals: Vec::new(),
    };
    let mut index: usize = 1;
    while let Some(arg) = args.get(index) {
        let (flag, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(flag, value)| (flag, Some(value)));
        match flag {
            "--shots" => {
                let value = take_value(args, &mut index, flag, inline)?;
                set_once(&mut parsed.shots, PathBuf::from(value), flag)?;
            }
            "--responsive-matrix" => {
                let value = take_value(args, &mut index, flag, inline)?;
                set_once(&mut parsed.responsive_matrix, PathBuf::from(value), flag)?;
            }
            "--shots-dir" => {
                let value = take_value(args, &mut index, flag, inline)?;
                set_once(&mut parsed.shots_dir, PathBuf::from(value), flag)?;
            }
            "--pseudo-locale" if inline.is_none() => {
                if parsed.pseudo_locale {
                    return Err(LaunchError::Duplicate(flag.to_owned()));
                }
                parsed.pseudo_locale = true;
            }
            "--text-scale" => {
                let value = take_value(args, &mut index, flag, inline)?;
                let scale = parse_text_scale(value)?;
                set_once(&mut parsed.text_scale, scale, flag)?;
            }
            "--viewport" => {
                let value = take_value(args, &mut index, flag, inline)?;
                let viewport = parse_viewport(value)?;
                set_once(&mut parsed.viewport, viewport, flag)?;
            }
            "--device-scale" => {
                let value = take_value(args, &mut index, flag, inline)?;
                let max = MAX_SHOT_DEVICE_SCALE;
                let scale = value
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .filter(|scale| (1..=max).contains(scale))
                    .ok_or_else(|| {
                        LaunchError::Scenario(format!(
                            "{flag}: expected a whole number from 1 to {max}, got {value:?}"
                        ))
                    })?;
                set_once(&mut parsed.device_scale, scale, flag)?;
            }
            "--distinct-letters" => {
                let value = take_value(args, &mut index, flag, inline)?;
                let on = match value.trim() {
                    "on" => true,
                    "off" => false,
                    other => {
                        return Err(LaunchError::Scenario(format!(
                            "{flag}: expected on or off, got {other:?}"
                        )));
                    }
                };
                set_once(&mut parsed.distinct_letters, on, flag)?;
            }
            "--scenario" => {
                let value = take_value(args, &mut index, flag, inline)?;
                set_once(&mut parsed.scenario, value.to_owned(), flag)?;
            }
            "--theme" => {
                let value = take_value(args, &mut index, flag, inline)?;
                if Theme::named(value).is_none() {
                    return Err(LaunchError::Scenario(format!(
                        "{flag}: no shipped theme is called {value:?} (one of {})",
                        Theme::ALL.join(", ")
                    )));
                }
                set_once(&mut parsed.theme, value.to_owned(), flag)?;
            }
            "--workspace-hue" => {
                let value = take_value(args, &mut index, flag, inline)?;
                let hue = value
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .filter(|hue| hue.is_finite())
                    .ok_or_else(|| {
                        LaunchError::Scenario(format!(
                            "{flag}: {value:?} is not a hue in OKLCH degrees"
                        ))
                    })?;
                set_once(
                    &mut parsed.window.workspace_hue,
                    hue.rem_euclid(360.0),
                    flag,
                )?;
            }
            "--your-colour" => {
                let value = take_value(args, &mut index, flag, inline)?;
                let hue = crate::tokens::PresenceHue::from_id(value).ok_or_else(|| {
                    LaunchError::Scenario(format!(
                        "{flag}: no presence colour is called {value:?} (one of {})",
                        crate::tokens::PresenceHue::ALL
                            .map(crate::tokens::PresenceHue::id)
                            .join(", ")
                    ))
                })?;
                set_once(&mut parsed.window.your_colour, hue, flag)?;
            }
            "--headless-drive" => {
                let value = take_value(args, &mut index, flag, inline)?;
                let value = headless::parse_spec(value)
                    .map_err(|error| LaunchError::Scenario(format!("{flag}: {error}")))?;
                set_once(&mut parsed.headless, value, flag)?;
            }
            "--native-audit" if inline.is_none() => {
                if parsed.native_audit {
                    return Err(LaunchError::Duplicate(flag.to_owned()));
                }
                parsed.native_audit = true;
            }
            "--search" => {
                let value = take_value(args, &mut index, flag, inline)?;
                set_once(&mut parsed.search, value.to_owned(), flag)?;
            }
            "--history" if inline.is_none() => {
                if parsed.history {
                    return Err(LaunchError::Duplicate(flag.to_owned()));
                }
                parsed.history = true;
            }
            // A Jump List task: run this host command once the window exists.
            #[cfg(all(windows, feature = "native"))]
            crate::host_chrome::windows_items::LAUNCH_OPTION => {
                let value = take_value(args, &mut index, flag, inline)?;
                let invocation = crate::host_chrome::windows_items::decode(value)
                    .ok_or_else(|| LaunchError::UnknownArgument(arg.clone()))?;
                crate::host_chrome::windows::request_startup_command(invocation);
            }
            extra
                if (allowed_flags.contains(&extra) || allowed_live_flags.contains(&extra))
                    && inline.is_none() =>
            {
                if parsed.extras.iter().any(|found| found == extra) {
                    return Err(LaunchError::Duplicate(extra.to_owned()));
                }
                parsed.extras.push(extra.to_owned());
            }
            extra if allowed_options.contains(&extra) || allowed_live_options.contains(&extra) => {
                if parsed.values.iter().any(|(found, _)| found == extra) {
                    return Err(LaunchError::Duplicate(extra.to_owned()));
                }
                let value = take_value(args, &mut index, extra, inline)?;
                parsed.values.push((extra.to_owned(), value.to_owned()));
            }
            positional
                if !positional.starts_with('-') && parsed.positionals.len() < max_positionals =>
            {
                parsed.positionals.push(positional.to_owned());
            }
            _ => return Err(LaunchError::UnknownArgument(arg.clone())),
        }
        index = index.saturating_add(1);
    }
    let mode_count = usize::from(parsed.shots.is_some())
        .saturating_add(usize::from(parsed.shots_dir.is_some()))
        .saturating_add(usize::from(parsed.responsive_matrix.is_some()))
        .saturating_add(usize::from(parsed.headless.is_some()))
        .saturating_add(usize::from(parsed.native_audit));
    if mode_count > 1 {
        return Err(LaunchError::ConflictingModes);
    }
    // Every scenario is captured; naming one is a single `--shots`.
    if parsed.shots_dir.is_some() && parsed.scenario.is_some() {
        return Err(LaunchError::ConflictingModes);
    }
    if parsed.history
        && parsed.shots.is_none()
        && parsed.shots_dir.is_none()
        && parsed.responsive_matrix.is_none()
    {
        return Err(LaunchError::ShotsOnly("--history".to_owned()));
    }
    if parsed.search.is_some()
        && parsed.shots.is_none()
        && parsed.shots_dir.is_none()
        && parsed.responsive_matrix.is_none()
    {
        return Err(LaunchError::ShotsOnly("--search".to_owned()));
    }
    if parsed.shots.is_none()
        && parsed.shots_dir.is_none()
        && parsed.responsive_matrix.is_none()
        && !parsed.native_audit
    {
        if parsed.viewport.is_some() {
            return Err(LaunchError::ShotsOnly("--viewport".to_owned()));
        }
        if parsed.theme.is_some() {
            return Err(LaunchError::ShotsOnly("--theme".to_owned()));
        }
        if parsed.device_scale.is_some() {
            return Err(LaunchError::ShotsOnly("--device-scale".to_owned()));
        }
        if parsed.distinct_letters.is_some() {
            return Err(LaunchError::ShotsOnly("--distinct-letters".to_owned()));
        }
        if parsed.window.workspace_hue.is_some() {
            return Err(LaunchError::ShotsOnly("--workspace-hue".to_owned()));
        }
        if parsed.window.your_colour.is_some() {
            return Err(LaunchError::ShotsOnly("--your-colour".to_owned()));
        }
        // A headless drive may name a scenario: it dumps that fixture's
        // geometry, the counterpart of `--shots --scenario`.
        if parsed.scenario.is_some() && parsed.headless.is_none() {
            return Err(LaunchError::ShotsOnly("--scenario".to_owned()));
        }
        if let Some(extra) = parsed
            .extras
            .iter()
            .find(|extra| !allowed_live_flags.contains(&extra.as_str()))
        {
            return Err(LaunchError::ShotsOnly(extra.clone()));
        }
        if let Some((option, _)) = parsed
            .values
            .iter()
            .find(|(option, _)| !allowed_live_options.contains(&option.as_str()))
        {
            return Err(LaunchError::ShotsOnly(option.clone()));
        }
    }
    Ok(parsed)
}

#[cfg(feature = "native")]
fn take_value<'a>(
    args: &'a [String],
    index: &mut usize,
    flag: &str,
    inline: Option<&'a str>,
) -> Result<&'a str, LaunchError> {
    if let Some(value) = inline {
        return (!value.is_empty())
            .then_some(value)
            .ok_or_else(|| LaunchError::MissingValue(flag.to_owned()));
    }
    *index = index.saturating_add(1);
    args.get(*index)
        .map(String::as_str)
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| LaunchError::MissingValue(flag.to_owned()))
}

#[cfg(feature = "native")]
fn set_once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<(), LaunchError> {
    if slot.is_some() {
        Err(LaunchError::Duplicate(flag.to_owned()))
    } else {
        *slot = Some(value);
        Ok(())
    }
}

#[cfg(feature = "native")]
fn parse_viewport(value: &str) -> Result<(u32, u32), LaunchError> {
    let invalid = || LaunchError::InvalidViewport(value.to_owned());
    let (width, height) = value.split_once(['x', 'X']).ok_or_else(invalid)?;
    let width: u32 = width.trim().parse().map_err(|_| invalid())?;
    let height: u32 = height.trim().parse().map_err(|_| invalid())?;
    if width == 0 || height == 0 {
        return Err(invalid());
    }
    Ok((width, height))
}

#[cfg(feature = "native")]
fn parse_text_scale(value: &str) -> Result<f32, LaunchError> {
    let invalid = || LaunchError::InvalidTextScale(value.to_owned());
    let scale: f32 = value.trim().parse().map_err(|_| invalid())?;
    if !scale.is_finite() || !(0.75..=2.0).contains(&scale) {
        return Err(invalid());
    }
    Ok(scale)
}

#[cfg(test)]
mod tests {
    #[cfg(all(unix, feature = "broker-transport"))]
    include!("launch/native_construction_owner_tests.rs");
    use super::*;

    /// The launcher's environment contract, pointed at a scratch data home.
    /// The variables are restored before the directory is removed.
    struct LaunchFixtureEnv {
        _env: tilo_test_env::ScopedEnv,
        _directory: tempfile::TempDir,
    }
    impl LaunchFixtureEnv {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_DATA_HOME", directory.path())
                .set("TILO_PACKAGE_ID", "so.tilo.launch-fixture")
                .set("TILO_WORKSPACE_ID", "00000000-0000-0000-0000-000000000000");
            Self {
                _env: env,
                _directory: directory,
            }
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    /// `run_parsed` starts the renderer preparation thread (Windows and Linux)
    /// and the runtime attachment exactly for these arguments.
    #[test]
    fn only_a_live_window_prepares_its_renderer_ahead() {
        let live = parse_args(&strings(&["app"]), &[], &[]).unwrap();
        assert!(opens_native_window(&live));
        let capture = parse_args(&strings(&["app", "--shots=/tmp/shot.png"]), &[], &[]).unwrap();
        assert!(!opens_native_window(&capture));
    }

    #[test]
    fn cli_parses_every_common_mode_and_preference() {
        // Case `parses_both_common_argument_forms`.
        {
            let args = strings(&[
                "app",
                "--shots=/tmp/shot.png",
                "--viewport",
                "640X480",
                "--scenario=wide",
                "--empty",
            ]);
            let parsed = parse_args(&args, &["--empty"], &[]).unwrap();
            assert_eq!(parsed.shots, Some(PathBuf::from("/tmp/shot.png")));
            assert_eq!(parsed.viewport, Some((640, 480)));
            assert_eq!(parsed.scenario.as_deref(), Some("wide"));
            assert!(parsed.has("--empty"));
            assert_eq!(
                parsed.window,
                crate::theme::WindowAppearance::default(),
                "a capture without a shell is neutral"
            );
        }

        // Case `a_capture_can_stand_in_a_workspace_for_a_person`.
        {
            let args = strings(&[
                "app",
                "--shots=/tmp/shot.png",
                "--workspace-hue=400",
                "--your-colour",
                "violet",
            ]);
            let parsed = parse_args(&args, &[], &[]).unwrap();
            assert_eq!(
                parsed.window,
                crate::theme::WindowAppearance {
                    workspace_hue: Some(40.0),
                    your_colour: Some(crate::tokens::PresenceHue::Violet),
                }
            );
            let theme = shot_theme(&LaunchArgs {
                theme: Some("light".into()),
                ..parsed
            });
            assert_eq!(theme.document.palette.hue, Some(40.0));
            assert_eq!(theme.tokens.colors.you, crate::tokens::PresenceHue::Violet);
            for bad in [
                &["app", "--shots=/tmp/s.png", "--workspace-hue=warm"][..],
                &["app", "--shots=/tmp/s.png", "--your-colour=chartreuse"][..],
                &["app", "--workspace-hue=40"][..],
            ] {
                assert!(parse_args(&strings(bad), &[], &[]).is_err(), "{bad:?}");
            }
        }

        // Case `parses_responsive_matrix_and_pseudo_locale_as_common_audit_modes`.
        {
            let parsed = parse_args(
                &strings(&[
                    "app",
                    "--responsive-matrix=/tmp/matrix",
                    "--pseudo-locale",
                    "--text-scale=2",
                    "--scenario=wide",
                ]),
                &[],
                &[],
            )
            .unwrap();
            assert_eq!(parsed.responsive_matrix, Some(PathBuf::from("/tmp/matrix")));
            assert!(parsed.pseudo_locale);
            assert_eq!(parsed.text_scale, Some(2.0));
            assert_eq!(parsed.scenario.as_deref(), Some("wide"));
        }

        // Case `a_capture_takes_a_device_scale_and_the_distinct_letters_setting`.
        {
            let parsed = parse_args(
                &strings(&[
                    "app",
                    "--shots=/tmp/s.png",
                    "--device-scale=4",
                    "--distinct-letters",
                    "on",
                ]),
                &[],
                &[],
            )
            .unwrap();
            assert_eq!(parsed.device_scale, Some(4));
            assert_eq!(parsed.distinct_letters, Some(true));
            let theme = shot_theme(&LaunchArgs {
                theme: Some("light".into()),
                ..parsed.clone()
            });
            assert!(theme.tokens.typography.distinct_letters);
            let off = shot_theme(&LaunchArgs {
                distinct_letters: Some(false),
                ..parsed
            });
            assert!(!off.tokens.typography.distinct_letters);
            for bad in [
                &["app", "--shots=/tmp/s.png", "--device-scale=0"][..],
                &["app", "--shots=/tmp/s.png", "--device-scale=5"][..],
                &["app", "--shots=/tmp/s.png", "--distinct-letters=yes"][..],
            ] {
                assert!(matches!(
                    parse_args(&strings(bad), &[], &[]),
                    Err(LaunchError::Scenario(_))
                ));
            }
            for live in [
                &["app", "--device-scale=2"][..],
                &["app", "--distinct-letters=on"][..],
            ] {
                assert!(matches!(
                    parse_args(&strings(live), &[], &[]),
                    Err(LaunchError::ShotsOnly(_))
                ));
            }
        }

        // Case `shots_dir_captures_every_scenario_and_names_none`.
        {
            let parsed = parse_args(
                &strings(&["app", "--shots-dir", "/tmp/gallery", "--viewport=640x480"]),
                &[],
                &[],
            )
            .unwrap();
            assert_eq!(parsed.shots_dir, Some(PathBuf::from("/tmp/gallery")));
            assert_eq!(parsed.viewport, Some((640, 480)));
            for conflicting in [
                &["app", "--shots-dir=/tmp/a", "--scenario=wide"][..],
                &["app", "--shots-dir=/tmp/a", "--shots=/tmp/b.png"][..],
            ] {
                assert!(matches!(
                    parse_args(&strings(conflicting), &[], &[]),
                    Err(LaunchError::ConflictingModes)
                ));
            }
        }

        // Case `parses_headless_payload_without_process_exit`.
        {
            let args = strings(&[
                "app",
                "--headless-drive",
                r#"{"viewport":[800,600],"actions":[{"key":"Enter"}]}"#,
            ]);
            let parsed = parse_args(&args, &[], &[]).unwrap();
            let spec = parsed.headless.expect("headless spec");
            assert_eq!(spec.viewport, Some((800, 600)));
            assert_eq!(spec.actions.len(), 1);
        }

        // Case `parses_native_audit_as_an_exclusive_common_mode`.
        {
            let parsed = parse_args(
                &strings(&[
                    "app",
                    "--native-audit",
                    "--viewport",
                    "390x844",
                    "--text-scale",
                    "2",
                    "--pseudo-locale",
                ]),
                &[],
                &[],
            )
            .unwrap();
            assert!(parsed.native_audit);
            assert_eq!(parsed.viewport, Some((390, 844)));
            assert_eq!(parsed.text_scale, Some(2.0));
            assert!(parsed.pseudo_locale);

            assert!(matches!(
                parse_args(
                    &strings(&["app", "--native-audit", "--headless-drive", "{}"]),
                    &[],
                    &[]
                ),
                Err(LaunchError::ConflictingModes)
            ));
            assert!(matches!(
                parse_args(
                    &strings(&["app", "--native-audit", "--native-audit"]),
                    &[],
                    &[]
                ),
                Err(LaunchError::Duplicate(_))
            ));
        }

        // Case `screenshot_text_scale_prefers_cli_then_compatibility_environment`.
        {
            for (cli, environment, expected) in [
                (Some(2.0), Some("1.25"), 2.0),
                (None, Some("1.25"), 1.25),
                (None, None, 1.0),
                (None, Some("invalid"), 1.0),
            ] {
                assert_eq!(shot_text_scale(cli, environment), expected);
            }
        }

        // Case `screenshot_motion_preference_matches_live_app_truthy_values`.
        {
            for value in ["", "  ", "0", " false ", "FALSE"] {
                assert!(
                    !motion_env_value_enabled(value),
                    "{value:?} disables reduced motion"
                );
            }
            for value in ["1", "true", "yes"] {
                assert!(
                    motion_env_value_enabled(value),
                    "{value:?} enables reduced motion"
                );
            }
        }
    }

    #[test]
    fn cli_rejects_malformed_unknown_and_conflicting_arguments() {
        // Case `rejects_malformed_and_zero_viewports`.
        {
            for value in ["wide", "800", "0x600", "800x0", "800xnope"] {
                let args = strings(&["app", "--viewport", value]);
                assert!(matches!(
                    parse_args(&args, &[], &[]),
                    Err(LaunchError::InvalidViewport(_))
                ));
            }
        }

        // Case `rejects_invalid_text_scales`.
        {
            for value in ["nope", "NaN", "0", "0.5", "2.1"] {
                let args = strings(&["app", "--text-scale", value]);
                assert!(matches!(
                    parse_args(&args, &[], &[]),
                    Err(LaunchError::InvalidTextScale(_))
                ));
            }
        }

        // Case `rejects_unknown_missing_and_duplicate_arguments`.
        {
            assert!(matches!(
                parse_args(&strings(&["app", "--mystery"]), &[], &[]),
                Err(LaunchError::UnknownArgument(_))
            ));
            assert!(matches!(
                parse_args(&strings(&["app", "--shots"]), &[], &[]),
                Err(LaunchError::MissingValue(_))
            ));
            assert!(matches!(
                parse_args(&strings(&["app", "--scenario=a", "--scenario=b"]), &[], &[],),
                Err(LaunchError::Duplicate(_))
            ));
        }

        // Case `history_is_a_capture_option`.
        {
            let shot = parse_args(
                &strings(&["app", "--shots", "/tmp/a.png", "--history"]),
                &[],
                &[],
            )
            .unwrap();
            assert!(shot.history);
            assert!(matches!(
                parse_args(&strings(&["app", "--history"]), &[], &[]),
                Err(LaunchError::ShotsOnly(flag)) if flag == "--history"
            ));
            let shot = parse_args(
                &strings(&["app", "--shots", "/tmp/a.png", "--search", "café"]),
                &[],
                &[],
            )
            .unwrap();
            assert_eq!(shot.search.as_deref(), Some("café"));
            assert!(matches!(
                parse_args(&strings(&["app", "--search", "café"]), &[], &[]),
                Err(LaunchError::ShotsOnly(flag)) if flag == "--search"
            ));
        }

        // Case `rejects_conflicting_modes_and_shot_options_in_live_mode`.
        {
            let both = strings(&["app", "--shots", "/tmp/a.png", "--headless-drive", "{}"]);
            assert!(matches!(
                parse_args(&both, &[], &[]),
                Err(LaunchError::ConflictingModes)
            ));
            let matrix_and_shot = strings(&[
                "app",
                "--shots",
                "/tmp/a.png",
                "--responsive-matrix",
                "/tmp/matrix",
            ]);
            assert!(matches!(
                parse_args(&matrix_and_shot, &[], &[]),
                Err(LaunchError::ConflictingModes)
            ));

            for option in [
                strings(&["app", "--viewport=800x600"]),
                strings(&["app", "--scenario=wide"]),
                strings(&["app", "--empty"]),
            ] {
                assert!(matches!(
                    parse_args(&option, &["--empty"], &[]),
                    Err(LaunchError::ShotsOnly(_))
                ));
            }
        }
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_initial_launch_persists_merged_history_without_a_local_edit() {
        use automerge::{ReadDoc, transaction::Transactable};
        let _environment = LaunchFixtureEnv::new();
        let room = uuid::Uuid::new_v4();
        let mut _env_override = tilo_test_env::ScopedEnv::new();
        _env_override.set("TILO_WORKSPACE_ID", room.to_string());
        let mut history = automerge::AutoCommit::new();
        history
            .put(
                automerge::ROOT,
                "initial_only",
                "must survive local restart",
            )
            .unwrap();
        history.commit();
        let (manager, _) =
            crate::collaboration::CollaborationManager::native_initial_fixture_for_test(
                "so.tilo.launch-fixture",
                room,
                history.save(),
            );
        let construction =
            crate::collaboration::native_initial::NativeConstruction::enter(manager).unwrap();
        let (mut state, mut saver) = crate::persist::open("so.tilo.launch-fixture");
        saver.request(&mut state.doc);
        saver.flush();
        assert!(saver.last_error().is_none());
        drop(saver);
        drop(construction);
        let mut store =
            crate::persist::DocStore::open_room("so.tilo.launch-fixture", &room.to_string())
                .unwrap();
        let doc = store
            .load_complete()
            .unwrap()
            .expect("initial capture must reach local persistence");
        assert_eq!(
            doc.get(automerge::ROOT, "initial_only")
                .unwrap()
                .unwrap()
                .0
                .as_str(),
            Some("must survive local restart")
        );
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_initial_launch_canonical_cache_reads_existing_history_before_defaults() {
        use automerge::{ReadDoc, transaction::Transactable};
        let environment = LaunchFixtureEnv::new();
        let room = uuid::Uuid::new_v4();
        let mut _env_override = tilo_test_env::ScopedEnv::new();
        _env_override.set("TILO_WORKSPACE_ID", room.to_string());
        let want = crate::record::RecordCheckout::new(
            crate::record::RecordId::minted(),
            crate::record::RecordKind::new("note"),
        );
        let mut history = automerge::AutoCommit::new();
        assert!(crate::record::init_scoped(
            &mut history,
            &want.id,
            &want.kind,
            &room.to_string(),
            Some(&room.to_string())
        ));
        history
            .put(
                automerge::ROOT,
                "canonical_history",
                "already admitted graph",
            )
            .unwrap();
        history.commit();
        let heads = history.get_heads();
        let (manager, joined) =
            crate::collaboration::CollaborationManager::native_initial_fixture_for_test(
                "so.tilo.launch-fixture",
                room,
                history.save(),
            );
        let construction =
            crate::collaboration::native_initial::NativeConstruction::enter(manager).unwrap();
        let mut owners = crate::record::RecordOwners::new(
            environment._directory.path().join("native-canonical"),
        );
        let records = owners
            .open("so.tilo.launch-fixture", &room.to_string())
            .unwrap();
        records
            .open_existing_record_checked(&want)
            .expect("admitted remote history may populate missing local cache");
        records
            .read_checked(&want.id, |doc| {
                assert_eq!(
                    doc.get(automerge::ROOT, "canonical_history")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str(),
                    Some("already admitted graph")
                );
                for head in &heads {
                    assert!(doc.get_change_by_hash(head).is_some());
                }
            })
            .unwrap();
        assert_eq!(joined(), 2);
        drop(construction);
        let other = crate::record::RecordCheckout::new(
            crate::record::RecordId::minted(),
            want.kind.clone(),
        );
        assert!(records.open_existing_record_checked(&other).is_err());
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_initial_launch_record_entry_legacy_cache_uses_actual_history() {
        use automerge::{ReadDoc, transaction::Transactable};
        let _environment = LaunchFixtureEnv::new();
        let room = uuid::Uuid::new_v4();
        let mut _env_override = tilo_test_env::ScopedEnv::new();
        _env_override.set("TILO_WORKSPACE_ID", room.to_string());
        let want = crate::record::RecordCheckout::new(
            crate::record::RecordId::minted(),
            crate::record::RecordKind::new("note"),
        );
        let mut history = automerge::AutoCommit::new();
        assert!(crate::record::init_scoped(
            &mut history,
            &want.id,
            &want.kind,
            &room.to_string(),
            Some(&room.to_string())
        ));
        history
            .put(automerge::ROOT, "retained", "original history")
            .unwrap();
        history.commit();
        let heads = history.get_heads();
        let (manager, _) =
            crate::collaboration::CollaborationManager::native_initial_fixture_for_test(
                "so.tilo.launch-fixture",
                room,
                history.save(),
            );
        let _construction =
            crate::collaboration::native_initial::NativeConstruction::enter(manager).unwrap();
        let records = crate::record::RecordStore::open("so.tilo.launch-fixture");
        records
            .open_existing_record_checked(&want)
            .expect("missing local cache is populated from the actual Existing read");
        records
            .read_checked(&want.id, |doc| {
                assert_eq!(
                    doc.get(automerge::ROOT, "retained")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str(),
                    Some("original history")
                );
                for head in heads {
                    assert!(doc.get_change_by_hash(&head).is_some());
                }
            })
            .unwrap();
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_initial_launch_record_entry_primary_keeps_admitted_route() {
        use automerge::transaction::Transactable;
        let environment = LaunchFixtureEnv::new();
        let room = uuid::Uuid::new_v4();
        let mut _env_override = tilo_test_env::ScopedEnv::new();
        _env_override.set("TILO_WORKSPACE_ID", room.to_string());
        let want = crate::record::RecordCheckout::package_primary(crate::record::RecordKind::new(
            "primary",
        ));
        let mut history = automerge::AutoCommit::new();
        assert!(crate::record::init_scoped(
            &mut history,
            &want.id,
            &want.kind,
            &room.to_string(),
            Some(&room.to_string())
        ));
        history.put(automerge::ROOT, "retained", "primary").unwrap();
        history.commit();
        let (manager, joined) =
            crate::collaboration::CollaborationManager::native_initial_fixture_for_test(
                "so.tilo.launch-fixture",
                room,
                history.save(),
            );
        let _construction =
            crate::collaboration::native_initial::NativeConstruction::enter(manager).unwrap();
        let mut owners =
            crate::record::RecordOwners::new(environment._directory.path().join("primary"));
        let records = owners
            .open("so.tilo.launch-fixture", &room.to_string())
            .unwrap();
        records.open_existing_record_checked(&want).unwrap();
        assert_eq!(
            joined(),
            1,
            "package-primary is a local custody key, never a second wire route"
        );
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_initial_launch_component_host_reads_before_factory_on_same_worker() {
        use crate::application::{
            HostContext, HostContextInputs, LaunchTarget, PreparedComponentInputs, RestoreInput,
        };
        use crate::application_test_support as probe;
        static RECHECK: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>> =
            std::sync::Mutex::new(None);
        use automerge::{ReadDoc, transaction::Transactable};
        fn prepare(
            definition: &crate::application::ApplicationDefinition,
            args: &LaunchArgs,
        ) -> Result<PreparedComponentInputs, LaunchError> {
            let root = std::path::PathBuf::from(args.value("--fixture-root").unwrap());
            let room = args.value("--fixture-room").unwrap();
            let id =
                crate::record::RecordId::parse(args.value("--fixture-record").unwrap()).unwrap();
            let want = crate::record::RecordCheckout::new(id, definition.record_kind.clone());
            let mut owners = crate::record::RecordOwners::new(root.clone());
            let records = owners
                .open(definition.app_id, room)
                .map_err(LaunchError::Record)?;
            let views = std::rc::Rc::new(
                crate::application_view::InstanceViewStore::new(root, probe::DEVICE, room)
                    .map_err(LaunchError::Restore)?,
            );
            let target = LaunchTarget::Record {
                checkout: want,
                reveal: None,
            };
            let context = HostContext::prepare(
                definition,
                HostContextInputs {
                    records,
                    views,
                    device_id: probe::DEVICE.into(),
                    instance_id: 11,
                    permissions: probe::runner_permissions(),
                    author: probe::author(),
                    action_context: probe::provenance(),
                },
                &target,
            )?;
            context
                .read_record(|doc| {
                    assert_eq!(
                        doc.get(automerge::ROOT, "body")
                            .unwrap()
                            .unwrap()
                            .0
                            .as_str(),
                        Some("existing before component factory")
                    )
                })
                .unwrap();
            let runner_inputs = probe::runner_inputs();
            if let Some(release) = RECHECK.lock().unwrap().take() {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    let _ = release.send(());
                });
            }
            Ok(PreparedComponentInputs {
                context,
                target,
                restore: RestoreInput::None,
                runner_inputs,
            })
        }
        let environment = LaunchFixtureEnv::new();
        let room = uuid::Uuid::new_v4();
        let id = crate::record::RecordId::minted();
        let mut history = automerge::AutoCommit::new();
        assert!(crate::record::init_scoped(
            &mut history,
            &id,
            &crate::record::RecordKind::new("probe"),
            &room.to_string(),
            Some(&room.to_string())
        ));
        history
            .put(automerge::ROOT, "body", "existing before component factory")
            .unwrap();
        history.commit();
        let (release, gate) = tokio::sync::oneshot::channel();
        *RECHECK.lock().unwrap() = Some(release);
        let (manager, joined) =
            crate::collaboration::CollaborationManager::native_initial_fixture_gated_for_test(
                probe::APP,
                room,
                history.save(),
                gate,
            );
        let spec = LaunchSpec::from_definition(probe::runner_probe_definition())
            .component_host(prepare)
            .native_services()
            .allowed_live_options(&["--fixture-root", "--fixture-room", "--fixture-record"]);
        let raw = vec![
            "app".into(),
            "--fixture-root".into(),
            environment
                ._directory
                .path()
                .join("component")
                .display()
                .to_string(),
            "--fixture-room".into(),
            room.to_string(),
            "--fixture-record".into(),
            id.as_str().into(),
        ];
        let args = parse_args_extended(
            &raw,
            &[],
            &[],
            &[],
            &["--fixture-root", "--fixture-room", "--fixture-record"],
            0,
        )
        .unwrap();
        let mut wake = None;
        let result = dispatch_definition_with_manager(
            &spec,
            &args,
            |component, inputs, native| {
                assert!(native);
                App::from_component_with_native_services(component, inputs)
            },
            || {
                wake = Some(crate::wake::WakeHandle::new());
                Ok(manager)
            },
        )
        .unwrap();
        let DefinitionDispatch::Native(app) = result else {
            panic!("native component expected")
        };
        assert!(app.collaboration_is_connected());
        assert_eq!(
            joined(),
            2,
            "primary discovery and exact record share one worker"
        );
        assert!(
            wake.as_ref().unwrap().is_active(),
            "same worker wake scope transfers into App"
        );
        drop(app);
        assert!(!wake.as_ref().unwrap().is_active());
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_component_host_admits_a_new_record_before_its_factory_writes_it() {
        use crate::application::{
            HostContext, LaunchTarget, PreparedComponentInputs, RestoreInput,
        };
        use crate::application_test_support as probe;
        fn prepare(
            definition: &crate::application::ApplicationDefinition,
            args: &LaunchArgs,
        ) -> Result<PreparedComponentInputs, LaunchError> {
            let root = std::path::PathBuf::from(args.value("--fixture-root").unwrap());
            let room = args.value("--fixture-room").unwrap();
            let id =
                crate::record::RecordId::parse(args.value("--fixture-record").unwrap()).unwrap();
            let mut owners = crate::record::RecordOwners::new(root.clone());
            let records = owners
                .open(definition.app_id, room)
                .map_err(LaunchError::Record)?;
            let views = std::rc::Rc::new(
                crate::application_view::InstanceViewStore::new(root, probe::DEVICE, room)
                    .map_err(LaunchError::Restore)?,
            );
            // A fresh profile: the default record is only reserved here, and
            // the component factory writes its genesis after admission.
            let context = HostContext::prepare_new_record(
                definition,
                crate::application::HostContextInputs {
                    records,
                    views,
                    device_id: probe::DEVICE.into(),
                    instance_id: 11,
                    permissions: probe::runner_permissions(),
                    author: probe::author(),
                    action_context: probe::provenance(),
                },
                id,
            )?;
            Ok(PreparedComponentInputs {
                context,
                target: LaunchTarget::New,
                restore: RestoreInput::None,
                runner_inputs: probe::runner_inputs(),
            })
        }
        let environment = LaunchFixtureEnv::new();
        let room = uuid::Uuid::new_v4();
        let id = crate::record::RecordId::minted();
        let manager = crate::collaboration::CollaborationManager::connected_fixture_for_test(
            probe::APP,
            room,
        );
        let spec = LaunchSpec::from_definition(probe::runner_probe_definition())
            .component_host(prepare)
            .native_services()
            .allowed_live_options(&["--fixture-root", "--fixture-room", "--fixture-record"]);
        let raw = vec![
            "app".into(),
            "--fixture-root".into(),
            environment
                ._directory
                .path()
                .join("component")
                .display()
                .to_string(),
            "--fixture-room".into(),
            room.to_string(),
            "--fixture-record".into(),
            id.as_str().into(),
        ];
        let args = parse_args_extended(
            &raw,
            &[],
            &[],
            &[],
            &["--fixture-root", "--fixture-room", "--fixture-record"],
            0,
        )
        .unwrap();
        let initializations = probe::initializations();
        let result = dispatch_definition_with_manager(
            &spec,
            &args,
            |component, inputs, native| {
                assert!(native);
                App::from_component_with_native_services(component, inputs)
            },
            || Ok(manager),
        );
        let app = match result {
            Ok(DefinitionDispatch::Native(app)) => app,
            Ok(_) => panic!("native component expected"),
            Err(error) => panic!("a New default record must pass native admission: {error}"),
        };
        assert!(app.collaboration_is_connected());
        assert_eq!(
            probe::initializations(),
            initializations + 1,
            "the factory wrote the admitted record's genesis"
        );
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_initial_launch_creation_requires_governor_before_reservation() {
        use automerge::transaction::Transactable;
        let environment = LaunchFixtureEnv::new();
        let room = uuid::Uuid::new_v4();
        let want = crate::record::RecordCheckout::new(
            crate::record::RecordId::minted(),
            crate::record::RecordKind::new("note"),
        );
        let mut history = automerge::AutoCommit::new();
        history.put(automerge::ROOT, "primary", "existing").unwrap();
        history.commit();
        let (manager, _) =
            crate::collaboration::CollaborationManager::native_initial_fixture_for_test(
                "so.tilo.launch-fixture",
                room,
                history.save(),
            );
        let _construction =
            crate::collaboration::native_initial::NativeConstruction::enter(manager).unwrap();
        let root = environment._directory.path().join("native-new");
        let mut owners = crate::record::RecordOwners::new(root.clone());
        let records = owners
            .open("so.tilo.launch-fixture", &room.to_string())
            .unwrap();
        let directory = app_state::DocStore::room_record_dir_at(
            &root,
            "so.tilo.launch-fixture",
            &room.to_string(),
            &want.id.storage_key(),
        )
        .unwrap();
        assert!(!directory.exists());
        assert!(
            records.reserve_record(&want).is_err(),
            "Required native cannot mint a canonical writer from local absence"
        );
        assert!(
            !directory.exists(),
            "no default graph, writer, or custody file before governor creation admission"
        );
        assert!(!records.is_ready(&want.id));
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_initial_launch_missing_configured_service_never_selects_local() {
        let _environment = LaunchFixtureEnv::new();
        let mut env_override = tilo_test_env::ScopedEnv::new();
        env_override.set("TILO_SVC_COLLAB_FD", "-1");
        let manager = crate::collaboration::CollaborationManager::for_native_launch(
            &crate::record::RecordKind::new("launch-fixture.document"),
        );
        drop(env_override);
        assert!(
            manager.is_err(),
            "configured but missing native service is unavailable, not a local launch"
        );
    }
}

#[cfg(test)]
mod prepared_launch_tests {
    use super::*;
    use crate::application::{
        HostContext, HostContextInputs, LaunchTarget, PreparedComponentInputs, RestoreInput,
    };
    use crate::application_test_support::*;
    use std::cell::Cell;
    use std::rc::Rc;
    thread_local! { static PREPARES: Cell<usize> = const { Cell::new(0) }; }
    thread_local! { static CAPTURE_SCENARIOS: Cell<usize> = const { Cell::new(0) }; }

    fn edit_capture(driver: &mut FrameHarness, _: &LaunchArgs) -> Result<(), LaunchError> {
        CAPTURE_SCENARIOS.set(CAPTURE_SCENARIOS.get().saturating_add(1));
        driver
            .invoke_bound_action(
                &crate::bound_dispatch::BoundActionAddress {
                    instance_id: 11,
                    address: tilo_actions::ActionAddress::new("probe:shared", "edit"),
                },
                &serde_json::Map::new(),
            )
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
        Ok(())
    }

    fn prepare(
        definition: &crate::application::ApplicationDefinition,
        args: &LaunchArgs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        prepare_target(definition, args, LaunchTarget::New, runner_permissions())
    }
    fn prepare_target(
        definition: &crate::application::ApplicationDefinition,
        args: &LaunchArgs,
        target: LaunchTarget,
        authority: Rc<std::cell::RefCell<crate::permissions::PermissionContext>>,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        PREPARES.set(PREPARES.get().saturating_add(1));
        let root = PathBuf::from(args.value("--fixture-root").expect("explicit test root"));
        let mut owners = crate::record::RecordOwners::new(root.clone());
        let records = owners.open(APP, ROOM).map_err(LaunchError::Record)?;
        let views = Rc::new(
            crate::application_view::InstanceViewStore::new(root, DEVICE, ROOM)
                .map_err(LaunchError::Restore)?,
        );
        let context = HostContext::prepare(
            definition,
            HostContextInputs {
                records,
                views,
                device_id: DEVICE.into(),
                instance_id: 11,
                permissions: authority,
                author: author(),
                action_context: provenance(),
            },
            &target,
        )?;
        Ok(PreparedComponentInputs {
            context,
            target,
            restore: RestoreInput::None,
            runner_inputs: runner_inputs(),
        })
    }
    fn run_metrics_exit_child(case: &str) -> ! {
        let root = PathBuf::from(std::env::var_os("TILO_METRICS_EXIT_TEST_ROOT").unwrap());
        let failing = case.ends_with("failure");
        let spec = LaunchSpec::from_definition(runner_probe_definition())
            .component_host(prepare)
            .allowed_live_options(&["--fixture-root"]);
        let actions = if failing {
            serde_json::json!([{ "click_named": "missing-metrics-target" }])
        } else {
            serde_json::json!([])
        };
        let payload = serde_json::json!({
            "actions": actions,
            "dump": root.join("document.json"),
        });
        let args = vec![
            "app".into(),
            "--headless-drive".into(),
            payload.to_string(),
            "--fixture-root".into(),
            root.join("records").to_string_lossy().into_owned(),
        ];
        // Same collector and guard as run(); libtest owns process arguments,
        // so pass the fixture's CLI arguments to the real dispatch directly.
        tilo_startup_metrics::init();
        let _metrics = tilo_startup_metrics::ScopedDumper;
        tilo_startup_metrics::mark("app-launch-start");
        let returned = run_with_args(&spec, &args);
        panic!("headless dispatch must exit instead of returning: {returned:?}");
    }

    #[test]
    fn headless_exit_writes_startup_metrics_in_subprocesses() {
        const CHILD_CASE: &str = "TILO_METRICS_EXIT_TEST_CASE";
        const CHILD_ROOT: &str = "TILO_METRICS_EXIT_TEST_ROOT";
        if let Ok(case) = std::env::var(CHILD_CASE) {
            run_metrics_exit_child(&case);
        }

        for case in ["prepared-success", "prepared-failure"] {
            let temp = tempfile::tempdir().unwrap();
            let metrics_path = temp.path().join("metrics.json");
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "launch::prepared_launch_tests::headless_exit_writes_startup_metrics_in_subprocesses", "--nocapture"])
                .env(CHILD_CASE, case)
                .env(CHILD_ROOT, temp.path())
                .env("TILO_STARTUP_METRICS_PATH", &metrics_path)
                .env("TILO_PACKAGE_ID", "so.tilo.metrics-fixture")
                .env("XDG_DATA_HOME", temp.path().join("legacy-data"))
                .output()
                .unwrap();
            let expected_code = i32::from(case.ends_with("failure"));
            assert_eq!(
                output.status.code(),
                Some(expected_code),
                "{case}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let bytes = std::fs::read(&metrics_path).unwrap_or_else(|error| {
                panic!("{case} must flush metrics before process::exit: {error}")
            });
            let metrics: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert!(metrics["origin_epoch_ms"].as_u64().unwrap() > 0);
            let events = metrics["events"].as_array().unwrap();
            let phases = [
                "app-launch-start",
                "headless-setup-start",
                "headless-setup-ready",
                "headless-actions-start",
                "headless-actions-done",
                "headless-output-done",
                "headless-durability-start",
                "headless-durability-done",
                "headless-exit",
            ];
            let mut previous = 0;
            for phase in phases {
                let event = events
                    .iter()
                    .find(|event| event["name"] == phase)
                    .unwrap_or_else(|| panic!("{case} omitted actual phase {phase}: {metrics}"));
                let time = event["ns"].as_u64().unwrap();
                assert!(time >= previous, "{case}: phase {phase} is out of order");
                previous = time;
            }
            assert_eq!(
                events
                    .iter()
                    .any(|event| event["name"] == "headless-action-failed"),
                case.ends_with("failure")
            );
            let document: serde_json::Value =
                serde_json::from_slice(&std::fs::read(temp.path().join("document.json")).unwrap())
                    .unwrap();
            assert!(
                document.is_object(),
                "the measured output phase wrote a real document dump"
            );
        }
    }

    fn parsed(spec: &LaunchSpec, values: &[String]) -> LaunchArgs {
        let args = parse_args_extended(
            values,
            spec.allowed_flags,
            spec.allowed_options,
            spec.allowed_live_flags,
            spec.allowed_live_options,
            spec.max_positionals,
        )
        .unwrap();
        validate_spec_args(&args, spec).unwrap();
        args
    }

    fn prepare_presentation(
        definition: &crate::application::ApplicationDefinition,
        args: &LaunchArgs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        let mut prepared = prepare(definition, args)?;
        prepared.runner_inputs = configure_presentation(prepared.runner_inputs, 2.0);
        Ok(prepared)
    }

    #[test]
    fn definition_preparation_resolves_wakes_scale_and_services_once() {
        // Case `definition_final_audit_and_shot_preparation_resolve_queued_component_wakes`.
        {
            for mode in ["audit", "shots"] {
                let temp = tempfile::tempdir().unwrap();
                let spec = LaunchSpec::from_definition(runner_probe_definition())
                    .component_host(prepare)
                    .allowed_live_options(&["--fixture-root"]);
                let mut values = vec![
                    "app".into(),
                    "--fixture-root".into(),
                    temp.path().join("state").to_string_lossy().into_owned(),
                ];
                if mode == "audit" {
                    values.push("--native-audit".into());
                } else {
                    values.extend([
                        "--shots".into(),
                        temp.path().join("shot.png").to_string_lossy().into_owned(),
                    ]);
                }
                let observed = observe_presentation();
                let args = parsed(&spec, &values);
                let (DefinitionDispatch::Audit(mut driver)
                | DefinitionDispatch::Rendered(mut driver)) =
                    dispatch_definition(&spec, &args).unwrap()
                else {
                    panic!("expected prepared output driver");
                };
                let description = |driver: &FrameHarness| {
                    tilo_actions::flatten(&driver.resources())
                        .into_iter()
                        .find(|action| action.action.id == "edit")
                        .unwrap()
                        .action
                        .description
                        .clone()
                        .unwrap()
                };
                assert_eq!(description(&driver), "construction wake");
                let draws = observed.borrow().local_draws.len();
                assert!(draws > 0);
                local_state().writer().set("queued final output".into());
                let _ = driver.native_app_audit();
                assert_eq!(
                    description(&driver),
                    "construction wake",
                    "read-only inspection remains a snapshot"
                );
                assert_eq!(observed.borrow().local_draws.len(), draws);
                if mode == "audit" {
                    observed.borrow_mut().local_resource_reads.clear();
                    let error = finish_component_audit(&mut driver, APP).unwrap_err();
                    assert_eq!(
                        error.to_string(),
                        format!("{APP} does not satisfy the Tilo-native application contract")
                    );
                    let observation = observed.borrow();
                    assert!(!observation.local_resource_reads.is_empty());
                    assert!(
                        observation
                            .local_resource_reads
                            .iter()
                            .all(|value| value == "queued final output"),
                        "final audit read a stale resource snapshot: {:?}",
                        observation.local_resource_reads
                    );
                } else {
                    driver.prepare_component_output();
                    assert_eq!(description(&driver), "queued final output");
                    close_component_capture(&mut driver).unwrap();
                }
                let observation = observed.borrow();
                assert!(
                    observation.local_draws.len() > draws,
                    "final output requires a resolved frame"
                );
                assert_eq!(
                    observation.local_draws.last().unwrap(),
                    "queued final output"
                );
                assert!(!temp.path().join("shot.png").exists());
                assert_eq!(driver.component_close_reports().len(), 1);
            }
        }

        // Case `definition_text_scale_overrides_before_all_runner_layouts`.
        {
            for mode in ["native", "headless", "shots", "matrix", "audit"] {
                for cli_scale in [None, Some("0.75")] {
                    let temp = tempfile::tempdir().unwrap();
                    let spec = LaunchSpec::from_definition(runner_probe_definition())
                        .component_host(prepare_presentation)
                        .allowed_live_options(&["--fixture-root"]);
                    let mut values = vec![
                        "app".into(),
                        "--fixture-root".into(),
                        temp.path().join("state").to_string_lossy().into_owned(),
                    ];
                    match mode {
                        "headless" => values.extend(["--headless-drive".into(), "{}".into()]),
                        "shots" => values.extend([
                            "--shots".into(),
                            temp.path().join("shot.png").to_string_lossy().into_owned(),
                        ]),
                        "matrix" => values.extend([
                            "--responsive-matrix".into(),
                            temp.path().join("matrix").to_string_lossy().into_owned(),
                        ]),
                        "audit" => values.push("--native-audit".into()),
                        _ => {}
                    }
                    if let Some(scale) = cli_scale {
                        values.extend(["--text-scale".into(), scale.into()]);
                    }
                    let args = parsed(&spec, &values);
                    let observed = observe_presentation();
                    let expected_scale = if cli_scale.is_some() { 0.75 } else { 1.5 };
                    match dispatch_definition(&spec, &args).unwrap() {
                        DefinitionDispatch::Native(mut app) => {
                            assert_eq!(app.text_scale(), expected_scale);
                            crate::app::draw_prepared_component_test_frame(&mut app);
                            app.close_components();
                        }
                        DefinitionDispatch::Headless(status) => assert_eq!(status, 0),
                        DefinitionDispatch::Rendered(mut driver)
                        | DefinitionDispatch::Audit(mut driver) => driver.close_components(),
                    }
                    let observation = observed.borrow();
                    assert!(!observation.layouts.is_empty());
                    assert!(!observation.draws.is_empty());
                    for theme in &observation.layouts {
                        assert_presentation(theme, expected_scale, 1.25);
                    }
                    for (theme, renderer_scale) in &observation.draws {
                        assert_presentation(theme, expected_scale, 1.25);
                        assert_eq!(*renderer_scale, expected_scale);
                    }
                    assert!(!observation.motion.is_empty());
                    assert!(observation.motion.iter().all(|value| *value));
                    assert!(!temp.path().join("shot.png").exists());
                    assert!(!temp.path().join("matrix").exists());
                }
            }
        }

        // Case `native_services_are_discovered_only_by_the_selected_native_runner`.
        {
            for enabled in [false, true] {
                for mode in ["native", "headless", "shots", "matrix", "audit"] {
                    let temp = tempfile::tempdir().unwrap();
                    let mut spec = LaunchSpec::from_definition(runner_probe_definition())
                        .component_host(prepare)
                        .allowed_live_options(&["--fixture-root"]);
                    if enabled {
                        spec = spec.native_services();
                    }
                    let mut values = vec![
                        "app".into(),
                        "--fixture-root".into(),
                        temp.path().join("state").to_string_lossy().into_owned(),
                    ];
                    match mode {
                        "headless" => values.extend(["--headless-drive".into(), "{}".into()]),
                        "audit" => values.push("--native-audit".into()),
                        "shots" => values.extend([
                            "--shots".into(),
                            temp.path().join("out.png").to_string_lossy().into_owned(),
                        ]),
                        "matrix" => values.extend([
                            "--responsive-matrix".into(),
                            temp.path().join("matrix").to_string_lossy().into_owned(),
                        ]),
                        _ => {}
                    }
                    let args = parsed(&spec, &values);
                    let before = calls();
                    let native_calls = Cell::new(0);
                    let dispatched = dispatch_definition_with_native(
                        &spec,
                        &args,
                        |component, inputs, services| {
                            native_calls.set(native_calls.get() + 1);
                            assert_eq!(services, enabled);
                            // The real native merge is covered in prepared_runner_tests;
                            // this mode-selection test must never discover ambient services.
                            App::from_component(component, inputs)
                        },
                    )
                    .unwrap();
                    assert_eq!(native_calls.get(), usize::from(mode == "native"));
                    assert_eq!(calls(), before + 1);
                    match dispatched {
                        DefinitionDispatch::Native(mut app) => app.close_components(),
                        DefinitionDispatch::Rendered(mut driver)
                        | DefinitionDispatch::Audit(mut driver) => {
                            close_component_capture(&mut driver).unwrap();
                        }
                        DefinitionDispatch::Headless(status) => assert_eq!(status, 0),
                    }
                }
            }
        }
    }

    #[test]
    fn definition_native_headless_and_render_selection_prepare_once() {
        for mode in ["native", "headless", "shots", "matrix", "audit"] {
            let temp = tempfile::tempdir().unwrap();
            let spec = LaunchSpec::from_definition(runner_probe_definition())
                .component_host(prepare)
                .capture_scenario(edit_capture)
                .allowed_live_options(&["--fixture-root"]);
            let mut values = vec![
                "app".into(),
                "--fixture-root".into(),
                temp.path().join("state").to_string_lossy().into_owned(),
            ];
            match mode {
                "headless" => values.extend(["--headless-drive".into(), "{}".into()]),
                "audit" => values.push("--native-audit".into()),
                "shots" => values.extend([
                    "--shots".into(),
                    temp.path().join("shot.png").to_string_lossy().into_owned(),
                ]),
                "matrix" => values.extend([
                    "--responsive-matrix".into(),
                    temp.path().join("matrix").to_string_lossy().into_owned(),
                ]),
                _ => {}
            }
            let before = calls();
            PREPARES.set(0);
            CAPTURE_SCENARIOS.set(0);
            let args = parsed(&spec, &values);
            let prepared = dispatch_definition(&spec, &args).unwrap();
            assert_eq!(PREPARES.get(), 1);
            assert_eq!(
                CAPTURE_SCENARIOS.get(),
                usize::from(matches!(mode, "shots" | "matrix"))
            );
            assert_eq!(calls(), before.saturating_add(1));
            assert!(matches!(
                (mode, &prepared),
                ("native", DefinitionDispatch::Native(_))
                    | ("headless", DefinitionDispatch::Headless(0))
                    | ("shots" | "matrix", DefinitionDispatch::Rendered(_))
                    | ("audit", DefinitionDispatch::Audit(_))
            ));
            assert!(!temp.path().join("shot.png").exists());
            assert!(!temp.path().join("matrix").exists());
            if let DefinitionDispatch::Rendered(mut driver) = prepared {
                close_component_capture(&mut driver).unwrap();
                drop(driver);
                let root = temp.path().join("state");
                let saved =
                    crate::application_view::InstanceViewStore::new(root.clone(), DEVICE, ROOM)
                        .unwrap()
                        .load(11)
                        .unwrap()
                        .unwrap();
                let checkout = crate::record::RecordCheckout::new(
                    crate::record::RecordId::parse(&saved.record_id).unwrap(),
                    runner_probe_definition().record_kind.clone(),
                );
                let records = crate::record::RecordOwners::new(root)
                    .open(APP, ROOM)
                    .unwrap();
                records.try_open_record(&checkout).unwrap();
                assert_eq!(
                    body(&records, &checkout),
                    "edited",
                    "capture actions persist through the same owner"
                );
            }
        }
    }

    #[test]
    fn definition_failures_keep_typed_errors_and_the_original_retry() {
        // Case `definition_missing_host_fails_before_factory`.
        {
            let before = calls();
            let spec = LaunchSpec::from_definition(runner_probe_definition());
            let args = parsed(&spec, &["app".into()]);
            assert!(matches!(
                dispatch_definition(&spec, &args),
                Err(LaunchError::MissingComponentHost)
            ));
            assert_eq!(calls(), before);
        }

        // Case `capture_scenario_failure_closes_component_and_retains_failed_save_retry`.
        {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("state");
            let spec = LaunchSpec::from_definition(runner_probe_definition())
                .component_host(prepare)
                .capture_scenario(|driver, args| {
                    edit_capture(driver, args)?;
                    let root = PathBuf::from(args.value("--fixture-root").unwrap());
                    std::fs::write(root.join("instance-views"), b"intentional collision").unwrap();
                    Err(LaunchError::Operation(
                        "scenario failed after editing".into(),
                    ))
                })
                .allowed_live_options(&["--fixture-root"]);
            let args = parsed(
                &spec,
                &[
                    "app".into(),
                    "--fixture-root".into(),
                    root.to_string_lossy().into_owned(),
                    "--shots".into(),
                    temp.path().join("never.png").to_string_lossy().into_owned(),
                ],
            );
            let Err(LaunchError::ComponentCapture { source, recovery }) =
                dispatch_definition(&spec, &args)
            else {
                panic!("capture must fail")
            };
            assert_eq!(source.to_string(), "scenario failed after editing");
            let mut failed = recovery
                .into_close_reports()
                .unwrap()
                .into_iter()
                .find(|report| report.view_error.is_some())
                .unwrap();
            assert!(failed.record_error.is_none());
            assert!(failed.view_retry.is_some());
            assert!(!temp.path().join("never.png").exists());
            std::fs::remove_file(root.join("instance-views")).unwrap();
            failed.retry_view().unwrap();
            let saved = crate::application_view::InstanceViewStore::new(root.clone(), DEVICE, ROOM)
                .unwrap()
                .load(11)
                .unwrap()
                .unwrap();
            let checkout = crate::record::RecordCheckout::new(
                crate::record::RecordId::parse(&saved.record_id).unwrap(),
                runner_probe_definition().record_kind.clone(),
            );
            let records = crate::record::RecordOwners::new(root)
                .open(APP, ROOM)
                .unwrap();
            records.try_open_record(&checkout).unwrap();
            assert_eq!(body(&records, &checkout), "edited");
        }
        // Case `definition_denied_and_invalid_preparation_preserve_typed_errors`.
        {
            static DENIED: std::sync::OnceLock<crate::application::ApplicationDefinition> =
                std::sync::OnceLock::new();
            let denied = DENIED.get_or_init(|| {
                let mut value = definition(action_probe_factory);
                value.required.push(denied_scope());
                value
            });
            for (definition, preparer, expect_denied) in [
                (denied, denied_preparer as ComponentHostPreparer, true),
                (
                    runner_probe_definition(),
                    invalid_preparer as ComponentHostPreparer,
                    false,
                ),
            ] {
                let temp = tempfile::tempdir().unwrap();
                let spec = LaunchSpec::from_definition(definition)
                    .component_host(preparer)
                    .allowed_live_options(&["--fixture-root"]);
                for mode in ["native", "headless"] {
                    let mut values = vec![
                        "app".into(),
                        "--fixture-root".into(),
                        temp.path().join(mode).to_string_lossy().into_owned(),
                    ];
                    if mode == "headless" {
                        values.extend(["--headless-drive".into(), "{}".into()]);
                    }
                    let args = parsed(&spec, &values);
                    let before = calls();
                    let result = dispatch_definition(&spec, &args);
                    assert_eq!(
                        matches!(result, Err(LaunchError::CapabilityDenied(_))),
                        expect_denied
                    );
                    assert!(expect_denied || matches!(result, Err(LaunchError::BindingMismatch)));
                    assert_eq!(calls(), before);
                }
            }
        }

        // Case `definition_headless_state_open_failure_is_durability_exit_with_original_bytes`.
        {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("state");
            let path = app_state::DocStore::room_record_dir_at(
                &root,
                APP,
                ROOM,
                &corrupt_checkout().id.storage_key(),
            )
            .unwrap()
            .join(app_state::DOCUMENT_FILE);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let original = b"corrupt explicit existing record";
            std::fs::write(&path, original).unwrap();
            let spec = LaunchSpec::from_definition(runner_probe_definition())
                .component_host(existing_preparer)
                .allowed_live_options(&["--fixture-root"]);
            let args = parsed(
                &spec,
                &[
                    "app".into(),
                    "--fixture-root".into(),
                    root.to_string_lossy().into_owned(),
                    "--headless-drive".into(),
                    "{}".into(),
                ],
            );
            let before = calls();
            assert!(matches!(
                dispatch_definition(&spec, &args),
                Ok(DefinitionDispatch::Headless(3))
            ));
            assert_eq!(calls(), before);
            assert_eq!(std::fs::read(&path).unwrap(), original);
            let native = parsed(
                &spec,
                &[
                    "app".into(),
                    "--fixture-root".into(),
                    root.to_string_lossy().into_owned(),
                ],
            );
            assert!(matches!(
                dispatch_definition(&spec, &native),
                Err(LaunchError::Record(_))
            ));
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }

        // Case `definition_audit_close_failure_is_operational_and_retains_original_retry`.
        {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("state");
            let spec = LaunchSpec::from_definition(runner_probe_definition())
                .component_host(prepare)
                .allowed_live_options(&["--fixture-root"]);
            let args = parsed(
                &spec,
                &[
                    "app".into(),
                    "--fixture-root".into(),
                    root.to_string_lossy().into_owned(),
                    "--native-audit".into(),
                ],
            );
            let DefinitionDispatch::Audit(mut driver) = dispatch_definition(&spec, &args).unwrap()
            else {
                panic!("expected prepared audit driver");
            };
            let path = root.join("instance-views/44/41/11.postcard");
            std::fs::create_dir_all(&path).unwrap();
            let writer = local_state().writer();
            let error = finish_component_audit(&mut driver, APP).unwrap_err();
            assert_eq!(error.to_string(), "component capture close failed");
            assert_eq!(error.exit_code(), 1);
            assert!(!writer.try_set("stale audit result".into()));
            let mut reports = driver.take_component_close_reports();
            assert_eq!(reports.len(), 1);
            assert!(reports[0].view_error.is_some());
            let saved = reports[0].recovery.clone().unwrap();
            assert_eq!(saved.app_id, APP);
            assert_eq!(saved.instance_id, 11);
            assert_eq!(saved.device_id, DEVICE);
            assert_eq!(saved.workspace_id, ROOM);
            drop(driver);
            std::fs::remove_dir(&path).unwrap();
            reports[0].retry_view().unwrap();
            assert!(reports[0].view_retry.is_none());
            assert_eq!(
                std::fs::read(path).unwrap(),
                postcard::to_allocvec(&saved).unwrap()
            );
        }
    }

    fn invalid_preparer(
        definition: &crate::application::ApplicationDefinition,
        args: &LaunchArgs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        let mut prepared = prepare(definition, args)?;
        prepared.target = existing(&crate::record::RecordCheckout::new(
            crate::record::RecordId::minted(),
            definition.record_kind.clone(),
        ));
        Ok(prepared)
    }
    fn denied_preparer(
        definition: &crate::application::ApplicationDefinition,
        args: &LaunchArgs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        prepare_target(definition, args, LaunchTarget::New, permissions())
    }
    fn corrupt_checkout() -> crate::record::RecordCheckout {
        crate::record::RecordCheckout::new(
            crate::record::RecordId::for_file("/explicit-test-record"),
            crate::record::RecordKind::new("probe"),
        )
    }
    fn existing_preparer(
        definition: &crate::application::ApplicationDefinition,
        args: &LaunchArgs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        prepare_target(
            definition,
            args,
            existing(&corrupt_checkout()),
            runner_permissions(),
        )
    }
}
