//! Standard command-line lifecycle for Tilo applications.
//!
//! Ordinary apps provide their live and screenshot roots; this module owns
//! common argument validation, persistence, headless driving, offscreen
//! rendering, logging, and live startup. Apps with different
//! lifecycles can continue to use the lower-level APIs directly.

use std::fmt;
use std::path::PathBuf;

use crate::headless::{self, FrameHarness};
use crate::widget::Widget;
use crate::{App, State, Theme};

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

type RootFactory = fn() -> Box<dyn Widget>;
type ArgsRootFactory = fn(&LaunchArgs) -> Result<Box<dyn Widget>, LaunchError>;
type LegacyPreparer = fn(&LaunchArgs) -> Result<LegacyLaunchInputs, LaunchError>;

/// Transitional inputs for existing state/widget apps. Canonical applications
/// should use `ApplicationDefinition` + `PreparedComponentInputs` instead. This
/// bridge preserves the SAME state, writer, record policy and registry in native
/// and headless execution; it is not a second application framework.
pub struct LegacyLaunchInputs {
    pub(crate) state: State,
    pub(crate) root: Box<dyn Widget>,
    pub(crate) autosave: crate::persist::Autosave,
    pub(crate) records: LegacyRecordPolicy,
    pub(crate) registry: tilo_actions::ActionRegistry,
    pub(crate) persistence: LegacyPersistencePolicy,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegacyPersistencePolicy {
    Host,
    WidgetOwned,
}

pub(crate) enum LegacyRecordPolicy {
    None,
    Explicit {
        records: crate::record::RecordSet,
        index: automerge::AutoCommit,
    },
    /// Compatibility for legacy specs without an explicit preparer. Captured
    /// BEFORE the opener runs; never re-resolve a custom opener's namespace.
    AutomaticHeadless {
        records: crate::record::RecordSet,
        index: automerge::AutoCommit,
        expected_index: std::path::PathBuf,
    },
}

impl LegacyLaunchInputs {
    pub fn new(state: State, root: Box<dyn Widget>, autosave: crate::persist::Autosave) -> Self {
        Self {
            state,
            root,
            autosave,
            records: LegacyRecordPolicy::None,
            registry: tilo_actions::ActionRegistry::new(),
            persistence: LegacyPersistencePolicy::Host,
        }
    }

    /// Supply the exact record owner and index used by the native app. An
    /// in-memory set remains in-memory in headless mode as well.
    #[must_use]
    pub fn with_records(
        mut self,
        records: crate::record::RecordSet,
        index: automerge::AutoCommit,
    ) -> Self {
        self.records = LegacyRecordPolicy::Explicit { records, index };
        self
    }

    #[must_use]
    pub fn with_registry(mut self, registry: tilo_actions::ActionRegistry) -> Self {
        self.registry = registry;
        self
    }

    /// Transitional ownership for a root with its own durable writer. Requires
    /// a disabled host writer, no record set, and an implemented checked
    /// `Widget::try_flush_owned_persistence` hook. Ordinary disabled writers
    /// remain failures; this never opens a substitute store.
    #[must_use]
    pub fn with_widget_owned_persistence(mut self) -> Self {
        self.persistence = LegacyPersistencePolicy::WidgetOwned;
        self
    }

    pub(crate) fn validate_persistence_ownership(&self) -> Result<(), String> {
        if self.persistence == LegacyPersistencePolicy::WidgetOwned
            && (self.autosave.is_enabled() || !matches!(self.records, LegacyRecordPolicy::None))
        {
            return Err("widget-owned persistence cannot also use a host or record writer".into());
        }
        Ok(())
    }
}
type ShotFactory = fn(&LaunchArgs) -> Result<Box<dyn Widget>, LaunchError>;
type ShotHandler = fn(&LaunchArgs) -> Result<(), LaunchError>;
type ViewportFactory = fn(&LaunchArgs) -> (u32, u32);
type ConfigureApp = fn(&mut App);
type OpenState = fn(&'static str) -> (State, crate::persist::Autosave);
type OpenStateWithArgs = fn(&'static str, &LaunchArgs) -> (State, crate::persist::Autosave);
type LiveHandler = fn(&LaunchArgs) -> Result<(), LaunchError>;
type CommandHandler = fn(&LaunchArgs) -> Result<bool, LaunchError>;

/// Parsed common launch arguments. Extra flags are retained for an app's
/// screenshot factory after being explicitly allow-listed by [`LaunchSpec`].
#[derive(Clone, Debug)]
pub struct LaunchArgs {
    pub shots: Option<PathBuf>,
    pub responsive_matrix: Option<PathBuf>,
    pub viewport: Option<(u32, u32)>,
    pub scenario: Option<String>,
    pub headless: Option<headless::Spec>,
    /// Lay out the production root and print its Tilo-native certification.
    pub native_audit: bool,
    pub pseudo_locale: bool,
    pub text_scale: Option<f32>,
    extras: Vec<String>,
    values: Vec<(String, String)>,
    positionals: Vec<String>,
}

impl LaunchArgs {
    pub fn has(&self, flag: &str) -> bool {
        self.extras.iter().any(|arg| arg == flag)
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
}

/// Configuration for an app using the standard lifecycle.
pub struct LaunchSpec {
    history_definition: Option<&'static crate::history_mode::HistoryDefinition>,
    history_app_global_storage: bool,
    definition: Option<&'static crate::application::ApplicationDefinition>,
    component_host: Option<ComponentHostPreparer>,
    component_capture_scenario: Option<ComponentCaptureScenario>,
    component_native_services: bool,
    legacy_setup_override: bool,
    app_id: &'static str,
    live_root: Option<RootFactory>,
    live_root_with_args: Option<ArgsRootFactory>,
    legacy_preparer: Option<LegacyPreparer>,
    /// `None` reuses the live root, so ordinary apps do not register the same
    /// factory twice just to render a screenshot.
    shot_root: Option<ShotFactory>,
    shot_handler: Option<ShotHandler>,
    shot_viewport: ViewportFactory,
    configure_app: Option<ConfigureApp>,
    live_handler: Option<LiveHandler>,
    command_handler: Option<CommandHandler>,
    headless_enabled: bool,
    runtime_attachment: fn(&LaunchArgs) -> bool,
    open_state: OpenState,
    open_state_with_args: Option<OpenStateWithArgs>,
    allowed_flags: &'static [&'static str],
    allowed_options: &'static [&'static str],
    allowed_live_flags: &'static [&'static str],
    allowed_live_options: &'static [&'static str],
    max_positionals: usize,
    scenarios: &'static [&'static str],
}

impl LaunchSpec {
    /// Managed runners require an explicit History policy, including external
    /// reasons. Canonical components retain their actual record custody.
    #[must_use]
    pub fn history(mut self, definition: &'static crate::history_mode::HistoryDefinition) -> Self {
        self.history_definition = Some(definition);
        self
    }
    /// Check the explicit declaration without constructing a widget or opening storage.
    /// Production runners will call this gate once all installed adapters are migrated.
    /// Detached test construction does not require a declaration.
    ///
    /// # Errors
    /// Reports a missing declaration or one belonging to a different app.
    pub fn validate_history_declaration(
        &self,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        let history = self
            .history_definition
            .ok_or(crate::history_mode::HistoryRegistrationError::Missing)?;
        if history.app_id != self.app_id {
            return Err(crate::history_mode::HistoryRegistrationError::ForeignSource);
        }
        Ok(())
    }

    /// Exercise the native builder with deterministic in-process services.
    /// The spec's real root, state opener, configure hook, and History
    /// installation path are retained; only native window/runtime discovery is
    /// omitted. Downstream app tests enable this with the `test-support`
    /// feature and provide an in-memory state opener on the spec.
    #[cfg(any(test, feature = "test-support"))]
    pub fn build_controlled_app_for_test(&self) -> Result<App, LaunchError> {
        let args = parse_args_extended(
            &["controlled-launch".to_string()],
            self.allowed_flags,
            self.allowed_options,
            self.allowed_live_flags,
            self.allowed_live_options,
            self.max_positionals,
        )?;
        validate_spec_args(&args, self)?;
        build_native_app_with_constructor(
            self,
            &args,
            App::from_scoped_parts_with_controlled_inputs,
        )
    }

    /// Explicit provenance for a custom app-global preferences/state opener.
    /// User-file documents use a real owned-record adapter instead.
    #[must_use]
    pub fn open_app_global_state(mut self, open: OpenState) -> Self {
        self = self.open_state(open);
        self.history_app_global_storage = true;
        self
    }

    /// Explicit app-global state provenance for an argument-aware opener.
    /// User-file documents require an owned-record adapter instead.
    #[must_use]
    pub fn open_app_global_state_with_args(mut self, open: OpenStateWithArgs) -> Self {
        self = self.open_state_with_args(open);
        self.history_app_global_storage = true;
        self
    }

    /// Select automatic runtime attachment for native launches. The default is
    /// enabled. Disabling bootstrap does not revoke inherited service handles;
    /// privacy-sensitive apps must also construct a local-only application.
    #[must_use]
    pub fn runtime_attachment(mut self, policy: fn(&LaunchArgs) -> bool) -> Self {
        self.runtime_attachment = policy;
        self
    }

    /// Adapt one common application definition. A runner must install an explicit host.
    pub fn from_definition(definition: &'static crate::application::ApplicationDefinition) -> Self {
        let mut spec = Self::new_with_args(definition.app_id, |_| {
            Err(LaunchError::MissingComponentHost)
        });
        spec.definition = Some(definition);
        spec.live_root_with_args = None;
        spec
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
        if self.legacy_setup_override {
            return Err(LaunchError::IncompatibleComponentSetup);
        }
        let definition = self
            .definition
            .ok_or(LaunchError::IncompatibleComponentSetup)?;
        let mut component = definition.instantiate(context, target, restore)?;
        if let Some(history) = self.history_definition
            && let Err(error) = component.register_document_history(history)
        {
            return Err(component.setup_error(LaunchError::Operation(error.to_string())));
        }
        Ok(component)
    }

    pub fn new(app_id: &'static str, live_root: RootFactory) -> Self {
        Self {
            history_definition: None,
            history_app_global_storage: true,
            definition: None,
            component_host: None,
            component_capture_scenario: None,
            component_native_services: false,
            legacy_setup_override: false,
            app_id,
            live_root: Some(live_root),
            live_root_with_args: None,
            legacy_preparer: None,
            shot_root: None,
            shot_handler: None,
            shot_viewport: |_| DEFAULT_VIEWPORT,
            configure_app: None,
            live_handler: None,
            command_handler: None,
            headless_enabled: true,
            runtime_attachment: |_| true,
            open_state: crate::persist::open,
            open_state_with_args: None,
            allowed_flags: &[],
            allowed_options: &[],
            allowed_live_flags: &[],
            allowed_live_options: &[],
            max_positionals: 0,
            scenarios: &[],
        }
    }

    /// Construct a specification whose live/headless root is fallible and
    /// depends on validated arguments.
    pub fn new_with_args(app_id: &'static str, live_root: ArgsRootFactory) -> Self {
        Self {
            history_definition: None,
            history_app_global_storage: true,
            definition: None,
            component_host: None,
            component_capture_scenario: None,
            component_native_services: false,
            legacy_setup_override: false,
            app_id,
            live_root: None,
            live_root_with_args: Some(live_root),
            legacy_preparer: None,
            shot_root: None,
            shot_handler: None,
            shot_viewport: |_| DEFAULT_VIEWPORT,
            configure_app: None,
            live_handler: None,
            command_handler: None,
            headless_enabled: true,
            runtime_attachment: |_| true,
            open_state: crate::persist::open,
            open_state_with_args: None,
            allowed_flags: &[],
            allowed_options: &[],
            allowed_live_flags: &[],
            allowed_live_options: &[],
            max_positionals: 0,
            scenarios: &[],
        }
    }

    /// Build the live/headless root from validated arguments. The callback may
    /// fail before a window is opened, which keeps fallible document/session
    /// initialization inside normal CLI error reporting.
    #[must_use]
    pub fn live_root_with_args(mut self, factory: ArgsRootFactory) -> Self {
        self.legacy_setup_override = true;
        self.live_root_with_args = Some(factory);
        self
    }

    /// Fallible shared preparation for legacy apps needing a coordinated
    /// state/root, explicit record policy, or action registry. Runs under the
    /// host's wake scope before either runner is constructed. Prepared component
    /// definitions reject this legacy override. It replaces `open_state/root`
    /// callbacks, which are not invoked a second time.
    #[must_use]
    pub fn prepare_legacy_with_args(mut self, prepare: LegacyPreparer) -> Self {
        self.legacy_setup_override = true;
        self.legacy_preparer = Some(prepare);
        self
    }

    #[must_use]
    pub fn shot_root(mut self, factory: ShotFactory) -> Self {
        self.legacy_setup_override = true;
        self.shot_root = Some(factory);
        self
    }

    /// Delegate screenshot execution to an app-specific pipeline while the
    /// standard runner still owns parsing, mode validation, logging, headless
    /// driving, persistence and live startup.
    #[must_use]
    pub fn shot_handler(mut self, handler: ShotHandler) -> Self {
        self.legacy_setup_override = true;
        self.shot_handler = Some(handler);
        self
    }

    /// Scenario-specific default viewport. An explicit `--viewport` always
    /// wins over this callback.
    #[must_use]
    pub fn shot_viewport(mut self, factory: ViewportFactory) -> Self {
        self.shot_viewport = factory;
        self
    }

    #[must_use]
    pub fn configure_app(mut self, configure: ConfigureApp) -> Self {
        self.legacy_setup_override = true;
        self.configure_app = Some(configure);
        self
    }

    /// Delegate windowed startup after standard argument parsing and after the
    /// screenshot/headless modes have been dispatched. This is for apps whose
    /// live [`App`] needs non-capturable setup such as an event-loop proxy or a
    /// coordinated file-backed state/root pair. Prefer [`Self::configure_app`]
    /// when ordinary `App::run` is sufficient.
    #[must_use]
    pub fn live_handler(mut self, handler: LiveHandler) -> Self {
        self.legacy_setup_override = true;
        self.live_handler = Some(handler);
        self
    }

    /// Handle an app-specific, non-windowed command after argument validation.
    /// Return `true` when the command was handled and launch should stop.
    ///
    /// This keeps developer utilities such as fixture generation and benchmark
    /// drivers in the standard parser without making them part of the normal
    /// screenshot or live-window lifecycle.
    #[must_use]
    pub fn command_handler(mut self, handler: CommandHandler) -> Self {
        self.legacy_setup_override = true;
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

    /// Override how the live app opens its persisted state. This is primarily
    /// for apps importing an older on-disk document through
    /// [`crate::persist::open_migrating`]; the runner still owns autosave and
    /// the live event loop.
    #[must_use]
    pub fn open_state(mut self, open: OpenState) -> Self {
        self.history_app_global_storage = false;
        self.legacy_setup_override = true;
        self.open_state = open;
        self.open_state_with_args = None;
        self
    }

    /// Open live state using validated launch arguments. Useful when opening a
    /// document must seed the persisted CRDT before the root's first sync.
    #[must_use]
    pub fn open_state_with_args(mut self, open: OpenStateWithArgs) -> Self {
        self.history_app_global_storage = false;
        self.legacy_setup_override = true;
        self.open_state_with_args = Some(open);
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

    fn live_root(&self, args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
        if let Some(factory) = self.live_root_with_args {
            return factory(args);
        }
        self.live_root.map_or_else(
            || {
                Err(LaunchError::Operation(
                    "launch specification has no live root".into(),
                ))
            },
            |factory| Ok(factory()),
        )
    }

    fn screenshot_root(&self, args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
        self.shot_root
            .map_or_else(|| self.live_root(args), |factory| factory(args))
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
    RecoveryRequired,
    ComponentSetup {
        source: Box<LaunchError>,
        record: crate::record::RecordCheckout,
        records: crate::record::RecordStore,
        reports: Vec<crate::application::ComponentCloseReport>,
    },
    ComponentCapture {
        source: Box<LaunchError>,
        reports: Vec<crate::application::ComponentCloseReport>,
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
            Self::RecoveryRequired => f.write_str("legacy state requires explicit recovery"),
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
                    "choose only one of --shots, --responsive-matrix, --headless-drive, or --native-audit"
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
                reports,
                ..
            } => f
                .debug_struct("ComponentSetup")
                .field("source", source)
                .field("record", record)
                .field("close_report_count", &reports.len())
                .finish_non_exhaustive(),
            Self::ComponentCapture { source, reports } => f
                .debug_struct("ComponentCapture")
                .field("source", source)
                .field("close_report_count", &reports.len())
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
            Self::RecoveryRequired => f.write_str("RecoveryRequired"),
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
                | Self::RecoveryRequired
        ) {
            1
        } else {
            2
        }
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
pub fn run(spec: &LaunchSpec) -> Result<(), LaunchError> {
    tilo_startup_metrics::init();
    let _startup_metrics = tilo_startup_metrics::ScopedDumper;
    tilo_startup_metrics::mark("app-launch-start");
    let _ = tracing_subscriber::fmt::try_init();
    let args: Vec<String> = std::env::args().collect();
    run_with_args(spec, &args)
}

fn run_with_args(spec: &LaunchSpec, raw_args: &[String]) -> Result<(), LaunchError> {
    run_with_args_with_attachment(spec, raw_args, runtime_portal::native::ensure_attached)
}

#[allow(
    clippy::exit,
    reason = "the --headless-drive path reports the harness's pass/fail \
              status as this process's exit code, exactly like --shots \
              reports success by returning Ok — there is no caller to \
              hand a status back to"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one ordered dispatch preserves legacy and prepared mode delivery"
)]
fn run_with_args_with_attachment(
    spec: &LaunchSpec,
    raw_args: &[String],
    attach: impl FnOnce(&str) -> std::io::Result<()>,
) -> Result<(), LaunchError> {
    let _detached = crate::wake::WakeScope::enter_detached();
    let args = parse_args_extended(
        raw_args,
        spec.allowed_flags,
        spec.allowed_options,
        spec.allowed_live_flags,
        spec.allowed_live_options,
        spec.max_positionals,
    )?;
    validate_spec_args(&args, spec)?;
    crate::i18n::I18nManager::set_pseudo_locale(if args.pseudo_locale {
        crate::i18n::PseudoLocale::Expanded
    } else {
        crate::i18n::PseudoLocale::Off
    });

    if spec.definition.is_some() {
        if args.headless.is_none()
            && args.shots.is_none()
            && args.responsive_matrix.is_none()
            && !args.native_audit
            && (spec.runtime_attachment)(&args)
        {
            attach(spec.app_id).map_err(|error| {
                LaunchError::Operation(format!("native runtime attachment failed: {error}"))
            })?;
        }
        #[cfg(windows)]
        let renderer = if args.shots.is_none()
            && args.responsive_matrix.is_none()
            && args.headless.is_none()
            && !args.native_audit
        {
            crate::app::prepare_native_renderer()
        } else {
            None
        };
        match dispatch_definition(spec, &args)? {
            DefinitionDispatch::Native(app) => {
                #[cfg(windows)]
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
        return Ok(());
    }

    // An automatic host exec resumes the already-selected live branch. Command
    // handlers may have side effects even when returning false; run them once.
    let resumed_live = std::env::var_os("QOS_RUNTIME_RESUME_LIVE").as_deref()
        == Some(std::ffi::OsStr::new(spec.app_id))
        && runtime_portal::service_fd("collab").is_some()
        && runtime_portal::service_fd("identity").is_some();
    if !resumed_live
        && let Some(handler) = spec.command_handler
        && handler(&args)?
    {
        return Ok(());
    }

    if let Some(out) = &args.shots {
        if let Some(handler) = spec.shot_handler {
            handler(&args)?;
            return Ok(());
        }
        let root = spec.screenshot_root(&args)?;
        let viewport = args.viewport.unwrap_or_else(|| (spec.shot_viewport)(&args));
        let harness = build_history_capture(spec, &args, root, viewport)?;
        harness.render_png(out);
        println!("{}: wrote shot to {}", spec.app_id, out.display());
        return Ok(());
    }

    if let Some(out_dir) = &args.responsive_matrix {
        std::fs::create_dir_all(out_dir).map_err(|error| {
            LaunchError::Operation(format!(
                "could not create responsive-matrix directory {}: {error}",
                out_dir.display()
            ))
        })?;
        for (name, viewport) in RESPONSIVE_VIEWPORTS {
            let out = out_dir.join(format!("{}-{name}.png", spec.app_id));
            let mut matrix_args = args.clone();
            matrix_args.responsive_matrix = None;
            matrix_args.shots = Some(out.clone());
            matrix_args.viewport = Some(viewport);
            if let Some(handler) = spec.shot_handler {
                handler(&matrix_args)?;
            } else {
                let root = spec.screenshot_root(&matrix_args)?;
                let harness = build_history_capture(spec, &matrix_args, root, viewport)?;
                harness.render_png(&out);
            }
            println!(
                "{}: wrote {name} responsive shot to {}",
                spec.app_id,
                out.display()
            );
        }
        return Ok(());
    }

    if let Some(headless) = &args.headless {
        let status = dispatch_legacy_headless(spec, &args, headless)?;
        // process::exit skips the ScopedDumper in run().
        tilo_startup_metrics::mark("headless-exit");
        tilo_startup_metrics::dump();
        std::process::exit(status);
    }

    if args.native_audit {
        let viewport = args.viewport.unwrap_or(DEFAULT_VIEWPORT);
        let prepared = prepare_legacy_launch(spec, &args)?;
        let _entry = prepared.scope.enter();
        // Install explicit record identity exactly as the native runner does,
        // but auditing is not a durability run or a request to flush a writer.
        let history = legacy_history_registration(spec, &prepared.inputs)?;
        let (mut harness, _autosave) =
            headless::build_audit_driver(prepared.inputs, viewport, shot_theme(&args));
        if let Some(history) = history {
            harness
                .install_history(history)
                .map_err(|error| LaunchError::Operation(error.to_string()))?;
        }
        let audit = harness.native_app_audit();
        let report = serde_json::to_string_pretty(&audit).map_err(|error| {
            LaunchError::Operation(format!("could not serialize native audit: {error}"))
        })?;
        println!("{report}");
        if !audit.is_certified() {
            return Err(LaunchError::Operation(format!(
                "{} does not satisfy the Tilo-native application contract",
                spec.app_id
            )));
        }
        return Ok(());
    }

    if (spec.runtime_attachment)(&args) {
        attach(spec.app_id).map_err(|error| {
            LaunchError::Operation(format!("native runtime attachment failed: {error}"))
        })?;
    }
    if let Some(handler) = spec.live_handler {
        return handler(&args);
    }

    run_native_app(spec, &args)
}

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
            reports: driver.component_close_reports().to_vec(),
        });
    }
    Ok(())
}

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

enum DefinitionDispatch {
    Native(Box<App>),
    Headless(i32),
    Rendered(Box<FrameHarness>),
    Audit(Box<FrameHarness>),
}

/// Construction/selection is testable without a process exit, native loop, or GPU.
fn dispatch_definition(
    spec: &LaunchSpec,
    args: &LaunchArgs,
) -> Result<DefinitionDispatch, LaunchError> {
    dispatch_definition_with_native(spec, args, |component, inputs, native_services| {
        if native_services {
            App::from_component_with_native_services(component, inputs)
        } else {
            App::from_component(component, inputs)
        }
    })
}

fn dispatch_definition_with_native(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    native: impl FnOnce(
        crate::application::AppComponent,
        crate::application::ComponentRunnerInputs,
        bool,
    ) -> Result<App, LaunchError>,
) -> Result<DefinitionDispatch, LaunchError> {
    validate_spec_args(args, spec)?;
    if spec.legacy_setup_override {
        return Err(LaunchError::IncompatibleComponentSetup);
    }
    let definition = spec
        .definition
        .ok_or(LaunchError::IncompatibleComponentSetup)?;
    let prepare = spec
        .component_host
        .ok_or(LaunchError::MissingComponentHost)?;
    let mut prepared = match prepare(definition, args) {
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
    let component =
        spec.prepare_component(args, prepared.context, prepared.target, prepared.restore)?;
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
        if !args.native_audit
            && let Some(scenario) = spec.component_capture_scenario
            && let Err(source) = scenario(&mut driver, args)
        {
            driver.close_components();
            return Err(LaunchError::ComponentCapture {
                source: Box::new(source),
                reports: driver.take_component_close_reports(),
            });
        }
        driver.enable_document_history_chrome();
        return Ok(if args.native_audit {
            DefinitionDispatch::Audit(driver)
        } else {
            DefinitionDispatch::Rendered(driver)
        });
    }
    let app = native(
        component,
        prepared.runner_inputs,
        spec.component_native_services,
    )?;
    Ok(DefinitionDispatch::Native(Box::new(app)))
}

fn build_history_capture(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    root: Box<dyn Widget>,
    viewport: (u32, u32),
) -> Result<FrameHarness, LaunchError> {
    let registration = root
        .history_registration()
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    let mut harness = FrameHarness::new_with_theme(State::new(), root, viewport, shot_theme(args));
    if let Some(registration) = registration {
        if registration.definition.app_id != spec.app_id {
            return Err(LaunchError::Operation(
                "History source belongs to another application".into(),
            ));
        }
        harness
            .install_history(registration)
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
    } else if let Some(history) = spec.history_definition {
        // Capture owns a fresh in-memory State, truthfully scoped to this
        // harness lifetime. It never aliases a native runner's document.
        let registration = crate::history_mode::HistoryRegistration::installed_app_state(
            history,
            None,
            uuid::Uuid::new_v4(),
        )
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
        harness
            .install_history(registration)
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
    }
    Ok(harness)
}

struct PreparedLegacyLaunch {
    scope: crate::wake::WakeScope,
    inputs: LegacyLaunchInputs,
}

fn prepare_legacy_launch(
    spec: &LaunchSpec,
    args: &LaunchArgs,
) -> Result<PreparedLegacyLaunch, LaunchError> {
    let scope = crate::wake::WakeScope::new();
    let _entry = scope.enter();
    let inputs = if let Some(prepare) = spec.legacy_preparer {
        prepare(args)?
    } else {
        // Capture legacy routing before custom state/root callbacks. Unsupported
        // custom namespaces are refused by headless, never silently redirected.
        let records = crate::record::RecordSet::open(spec.app_id);
        let expected_index =
            app_state::DocStore::legacy_package_document_path(records.package_id())
                .map_err(|error| LaunchError::Operation(error.to_string()))?;
        let (state, autosave) = spec.open_state_with_args.map_or_else(
            || (spec.open_state)(spec.app_id),
            |open| open(spec.app_id, args),
        );
        let index = state.doc.clone();
        let root = if args.headless.is_some() && args.scenario.is_some() {
            spec.screenshot_root(args)?
        } else {
            spec.live_root(args)?
        };
        let mut inputs = LegacyLaunchInputs::new(state, root, autosave);
        inputs.records = LegacyRecordPolicy::AutomaticHeadless {
            records,
            index,
            expected_index,
        };
        inputs
    };
    inputs
        .validate_persistence_ownership()
        .map_err(LaunchError::Operation)?;
    Ok(PreparedLegacyLaunch { scope, inputs })
}

/// Exact production branch, testable without `process::exit` or a native window.
fn dispatch_legacy_headless(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    headless: &headless::Spec,
) -> Result<i32, LaunchError> {
    validate_spec_args(args, spec)?;
    tilo_startup_metrics::mark("headless-setup-start");
    let prepared = prepare_legacy_launch(spec, args).inspect_err(|_| {
        tilo_startup_metrics::mark("headless-setup-failed");
    })?;
    let _entry = prepared.scope.enter();
    let history = legacy_history_registration(spec, &prepared.inputs)?;
    Ok(headless::run_initialized_with_history(
        headless,
        prepared.inputs,
        shot_theme(args),
        history,
    ))
}

fn legacy_history_registration(
    spec: &LaunchSpec,
    inputs: &LegacyLaunchInputs,
) -> Result<Option<crate::history_mode::HistoryRegistration>, LaunchError> {
    if let Some(registration) = inputs
        .root
        .history_registration()
        .map_err(|error| LaunchError::Operation(error.to_string()))?
    {
        if registration.definition.app_id != spec.app_id {
            return Err(LaunchError::Operation(
                "History source belongs to another application".into(),
            ));
        }
        return Ok(Some(registration));
    }
    let Some(history) = spec.history_definition else {
        return Ok(None);
    };
    if !spec.history_app_global_storage || inputs.persistence != LegacyPersistencePolicy::Host {
        return Err(LaunchError::Operation(
            "History requires explicit storage provenance for a custom opener".into(),
        ));
    }
    crate::history_mode::HistoryRegistration::installed_app_state(
        history,
        Some(&inputs.autosave),
        uuid::Uuid::new_v4(),
    )
    .map(Some)
    .map_err(|error| LaunchError::Operation(error.to_string()))
}

fn build_native_app(spec: &LaunchSpec, args: &LaunchArgs) -> Result<App, LaunchError> {
    build_native_app_with_constructor(spec, args, App::from_scoped_parts)
}

fn build_native_app_with_constructor(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    construct: impl FnOnce(crate::wake::WakeScope, State, Box<dyn Widget>) -> App,
) -> Result<App, LaunchError> {
    let prepared = prepare_legacy_launch(spec, args)?;
    let LegacyLaunchInputs {
        state,
        root,
        autosave,
        records,
        registry,
        persistence,
    } = prepared.inputs;
    let root_history = root
        .history_registration()
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    let mut app = construct(prepared.scope, state, root)
        .with_autosave(autosave)
        .with_legacy_persistence(persistence);
    if let LegacyRecordPolicy::Explicit { records, index } = records {
        app = app.with_records(records, index);
    }
    *app.action_registry_mut() = registry;
    let installed_storage = app.history_storage_generation();
    if let Some(text_scale) = args.text_scale {
        app.set_text_scale(text_scale);
    }
    if let Some(configure) = spec.configure_app {
        let _configure_entry = app.enter_wake_scope();
        configure(&mut app);
    }
    if let Some(registration) = root_history {
        if registration.definition.app_id != spec.app_id {
            return Err(LaunchError::Operation(
                "History source belongs to another application".into(),
            ));
        }
        app.install_history(registration)
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
    } else if let Some(history) = spec.history_definition {
        if history.app_id != spec.app_id
            || !spec.history_app_global_storage
            || app.history_storage_generation() != installed_storage
        {
            return Err(LaunchError::Operation(
                "History requires the final installed document's explicit storage provenance"
                    .into(),
            ));
        }
        app.install_app_state_history(history)
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
    }
    Ok(app)
}

fn run_native_app(spec: &LaunchSpec, args: &LaunchArgs) -> Result<(), LaunchError> {
    crate::host::native::install_default_if_unset(Some(spec.app_id));
    #[cfg(windows)]
    let renderer = crate::app::prepare_native_renderer();
    let app = build_native_app(spec, args)?;
    #[cfg(windows)]
    let app = app.with_startup_renderer(renderer);
    app.run();
    Ok(())
}

fn validate_spec_args(args: &LaunchArgs, spec: &LaunchSpec) -> Result<(), LaunchError> {
    if spec
        .history_definition
        .is_some_and(|history| history.app_id != spec.app_id)
    {
        return Err(LaunchError::Operation(
            "History belongs to a different installed application".into(),
        ));
    }
    if spec.legacy_preparer.is_some() && spec.live_handler.is_some() {
        return Err(LaunchError::Operation(
            "prepare_legacy_with_args and live_handler cannot be combined".into(),
        ));
    }
    if args.headless.is_some() && !spec.headless_enabled {
        return Err(LaunchError::HeadlessUnsupported);
    }
    if let Some(scenario) = args.scenario.as_deref()
        && !spec.scenarios.contains(&scenario)
    {
        return Err(LaunchError::Scenario(format!(
            "unknown screenshot scenario {scenario:?}; expected one of {}",
            spec.scenarios
                .iter()
                .map(|name| format!("{name:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(())
}

/// Resolve the palette, text scale, and motion policy used by screenshot
/// capture. An explicit `--text-scale` wins over the compatibility
/// `QOS_SHOTS_TEXT_SCALE` environment variable.
#[must_use]
pub fn shot_theme(args: &LaunchArgs) -> Theme {
    capture_theme(args.text_scale)
}

/// The same capture presentation for app-specific fixture drivers.
#[must_use]
pub fn capture_theme(text_scale: Option<f32>) -> Theme {
    let theme = match std::env::var("TILO_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    let theme = if let Some(scale) = text_scale {
        theme.with_text_scale(scale)
    } else {
        theme
    };
    // Screenshot review must use the same motion policy as a real app.  The
    // headless harness does not construct `tilo_ui::App`, where this environment
    // preference is normally read, so thread it onto the shot theme here.
    theme.with_reduce_motion(shot_motion_reduced())
}

/// The screenshot theme for capture frontends that have no [`LaunchArgs`]
/// (the `ui-shots` examples): `QOS_SHOTS_THEME`, `QOS_SHOTS_TEXT_SCALE` and
/// the motion preference through the same policy as [`shot_theme`]. An
/// unknown theme name is an error rather than a silent fall back to dark, so
/// a mis-typed light capture cannot masquerade as a dark one.
pub fn shot_theme_from_env() -> Result<Theme, String> {
    let theme =
        match std::env::var("TILO_SHOTS_THEME").or_else(|_| std::env::var("QOS_SHOTS_THEME")) {
            Ok(name) => {
                Theme::named(&name).ok_or_else(|| format!("unknown QOS_SHOTS_THEME `{name}`"))?
            }
            Err(_) => Theme::dark(),
        };
    let environment_scale = std::env::var("TILO_SHOTS_TEXT_SCALE")
        .or_else(|_| std::env::var("QOS_SHOTS_TEXT_SCALE"))
        .ok();
    Ok(theme
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
        "QOS_SHOTS_REDUCE_MOTION",
        "QOS_A11Y_REDUCE_MOTION",
        "QOS_REDUCE_MOTION",
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
        responsive_matrix: None,
        viewport: None,
        scenario: None,
        headless: None,
        native_audit: false,
        pseudo_locale: false,
        text_scale: None,
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
            "--scenario" => {
                let value = take_value(args, &mut index, flag, inline)?;
                set_once(&mut parsed.scenario, value.to_owned(), flag)?;
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
        .saturating_add(usize::from(parsed.responsive_matrix.is_some()))
        .saturating_add(usize::from(parsed.headless.is_some()))
        .saturating_add(usize::from(parsed.native_audit));
    if mode_count > 1 {
        return Err(LaunchError::ConflictingModes);
    }
    if parsed.shots.is_none() && parsed.responsive_matrix.is_none() && !parsed.native_audit {
        if parsed.viewport.is_some() {
            return Err(LaunchError::ShotsOnly("--viewport".to_owned()));
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

fn set_once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<(), LaunchError> {
    if slot.is_some() {
        Err(LaunchError::Duplicate(flag.to_owned()))
    } else {
        *slot = Some(value);
        Ok(())
    }
}

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
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn legacy_preparer_and_live_handler_are_rejected_before_either_runs() {
        let spec = LaunchSpec::new("so.tilo.launch-fixture", dummy_root)
            .prepare_legacy_with_args(|_| panic!("preparer must not run"))
            .live_handler(|_| panic!("live handler must not run"));
        for raw in [
            strings(&["app"]),
            strings(&["app", "--headless-drive", "{}"]),
        ] {
            assert!(
                matches!(run_with_args(&spec, &raw), Err(LaunchError::Operation(message))
                if message == "prepare_legacy_with_args and live_handler cannot be combined")
            );
        }
    }

    struct LaunchFixtureEnv {
        _directory: tempfile::TempDir,
        previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }
    impl LaunchFixtureEnv {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let mut previous = Vec::new();
            for (key, value) in [
                ("XDG_DATA_HOME", directory.path().as_os_str().to_os_string()),
                ("TILO_PACKAGE_ID", "so.tilo.launch-fixture".into()),
                (
                    "TILO_WORKSPACE_ID",
                    "00000000-0000-0000-0000-000000000000".into(),
                ),
            ] {
                previous.push((key, std::env::var_os(key)));
                unsafe {
                    std::env::set_var(key, value);
                }
            }
            Self {
                _directory: directory,
                previous,
            }
        }
    }
    impl Drop for LaunchFixtureEnv {
        fn drop(&mut self) {
            for (key, value) in &self.previous {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }
    fn migrated_fixture_open(app_id: &'static str) -> (State, crate::persist::Autosave) {
        use automerge::transaction::Transactable;
        let (mut state, autosave) = crate::persist::open_for(app_id).unwrap();
        state
            .doc
            .put(automerge::ROOT, "migration_marker", "configured-loader")
            .unwrap();
        (state, autosave)
    }
    fn fixture_preparer(args: &LaunchArgs) -> Result<LegacyLaunchInputs, LaunchError> {
        use automerge::transaction::Transactable;
        let path = args
            .positionals()
            .first()
            .ok_or_else(|| LaunchError::Operation("missing fixture".into()))?;
        let contents =
            std::fs::read_to_string(path).map_err(|e| LaunchError::Operation(e.to_string()))?;
        let (mut index, autosave) = crate::persist::open_for("so.tilo.launch-fixture")
            .map_err(|e| LaunchError::Operation(e.to_string()))?;
        index
            .doc
            .put(automerge::ROOT, "index_marker", "original-index")
            .unwrap();
        let mut state = State::new();
        state
            .doc
            .put(automerge::ROOT, "file_marker", contents)
            .unwrap();
        Ok(LegacyLaunchInputs::new(state, dummy_root(), autosave)
            .with_records(crate::record::RecordSet::in_memory(), index.doc))
    }
    fn headless_fixture_args(
        source: Option<&std::path::Path>,
        dump: &std::path::Path,
    ) -> LaunchArgs {
        let mut raw = vec![
            "app".into(),
            "--headless-drive".into(),
            serde_json::json!({"dump": dump}).to_string(),
        ];
        if let Some(source) = source {
            raw.push(source.to_string_lossy().into_owned());
        }
        parse_args_extended(&raw, &[], &[], &[], &[], 1).unwrap()
    }
    #[test]
    #[serial_test::serial(process_env)]
    fn headless_dispatch_uses_configured_migration_loader() {
        let fixture = LaunchFixtureEnv::new();
        let dump = fixture._directory.path().join("dump.json");
        let args = headless_fixture_args(None, &dump);
        let spec =
            LaunchSpec::new("so.tilo.launch-fixture", dummy_root).open_state(migrated_fixture_open);
        assert_eq!(
            dispatch_legacy_headless(&spec, &args, args.headless.as_ref().unwrap()).unwrap(),
            0
        );
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dump).unwrap()).unwrap();
        assert_eq!(state["migration_marker"], "configured-loader");
    }
    #[test]
    #[serial_test::serial(process_env)]
    fn prepared_file_state_and_original_room_index_reach_real_headless_dispatch() {
        use automerge::ReadDoc;
        let fixture = LaunchFixtureEnv::new();
        let source = fixture._directory.path().join("document.md");
        let dump = fixture._directory.path().join("dump.json");
        std::fs::write(&source, "fresh-file-content").unwrap();
        let args = headless_fixture_args(Some(&source), &dump);
        let spec = LaunchSpec::new("so.tilo.launch-fixture", dummy_root)
            .max_positionals(1)
            .prepare_legacy_with_args(fixture_preparer);
        assert_eq!(
            dispatch_legacy_headless(&spec, &args, args.headless.as_ref().unwrap()).unwrap(),
            0
        );
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dump).unwrap()).unwrap();
        assert_eq!(state["file_marker"], "fresh-file-content");
        let (reopened, _) = crate::persist::open_for("so.tilo.launch-fixture").unwrap();
        assert_eq!(
            reopened
                .doc
                .get(automerge::ROOT, "index_marker")
                .unwrap()
                .unwrap()
                .0
                .to_str(),
            Some("original-index")
        );
        assert!(
            reopened
                .doc
                .get(automerge::ROOT, "file_marker")
                .unwrap()
                .is_none(),
            "headless must not replace the native room index with the imported body"
        );
        // Reopen a changed file against the SAME profile: the explicitly
        // in-memory native policy must not merge a stale headless body cache.
        std::fs::write(&source, "replacement-file-content").unwrap();
        assert_eq!(
            dispatch_legacy_headless(&spec, &args, args.headless.as_ref().unwrap()).unwrap(),
            0
        );
        let state: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture._directory.path().join("dump.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(state["file_marker"], "replacement-file-content");
    }
    #[test]
    #[serial_test::serial(process_env)]
    fn native_constructor_consumes_the_same_fallible_preparer() {
        use automerge::ReadDoc;
        let fixture = LaunchFixtureEnv::new();
        let source = fixture._directory.path().join("document.md");
        std::fs::write(&source, "native-file-content").unwrap();
        let args = headless_fixture_args(
            Some(&source),
            &fixture._directory.path().join("unused.json"),
        );
        let spec = LaunchSpec::new("so.tilo.launch-fixture", dummy_root)
            .max_positionals(1)
            .prepare_legacy_with_args(fixture_preparer);
        let app = build_native_app_with_constructor(&spec, &args, |scope, state, root| {
            assert_eq!(
                state
                    .doc
                    .get(automerge::ROOT, "file_marker")
                    .unwrap()
                    .unwrap()
                    .0
                    .to_str(),
                Some("native-file-content")
            );
            App::from_scoped_parts_with_controlled_inputs(scope, state, root)
        })
        .unwrap();
        drop(app);
    }
    fn wrong_namespace_open(_: &'static str) -> (State, crate::persist::Autosave) {
        crate::persist::open_for("so.tilo.other-fixture").unwrap()
    }

    #[test]
    fn widget_owned_persistence_rejects_mixed_record_owner_before_either_runner() {
        let spec =
            LaunchSpec::new("so.tilo.launch-fixture", dummy_root).prepare_legacy_with_args(|_| {
                Ok(LegacyLaunchInputs::new(
                    State::new(),
                    dummy_root(),
                    crate::persist::Autosave::disabled("owned fixture"),
                )
                .with_records(
                    crate::record::RecordSet::in_memory(),
                    automerge::AutoCommit::new(),
                )
                .with_widget_owned_persistence())
            });
        let args = parse_args_extended(&strings(&["app"]), &[], &[], &[], &[], 0).unwrap();
        assert!(
            matches!(build_native_app_with_constructor(&spec, &args, |_, _, _| panic!("invalid ownership must fail before native construction")), Err(LaunchError::Operation(message)) if message.contains("cannot also use"))
        );
        assert!(
            matches!(dispatch_legacy_headless(&spec, &args, &headless::Spec::default()), Err(LaunchError::Operation(message)) if message.contains("cannot also use"))
        );
    }
    #[test]
    #[serial_test::serial(process_env)]
    fn headless_custom_namespace_and_disabled_writer_fail_without_default_fallback() {
        let fixture = LaunchFixtureEnv::new();
        for open in [
            wrong_namespace_open as OpenState,
            scoped_open_state as OpenState,
        ] {
            let dump = fixture._directory.path().join("must-not-exist.json");
            let args = headless_fixture_args(None, &dump);
            let spec = LaunchSpec::new("so.tilo.launch-fixture", dummy_root).open_state(open);
            assert_eq!(
                dispatch_legacy_headless(&spec, &args, args.headless.as_ref().unwrap()).unwrap(),
                3
            );
            assert!(!dump.exists());
        }
        assert!(
            !fixture
                ._directory
                .path()
                .join("so.tilo.launch-fixture/state.automerge")
                .exists()
        );
    }
    #[test]
    #[serial_test::serial(process_env)]
    fn failed_file_preparer_stops_real_dispatch_before_output_or_fallback() {
        let fixture = LaunchFixtureEnv::new();
        let dump = fixture._directory.path().join("must-not-exist.json");
        let missing = fixture._directory.path().join("missing.md");
        let args = headless_fixture_args(Some(&missing), &dump);
        let spec = LaunchSpec::new("so.tilo.launch-fixture", dummy_root)
            .max_positionals(1)
            .prepare_legacy_with_args(fixture_preparer);
        assert!(matches!(
            dispatch_legacy_headless(&spec, &args, args.headless.as_ref().unwrap()),
            Err(LaunchError::Operation(_))
        ));
        assert!(!dump.exists());
        assert!(
            !fixture
                ._directory
                .path()
                .join("so.tilo.launch-fixture")
                .exists()
        );
    }

    static CUSTOM_SHOT_RAN: AtomicBool = AtomicBool::new(false);
    static CUSTOM_LIVE_RAN: AtomicBool = AtomicBool::new(false);
    static CUSTOM_COMMAND_RAN: AtomicBool = AtomicBool::new(false);

    thread_local! {
        static LAUNCH_WAKE_CAPTURES: std::cell::RefCell<Vec<crate::WakeHandle>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    fn capture_launch_wake() {
        LAUNCH_WAKE_CAPTURES.with(|captures| captures.borrow_mut().push(crate::WakeHandle::new()));
    }

    fn scoped_open_state(_: &'static str) -> (State, crate::persist::Autosave) {
        capture_launch_wake();
        (
            State::new(),
            crate::persist::Autosave::disabled("wake routing test"),
        )
    }

    fn scoped_root() -> Box<dyn Widget> {
        capture_launch_wake();
        Box::new(crate::widget::Text::new("scope test"))
    }

    fn scoped_configure(_: &mut App) {
        capture_launch_wake();
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parses_both_common_argument_forms() {
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
    }

    #[test]
    fn parses_responsive_matrix_and_pseudo_locale_as_common_audit_modes() {
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

    #[test]
    fn rejects_malformed_and_zero_viewports() {
        for value in ["wide", "800", "0x600", "800x0", "800xnope"] {
            let args = strings(&["app", "--viewport", value]);
            assert!(matches!(
                parse_args(&args, &[], &[]),
                Err(LaunchError::InvalidViewport(_))
            ));
        }
    }

    #[test]
    fn rejects_invalid_text_scales() {
        for value in ["nope", "NaN", "0", "0.5", "2.1"] {
            let args = strings(&["app", "--text-scale", value]);
            assert!(matches!(
                parse_args(&args, &[], &[]),
                Err(LaunchError::InvalidTextScale(_))
            ));
        }
    }

    #[test]
    fn screenshot_text_scale_prefers_cli_then_compatibility_environment() {
        for (cli, environment, expected) in [
            (Some(2.0), Some("1.25"), 2.0),
            (None, Some("1.25"), 1.25),
            (None, None, 1.0),
            (None, Some("invalid"), 1.0),
        ] {
            assert_eq!(shot_text_scale(cli, environment), expected);
        }
    }

    #[test]
    fn rejects_unknown_missing_and_duplicate_arguments() {
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

    #[test]
    fn parses_headless_payload_without_process_exit() {
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

    #[test]
    fn parses_native_audit_as_an_exclusive_common_mode() {
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

    #[test]
    fn rejects_conflicting_modes_and_shot_options_in_live_mode() {
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

    fn dummy_root() -> Box<dyn Widget> {
        Box::new(crate::widget::Button::new("test", "Test", None))
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "signature is fixed by LaunchSpec::command_handler"
    )]
    fn custom_command(_: &LaunchArgs) -> Result<bool, LaunchError> {
        CUSTOM_COMMAND_RAN.store(true, Ordering::SeqCst);
        Ok(true)
    }

    #[test]
    fn app_command_can_finish_before_window_startup() {
        CUSTOM_COMMAND_RAN.store(false, Ordering::SeqCst);
        let spec = LaunchSpec::new("so.tilo.test", dummy_root)
            .allowed_live_flags(&["--utility"])
            .command_handler(custom_command);
        run_with_args(&spec, &strings(&["app", "--utility"])).unwrap();
        assert!(CUSTOM_COMMAND_RAN.load(Ordering::SeqCst));
    }

    #[test]
    fn history_declaration_gate_rejects_omission_for_standard_and_custom_specs() {
        fn custom(_: &LaunchArgs) -> Result<(), LaunchError> {
            panic!("validation cannot launch");
        }
        for spec in [
            LaunchSpec::new("dev.qos.test", dummy_root),
            LaunchSpec::new("dev.qos.test", dummy_root).live_handler(custom),
            LaunchSpec::new_with_args("dev.qos.test", |_| panic!("validation cannot construct")),
            LaunchSpec::from_definition(crate::application_test_support::runner_probe_definition()),
        ] {
            assert_eq!(
                spec.validate_history_declaration(),
                Err(crate::history_mode::HistoryRegistrationError::Missing)
            );
        }
    }

    #[test]
    fn history_declaration_gate_accepts_matching_external_policy_and_refuses_foreign_identity() {
        static HISTORY: crate::history_mode::HistoryDefinition =
            crate::history_mode::HistoryDefinition {
                app_id: "dev.qos.test",
                scopes: &[crate::history_mode::HistoryScopeDefinition {
                    key: "process",
                    title: "Process",
                    presentation: crate::history_mode::HistoryPresentation::External {
                        reason: "The live process is not versioned.",
                    },
                    content_schema: None,
                }],
            };
        assert!(
            LaunchSpec::new(HISTORY.app_id, dummy_root)
                .history(&HISTORY)
                .validate_history_declaration()
                .is_ok()
        );
        assert_eq!(
            LaunchSpec::new("dev.qos.other", dummy_root)
                .history(&HISTORY)
                .validate_history_declaration(),
            Err(crate::history_mode::HistoryRegistrationError::ForeignSource)
        );
    }

    #[test]
    fn managed_standard_native_launch_installs_history_without_detaching_wake_scope() {
        static HISTORY: crate::history_mode::HistoryDefinition =
            crate::history_mode::HistoryDefinition {
                app_id: "dev.qos.wake-test",
                scopes: &[crate::history_mode::HistoryScopeDefinition {
                    key: "preferences",
                    title: "Preferences",
                    presentation: crate::history_mode::HistoryPresentation::Domain {
                        create: crate::history_mode::test_preview,
                    },
                    content_schema: None,
                }],
            };
        LAUNCH_WAKE_CAPTURES.with(|captures| captures.borrow_mut().clear());
        let spec = LaunchSpec::new(HISTORY.app_id, scoped_root)
            .open_app_global_state(scoped_open_state)
            .configure_app(scoped_configure)
            .history(&HISTORY);
        let mut app = spec.build_controlled_app_for_test().unwrap();
        assert!(
            app.build_a11y_tree_update(1.0)
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some("History")),
            "production constructor must consume managed History metadata"
        );
        let captures =
            LAUNCH_WAKE_CAPTURES.with(|captures| std::mem::take(&mut *captures.borrow_mut()));
        assert!(captures.iter().all(crate::WakeHandle::is_active));
        drop(app);
        assert!(captures.iter().all(|handle| !handle.is_active()));
    }

    #[test]
    fn photos_argument_global_opener_reaches_managed_production_constructor() {
        use automerge::{ReadDoc, transaction::Transactable};
        static HISTORY: crate::history_mode::HistoryDefinition =
            crate::history_mode::HistoryDefinition {
                app_id: "dev.qos.argument-test",
                scopes: &[crate::history_mode::HistoryScopeDefinition {
                    key: "photos",
                    title: "Photos",
                    presentation: crate::history_mode::HistoryPresentation::Domain {
                        create: crate::history_mode::test_preview,
                    },
                    content_schema: None,
                }],
            };
        fn open(_: &'static str, args: &LaunchArgs) -> (State, crate::persist::Autosave) {
            let mut state = State::new();
            state
                .doc
                .put(
                    automerge::ROOT,
                    "selected_argument",
                    args.value("--open").unwrap_or("saved-state"),
                )
                .unwrap();
            (
                state,
                crate::persist::Autosave::disabled("detached argument opener test"),
            )
        }
        fn replace_storage(app: &mut App) {
            let owned = std::mem::replace(
                app,
                App::from_scoped_parts_with_controlled_inputs(
                    crate::WakeScope::new(),
                    State::new(),
                    dummy_root(),
                ),
            );
            *app = owned.with_autosave(crate::persist::Autosave::disabled("replacement"));
        }
        for raw in [
            strings(&["app"]),
            strings(&["app", "--open", "selected.png"]),
        ] {
            let args = parse_args_extended(&raw, &[], &[], &[], &["--open"], 0).unwrap();
            let spec = LaunchSpec::new(HISTORY.app_id, dummy_root)
                .open_app_global_state_with_args(open)
                .history(&HISTORY);
            let mut app = build_native_app_with_constructor(&spec, &args, |scope, state, root| {
                assert_eq!(
                    state
                        .doc
                        .get(automerge::ROOT, "selected_argument")
                        .unwrap()
                        .unwrap()
                        .0
                        .to_str(),
                    Some(args.value("--open").unwrap_or("saved-state"))
                );
                App::from_scoped_parts_with_controlled_inputs(scope, state, root)
            })
            .unwrap();
            assert!(
                app.build_a11y_tree_update(1.)
                    .nodes
                    .iter()
                    .any(|(_, n)| n.label() == Some("History"))
            );
            drop(app);
            let (state, saver) = spec.open_state_with_args.unwrap()(HISTORY.app_id, &args);
            let registration = crate::history_mode::HistoryRegistration::installed_app_state(
                &HISTORY,
                Some(&saver),
                uuid::Uuid::new_v4(),
            )
            .unwrap();
            let mut driver = crate::headless::Driver::from_managed_root(
                state,
                crate::history_mode::ManagedRoot::new(dummy_root(), registration),
                (900, 650),
            )
            .unwrap();
            driver.click_named("history-open").unwrap();
            assert!(driver.targets().contains_key("activity_timeline"));
            assert_eq!(
                driver
                    .state_mut()
                    .doc
                    .get(automerge::ROOT, "selected_argument")
                    .unwrap()
                    .unwrap()
                    .0
                    .to_str(),
                Some(args.value("--open").unwrap_or("saved-state"))
            );
            for rejected in [
                LaunchSpec::new(HISTORY.app_id, dummy_root)
                    .open_state_with_args(open)
                    .history(&HISTORY),
                LaunchSpec::new(HISTORY.app_id, dummy_root)
                    .open_app_global_state_with_args(open)
                    .open_state_with_args(open)
                    .history(&HISTORY),
                LaunchSpec::new(HISTORY.app_id, dummy_root)
                    .open_app_global_state_with_args(open)
                    .open_state(scoped_open_state)
                    .history(&HISTORY),
                spec.configure_app(replace_storage),
            ] {
                assert!(
                    matches!(build_native_app_with_constructor(&rejected,&args,App::from_scoped_parts_with_controlled_inputs),Err(LaunchError::Operation(message)) if message.contains("storage provenance"))
                );
            }
        }
    }

    #[test]
    fn managed_launch_refuses_undeclared_custom_storage_and_post_configuration_replacement() {
        static HISTORY: crate::history_mode::HistoryDefinition =
            crate::history_mode::HistoryDefinition {
                app_id: "dev.qos.wake-test",
                scopes: &[crate::history_mode::HistoryScopeDefinition {
                    key: "preferences",
                    title: "Preferences",
                    presentation: crate::history_mode::HistoryPresentation::Domain {
                        create: crate::history_mode::test_preview,
                    },
                    content_schema: None,
                }],
            };
        fn replace_storage(app: &mut App) {
            let owned = std::mem::replace(
                app,
                App::from_scoped_parts_with_controlled_inputs(
                    crate::WakeScope::new(),
                    State::new(),
                    dummy_root(),
                ),
            );
            *app = owned.with_autosave(crate::persist::Autosave::disabled("replacement"));
        }
        let args = parse_args_extended(&strings(&["app"]), &[], &[], &[], &[], 0).unwrap();
        for spec in [
            LaunchSpec::new(HISTORY.app_id, scoped_root)
                .open_state(scoped_open_state)
                .history(&HISTORY),
            LaunchSpec::new(HISTORY.app_id, scoped_root)
                .open_app_global_state(scoped_open_state)
                .configure_app(replace_storage)
                .history(&HISTORY),
        ] {
            let result = build_native_app_with_constructor(
                &spec,
                &args,
                App::from_scoped_parts_with_controlled_inputs,
            );
            assert!(
                matches!(result, Err(LaunchError::Operation(message)) if message.contains("storage provenance"))
            );
        }
        let spec = LaunchSpec::new(HISTORY.app_id, scoped_root).history(&HISTORY);
        assert!(
            spec.history_app_global_storage,
            "the standard app-global opener is known by the runner"
        );
        let mut capture = build_history_capture(&spec, &args, scoped_root(), (900, 650)).unwrap();
        assert!(capture.targets().contains_key("history-open"));
        capture.click_named("history-open").unwrap();
        assert!(capture.targets().contains_key("activity_timeline"));
    }

    #[test]
    fn standard_live_factory_and_configuration_capture_the_same_app_lifetime() {
        LAUNCH_WAKE_CAPTURES.with(|captures| captures.borrow_mut().clear());
        let spec = LaunchSpec::new("so.tilo.wake-test", scoped_root)
            .open_state(scoped_open_state)
            .configure_app(scoped_configure);
        let args = parse_args_extended(&strings(&["app"]), &[], &[], &[], &[], 0).unwrap();
        let mut outer = crate::WakeScope::new();
        let _outer = outer.enter();
        let app = build_native_app_with_constructor(
            &spec,
            &args,
            App::from_scoped_parts_with_controlled_inputs,
        )
        .unwrap();
        let captured =
            LAUNCH_WAKE_CAPTURES.with(|captures| std::mem::take(&mut *captures.borrow_mut()));
        assert_eq!(captured.len(), 3);
        assert!(captured.iter().all(crate::WakeHandle::is_active));
        // Closing the parent must not close any child construction capture.
        outer.close();
        assert!(captured.iter().all(crate::WakeHandle::is_active));
        drop(app);
        assert!(captured.iter().all(|handle| !handle.is_active()));
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "signature is fixed by LaunchSpec::command_handler"
    )]
    fn capture_command_wake(_: &LaunchArgs) -> Result<bool, LaunchError> {
        capture_launch_wake();
        Ok(true)
    }

    #[test]
    fn cli_utility_capture_stays_detached_under_a_host_callback() {
        LAUNCH_WAKE_CAPTURES.with(|captures| captures.borrow_mut().clear());
        let mut outer = crate::WakeScope::new();
        let _outer = outer.enter();
        let spec = LaunchSpec::new("so.tilo.wake-utility", dummy_root)
            .command_handler(capture_command_wake);
        run_with_args(&spec, &strings(&["app"])).unwrap();
        outer.close();
        let captures =
            LAUNCH_WAKE_CAPTURES.with(|captures| std::mem::take(&mut *captures.borrow_mut()));
        assert_eq!(captures.len(), 1);
        assert!(captures.iter().all(crate::WakeHandle::is_active));
    }

    #[test]
    fn privileged_app_can_reject_headless_driving() {
        let spec = LaunchSpec::new("so.tilo.test", dummy_root).disable_headless();
        let result = run_with_args(&spec, &strings(&["app", "--headless-drive", "{}"]));
        assert!(matches!(result, Err(LaunchError::HeadlessUnsupported)));
    }

    #[test]
    fn scenarios_must_be_declared_by_the_app() {
        let args = parse_args(
            &strings(&["app", "--shots=/tmp/a.png", "--scenario=wide"]),
            &[],
            &[],
        )
        .unwrap();
        let undeclared = LaunchSpec::new("so.tilo.test", dummy_root);
        assert!(matches!(
            validate_spec_args(&args, &undeclared),
            Err(LaunchError::Scenario(_))
        ));
        let declared = LaunchSpec::new("so.tilo.test", dummy_root).scenarios(&["wide"]);
        validate_spec_args(&args, &declared).unwrap();
    }

    #[test]
    fn ordinary_launch_spec_reuses_live_root_for_screenshots() {
        let args = parse_args(&strings(&["app", "--shots=/tmp/ordinary.png"]), &[], &[]).unwrap();
        let spec = LaunchSpec::new("so.tilo.test", dummy_root);
        let root = spec
            .screenshot_root(&args)
            .expect("default screenshot root");
        assert_eq!(root.name(), "Button");
    }

    #[test]
    fn app_specific_options_accept_both_forms_and_are_shots_only() {
        for raw in [
            strings(&["app", "--shots=/tmp/a.png", "--screen", "add"]),
            strings(&["app", "--shots=/tmp/a.png", "--screen=add"]),
        ] {
            let parsed = parse_args(&raw, &[], &["--screen"]).unwrap();
            assert_eq!(parsed.value("--screen"), Some("add"));
        }
        assert!(matches!(
            parse_args(
                &strings(&["app", "--screen=add"]),
                &[],
                &["--screen"]
            ),
            Err(LaunchError::ShotsOnly(option)) if option == "--screen"
        ));
        assert!(matches!(
            parse_args(
                &strings(&[
                    "app",
                    "--shots=/tmp/a.png",
                    "--screen=add",
                    "--screen=empty"
                ]),
                &[],
                &["--screen"]
            ),
            Err(LaunchError::Duplicate(option)) if option == "--screen"
        ));
    }

    #[test]
    fn live_options_and_positionals_are_validated_then_exposed() {
        let parsed = parse_args_extended(
            &strings(&["app", "--open=notes.qdoc", "workspace"]),
            &[],
            &[],
            &[],
            &["--open"],
            1,
        )
        .unwrap();
        assert_eq!(parsed.value("--open"), Some("notes.qdoc"));
        assert_eq!(parsed.positionals(), &["workspace"]);

        assert!(matches!(
            parse_args_extended(
                &strings(&["app", "one", "two"]),
                &[],
                &[],
                &[],
                &[],
                1,
            ),
            Err(LaunchError::UnknownArgument(argument)) if argument == "two"
        ));
    }

    fn fallible_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
        Err(LaunchError::Operation(format!(
            "cannot open {}",
            args.positionals().first().map_or("nothing", String::as_str)
        )))
    }

    #[test]
    fn argument_aware_root_propagates_initialization_failure() {
        let args =
            parse_args_extended(&strings(&["app", "broken.qdoc"]), &[], &[], &[], &[], 1).unwrap();
        let spec = LaunchSpec::new("so.tilo.test", dummy_root)
            .live_root_with_args(fallible_root)
            .max_positionals(1);
        match spec.live_root(&args) {
            Err(error) => assert!(
                matches!(error, LaunchError::Operation(message) if message == "cannot open broken.qdoc")
            ),
            Ok(_) => panic!("fallible root unexpectedly succeeded"),
        }
    }

    fn custom_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
        if args.shots.as_deref() != Some(std::path::Path::new("/tmp/custom.png")) {
            return Err(LaunchError::Operation("wrong shot path".into()));
        }
        CUSTOM_SHOT_RAN.store(true, Ordering::SeqCst);
        Ok(())
    }

    #[test]
    fn custom_shot_pipeline_uses_standard_mode_dispatch() {
        CUSTOM_SHOT_RAN.store(false, Ordering::SeqCst);
        let spec = LaunchSpec::new("so.tilo.test", dummy_root)
            .shot_handler(custom_shot)
            .live_handler(unexpected_live);
        run_with_args(&spec, &strings(&["app", "--shots", "/tmp/custom.png"])).unwrap();
        assert!(CUSTOM_SHOT_RAN.load(Ordering::SeqCst));
    }

    fn unexpected_live(_args: &LaunchArgs) -> Result<(), LaunchError> {
        Err(LaunchError::Operation(
            "screenshot dispatch reached the live handler".into(),
        ))
    }

    fn custom_live(args: &LaunchArgs) -> Result<(), LaunchError> {
        if !args.positionals().is_empty() {
            return Err(LaunchError::Operation("unexpected positional".into()));
        }
        CUSTOM_LIVE_RAN.store(true, Ordering::SeqCst);
        Ok(())
    }

    #[test]
    fn custom_live_pipeline_runs_after_standard_mode_dispatch() {
        CUSTOM_LIVE_RAN.store(false, Ordering::SeqCst);
        let spec = LaunchSpec::new("dev.qos.test", dummy_root).live_handler(custom_live);
        run_with_args_with_attachment(&spec, &strings(&["app"]), |_| Ok(())).unwrap();
        assert!(CUSTOM_LIVE_RAN.load(Ordering::SeqCst));
    }

    #[test]
    fn native_runtime_attachment_precedes_default_custom_and_definition_construction() {
        fn fail_root(_: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
            Err(LaunchError::Operation(
                "root was constructed before attachment".into(),
            ))
        }
        fn fail_prepare(
            _: &crate::application::ApplicationDefinition,
            _: &LaunchArgs,
        ) -> Result<crate::application::PreparedComponentInputs, LaunchError> {
            Err(LaunchError::Operation(
                "definition was prepared before attachment".into(),
            ))
        }
        let specs = [
            LaunchSpec::new_with_args("dev.qos.test", fail_root),
            LaunchSpec::new("dev.qos.test", dummy_root).live_handler(unexpected_live),
            LaunchSpec::from_definition(crate::application_test_support::runner_probe_definition())
                .component_host(fail_prepare),
        ];
        for spec in specs {
            let result = run_with_args_with_attachment(&spec, &strings(&["app"]), |app_id| {
                assert_eq!(app_id, spec.app_id);
                Err(std::io::Error::other(
                    "attachment stopped before construction",
                ))
            });
            assert!(
                matches!(result, Err(LaunchError::Operation(message)) if message.contains("attachment stopped before construction"))
            );
        }
    }

    #[test]
    fn diagnostics_and_handled_commands_never_attach_runtime() {
        fn fail_prepare(
            _: &crate::application::ApplicationDefinition,
            _: &LaunchArgs,
        ) -> Result<crate::application::PreparedComponentInputs, LaunchError> {
            Err(LaunchError::Operation("detached preparation".into()))
        }
        let spec =
            LaunchSpec::from_definition(crate::application_test_support::runner_probe_definition())
                .component_host(fail_prepare);
        for args in [
            strings(&["app", "--shots", "/tmp/not-written.png"]),
            strings(&["app", "--responsive-matrix", "/tmp/not-written"]),
            strings(&["app", "--native-audit"]),
            strings(&["app", "--headless-drive", "{}"]),
        ] {
            let result = run_with_args_with_attachment(&spec, &args, |_| {
                panic!("diagnostic attached runtime")
            });
            assert!(
                matches!(result, Err(LaunchError::Operation(message)) if message == "detached preparation")
            );
        }
        let spec = LaunchSpec::new("dev.qos.test", dummy_root).command_handler(custom_command);
        run_with_args_with_attachment(&spec, &strings(&["app"]), |_| {
            panic!("handled command attached runtime")
        })
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn automatic_reexec_skips_only_the_already_run_command_handler() {
        use std::os::{
            fd::AsRawFd,
            unix::{net::UnixStream, process::CommandExt},
        };
        static COMMANDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        fn command(_: &LaunchArgs) -> Result<bool, LaunchError> {
            COMMANDS.fetch_add(1, Ordering::SeqCst);
            Ok(false)
        }
        fn live(_: &LaunchArgs) -> Result<(), LaunchError> {
            Ok(())
        }
        if let Ok(mode) = std::env::var("QOS_LAUNCH_REEXEC_TEST") {
            let spec = LaunchSpec::new("dev.qos.reexec-test", dummy_root)
                .command_handler(command)
                .live_handler(live);
            if mode == "descendant" {
                run_with_args_with_attachment(&spec, &strings(&["app"]), |_| Ok(())).unwrap();
            } else if mode == "local" {
                let spec = spec.runtime_attachment(|_| false);
                run_with_args_with_attachment(&spec, &strings(&["app"]), |_| {
                    panic!("local policy attached inherited runtime")
                })
                .unwrap();
            } else {
                run_with_args(&spec, &strings(&["app"])).unwrap();
            }
            assert_eq!(
                COMMANDS.load(Ordering::SeqCst),
                usize::from(mode != "resumed")
            );
            return;
        }
        for mode in ["initial", "resumed", "local", "descendant"] {
            let (collab, _collab_peer) = UnixStream::pair().unwrap();
            let (identity, _identity_peer) = UnixStream::pair().unwrap();
            let fds = [collab.as_raw_fd(), identity.as_raw_fd()];
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "launch::tests::automatic_reexec_skips_only_the_already_run_command_handler",
                    "--nocapture",
                ])
                .env("QOS_LAUNCH_REEXEC_TEST", mode)
                .env("TILO_SVC_COLLAB_FD", fds[0].to_string())
                .env("TILO_SVC_IDENTITY_FD", fds[1].to_string())
                .env_remove("QOS_RUNTIME_RESUME_LIVE");
            if mode == "resumed" || mode == "descendant" {
                child.env("QOS_RUNTIME_RESUME_LIVE", "dev.qos.reexec-test");
            }
            if mode == "descendant" {
                child.env(
                    "QOS_RUNTIME_CONNECTION_OWNER_PID",
                    std::process::id().to_string(),
                );
            }
            // SAFETY: only fcntl runs after fork; both sockets stay owned by
            // this parent until the isolated assertion process exits.
            unsafe {
                child.pre_exec(move || {
                    for fd in fds {
                        if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
            assert!(child.status().unwrap().success());
        }
    }

    #[test]
    fn explicit_runtime_attachment_policy_skips_bootstrap_for_native_launch() {
        let spec = LaunchSpec::new("dev.qos.test", dummy_root)
            .runtime_attachment(|_| false)
            .live_handler(unexpected_live);
        let result = run_with_args_with_attachment(&spec, &strings(&["app"]), |_| {
            Err(std::io::Error::other("unexpected attachment"))
        });
        assert!(
            matches!(result, Err(LaunchError::Operation(message)) if message == "screenshot dispatch reached the live handler")
        );
    }

    fn custom_open_state(app_id: &'static str) -> (State, crate::persist::Autosave) {
        crate::persist::open(app_id)
    }

    #[test]
    fn app_can_replace_only_the_persistence_loader() {
        let spec = LaunchSpec::new("so.tilo.test", dummy_root).open_state(custom_open_state);
        assert!(std::ptr::fn_addr_eq(
            spec.open_state,
            custom_open_state as OpenState
        ));
        assert!(
            spec.live_root
                .is_some_and(|root| std::ptr::fn_addr_eq(root, dummy_root as RootFactory))
        );
    }

    #[test]
    fn operational_failures_are_not_reported_as_cli_usage_errors() {
        assert_eq!(LaunchError::Operation("failed".into()).exit_code(), 1);
        assert_eq!(LaunchError::UnknownArgument("--bad".into()).exit_code(), 2);
    }

    #[test]
    fn screenshot_motion_preference_matches_live_app_truthy_values() {
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

#[cfg(test)]
mod prepared_launch_tests {
    use super::*;
    use crate::application::{HostContext, LaunchTarget, PreparedComponentInputs, RestoreInput};
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
            records,
            views,
            DEVICE.into(),
            11,
            authority,
            author(),
            provenance(),
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
        let prepared = case.starts_with("prepared");
        let failing = case.ends_with("failure");
        let spec = if prepared {
            LaunchSpec::from_definition(runner_probe_definition())
                .component_host(prepare)
                .allowed_live_options(&["--fixture-root"])
        } else {
            LaunchSpec::new("so.tilo.metrics-fixture", || {
                Box::new(crate::widget::Button::new(
                    "metrics-button",
                    "Metrics",
                    None,
                ))
            })
        };
        let actions = if failing {
            serde_json::json!([{ "click_named": "missing-metrics-target" }])
        } else {
            serde_json::json!([])
        };
        let payload = serde_json::json!({
            "actions": actions,
            "dump": root.join("document.json"),
        });
        let mut args = vec!["app".into(), "--headless-drive".into(), payload.to_string()];
        if prepared {
            args.extend([
                "--fixture-root".into(),
                root.join("records").to_string_lossy().into_owned(),
            ]);
        }
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

        for case in [
            "legacy-success",
            "legacy-failure",
            "prepared-success",
            "prepared-failure",
        ] {
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
    fn definition_final_audit_and_shot_preparation_resolve_queued_component_wakes() {
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
            let (DefinitionDispatch::Audit(mut driver) | DefinitionDispatch::Rendered(mut driver)) =
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

    #[test]
    fn definition_text_scale_overrides_before_all_runner_layouts() {
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

    #[test]
    fn definition_missing_host_fails_before_factory() {
        let before = calls();
        let spec = LaunchSpec::from_definition(runner_probe_definition());
        let args = parsed(&spec, &["app".into()]);
        assert!(matches!(
            dispatch_definition(&spec, &args),
            Err(LaunchError::MissingComponentHost)
        ));
        assert_eq!(calls(), before);
    }

    #[test]
    fn native_services_are_discovered_only_by_the_selected_native_runner() {
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
                let dispatched =
                    dispatch_definition_with_native(&spec, &args, |component, inputs, services| {
                        native_calls.set(native_calls.get() + 1);
                        assert_eq!(services, enabled);
                        // The real native merge is covered in prepared_runner_tests;
                        // this mode-selection test must never discover ambient services.
                        App::from_component(component, inputs)
                    })
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
    fn capture_scenario_failure_closes_component_and_retains_failed_save_retry() {
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
        let Err(LaunchError::ComponentCapture { source, reports }) =
            dispatch_definition(&spec, &args)
        else {
            panic!("capture must fail")
        };
        assert_eq!(source.to_string(), "scenario failed after editing");
        let mut failed = reports
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

    #[test]
    fn definition_rejects_legacy_setup_overrides_before_preparation() {
        for spec in [
            LaunchSpec::from_definition(runner_probe_definition())
                .configure_app(|_| panic!("legacy configure")),
            LaunchSpec::from_definition(runner_probe_definition())
                .open_state(|_| panic!("legacy state")),
            LaunchSpec::from_definition(runner_probe_definition())
                .live_root_with_args(|_| panic!("legacy root")),
            LaunchSpec::from_definition(runner_probe_definition())
                .prepare_legacy_with_args(|_| panic!("legacy preparer")),
        ] {
            let before = calls();
            let args = parse_args(&["app".into()], &[], &[]).unwrap();
            assert!(matches!(
                dispatch_definition(&spec, &args),
                Err(LaunchError::IncompatibleComponentSetup)
            ));
            assert_eq!(calls(), before);
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
    #[test]
    fn definition_denied_and_invalid_preparation_preserve_typed_errors() {
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
    #[test]
    fn definition_headless_state_open_failure_is_durability_exit_with_original_bytes() {
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

    #[test]
    fn definition_audit_close_failure_is_operational_and_retains_original_retry() {
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
