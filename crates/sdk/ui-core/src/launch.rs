//! Shared application launch errors and native command-line runner.
use crate::headless;
use std::fmt;
use std::path::PathBuf;
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
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

pub const DEFAULT_VIEWPORT: (u32, u32) = headless::DEFAULT_VIEWPORT;

/// Explicit runner authority/presentation preparation; application metadata is never authority.
pub type ComponentHostPreparer =
    fn(
        &crate::application::ApplicationDefinition,
        &LaunchArgs,
    ) -> Result<crate::application::PreparedComponentInputs, LaunchError>;

/// Drive a mounted capture through its normal actions/input before rendering.
/// The callback receives no unbound root or document mutation shortcut.
pub type ComponentCaptureScenario =
    fn(&mut headless::FrameHarness, &LaunchArgs) -> Result<(), LaunchError>;

/// Parsed common launch arguments. Extra flags are retained for an app's
/// screenshot factory after being explicitly allow-listed by the native LaunchSpec.
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
