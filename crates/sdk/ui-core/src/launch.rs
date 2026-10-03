//! Standard command-line lifecycle for Tilo applications.
//!
//! Ordinary apps provide their live and screenshot roots; this module owns
//! common argument validation, persistence, headless driving, offscreen
//! rendering, logging, and live startup. Apps with different
//! lifecycles can continue to use the lower-level APIs directly.

use std::fmt;
use std::path::PathBuf;

#[cfg(not(target_arch = "wasm32"))]
use crate::App;
use crate::headless::{self, FrameHarness};
use crate::widget::Widget;
use crate::{State, Theme};

mod component_adapter;
mod shot_frame;
pub use component_adapter::{ComponentActions, ComponentLocales, RecordLaunch, record_launch};
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
    pub(crate) autosave: crate::persist::DocWriter,
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
    pub fn new(state: State, root: Box<dyn Widget>, autosave: crate::persist::DocWriter) -> Self {
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
// The windowed `App` runner is native-only; browser hosts mount components.
#[cfg(not(target_arch = "wasm32"))]
type ConfigureApp = fn(&mut App);
type OpenState = fn(&'static str) -> (State, crate::persist::DocWriter);
type OpenStateWithArgs = fn(&'static str, &LaunchArgs) -> (State, crate::persist::DocWriter);
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
    /// `--theme NAME`: the shipped theme a capture renders in
    /// ([`Theme::named`]), over `TILO_SHOTS_THEME`.
    pub theme: Option<String>,
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
            responsive_matrix: None,
            viewport: None,
            scenario: None,
            headless: None,
            native_audit: false,
            pseudo_locale: false,
            text_scale: None,
            theme: None,
            extras: Vec::new(),
            values: Vec::new(),
            positionals: Vec::new(),
        }
    }

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

/// Prepared native construction owns its one admitted SDK worker.
/// No caller-supplied mode or authority can construct this value.
#[cfg(feature = "native")]
pub struct PreparedNativeLaunch<'a> {
    spec: LaunchSpec,
    args: &'a LaunchArgs,
    scope: crate::wake::WakeScope,
    construction: crate::collaboration::native_initial::NativeConstruction,
    _wake_entry: crate::wake::WakeScopeGuard,
}
#[cfg(feature = "native")]
impl<'a> PreparedNativeLaunch<'a> {
    #[cfg(test)]
    fn with_manager(
        spec: &LaunchSpec,
        args: &'a LaunchArgs,
        manager: crate::collaboration::CollaborationManager,
    ) -> Result<Self, LaunchError> {
        Self::with_scope_manager(spec, args, crate::wake::WakeScope::new(), manager)
    }
    fn with_scope_manager(
        spec: &LaunchSpec,
        args: &'a LaunchArgs,
        scope: crate::wake::WakeScope,
        manager: crate::collaboration::CollaborationManager,
    ) -> Result<Self, LaunchError> {
        let wake_entry = scope.enter();
        let construction = crate::collaboration::native_initial::NativeConstruction::enter(manager)
            .map_err(LaunchError::Operation)?;
        Ok(Self {
            spec: spec.clone(),
            args,
            scope,
            construction,
            _wake_entry: wake_entry,
        })
    }
    pub fn build_root(
        self,
        factory: impl FnOnce() -> Result<Box<dyn Widget>, LaunchError>,
    ) -> Result<App, LaunchError> {
        self.build_root_inner(|| factory().map(|root| (root, None)))
    }
    /// Select the immutable History declaration derived from this exact root.
    /// The factory still runs only after the actual initial history is ready.
    pub fn build_root_with_history(
        self,
        factory: impl FnOnce() -> Result<
            (
                Box<dyn Widget>,
                &'static crate::history_mode::HistoryDefinition,
            ),
            LaunchError,
        >,
    ) -> Result<App, LaunchError> {
        self.build_root_inner(|| factory().map(|(root, history)| (root, Some(history))))
    }
    fn build_root_inner(
        mut self,
        factory: impl FnOnce() -> Result<
            (
                Box<dyn Widget>,
                Option<&'static crate::history_mode::HistoryDefinition>,
            ),
            LaunchError,
        >,
    ) -> Result<App, LaunchError> {
        self.construction.check().map_err(LaunchError::Operation)?;
        let (state, autosave) = self.spec.open_state_with_args.map_or_else(
            || (self.spec.open_state)(self.spec.app_id),
            |open| open(self.spec.app_id, self.args),
        );
        self.construction
            .state_opened()
            .map_err(LaunchError::Operation)?;
        let (root, history) = factory()?;
        self.construction.check().map_err(LaunchError::Operation)?;
        if let Some(history) = history {
            self.spec.history_definition = Some(history);
        }
        finalize_native_app_with_constructor(
            &self.spec,
            self.args,
            self.scope,
            LegacyLaunchInputs::new(state, root, autosave),
            crate::ControlDomain::outer(),
            App::from_scoped_parts_with_custody,
        )
    }
}

/// Configuration for an app using the standard lifecycle.
#[cfg_attr(
    not(feature = "native"),
    allow(
        dead_code,
        reason = "some fields are read only by the native event-loop runner"
    )
)]
#[derive(Clone)]
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
    #[cfg(not(target_arch = "wasm32"))]
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
    component_actions: Option<ComponentActions>,
    component_locales: ComponentLocales,
}

impl LaunchSpec {
    /// Establish the actual SDK mode before a native root or state opener runs.
    #[cfg(feature = "native")]
    pub fn prepare_native<'a>(
        &self,
        args: &'a LaunchArgs,
    ) -> Result<PreparedNativeLaunch<'a>, LaunchError> {
        validate_spec_args(args, self)?;
        let scope = crate::wake::WakeScope::new();
        let _entry = scope.enter();
        let manager = native_launch_manager(self, args)?;
        PreparedNativeLaunch::with_scope_manager(self, args, scope, manager)
    }

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
    #[cfg(all(feature = "native", any(test, feature = "test-support")))]
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
            App::from_scoped_parts_with_controlled_custody,
        )
    }

    /// Build an application around a root already prepared under `wake_scope`.
    ///
    /// # Errors
    /// Returns launch, storage, root registration, or History installation failures.
    #[cfg(feature = "native")]
    pub fn build_prepared_root(
        &self,
        args: &LaunchArgs,
        wake_scope: crate::WakeScope,
        root: Box<dyn Widget>,
    ) -> Result<App, LaunchError> {
        validate_spec_args(args, self)?;
        build_prepared_root_with_constructor(
            self,
            args,
            wake_scope,
            root,
            App::from_scoped_parts_with_custody,
        )
    }

    /// Controlled counterpart to [`Self::build_prepared_root`].
    ///
    /// # Errors
    /// Returns launch, storage, root registration, or History installation failures.
    #[cfg(all(feature = "native", any(test, feature = "test-support")))]
    pub fn build_controlled_prepared_root_for_test(
        &self,
        args: &LaunchArgs,
        wake_scope: crate::WakeScope,
        root: Box<dyn Widget>,
    ) -> Result<App, LaunchError> {
        validate_spec_args(args, self)?;
        build_prepared_root_with_constructor(
            self,
            args,
            wake_scope,
            root,
            App::from_scoped_parts_with_controlled_custody,
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
            #[cfg(not(target_arch = "wasm32"))]
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
            component_actions: None,
            component_locales: &[],
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
            #[cfg(not(target_arch = "wasm32"))]
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
            component_actions: None,
            component_locales: &[],
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
    #[cfg(not(target_arch = "wasm32"))]
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

    /// Override how the live app opens its persisted state; the runner still
    /// owns autosave and the live event loop.
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

    #[cfg(feature = "native")]
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

    /// A fresh instance of the live root for History's read-only preview.
    #[cfg(feature = "native")]
    fn history_preview_root(
        &self,
        args: &LaunchArgs,
    ) -> impl Fn() -> Result<Box<dyn Widget>, String> + 'static {
        let live_root = self.live_root;
        let live_root_with_args = self.live_root_with_args;
        let args = args.clone();
        move || match (live_root_with_args, live_root) {
            (Some(factory), _) => factory(&args).map_err(|error| error.to_string()),
            (None, Some(factory)) => Ok(factory()),
            (None, None) => Err("launch specification has no live root".into()),
        }
    }

    #[cfg(feature = "native")]
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

/// One named screenshot pose for a legacy app that is not yet on
/// [`crate::framework::ResourceApp`]: a label for `--scenario` and the
/// factory that builds that pose.
pub struct ShotScenario {
    pub name: &'static str,
    pub build: fn(&LaunchArgs) -> Result<Box<dyn Widget>, LaunchError>,
}

/// Build the widget for `args.scenario` from a pose table.
///
/// `default` is used when `--scenario` is omitted (plain `--shots`).
/// Declaring the names on [`LaunchSpec::scenarios`] still belongs on the
/// spec so an unknown name is rejected before this runs.
///
/// # Errors
/// Propagates the chosen factory's error. An undeclared scenario name
/// is already refused by [`LaunchSpec`] validation.
pub fn build_shot_scenario(
    args: &LaunchArgs,
    table: &[ShotScenario],
    default: fn(&LaunchArgs) -> Result<Box<dyn Widget>, LaunchError>,
) -> Result<Box<dyn Widget>, LaunchError> {
    let Some(name) = args.scenario.as_deref() else {
        return default(args);
    };
    for pose in table {
        if pose.name == name {
            return (pose.build)(args);
        }
    }
    // Validation should have rejected unknown names; keep a hard error
    // so a table/spec drift cannot silently fall through to the default.
    Err(LaunchError::Scenario(format!(
        "unknown screenshot scenario {name:?}; expected one of {}",
        table
            .iter()
            .map(|pose| format!("{:?}", pose.name))
            .collect::<Vec<_>>()
            .join(", ")
    )))
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

    /// Whether the owners are reachable from this thread.
    #[must_use]
    pub fn is_reachable(&self) -> bool {
        self.here()
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
    #[cfg(all(unix, feature = "native"))]
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
                    driver.render_window_png(out);
                } else if let Some(directory) = &args.responsive_matrix {
                    for (name, viewport) in RESPONSIVE_VIEWPORTS {
                        driver.resize_component_capture(viewport);
                        driver.render_window_png(
                            &directory.join(format!("{}-{name}.png", spec.app_id)),
                        );
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
    let resumed_live = std::env::var_os("TILO_RUNTIME_RESUME_LIVE").as_deref()
        == Some(std::ffi::OsStr::new(spec.app_id))
        && runtime_portal::service_fd("collab").is_some()
        && runtime_portal::service_fd("identity").is_some();
    if !resumed_live && let Some(handler) = spec.command_handler {
        // The parsed command entrypoint selects local construction only for
        // this invocation; a handler falling through cannot qualify live launch.
        let selected_local = selected_local_construction()?;
        let handled = handler(&args)?;
        drop(selected_local);
        if handled {
            return Ok(());
        }
    }
    let _selected_local =
        (args.shots.is_some() || args.responsive_matrix.is_some() || args.native_audit)
            .then(selected_local_construction)
            .transpose()?;

    if let Some(out) = &args.shots {
        if out.is_dir() {
            return Err(LaunchError::Operation(format!(
                "--shots writes one PNG file, but {} is a directory; name a file \
                 (e.g. {}) or use --responsive-matrix DIR for a set of shots",
                out.display(),
                out.join(format!("{}.png", spec.app_id)).display()
            )));
        }
        if let Some(handler) = spec.shot_handler {
            handler(&args)?;
            return Ok(());
        }
        let root = spec.screenshot_root(&args)?;
        let viewport = args.viewport.unwrap_or_else(|| (spec.shot_viewport)(&args));
        let harness = build_history_capture(spec, &args, root, viewport)?;
        harness.render_window_png(out);
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
                harness.render_window_png(&out);
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
        if native_services {
            App::from_component_with_native_services(component, inputs)
        } else {
            App::from_component(component, inputs)
        }
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
    if spec.legacy_setup_override {
        return Err(LaunchError::IncompatibleComponentSetup);
    }
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
    if let Some(construction) = &construction {
        construction
            .bind_component(&prepared.context.callback_binding())
            .map_err(LaunchError::Operation)?;
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

#[cfg(feature = "native")]
fn build_history_capture(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    root: Box<dyn Widget>,
    viewport: (u32, u32),
) -> Result<FrameHarness, LaunchError> {
    let registration = crate::history_mode::capability::history_registration(root.as_ref())
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

#[cfg(feature = "native")]
struct PreparedLegacyLaunch {
    scope: crate::wake::WakeScope,
    inputs: LegacyLaunchInputs,
}

#[cfg(feature = "native")]
fn prepare_legacy_launch(
    spec: &LaunchSpec,
    args: &LaunchArgs,
) -> Result<PreparedLegacyLaunch, LaunchError> {
    prepare_legacy_launch_in_scope(spec, args, crate::wake::WakeScope::new())
}

#[cfg(feature = "native")]
fn prepare_legacy_launch_in_scope(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    scope: crate::wake::WakeScope,
) -> Result<PreparedLegacyLaunch, LaunchError> {
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
        crate::collaboration::native_initial::check_current().map_err(LaunchError::Operation)?;
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

#[cfg(feature = "native")]
fn selected_local_construction()
-> Result<crate::collaboration::native_initial::NativeConstruction, LaunchError> {
    crate::collaboration::native_initial::NativeConstruction::enter(
        crate::collaboration::CollaborationManager::disabled(),
    )
    .map_err(LaunchError::Operation)
}

/// Exact production branch, testable without `process::exit` or a native window.
#[cfg(feature = "native")]
fn dispatch_legacy_headless(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    headless: &headless::Spec,
) -> Result<i32, LaunchError> {
    validate_spec_args(args, spec)?;
    let _selected_local = selected_local_construction()?;
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

#[cfg(feature = "native")]
fn legacy_history_registration(
    spec: &LaunchSpec,
    inputs: &LegacyLaunchInputs,
) -> Result<Option<crate::history_mode::HistoryRegistration>, LaunchError> {
    if let Some(registration) =
        crate::history_mode::capability::history_registration(inputs.root.as_ref())
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

#[cfg(feature = "native")]
fn build_native_app(spec: &LaunchSpec, args: &LaunchArgs) -> Result<App, LaunchError> {
    build_native_app_with_constructor(spec, args, App::from_scoped_parts_with_custody)
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
    crate::collaboration::CollaborationManager::for_native_launch().map_err(LaunchError::Operation)
}

#[cfg(feature = "native")]
fn build_native_app_with_constructor(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    construct: impl FnOnce(crate::wake::WakeScope, State, Box<dyn Widget>, crate::ControlDomain) -> App,
) -> Result<App, LaunchError> {
    let scope = crate::wake::WakeScope::new();
    let _entry = scope.enter();
    let manager = native_launch_manager(spec, args)?;
    build_native_app_with_manager(spec, args, scope, manager, construct)
}

#[cfg(feature = "native")]
fn build_native_app_with_manager(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    scope: crate::wake::WakeScope,
    manager: crate::collaboration::CollaborationManager,
    construct: impl FnOnce(crate::wake::WakeScope, State, Box<dyn Widget>, crate::ControlDomain) -> App,
) -> Result<App, LaunchError> {
    let construction = crate::collaboration::native_initial::NativeConstruction::enter(manager)
        .map_err(LaunchError::Operation)?;
    let prepared = prepare_legacy_launch_in_scope(spec, args, scope)?;
    construction
        .state_opened()
        .map_err(LaunchError::Operation)?;
    finalize_native_app_with_constructor(
        spec,
        args,
        prepared.scope,
        prepared.inputs,
        crate::ControlDomain::outer(),
        construct,
    )
}

#[cfg(feature = "native")]
fn build_prepared_root_with_constructor(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    scope: crate::wake::WakeScope,
    root: Box<dyn Widget>,
    construct: impl FnOnce(crate::wake::WakeScope, State, Box<dyn Widget>, crate::ControlDomain) -> App,
) -> Result<App, LaunchError> {
    let _entry = scope.enter();
    let manager = native_launch_manager(spec, args)?;
    build_prepared_root_with_manager(spec, args, scope, root, manager, construct)
}

#[cfg(feature = "native")]
fn build_prepared_root_with_manager(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    scope: crate::wake::WakeScope,
    root: Box<dyn Widget>,
    manager: crate::collaboration::CollaborationManager,
    construct: impl FnOnce(crate::wake::WakeScope, State, Box<dyn Widget>, crate::ControlDomain) -> App,
) -> Result<App, LaunchError> {
    let _entry = scope.enter();
    let construction =
        crate::collaboration::native_initial::NativeConstruction::enter_prebuilt(manager)
            .map_err(LaunchError::Operation)?;
    let (state, autosave) = spec.open_state_with_args.map_or_else(
        || (spec.open_state)(spec.app_id),
        |open| open(spec.app_id, args),
    );
    construction
        .state_opened()
        .map_err(LaunchError::Operation)?;
    finalize_native_app_with_constructor(
        spec,
        args,
        scope,
        LegacyLaunchInputs::new(state, root, autosave),
        crate::ControlDomain::outer(),
        construct,
    )
}

#[cfg(feature = "native")]
fn finalize_native_app_with_constructor(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    scope: crate::wake::WakeScope,
    inputs: LegacyLaunchInputs,
    domain: crate::ControlDomain,
    construct: impl FnOnce(crate::wake::WakeScope, State, Box<dyn Widget>, crate::ControlDomain) -> App,
) -> Result<App, LaunchError> {
    inputs
        .validate_persistence_ownership()
        .map_err(LaunchError::Operation)?;
    let LegacyLaunchInputs {
        state,
        root,
        autosave,
        records,
        registry,
        persistence,
    } = inputs;
    let root_history = crate::history_mode::capability::history_registration(root.as_ref())
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    // A second root may only be built where the spec builds it and the host,
    // not the root, owns persistence: a preview must never open a store.
    let preview_root = (spec.legacy_preparer.is_none()
        && matches!(persistence, LegacyPersistencePolicy::Host))
    .then(|| spec.history_preview_root(args));
    let mut app = crate::collaboration::native_initial::transfer_to_app(|| {
        construct(scope, state, root, domain)
    })
    .map_err(LaunchError::Operation)?
    .with_autosave(autosave)
    .with_legacy_persistence(persistence);
    if let Some(preview_root) = preview_root {
        app = app.with_history_preview_root(preview_root);
    }
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
    crate::collaboration::native_initial::check_current().map_err(LaunchError::Operation)?;
    Ok(app)
}

#[cfg(feature = "native")]
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
    let environment_scale = std::env::var("TILO_SHOTS_TEXT_SCALE")
        .or_else(|_| std::env::var("TILO_SHOTS_TEXT_SCALE"))
        .ok();
    let theme = theme.with_text_scale(shot_text_scale(
        args.text_scale,
        environment_scale.as_deref(),
    ));
    // Screenshot review must use the same motion policy as a real app.  The
    // headless harness does not construct `ui::App`, where this environment
    // preference is normally read, so thread it onto the shot theme here.
    theme.with_reduce_motion(shot_motion_reduced())
}

/// The same capture presentation for app-specific fixture drivers.
#[must_use]
pub fn capture_theme(text_scale: Option<f32>) -> Theme {
    let theme = std::env::var("TILO_SHOTS_THEME")
        .ok()
        .and_then(|name| Theme::named(&name))
        .unwrap_or_else(Theme::dark);
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
        responsive_matrix: None,
        viewport: None,
        scenario: None,
        headless: None,
        native_audit: false,
        pseudo_locale: false,
        text_scale: None,
        theme: None,
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
        if parsed.theme.is_some() {
            return Err(LaunchError::ShotsOnly("--theme".to_owned()));
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
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn setup_and_initialization_failures_recover_or_report_operationally() {
        // Case `a_setup_error_crosses_threads_and_its_recovery_returns_home`.
        {
            let temp = tempfile::tempdir().unwrap();
            let records = crate::record::RecordOwners::new(temp.path().to_path_buf())
                .open("so.tilo.launch-fixture", "workspace")
                .unwrap();
            let checkout = crate::record::RecordCheckout::new(
                crate::record::RecordId::minted(),
                crate::record::RecordKind::new("launch.fixture"),
            );
            let error = LaunchError::ComponentSetup {
                source: Box::new(LaunchError::Operation("factory failed".into())),
                record: Box::new(checkout.clone()),
                recovery: LaunchRecovery::new(Some(records), Vec::new()),
            };
            // Off its thread the error is ordinary data: it displays, chains its
            // source and boxes as a thread-safe error, but lends no owner.
            let error = std::thread::spawn(move || {
                let LaunchError::ComponentSetup { recovery, .. } = &error else {
                    unreachable!()
                };
                assert!(!recovery.is_reachable());
                assert!(recovery.record_store().is_none());
                let boxed: Box<dyn std::error::Error + Send + Sync> = Box::new(error);
                assert_eq!(
                    boxed.to_string(),
                    "application setup failed: factory failed"
                );
                assert_eq!(
                    boxed.source().map(ToString::to_string).as_deref(),
                    Some("factory failed")
                );
                boxed
            })
            .join()
            .unwrap();
            let error = error.downcast::<LaunchError>().unwrap();
            let LaunchError::ComponentSetup {
                record, recovery, ..
            } = *error
            else {
                unreachable!()
            };
            assert_eq!(*record, checkout);
            assert!(recovery.is_reachable());
            let store = recovery.into_record_store().unwrap();
            assert!(store.is_some(), "the record store survives the round trip");
        }

        // Case `recovery_is_withheld_on_another_thread`.
        {
            let recovery = std::thread::spawn(|| LaunchRecovery::reports(Vec::new()))
                .join()
                .unwrap();
            assert!(!recovery.is_reachable());
            assert!(recovery.into_parts().is_err());
        }

        // Case `argument_aware_root_propagates_initialization_failure`.
        {
            let args =
                parse_args_extended(&strings(&["app", "broken.qdoc"]), &[], &[], &[], &[], 1)
                    .unwrap();
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

        // Case `operational_failures_are_not_reported_as_cli_usage_errors`.
        {
            assert_eq!(LaunchError::Operation("failed".into()).exit_code(), 1);
            assert_eq!(LaunchError::UnknownArgument("--bad".into()).exit_code(), 2);
        }
    }

    #[test]
    fn preparers_and_loaders_gate_dispatch_without_default_fallback() {
        // Case `legacy_preparer_and_live_handler_are_rejected_before_either_runs`.
        {
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

        // Case `headless_dispatch_uses_configured_migration_loader`.
        {
            let fixture = LaunchFixtureEnv::new();
            let dump = fixture._directory.path().join("dump.json");
            let args = headless_fixture_args(None, &dump);
            let spec = LaunchSpec::new("so.tilo.launch-fixture", dummy_root)
                .open_state(migrated_fixture_open);
            assert_eq!(
                dispatch_legacy_headless(&spec, &args, args.headless.as_ref().unwrap()).unwrap(),
                0
            );
            let state: serde_json::Value =
                serde_json::from_slice(&std::fs::read(dump).unwrap()).unwrap();
            assert_eq!(state["migration_marker"], "configured-loader");
        }

        // Case `prepared_file_state_and_original_room_index_reach_real_headless_dispatch`.
        {
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
                    .as_str(),
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

        // Case `native_constructor_consumes_the_same_fallible_preparer`.
        {
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
            let app =
                build_native_app_with_constructor(&spec, &args, |scope, state, root, domain| {
                    assert_eq!(
                        state
                            .doc
                            .get(automerge::ROOT, "file_marker")
                            .unwrap()
                            .unwrap()
                            .0
                            .as_str(),
                        Some("native-file-content")
                    );
                    App::from_scoped_parts_with_controlled_custody(scope, state, root, domain)
                })
                .unwrap();
            drop(app);
        }

        // Case `widget_owned_persistence_rejects_mixed_record_owner_before_either_runner`.
        {
            let spec = LaunchSpec::new("so.tilo.launch-fixture", dummy_root)
                .prepare_legacy_with_args(|_| {
                    Ok(LegacyLaunchInputs::new(
                        State::new(),
                        dummy_root(),
                        crate::persist::DocWriter::disabled("owned fixture"),
                    )
                    .with_records(
                        crate::record::RecordSet::in_memory(),
                        automerge::AutoCommit::new(),
                    )
                    .with_widget_owned_persistence())
                });
            let args = parse_args_extended(&strings(&["app"]), &[], &[], &[], &[], 0).unwrap();
            assert!(
                matches!(build_native_app_with_constructor(&spec, &args, |_, _, _, _| panic!("invalid ownership must fail before native construction")), Err(LaunchError::Operation(message)) if message.contains("cannot also use"))
            );
            assert!(
                matches!(dispatch_legacy_headless(&spec, &args, &headless::Spec::default()), Err(LaunchError::Operation(message)) if message.contains("cannot also use"))
            );
        }

        // Case `headless_custom_namespace_and_disabled_writer_fail_without_default_fallback`.
        {
            let fixture = LaunchFixtureEnv::new();
            for open in [
                wrong_namespace_open as OpenState,
                scoped_open_state as OpenState,
            ] {
                let dump = fixture._directory.path().join("must-not-exist.json");
                let args = headless_fixture_args(None, &dump);
                let spec = LaunchSpec::new("so.tilo.launch-fixture", dummy_root).open_state(open);
                assert_eq!(
                    dispatch_legacy_headless(&spec, &args, args.headless.as_ref().unwrap())
                        .unwrap(),
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

        // Case `failed_file_preparer_stops_real_dispatch_before_output_or_fallback`.
        {
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

        // Case `app_can_replace_only_the_persistence_loader`.
        {
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
    }

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
    fn migrated_fixture_open(app_id: &'static str) -> (State, crate::persist::DocWriter) {
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
    fn wrong_namespace_open(_: &'static str) -> (State, crate::persist::DocWriter) {
        crate::persist::open_for("so.tilo.other-fixture").unwrap()
    }

    static CUSTOM_SHOT_RAN: AtomicBool = AtomicBool::new(false);
    static CUSTOM_LIVE_RAN: AtomicBool = AtomicBool::new(false);
    static CUSTOM_COMMAND_RAN: AtomicBool = AtomicBool::new(false);

    thread_local! {
        static LAUNCH_WAKE_CAPTURES: std::cell::RefCell<Vec<crate::WakeHandle>> =
            const { std::cell::RefCell::new(Vec::new()) };
        static PREPARED_COUNTS: std::cell::RefCell<(usize, usize)> =
            const { std::cell::RefCell::new((0, 0)) };
        static FOREIGN_OPENED_STATE: std::cell::RefCell<Option<(State, crate::persist::DocWriter)>> =
            const { std::cell::RefCell::new(None) };
    }

    fn capture_launch_wake() {
        LAUNCH_WAKE_CAPTURES.with(|captures| captures.borrow_mut().push(crate::WakeHandle::new()));
    }

    fn scoped_open_state(_: &'static str) -> (State, crate::persist::DocWriter) {
        capture_launch_wake();
        (
            State::new(),
            crate::persist::DocWriter::disabled("wake routing test"),
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
    fn launch_spec_options_scenarios_and_pipelines_follow_standard_dispatch() {
        // Case `app_command_can_finish_before_window_startup`.
        {
            CUSTOM_COMMAND_RAN.store(false, Ordering::SeqCst);
            let spec = LaunchSpec::new("so.tilo.test", dummy_root)
                .allowed_live_flags(&["--utility"])
                .command_handler(custom_command);
            run_with_args(&spec, &strings(&["app", "--utility"])).unwrap();
            assert!(CUSTOM_COMMAND_RAN.load(Ordering::SeqCst));
        }

        // Case `privileged_app_can_reject_headless_driving`.
        {
            let spec = LaunchSpec::new("so.tilo.test", dummy_root).disable_headless();
            let result = run_with_args(&spec, &strings(&["app", "--headless-drive", "{}"]));
            assert!(matches!(result, Err(LaunchError::HeadlessUnsupported)));
        }

        // Case `scenarios_must_be_declared_by_the_app`.
        {
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

        // Case `ordinary_launch_spec_reuses_live_root_for_screenshots`.
        {
            let args =
                parse_args(&strings(&["app", "--shots=/tmp/ordinary.png"]), &[], &[]).unwrap();
            let spec = LaunchSpec::new("so.tilo.test", dummy_root);
            let root = spec
                .screenshot_root(&args)
                .expect("default screenshot root");
            assert_eq!(root.name(), "Button");
        }

        // Case `app_specific_options_accept_both_forms_and_are_shots_only`.
        {
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

        // Case `live_options_and_positionals_are_validated_then_exposed`.
        {
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

        // Case `custom_shot_pipeline_uses_standard_mode_dispatch`.
        {
            CUSTOM_SHOT_RAN.store(false, Ordering::SeqCst);
            let spec = LaunchSpec::new("so.tilo.test", dummy_root)
                .shot_handler(custom_shot)
                .live_handler(unexpected_live);
            run_with_args(&spec, &strings(&["app", "--shots", "/tmp/custom.png"])).unwrap();
            assert!(CUSTOM_SHOT_RAN.load(Ordering::SeqCst));
        }

        // Case `custom_live_pipeline_runs_after_standard_mode_dispatch`.
        {
            CUSTOM_LIVE_RAN.store(false, Ordering::SeqCst);
            let spec = LaunchSpec::new("dev.tilo.test", dummy_root).live_handler(custom_live);
            run_with_args_with_attachment(&spec, &strings(&["app"]), |_| Ok(())).unwrap();
            assert!(CUSTOM_LIVE_RAN.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn history_declaration_gate_and_managed_launch_install_declared_history() {
        // Case `history_declaration_gate_rejects_omission_for_standard_and_custom_specs`.
        {
            fn custom(_: &LaunchArgs) -> Result<(), LaunchError> {
                panic!("validation cannot launch");
            }
            for spec in [
                LaunchSpec::new("dev.tilo.test", dummy_root),
                LaunchSpec::new("dev.tilo.test", dummy_root).live_handler(custom),
                LaunchSpec::new_with_args("dev.tilo.test", |_| {
                    panic!("validation cannot construct")
                }),
                LaunchSpec::from_definition(
                    crate::application_test_support::runner_probe_definition(),
                ),
            ] {
                assert_eq!(
                    spec.validate_history_declaration(),
                    Err(crate::history_mode::HistoryRegistrationError::Missing)
                );
            }
        }

        // Case `history_declaration_gate_accepts_matching_external_policy_and_refuses_foreign_identity`.
        {
            static HISTORY: crate::history_mode::HistoryDefinition =
                crate::history_mode::HistoryDefinition {
                    app_id: "dev.tilo.test",
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
                LaunchSpec::new("dev.tilo.other", dummy_root)
                    .history(&HISTORY)
                    .validate_history_declaration(),
                Err(crate::history_mode::HistoryRegistrationError::ForeignSource)
            );
        }

        // Case `managed_standard_native_launch_installs_history_without_detaching_wake_scope`.
        {
            static HISTORY: crate::history_mode::HistoryDefinition =
                crate::history_mode::HistoryDefinition {
                    app_id: "dev.tilo.wake-test",
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

        // Case `photos_argument_global_opener_reaches_managed_production_constructor`.
        {
            use automerge::{ReadDoc, transaction::Transactable};
            static HISTORY: crate::history_mode::HistoryDefinition =
                crate::history_mode::HistoryDefinition {
                    app_id: "dev.tilo.argument-test",
                    scopes: &[crate::history_mode::HistoryScopeDefinition {
                        key: "photos",
                        title: "Photos",
                        presentation: crate::history_mode::HistoryPresentation::Domain {
                            create: crate::history_mode::test_preview,
                        },
                        content_schema: None,
                    }],
                };
            fn open(_: &'static str, args: &LaunchArgs) -> (State, crate::persist::DocWriter) {
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
                    crate::persist::DocWriter::disabled("detached argument opener test"),
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
                *app = owned.with_autosave(crate::persist::DocWriter::disabled("replacement"));
            }
            for raw in [
                strings(&["app"]),
                strings(&["app", "--open", "selected.png"]),
            ] {
                let args = parse_args_extended(&raw, &[], &[], &[], &["--open"], 0).unwrap();
                let spec = LaunchSpec::new(HISTORY.app_id, dummy_root)
                    .open_app_global_state_with_args(open)
                    .history(&HISTORY);
                let mut app = build_native_app_with_constructor(
                    &spec,
                    &args,
                    |scope, state, root, domain| {
                        assert_eq!(
                            state
                                .doc
                                .get(automerge::ROOT, "selected_argument")
                                .unwrap()
                                .unwrap()
                                .0
                                .as_str(),
                            Some(args.value("--open").unwrap_or("saved-state"))
                        );
                        App::from_scoped_parts_with_controlled_custody(scope, state, root, domain)
                    },
                )
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
                        .as_str(),
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
                        matches!(build_native_app_with_constructor(&rejected,&args,App::from_scoped_parts_with_controlled_custody),Err(LaunchError::Operation(message)) if message.contains("storage provenance"))
                    );
                }
            }
        }

        // Case `managed_launch_refuses_undeclared_custom_storage_and_post_configuration_replacement`.
        {
            static HISTORY: crate::history_mode::HistoryDefinition =
                crate::history_mode::HistoryDefinition {
                    app_id: "dev.tilo.wake-test",
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
                *app = owned.with_autosave(crate::persist::DocWriter::disabled("replacement"));
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
                    App::from_scoped_parts_with_controlled_custody,
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
            let mut capture =
                build_history_capture(&spec, &args, scoped_root(), (900, 650)).unwrap();
            assert!(capture.targets().contains_key("history-open"));
            capture.click_named("history-open").unwrap();
            assert!(capture.targets().contains_key("activity_timeline"));
        }
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "signature is fixed by LaunchSpec::command_handler"
    )]
    fn capture_command_wake(_: &LaunchArgs) -> Result<bool, LaunchError> {
        capture_launch_wake();
        Ok(true)
    }

    fn fallible_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
        Err(LaunchError::Operation(format!(
            "cannot open {}",
            args.positionals().first().map_or("nothing", String::as_str)
        )))
    }

    fn custom_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
        if args.shots.as_deref() != Some(std::path::Path::new("/tmp/custom.png")) {
            return Err(LaunchError::Operation("wrong shot path".into()));
        }
        CUSTOM_SHOT_RAN.store(true, Ordering::SeqCst);
        Ok(())
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
    fn runtime_attachment_and_capture_follow_the_launch_policy() {
        // Case `native_runtime_attachment_precedes_default_custom_and_definition_construction`.
        {
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
                LaunchSpec::new_with_args("dev.tilo.test", fail_root),
                LaunchSpec::new("dev.tilo.test", dummy_root).live_handler(unexpected_live),
                LaunchSpec::from_definition(
                    crate::application_test_support::runner_probe_definition(),
                )
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

        // Case `diagnostics_and_handled_commands_never_attach_runtime`.
        {
            fn fail_prepare(
                _: &crate::application::ApplicationDefinition,
                _: &LaunchArgs,
            ) -> Result<crate::application::PreparedComponentInputs, LaunchError> {
                Err(LaunchError::Operation("detached preparation".into()))
            }
            let spec = LaunchSpec::from_definition(
                crate::application_test_support::runner_probe_definition(),
            )
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
            let spec = LaunchSpec::new("dev.tilo.test", dummy_root).command_handler(custom_command);
            run_with_args_with_attachment(&spec, &strings(&["app"]), |_| {
                panic!("handled command attached runtime")
            })
            .unwrap();
        }

        // Case `explicit_runtime_attachment_policy_skips_bootstrap_for_native_launch`.
        {
            let spec = LaunchSpec::new("dev.tilo.test", dummy_root)
                .runtime_attachment(|_| false)
                .live_handler(unexpected_live);
            let result = run_with_args_with_attachment(&spec, &strings(&["app"]), |_| {
                Err(std::io::Error::other("unexpected attachment"))
            });
            assert!(
                matches!(result, Err(LaunchError::Operation(message)) if message == "screenshot dispatch reached the live handler")
            );
        }

        // Case `standard_live_factory_and_configuration_capture_the_same_app_lifetime`.
        {
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
                App::from_scoped_parts_with_controlled_custody,
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

        // Case `cli_utility_capture_stays_detached_under_a_host_callback`.
        {
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
        if let Ok(mode) = std::env::var("TILO_LAUNCH_REEXEC_TEST") {
            let spec = LaunchSpec::new("dev.tilo.reexec-test", dummy_root)
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
                .env("TILO_LAUNCH_REEXEC_TEST", mode)
                .env("TILO_SVC_COLLAB_FD", fds[0].to_string())
                .env("TILO_SVC_IDENTITY_FD", fds[1].to_string())
                .env_remove("TILO_RUNTIME_RESUME_LIVE");
            if mode == "resumed" || mode == "descendant" {
                child.env("TILO_RUNTIME_RESUME_LIVE", "dev.tilo.reexec-test");
            }
            if mode == "descendant" {
                child.env(
                    "TILO_RUNTIME_CONNECTION_OWNER_PID",
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
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "{mode}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            // libtest exits 0 when `--exact` selects nothing; require the child ran.
            assert!(
                stdout.contains("test result: ok. 1 passed"),
                "{mode}: the child ran no test: {stdout}"
            );
        }
    }

    fn custom_open_state(app_id: &'static str) -> (State, crate::persist::DocWriter) {
        crate::persist::open(app_id)
    }

    static CHECKED_HISTORY: crate::history_mode::HistoryDefinition =
        crate::history_mode::HistoryDefinition {
            app_id: "so.tilo.files-like",
            scopes: &[crate::history_mode::HistoryScopeDefinition {
                key: "file",
                title: "File",
                presentation: crate::history_mode::HistoryPresentation::CheckedFiles {
                    create: checked_preview,
                },
                content_schema: None,
            }],
        };

    fn checked_preview(
        _: crate::history_mode::checked_files::FilePreviewInput,
    ) -> Result<Box<dyn crate::history_mode::HistoryPreview>, String> {
        Err("preview is not used by launch ownership tests".into())
    }

    fn foreign_opened_state(_: &'static str) -> (State, crate::persist::DocWriter) {
        FOREIGN_OPENED_STATE.with(|opened| {
            opened
                .borrow_mut()
                .take()
                .expect("foreign saver fixture is installed")
        })
    }

    enum RegistrationProbe {
        Checked(crate::history_mode::checked_files::FileHistorySource),
        Malformed,
    }

    struct RegistrationRoot {
        inner: Box<dyn Widget>,
        registration: RegistrationProbe,
    }

    impl RegistrationRoot {
        fn checked(source: crate::history_mode::checked_files::FileHistorySource) -> Self {
            Self {
                inner: Box::new(crate::widget::Text::new("checked root")),
                registration: RegistrationProbe::Checked(source),
            }
        }

        fn malformed() -> Self {
            Self {
                inner: Box::new(crate::widget::Text::new("malformed root")),
                registration: RegistrationProbe::Malformed,
            }
        }
    }

    impl Widget for RegistrationRoot {
        crate::provides!(dyn crate::history_mode::capability::DocumentHistory);

        crate::testing::delegate_widget_methods!(
            inner: sync,
            build_layout,
            apply_layout,
            draw,
            handle_event,
            get_rect
        );
        fn name(&self) -> &str {
            "Registration root"
        }
    }

    impl crate::history_mode::capability::DocumentHistory for RegistrationRoot {
        fn history_registration(
            &self,
        ) -> Result<
            Option<crate::history_mode::HistoryRegistration>,
            crate::history_mode::HistoryRegistrationError,
        > {
            match &self.registration {
                RegistrationProbe::Checked(source) => {
                    crate::history_mode::HistoryRegistration::new(
                        &CHECKED_HISTORY,
                        vec![
                            crate::history_mode::HistorySourceRegistration::CheckedFiles {
                                scope: "file",
                                source: source.clone(),
                            },
                        ],
                    )
                    .map(Some)
                }
                RegistrationProbe::Malformed => {
                    Err(crate::history_mode::HistoryRegistrationError::ForeignSource)
                }
            }
        }
        fn document_history_definition(
            &self,
        ) -> Option<&'static crate::history_mode::HistoryDefinition> {
            match &self.registration {
                RegistrationProbe::Checked(_) => Some(&CHECKED_HISTORY),
                RegistrationProbe::Malformed => None,
            }
        }
    }

    fn replace_final_root(app: &mut App) {
        let replacement = App::from_scoped_parts_with_controlled_inputs(
            crate::WakeScope::new(),
            State::new(),
            dummy_root(),
        )
        .with_autosave(crate::persist::DocWriter::disabled("replacement"));
        let _old = std::mem::replace(app, replacement);
    }

    fn replace_installed_storage(app: &mut App) {
        let owned = std::mem::replace(
            app,
            App::from_scoped_parts_with_controlled_inputs(
                crate::WakeScope::new(),
                State::new(),
                dummy_root(),
            ),
        );
        *app = owned.with_autosave(crate::persist::DocWriter::disabled("replacement"));
    }

    fn poisoned_prepared_root() -> Box<dyn Widget> {
        PREPARED_COUNTS.with(|counts| counts.borrow_mut().0 += 1);
        Box::new(crate::widget::Text::new("wrong root"))
    }

    fn prepared_open_state(_: &'static str) -> (State, crate::persist::DocWriter) {
        PREPARED_COUNTS.with(|counts| counts.borrow_mut().1 += 1);
        capture_launch_wake();
        (
            State::new(),
            crate::persist::DocWriter::disabled("prepared launch test"),
        )
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_initial_launch_reads_before_custom_opener_and_factory_on_one_worker() {
        use automerge::{ReadDoc, transaction::Transactable};
        let _environment = LaunchFixtureEnv::new();
        let room = uuid::Uuid::new_v4();
        let mut _env_override = tilo_test_env::ScopedEnv::new();
        _env_override.set("TILO_WORKSPACE_ID", room.to_string());
        let mut history = automerge::AutoCommit::new();
        history
            .put(automerge::ROOT, "initial_history", "real committed content")
            .unwrap();
        history.commit();
        let (manager, joined) =
            crate::collaboration::CollaborationManager::native_initial_fixture_for_test(
                "so.tilo.launch-fixture",
                room,
                history.save(),
            );
        fn custom_open(app: &'static str) -> (State, crate::persist::DocWriter) {
            let (mut state, saver) = crate::persist::open(app);
            assert_eq!(
                state
                    .doc
                    .get(automerge::ROOT, "initial_history")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("real committed content"),
                "custom default initializer must receive actual history first"
            );
            state
                .doc
                .put(automerge::ROOT, "custom_default", "after history")
                .unwrap();
            state.doc.commit();
            (state, saver)
        }
        let spec = LaunchSpec::new("so.tilo.launch-fixture", dummy_root).open_state(custom_open);
        let args = parse_args_extended(&strings(&["app"]), &[], &[], &[], &[], 0).unwrap();
        let mut factory_called = false;
        let app = PreparedNativeLaunch::with_manager(&spec, &args, manager)
            .unwrap()
            .build_root(|| {
                factory_called = true;
                Ok(dummy_root())
            })
            .unwrap();
        assert!(factory_called);
        assert!(
            app.collaboration_is_connected(),
            "App must own the prepared worker"
        );
        assert_eq!(
            joined(),
            1,
            "prepare and App must transfer the same inherited transport"
        );
        drop(app);
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_initial_launch_retains_record_bridge_before_record_defaults() {
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
            .put(automerge::ROOT, "initial_record", "original graph")
            .unwrap();
        history.commit();
        let expected = history.get_heads();
        let (manager, joined) =
            crate::collaboration::CollaborationManager::native_initial_fixture_for_test(
                "so.tilo.launch-fixture",
                room,
                history.save(),
            );
        let spec = LaunchSpec::new("so.tilo.launch-fixture", dummy_root);
        let args = parse_args_extended(&strings(&["app"]), &[], &[], &[], &[], 0).unwrap();
        let mut retained = None;
        let app = PreparedNativeLaunch::with_manager(&spec, &args, manager)
            .unwrap()
            .build_root(|| {
                retained = Some(crate::record::RecordStore::open("so.tilo.launch-fixture"));
                Ok(dummy_root())
            })
            .unwrap();
        let records = retained.unwrap();
        // The construction TLS has ended. The actual retained store still
        // borrows the App's one worker, before record initialization can run.
        assert!(records.open_record(&want));
        records
            .read_checked(&want.id, |doc| {
                assert_eq!(
                    doc.get(automerge::ROOT, "initial_record")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str(),
                    Some("original graph")
                );
                for head in &expected {
                    assert!(doc.get_change_by_hash(head).is_some());
                }
            })
            .unwrap();
        assert_eq!(joined(), 2, "primary and exact record share one worker");
        drop(app);
        let other = crate::record::RecordCheckout::new(
            crate::record::RecordId::minted(),
            want.kind.clone(),
        );
        assert!(
            !records.open_record(&other),
            "closed actual worker cannot fall back to empty record defaults"
        );
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
    fn native_initial_launch_standard_builder_reads_before_legacy_callbacks() {
        use automerge::{ReadDoc, transaction::Transactable};
        let _environment = LaunchFixtureEnv::new();
        let room = uuid::Uuid::new_v4();
        let mut _env_override = tilo_test_env::ScopedEnv::new();
        _env_override.set("TILO_WORKSPACE_ID", room.to_string());
        let mut history = automerge::AutoCommit::new();
        history
            .put(automerge::ROOT, "standard_initial", "before callback")
            .unwrap();
        history.commit();
        let (manager, joined) =
            crate::collaboration::CollaborationManager::native_initial_fixture_for_test(
                "so.tilo.launch-fixture",
                room,
                history.save(),
            );
        fn prepare(_: &LaunchArgs) -> Result<LegacyLaunchInputs, LaunchError> {
            let records = crate::record::RecordSet::open("so.tilo.launch-fixture");
            let (state, saver) =
                crate::persist::open_room("so.tilo.launch-fixture", records.home());
            assert_eq!(
                state
                    .doc
                    .get(automerge::ROOT, "standard_initial")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("before callback")
            );
            Ok(LegacyLaunchInputs::new(state, dummy_root(), saver))
        }
        let spec =
            LaunchSpec::new("so.tilo.launch-fixture", dummy_root).prepare_legacy_with_args(prepare);
        let args = parse_args_extended(&strings(&["app"]), &[], &[], &[], &[], 0).unwrap();
        let app = build_native_app_with_manager(
            &spec,
            &args,
            crate::wake::WakeScope::new(),
            manager,
            App::from_scoped_parts_with_custody,
        )
        .unwrap();
        assert!(app.collaboration_is_connected());
        assert_eq!(joined(), 1);
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
    fn native_initial_launch_rejects_opener_that_bypasses_required_read() {
        use automerge::transaction::Transactable;
        let _environment = LaunchFixtureEnv::new();
        let room = uuid::Uuid::new_v4();
        let mut history = automerge::AutoCommit::new();
        history.put(automerge::ROOT, "existing", "history").unwrap();
        history.commit();
        let (manager, _) =
            crate::collaboration::CollaborationManager::native_initial_fixture_for_test(
                "so.tilo.launch-fixture",
                room,
                history.save(),
            );
        let spec = LaunchSpec::new("so.tilo.launch-fixture", dummy_root).open_state(|_| {
            (
                State::new(),
                crate::persist::DocWriter::disabled("bypassing custom opener"),
            )
        });
        let args = parse_args_extended(&strings(&["app"]), &[], &[], &[], &[], 0).unwrap();
        let mut factory_called = false;
        let result = PreparedNativeLaunch::with_manager(&spec, &args, manager)
            .unwrap()
            .build_root(|| {
                factory_called = true;
                Ok(dummy_root())
            });
        assert!(
            result.is_err(),
            "actual Required read cannot be bypassed by a custom empty opener"
        );
        assert!(
            !factory_called,
            "defaults must not be constructed after bypass"
        );
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_initial_launch_rejects_standard_bypass_before_factory() {
        use automerge::transaction::Transactable;
        static CALLED: AtomicBool = AtomicBool::new(false);
        fn root() -> Box<dyn Widget> {
            CALLED.store(true, Ordering::SeqCst);
            dummy_root()
        }
        let _environment = LaunchFixtureEnv::new();
        CALLED.store(false, Ordering::SeqCst);
        let room = uuid::Uuid::new_v4();
        let mut history = automerge::AutoCommit::new();
        history.put(automerge::ROOT, "existing", "history").unwrap();
        history.commit();
        let (manager, _) =
            crate::collaboration::CollaborationManager::native_initial_fixture_for_test(
                "so.tilo.launch-fixture",
                room,
                history.save(),
            );
        let spec = LaunchSpec::new("so.tilo.launch-fixture", root)
            .open_state(|_| (State::new(), crate::persist::DocWriter::disabled("bypass")));
        let args = parse_args_extended(&strings(&["app"]), &[], &[], &[], &[], 0).unwrap();
        let result = build_native_app_with_manager(
            &spec,
            &args,
            crate::wake::WakeScope::new(),
            manager,
            App::from_scoped_parts_with_custody,
        );
        assert!(result.is_err());
        assert!(
            !CALLED.load(Ordering::SeqCst),
            "standard live root must not run after an unhydrated custom opener"
        );
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_initial_launch_rejects_already_built_root_before_storage() {
        use automerge::transaction::Transactable;
        static OPENED: AtomicBool = AtomicBool::new(false);
        fn open(_: &'static str) -> (State, crate::persist::DocWriter) {
            OPENED.store(true, Ordering::SeqCst);
            (
                State::new(),
                crate::persist::DocWriter::disabled("already constructed root"),
            )
        }
        let _environment = LaunchFixtureEnv::new();
        OPENED.store(false, Ordering::SeqCst);
        let room = uuid::Uuid::new_v4();
        let mut history = automerge::AutoCommit::new();
        history.put(automerge::ROOT, "existing", "history").unwrap();
        history.commit();
        let (manager, _) =
            crate::collaboration::CollaborationManager::native_initial_fixture_for_test(
                "so.tilo.launch-fixture",
                room,
                history.save(),
            );
        let spec = LaunchSpec::new("so.tilo.launch-fixture", dummy_root).open_state(open);
        let args = parse_args_extended(&strings(&["app"]), &[], &[], &[], &[], 0).unwrap();
        let result = build_prepared_root_with_manager(
            &spec,
            &args,
            crate::wake::WakeScope::new(),
            dummy_root(),
            manager,
            App::from_scoped_parts_with_custody,
        );
        assert!(
            result.is_err(),
            "already-built roots cannot be retrospectively authorized as hydrated construction"
        );
        assert!(!OPENED.load(Ordering::SeqCst));
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_initial_launch_nested_app_cannot_take_prepared_worker() {
        use automerge::transaction::Transactable;
        let _environment = LaunchFixtureEnv::new();
        let room = uuid::Uuid::new_v4();
        let mut _env_override = tilo_test_env::ScopedEnv::new();
        _env_override.set("TILO_WORKSPACE_ID", room.to_string());
        let mut history = automerge::AutoCommit::new();
        history.put(automerge::ROOT, "existing", "history").unwrap();
        history.commit();
        let (manager, joined) =
            crate::collaboration::CollaborationManager::native_initial_fixture_for_test(
                "so.tilo.launch-fixture",
                room,
                history.save(),
            );
        let spec = LaunchSpec::new("so.tilo.launch-fixture", dummy_root);
        let args = parse_args_extended(&strings(&["app"]), &[], &[], &[], &[], 0).unwrap();
        let mut child = None;
        let result = PreparedNativeLaunch::with_manager(&spec, &args, manager)
            .unwrap()
            .build_root(|| {
                let nested = App::from_scoped_parts_with_custody(
                    crate::wake::WakeScope::new(),
                    State::new(),
                    dummy_root(),
                    crate::ControlDomain::outer(),
                );
                assert!(
                    !nested.collaboration_is_connected(),
                    "factory-created child must not acquire the prepared primary worker"
                );
                child = Some(nested);
                Ok(dummy_root())
            });
        assert!(
            result.is_err(),
            "illegal transfer must prevent outer launch instead of creating another worker"
        );
        assert_eq!(joined(), 1);
        drop(child);
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
        let construction =
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
        construction
            .state_opened()
            .expect("canonical primary consumed the initial read");
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
    fn native_initial_launch_file_record_retains_required_bridge_without_writer() {
        use automerge::{ReadDoc, transaction::Transactable};
        let environment = LaunchFixtureEnv::new();
        let room = uuid::Uuid::new_v4();
        let mut _env_override = tilo_test_env::ScopedEnv::new();
        _env_override.set("TILO_WORKSPACE_ID", room.to_string());
        let want = crate::record::RecordCheckout::new(
            crate::record::RecordId::for_file(environment._directory.path().join("selected.md")),
            crate::record::RecordKind::new("document"),
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
            .put(automerge::ROOT, "retained", "file history")
            .unwrap();
        history.commit();
        let heads = history.get_heads();
        let (manager, _) =
            crate::collaboration::CollaborationManager::native_initial_fixture_for_test(
                "so.tilo.launch-fixture",
                room,
                history.save(),
            );
        let construction =
            crate::collaboration::native_initial::NativeConstruction::enter(manager).unwrap();
        let records = crate::record::RecordSet::for_local_files("so.tilo.launch-fixture").unwrap();
        let reader = records.file_history_reader().unwrap();
        assert!(
            reader.require_local_creation().is_err(),
            "Required file creation needs governor admission"
        );
        let capture = reader
            .read(&want)
            .unwrap()
            .expect("actual Required Existing history");
        let mut installed = automerge::AutoCommit::new();
        capture.install_into(&mut installed).unwrap();
        assert_eq!(
            installed
                .get(automerge::ROOT, "retained")
                .unwrap()
                .unwrap()
                .0
                .as_str(),
            Some("file history")
        );
        for head in heads {
            assert!(installed.get_change_by_hash(&head).is_some());
        }
        let package_document =
            app_state::DocStore::legacy_package_document_path("so.tilo.launch-fixture").unwrap();
        let data_home = package_document
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap();
        let local_cache = app_state::DocStore::room_record_dir_at(
            data_home,
            "so.tilo.launch-fixture",
            &room.to_string(),
            &want.id.storage_key(),
        )
        .unwrap();
        assert!(
            !local_cache.exists(),
            "file-owned history must not create a second canonical writer"
        );
        drop(construction);
        assert!(
            reader.read(&want).is_err(),
            "closed Required worker must never become local None"
        );
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_initial_launch_file_record_local_mode_is_explicit() {
        let environment = LaunchFixtureEnv::new();
        let want = crate::record::RecordCheckout::new(
            crate::record::RecordId::for_file(environment._directory.path().join("local.md")),
            crate::record::RecordKind::new("document"),
        );
        let records = crate::record::RecordSet::for_local_files("so.tilo.launch-fixture").unwrap();
        assert!(records.initial_file_history(&want).unwrap().is_none());
        records
            .file_history_reader()
            .unwrap()
            .require_local_creation()
            .unwrap();
    }

    #[test]
    fn native_initial_launch_file_record_unresolved_set_is_not_local() {
        let want = crate::record::RecordCheckout::new(
            crate::record::RecordId::minted(),
            crate::record::RecordKind::new("document"),
        );
        assert!(
            crate::record::RecordSet::in_memory()
                .initial_file_history(&want)
                .is_err(),
            "missing private mode selection cannot authorize local genesis"
        );
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_initial_launch_explicit_headless_selects_local_despite_inherited_fd() {
        use std::os::fd::AsRawFd;
        let fixture = LaunchFixtureEnv::new();
        let (socket, _other) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut _env_override = tilo_test_env::ScopedEnv::new();
        _env_override.set("TILO_SVC_COLLAB_FD", socket.as_raw_fd().to_string());
        fn open(app: &'static str) -> (State, crate::persist::DocWriter) {
            let files = crate::record::RecordSet::for_local_files(app)
                .expect("parser-selected local headless scope");
            files
                .file_history_reader()
                .unwrap()
                .require_local_creation()
                .unwrap();
            crate::persist::open(app)
        }
        assert!(crate::record::RecordSet::for_local_files("so.tilo.launch-fixture").is_err());
        let args =
            headless_fixture_args(None, &fixture._directory.path().join("headless-local.json"));
        let spec = LaunchSpec::new("so.tilo.launch-fixture", dummy_root).open_state(open);
        assert_eq!(
            dispatch_legacy_headless(&spec, &args, args.headless.as_ref().unwrap()).unwrap(),
            0
        );
        assert!(
            crate::record::RecordSet::for_local_files("so.tilo.launch-fixture").is_err(),
            "headless choice cannot leak into a later native launch"
        );
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_initial_launch_explicit_command_selects_local_despite_inherited_fd() {
        use std::os::fd::AsRawFd;
        let _fixture = LaunchFixtureEnv::new();
        let (socket, _other) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut _env_override = tilo_test_env::ScopedEnv::new();
        _env_override.set("TILO_SVC_COLLAB_FD", socket.as_raw_fd().to_string());
        fn command(_: &LaunchArgs) -> Result<bool, LaunchError> {
            let files = crate::record::RecordSet::for_local_files("so.tilo.launch-fixture")
                .map_err(LaunchError::Operation)?;
            files
                .file_history_reader()
                .unwrap()
                .require_local_creation()
                .unwrap();
            Ok(true)
        }
        let spec = LaunchSpec::new("so.tilo.launch-fixture", dummy_root).command_handler(command);
        run_with_args_with_attachment(&spec, &strings(&["app"]), |_| {
            panic!("command must not attach")
        })
        .unwrap();
        assert!(crate::record::RecordSet::for_local_files("so.tilo.launch-fixture").is_err());
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn native_initial_launch_missing_configured_service_never_selects_local() {
        let _environment = LaunchFixtureEnv::new();
        let mut env_override = tilo_test_env::ScopedEnv::new();
        env_override.set("TILO_SVC_COLLAB_FD", "-1");
        let manager = crate::collaboration::CollaborationManager::for_native_launch();
        let files = crate::record::RecordSet::for_local_files("so.tilo.launch-fixture");
        drop(env_override);
        assert!(
            manager.is_err(),
            "configured but missing native service is unavailable, not a local launch"
        );
        assert!(
            files.is_err(),
            "missing native bridge cannot silently authorize a local file genesis"
        );
    }

    #[test]
    fn prepared_root_uses_the_supplied_scope_and_root_and_opens_once() {
        static HISTORY: crate::history_mode::HistoryDefinition =
            crate::history_mode::HistoryDefinition {
                app_id: "so.tilo.prepared-test",
                scopes: &[crate::history_mode::HistoryScopeDefinition {
                    key: "process",
                    title: "Process",
                    presentation: crate::history_mode::HistoryPresentation::External {
                        reason: "The process is not versioned.",
                    },
                    content_schema: None,
                }],
            };
        LAUNCH_WAKE_CAPTURES.with(|captures| captures.borrow_mut().clear());
        PREPARED_COUNTS.with(|counts| *counts.borrow_mut() = (0, 0));
        let args = parse_args_extended(
            &strings(&["app", "--text-scale", "1.5"]),
            &[],
            &[],
            &[],
            &[],
            0,
        )
        .unwrap();
        let spec = LaunchSpec::new(HISTORY.app_id, poisoned_prepared_root)
            .open_app_global_state(prepared_open_state)
            .configure_app(scoped_configure)
            .history(&HISTORY);
        let scope = crate::WakeScope::new();
        let _entry = scope.enter();
        let supplied_capture = crate::WakeHandle::new();
        let mut app = spec
            .build_controlled_prepared_root_for_test(
                &args,
                scope,
                Box::new(crate::widget::Text::new("supplied root")),
            )
            .unwrap();

        assert_eq!(PREPARED_COUNTS.with(|counts| *counts.borrow()), (0, 1));
        assert_eq!(app.text_scale(), 1.5);
        let initial_tree = app.build_a11y_tree_update(1.0);
        let history_button = initial_tree
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("History"))
            .map(|(id, _)| *id)
            .expect("prepared app installs the shared History entry point");
        app.dispatch_a11y_action(history_button, &crate::a11y::Action::Activate);
        assert!(
            app.build_a11y_tree_update(1.0)
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some("The process is not versioned."))
        );
        let captures =
            LAUNCH_WAKE_CAPTURES.with(|captures| std::mem::take(&mut *captures.borrow_mut()));
        assert_eq!(captures.len(), 2, "opener and configure run exactly once");
        assert!(supplied_capture.is_active());
        assert!(captures.iter().all(crate::WakeHandle::is_active));
        drop(app);
        assert!(!supplied_capture.is_active());
        assert!(captures.iter().all(|handle| !handle.is_active()));
    }

    #[test]
    fn prepared_root_owned_checked_files_precedes_custom_storage_and_is_revalidated() {
        let args = parse_args_extended(&strings(&["app"]), &[], &[], &[], &[], 0).unwrap();
        let source = crate::history_mode::checked_files::FileHistorySource::default();
        let spec = LaunchSpec::new(CHECKED_HISTORY.app_id, poisoned_prepared_root)
            .open_state(prepared_open_state);
        let scope = crate::WakeScope::new();
        let _entry = scope.enter();
        let app = spec
            .build_controlled_prepared_root_for_test(
                &args,
                scope,
                Box::new(RegistrationRoot::checked(source.clone())),
            )
            .unwrap();
        let (installed, root) = app.controlled_history_definitions_for_test();
        assert!(
            installed
                .is_some_and(|definition| std::ptr::eq(definition, &raw const CHECKED_HISTORY))
        );
        assert!(
            root.is_some_and(|definition| std::ptr::eq(definition, &raw const CHECKED_HISTORY))
        );
        drop(app);

        let rejected = LaunchSpec::new(CHECKED_HISTORY.app_id, poisoned_prepared_root)
            .open_state(prepared_open_state)
            .configure_app(replace_final_root);
        let scope = crate::WakeScope::new();
        let _entry = scope.enter();
        let result = rejected.build_controlled_prepared_root_for_test(
            &args,
            scope,
            Box::new(RegistrationRoot::checked(source)),
        );
        match result {
            Err(LaunchError::Operation(message)) => assert!(
                message.contains("document owner"),
                "unexpected root-witness error: {message}"
            ),
            Err(error) => panic!("unexpected launch error: {error}"),
            Ok(_) => panic!("configure-time checked owner replacement must fail"),
        }
    }

    #[test]
    fn prepared_root_errors_are_not_masked_and_missing_registration_stays_optional() {
        static HISTORY: crate::history_mode::HistoryDefinition =
            crate::history_mode::HistoryDefinition {
                app_id: "so.tilo.prepared-errors",
                scopes: &[crate::history_mode::HistoryScopeDefinition {
                    key: "process",
                    title: "Process",
                    presentation: crate::history_mode::HistoryPresentation::External {
                        reason: "The process is not versioned.",
                    },
                    content_schema: None,
                }],
            };
        let args = parse_args_extended(&strings(&["app"]), &[], &[], &[], &[], 0).unwrap();
        let malformed = LaunchSpec::new(HISTORY.app_id, poisoned_prepared_root)
            .open_app_global_state(prepared_open_state)
            .history(&HISTORY);
        let scope = crate::WakeScope::new();
        let _entry = scope.enter();
        let result = malformed.build_controlled_prepared_root_for_test(
            &args,
            scope,
            Box::new(RegistrationRoot::malformed()),
        );
        assert!(matches!(
            result,
            Err(LaunchError::Operation(message))
                if message.contains("another application")
        ));

        for unprovenanced in [
            LaunchSpec::new(HISTORY.app_id, poisoned_prepared_root)
                .open_state(prepared_open_state)
                .history(&HISTORY),
            LaunchSpec::new(HISTORY.app_id, poisoned_prepared_root)
                .open_app_global_state(prepared_open_state)
                .configure_app(replace_installed_storage)
                .history(&HISTORY),
        ] {
            let scope = crate::WakeScope::new();
            let _entry = scope.enter();
            let result = unprovenanced.build_controlled_prepared_root_for_test(
                &args,
                scope,
                Box::new(crate::widget::Text::new("provenance probe")),
            );
            assert!(matches!(
                result,
                Err(LaunchError::Operation(message))
                    if message.contains("storage provenance")
            ));
        }

        let foreign = LaunchSpec::new("so.tilo.foreign", poisoned_prepared_root)
            .open_app_global_state(prepared_open_state)
            .history(&HISTORY);
        let scope = crate::WakeScope::new();
        let _entry = scope.enter();
        let result = foreign.build_controlled_prepared_root_for_test(
            &args,
            scope,
            Box::new(crate::widget::Text::new("foreign definition probe")),
        );
        assert!(matches!(
            result,
            Err(LaunchError::Operation(message))
                if message.contains("different installed application")
        ));

        let temp = tempfile::tempdir().unwrap();
        let opened = crate::persist::open_canonical_at("so.tilo.foreign", temp.path());
        FOREIGN_OPENED_STATE.with(|slot| {
            *slot.borrow_mut() = Some(opened);
        });
        let foreign_saver = LaunchSpec::new(HISTORY.app_id, poisoned_prepared_root)
            .open_app_global_state(foreign_opened_state)
            .history(&HISTORY);
        let scope = crate::WakeScope::new();
        let _entry = scope.enter();
        let result = foreign_saver.build_controlled_prepared_root_for_test(
            &args,
            scope,
            Box::new(crate::widget::Text::new("foreign saver probe")),
        );
        assert!(matches!(
            result,
            Err(LaunchError::Operation(message))
                if message.contains("another application")
        ));

        let optional =
            LaunchSpec::new(HISTORY.app_id, poisoned_prepared_root).open_state(prepared_open_state);
        let scope = crate::WakeScope::new();
        let _entry = scope.enter();
        let app = optional
            .build_controlled_prepared_root_for_test(
                &args,
                scope,
                Box::new(crate::widget::Text::new("unmanaged rollout root")),
            )
            .unwrap();
        let (installed, root) = app.controlled_history_definitions_for_test();
        assert!(installed.is_none());
        assert!(root.is_none());
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

        // Case `definition_rejects_legacy_setup_overrides_before_preparation`.
        {
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
