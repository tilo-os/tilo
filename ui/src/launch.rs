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
type ShotFactory = fn(&LaunchArgs) -> Result<Box<dyn Widget>, LaunchError>;
type ViewportFactory = fn(&LaunchArgs) -> (u32, u32);
type ConfigureApp = fn(&mut App);

/// Parsed common launch arguments. Extra flags are retained for an app's
/// screenshot factory after being explicitly allow-listed by [`LaunchSpec`].
#[derive(Debug)]
pub struct LaunchArgs {
    pub shots: Option<PathBuf>,
    pub viewport: Option<(u32, u32)>,
    pub scenario: Option<String>,
    pub headless: Option<headless::Spec>,
    extras: Vec<String>,
}

impl LaunchArgs {
    pub fn has(&self, flag: &str) -> bool {
        self.extras.iter().any(|arg| arg == flag)
    }
}

/// Configuration for an app using the standard lifecycle.
pub struct LaunchSpec {
    app_id: &'static str,
    live_root: RootFactory,
    shot_root: ShotFactory,
    shot_viewport: ViewportFactory,
    configure_app: Option<ConfigureApp>,
    allowed_flags: &'static [&'static str],
    scenarios: &'static [&'static str],
}

impl LaunchSpec {
    pub fn new(app_id: &'static str, live_root: RootFactory) -> Self {
        Self {
            app_id,
            live_root,
            shot_root: |_| Err(LaunchError::ShotsUnsupported),
            shot_viewport: |_| DEFAULT_VIEWPORT,
            configure_app: None,
            allowed_flags: &[],
            scenarios: &[],
        }
    }

    pub fn shot_root(mut self, factory: ShotFactory) -> Self {
        self.shot_root = factory;
        self
    }

    /// Scenario-specific default viewport. An explicit `--viewport` always
    /// wins over this callback.
    pub fn shot_viewport(mut self, factory: ViewportFactory) -> Self {
        self.shot_viewport = factory;
        self
    }

    pub fn configure_app(mut self, configure: ConfigureApp) -> Self {
        self.configure_app = Some(configure);
        self
    }

    /// Allow app-specific boolean flags. Value-taking common options belong
    /// in `LaunchArgs`; specialized CLIs should use the lower-level APIs.
    pub fn allowed_flags(mut self, flags: &'static [&'static str]) -> Self {
        self.allowed_flags = flags;
        self
    }

    /// Declare every accepted screenshot scenario. With no declaration,
    /// `--scenario` is rejected instead of being silently ignored.
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
            Self::Scenario(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for LaunchError {}

/// Run the standard app lifecycle and report command-line errors using the
/// conventional usage-error exit status (`2`).
pub fn run_or_exit(spec: LaunchSpec) {
    if let Err(error) = run(spec) {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
}

/// Run the standard app lifecycle using `std::env::args`.
pub fn run(spec: LaunchSpec) -> Result<(), LaunchError> {
    let _ = tracing_subscriber::fmt::try_init();
    let args: Vec<String> = std::env::args().collect();
    let args = parse_args(&args, spec.allowed_flags)?;
    validate_spec_args(&args, &spec)?;

    if let Some(out) = &args.shots {
        let root = (spec.shot_root)(&args)?;
        let viewport = args.viewport.unwrap_or_else(|| (spec.shot_viewport)(&args));
        let harness = FrameHarness::new_with_theme(State::new(), root, viewport, shot_theme());
        harness.render_png(out);
        println!("{}: wrote shot to {}", spec.app_id, out.display());
        return Ok(());
    }

    if let Some(headless) = args.headless {
        std::process::exit(headless::run(headless, (spec.live_root)(), spec.app_id));
    }

    let (state, autosave) = crate::persist::open(spec.app_id);
    let mut app = App::new(state, (spec.live_root)()).with_autosave(autosave);
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

fn parse_args(args: &[String], allowed_flags: &[&str]) -> Result<LaunchArgs, LaunchError> {
    let mut parsed = LaunchArgs {
        shots: None,
        viewport: None,
        scenario: None,
        headless: None,
        extras: Vec::new(),
    };
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
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
            extra if allowed_flags.contains(&extra) && inline.is_none() => {
                if parsed.extras.iter().any(|found| found == extra) {
                    return Err(LaunchError::Duplicate(extra.to_owned()));
                }
                parsed.extras.push(extra.to_owned());
            }
            _ => return Err(LaunchError::UnknownArgument(arg.clone())),
        }
        index += 1;
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
        if let Some(extra) = parsed.extras.first() {
            return Err(LaunchError::ShotsOnly(extra.clone()));
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
    *index += 1;
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
        let parsed = parse_args(&args, &["--empty"]).unwrap();
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
                parse_args(&args, &[]),
                Err(LaunchError::InvalidViewport(_))
            ));
        }
    }

    #[test]
    fn rejects_unknown_missing_and_duplicate_arguments() {
        assert!(matches!(
            parse_args(&strings(&["app", "--mystery"]), &[]),
            Err(LaunchError::UnknownArgument(_))
        ));
        assert!(matches!(
            parse_args(&strings(&["app", "--shots"]), &[]),
            Err(LaunchError::MissingValue(_))
        ));
        assert!(matches!(
            parse_args(&strings(&["app", "--scenario=a", "--scenario=b"]), &[]),
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
        let parsed = parse_args(&args, &[]).unwrap();
        let spec = parsed.headless.expect("headless spec");
        assert_eq!(spec.viewport, Some((800, 600)));
        assert_eq!(spec.actions.len(), 1);
    }

    #[test]
    fn rejects_conflicting_modes_and_shot_options_in_live_mode() {
        let both = strings(&["app", "--shots", "/tmp/a.png", "--headless-drive", "{}"]);
        assert_eq!(
            parse_args(&both, &[]).unwrap_err(),
            LaunchError::ConflictingModes
        );

        for option in [
            strings(&["app", "--viewport=800x600"]),
            strings(&["app", "--scenario=wide"]),
            strings(&["app", "--empty"]),
        ] {
            assert!(matches!(
                parse_args(&option, &["--empty"]),
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
}
