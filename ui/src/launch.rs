//! Standard command-line lifecycle for qOS applications.
//!
//! Ordinary apps provide their live and screenshot roots; this module owns
//! common argument validation, persistence, headless driving, offscreen
//! rendering, logging, and live startup. Apps with genuinely different
//! lifecycles can continue to use the lower-level APIs directly.

use std::fmt;
use std::path::PathBuf;

use crate::headless::{self, FrameHarness};
use crate::widget::Widget;
use crate::{App, State, Theme};

pub const DEFAULT_VIEWPORT: (u32, u32) = headless::DEFAULT_VIEWPORT;

type RootFactory = fn() -> Box<dyn Widget>;
type ArgsRootFactory = fn(&LaunchArgs) -> Result<Box<dyn Widget>, LaunchError>;
type ShotFactory = fn(&LaunchArgs) -> Result<Box<dyn Widget>, LaunchError>;
type ShotHandler = fn(&LaunchArgs) -> Result<(), LaunchError>;
type ViewportFactory = fn(&LaunchArgs) -> (u32, u32);
type ConfigureApp = fn(&mut App);
type OpenState = fn(&'static str) -> (State, crate::persist::Autosave);
type OpenStateWithArgs = fn(&'static str, &LaunchArgs) -> (State, crate::persist::Autosave);
type LiveHandler = fn(&LaunchArgs) -> Result<(), LaunchError>;

/// Parsed common launch arguments. Extra flags are retained for an app's
/// screenshot factory after being explicitly allow-listed by [`LaunchSpec`].
#[derive(Debug)]
pub struct LaunchArgs {
    pub shots: Option<PathBuf>,
    pub viewport: Option<(u32, u32)>,
    pub scenario: Option<String>,
    pub headless: Option<headless::Spec>,
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
    app_id: &'static str,
    live_root: Option<RootFactory>,
    live_root_with_args: Option<ArgsRootFactory>,
    shot_root: ShotFactory,
    shot_handler: Option<ShotHandler>,
    shot_viewport: ViewportFactory,
    configure_app: Option<ConfigureApp>,
    live_handler: Option<LiveHandler>,
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
    pub fn new(app_id: &'static str, live_root: RootFactory) -> Self {
        Self {
            app_id,
            live_root: Some(live_root),
            live_root_with_args: None,
            shot_root: |_| Err(LaunchError::ShotsUnsupported),
            shot_handler: None,
            shot_viewport: |_| DEFAULT_VIEWPORT,
            configure_app: None,
            live_handler: None,
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
            app_id,
            live_root: None,
            live_root_with_args: Some(live_root),
            shot_root: |_| Err(LaunchError::ShotsUnsupported),
            shot_handler: None,
            shot_viewport: |_| DEFAULT_VIEWPORT,
            configure_app: None,
            live_handler: None,
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
        self.live_root_with_args = Some(factory);
        self
    }

    #[must_use]
    pub fn shot_root(mut self, factory: ShotFactory) -> Self {
        self.shot_root = factory;
        self
    }

    /// Delegate screenshot execution to an app-specific pipeline while the
    /// standard runner still owns parsing, mode validation, logging, headless
    /// driving, persistence and live startup.
    #[must_use]
    pub fn shot_handler(mut self, handler: ShotHandler) -> Self {
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
        self.live_handler = Some(handler);
        self
    }

    /// Override how the live app opens its persisted state. This is primarily
    /// for apps importing an older on-disk document through
    /// [`crate::persist::open_migrating`]; the runner still owns autosave and
    /// the live event loop.
    #[must_use]
    pub fn open_state(mut self, open: OpenState) -> Self {
        self.open_state = open;
        self.open_state_with_args = None;
        self
    }

    /// Open live state using validated launch arguments. Useful when opening a
    /// document must seed the persisted CRDT before the root's first sync.
    #[must_use]
    pub fn open_state_with_args(mut self, open: OpenStateWithArgs) -> Self {
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

    /// Declare every accepted screenshot scenario. With no declaration,
    /// `--scenario` is rejected instead of being silently ignored.
    #[must_use]
    pub fn scenarios(mut self, scenarios: &'static [&'static str]) -> Self {
        self.scenarios = scenarios;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    MissingValue(String),
    Duplicate(String),
    InvalidViewport(String),
    UnknownArgument(String),
    ConflictingModes,
    ShotsOnly(String),
    ShotsUnsupported,
    Scenario(String),
    Operation(String),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingValue(flag) => write!(f, "{flag} requires a value"),
            Self::Duplicate(flag) => write!(f, "{flag} was supplied more than once"),
            Self::InvalidViewport(value) => write!(
                f,
                "invalid viewport {value:?}; expected non-zero WIDTHxHEIGHT"
            ),
            Self::UnknownArgument(arg) => write!(f, "unknown argument {arg:?}"),
            Self::ConflictingModes => {
                write!(f, "--shots and --headless-drive cannot be used together")
            }
            Self::ShotsOnly(arg) => write!(f, "{arg} is only meaningful with --shots"),
            Self::ShotsUnsupported => write!(f, "this application does not support --shots"),
            Self::Scenario(message) | Self::Operation(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for LaunchError {}

impl LaunchError {
    fn exit_code(&self) -> i32 {
        if matches!(self, Self::Operation(_)) {
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
pub fn run_or_exit(spec: LaunchSpec) {
    if let Err(error) = run(&spec) {
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
/// undeclared `--scenario`), if the app has no [`LaunchSpec::shot_root`] /
/// [`LaunchSpec::shot_handler`] and `--shots` was requested, or if the
/// app's own shot factory or shot handler returns one.
pub fn run(spec: &LaunchSpec) -> Result<(), LaunchError> {
    let _ = tracing_subscriber::fmt::try_init();
    let args: Vec<String> = std::env::args().collect();
    run_with_args(spec, &args)
}

#[allow(
    clippy::exit,
    reason = "the --headless-drive path reports the harness's pass/fail \
              status as this process's exit code, exactly like --shots \
              reports success by returning Ok — there is no caller to \
              hand a status back to"
)]
fn run_with_args(spec: &LaunchSpec, raw_args: &[String]) -> Result<(), LaunchError> {
    let args = parse_args_extended(
        raw_args,
        spec.allowed_flags,
        spec.allowed_options,
        spec.allowed_live_flags,
        spec.allowed_live_options,
        spec.max_positionals,
    )?;
    validate_spec_args(&args, spec)?;

    if let Some(out) = &args.shots {
        if let Some(handler) = spec.shot_handler {
            handler(&args)?;
            return Ok(());
        }
        let root = (spec.shot_root)(&args)?;
        let viewport = args.viewport.unwrap_or_else(|| (spec.shot_viewport)(&args));
        let harness = FrameHarness::new_with_theme(State::new(), root, viewport, shot_theme());
        harness.render_png(out);
        println!("{}: wrote shot to {}", spec.app_id, out.display());
        return Ok(());
    }

    if let Some(headless) = &args.headless {
        std::process::exit(headless::run(headless, spec.live_root(&args)?, spec.app_id));
    }

    if let Some(handler) = spec.live_handler {
        return handler(&args);
    }

    let (state, autosave) = spec.open_state_with_args.map_or_else(
        || (spec.open_state)(spec.app_id),
        |open| open(spec.app_id, &args),
    );
    let mut app = App::new(state, spec.live_root(&args)?).with_autosave(autosave);
    if let Some(configure) = spec.configure_app {
        configure(&mut app);
    }
    app.run();
    Ok(())
}

fn validate_spec_args(args: &LaunchArgs, spec: &LaunchSpec) -> Result<(), LaunchError> {
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

fn shot_theme() -> Theme {
    match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    }
}

#[cfg(test)]
fn parse_args(
    args: &[String],
    allowed_flags: &[&str],
    allowed_options: &[&str],
) -> Result<LaunchArgs, LaunchError> {
    parse_args_extended(args, allowed_flags, allowed_options, &[], &[], 0)
}

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
        viewport: None,
        scenario: None,
        headless: None,
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
    if parsed.shots.is_some() && parsed.headless.is_some() {
        return Err(LaunchError::ConflictingModes);
    }
    if parsed.shots.is_none() {
        if parsed.viewport.is_some() {
            return Err(LaunchError::ShotsOnly("--viewport".to_owned()));
        }
        if parsed.scenario.is_some() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    static CUSTOM_SHOT_RAN: AtomicBool = AtomicBool::new(false);
    static CUSTOM_LIVE_RAN: AtomicBool = AtomicBool::new(false);

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
    fn rejects_conflicting_modes_and_shot_options_in_live_mode() {
        let both = strings(&["app", "--shots", "/tmp/a.png", "--headless-drive", "{}"]);
        assert_eq!(
            parse_args(&both, &[], &[]).unwrap_err(),
            LaunchError::ConflictingModes
        );

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

    #[test]
    fn scenarios_must_be_declared_by_the_app() {
        let args = parse_args(
            &strings(&["app", "--shots=/tmp/a.png", "--scenario=wide"]),
            &[],
            &[],
        )
        .unwrap();
        let undeclared = LaunchSpec::new("dev.qos.test", dummy_root);
        assert!(matches!(
            validate_spec_args(&args, &undeclared),
            Err(LaunchError::Scenario(_))
        ));
        let declared = LaunchSpec::new("dev.qos.test", dummy_root).scenarios(&["wide"]);
        validate_spec_args(&args, &declared).unwrap();
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
        let spec = LaunchSpec::new("dev.qos.test", dummy_root)
            .live_root_with_args(fallible_root)
            .max_positionals(1);
        match spec.live_root(&args) {
            Err(error) => assert_eq!(
                error,
                LaunchError::Operation("cannot open broken.qdoc".into())
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
        let spec = LaunchSpec::new("dev.qos.test", dummy_root)
            .shot_handler(custom_shot)
            .live_handler(unexpected_live);
        run_with_args(&spec, &strings(&["app", "--shots", "/tmp/custom.png"])).unwrap();
        assert!(CUSTOM_SHOT_RAN.load(Ordering::SeqCst));
    }

    fn unexpected_live(_args: &LaunchArgs) -> Result<(), LaunchError> {
        panic!("screenshot dispatch reached the live handler")
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
        run_with_args(&spec, &strings(&["app"])).unwrap();
        assert!(CUSTOM_LIVE_RAN.load(Ordering::SeqCst));
    }

    fn custom_open_state(app_id: &'static str) -> (State, crate::persist::Autosave) {
        crate::persist::open(app_id)
    }

    #[test]
    fn app_can_replace_only_the_persistence_loader() {
        let spec = LaunchSpec::new("dev.qos.test", dummy_root).open_state(custom_open_state);
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
}
