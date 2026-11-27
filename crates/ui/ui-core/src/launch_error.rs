//! Why an app did not open: the error its launch returns, and what a failed launch still owns.

use std::fmt;

/// Why a launch failed.
pub enum LaunchError {
    #[cfg(feature = "model")]
    Record(crate::resource::ResourceOpenError),
    Restore(crate::view::ThawError),
    DuplicateApplication(String),
    UnknownApplication(String),
    BindingMismatch,
    MissingComponentHost,
    /// Setup failed after the record became Ready: `recovery` holds its store (and any close reports)
    /// so the caller can reopen or flush it.
    #[cfg(feature = "model")]
    ComponentSetup {
        source: Box<LaunchError>,
        record: Box<crate::resource::DocumentCheckout>,
        recovery: LaunchRecovery,
    },
    /// A capture failed or closed dirty: `recovery` holds the close reports.
    #[cfg(feature = "model")]
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
    /// A capture would have set text in the host's faces instead of the product's, because no root
    /// holds this build's font catalog.
    SubstituteFont(String),
    /// The record the launch opens is not on this device yet, and the workspace's join, still under
    /// way, brings it: the launch builds once the join is over instead of creating a copy here.
    ResourceNotHere,
    /// A window of a workspace with no connection to Tilo on its device: its records live in the
    /// workspace, which only Tilo reaches, so nothing done in it would be kept.
    NotConnected,
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // The person's records live in the workspace alone: a window opens on none in their
            // place, which would lose what they type into it.
            // What the connection said comes last, for whoever reads why it did not answer.
            #[cfg(feature = "model")]
            Self::Record(crate::resource::ResourceOpenError::Unreachable(cause))
            | Self::Restore(crate::view::ThawError::Unreachable(cause)) => write!(
                f,
                "Your workspace didn't answer, so this window can't open what it shows. Nothing \
                 was lost: open it again once Tilo is running ({cause})",
            ),
            #[cfg(not(feature = "model"))]
            Self::Restore(crate::view::ThawError::Unreachable(cause)) => write!(
                f,
                "Your workspace didn't answer, so this window can't open what it shows. Nothing \
                 was lost: open it again once Tilo is running ({cause})",
            ),
            #[cfg(feature = "model")]
            Self::Record(error) => write!(f, "record open failed: {error}"),
            Self::Restore(error) => write!(f, "view restore failed: {error}"),
            Self::DuplicateApplication(id) => write!(f, "application {id:?} is already registered"),
            Self::UnknownApplication(id) => write!(f, "application {id:?} is not available"),
            Self::MissingComponentHost => {
                f.write_str("application definition requires an explicit component host")
            }
            Self::BindingMismatch => f.write_str("application binding does not match"),
            #[cfg(feature = "model")]
            Self::ComponentSetup { source, .. } => write!(f, "application setup failed: {source}"),
            #[cfg(feature = "model")]
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
            Self::SubstituteFont(problem) => write!(f, "refusing to capture: {problem}"),
            Self::ResourceNotHere => {
                f.write_str("the record is not on this device yet; it arrives with the workspace")
            }
            Self::NotConnected => f.write_str(
                "Tilo is not running on this device, so nothing done in this app would be kept: \
                 start Tilo, then open the app from it",
            ),
        }
    }
}

impl fmt::Debug for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "model")]
            Self::Record(error) => f.debug_tuple("Record").field(error).finish(),
            Self::Restore(error) => f.debug_tuple("Restore").field(error).finish(),
            #[cfg(feature = "model")]
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
            #[cfg(feature = "model")]
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
            Self::BindingMismatch => f.write_str("BindingMismatch"),
            Self::ResourceNotHere => f.write_str("ResourceNotHere"),
            Self::NotConnected => f.write_str("NotConnected"),
            Self::SubstituteFont(_) => f.write_str("SubstituteFont"),
        }
    }
}

impl std::error::Error for LaunchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            #[cfg(feature = "model")]
            Self::Record(error) => Some(error),
            Self::Restore(error) => Some(error),
            #[cfg(feature = "model")]
            Self::ComponentSetup { source, .. } | Self::ComponentCapture { source, .. } => {
                Some(source.as_ref())
            }
            _ => None,
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

/// What a failed launch still owns, handed back to the caller so nothing dirty is lost: the record
/// store holding the canonical record (reopen it, or flush it), and the close reports of components
/// torn down on the way (retry their view or record durability). A recovery dropped on another thread
/// is leaked rather than dropped there — its stores are never touched off their thread — so move the
/// error back to the launching thread (or consume the recovery there) when it must be released.
#[cfg(feature = "model")]
pub struct LaunchRecovery {
    owner: std::thread::ThreadId,
    report_count: usize,
    parts: std::mem::ManuallyDrop<Box<RecoveryParts>>,
}

#[cfg(feature = "model")]
struct RecoveryParts {
    records: Option<crate::resource::ResourceStore>,
    reports: Vec<crate::application::ComponentCloseReport>,
}

// SAFETY: every access to `parts` (shared or exclusive, including drop) is
// held on the current thread being `owner`, the thread that created them.
// On any other thread the owners are neither read, cloned nor dropped, so
// their non-atomic reference counts are only ever touched by one thread.
#[allow(
    clippy::non_send_fields_in_send_ty,
    reason = "the single-threaded owners are reachable and droppable only on `owner`"
)]
#[cfg(feature = "model")]
unsafe impl Send for LaunchRecovery {}
// SAFETY: as above; `&LaunchRecovery` exposes the owners only on `owner`.
#[cfg(feature = "model")]
unsafe impl Sync for LaunchRecovery {}

#[cfg(feature = "model")]
impl LaunchRecovery {
    /// Recovery owners for the current thread.
    #[must_use]
    pub fn new(
        records: Option<crate::resource::ResourceStore>,
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
    pub fn records(records: crate::resource::ResourceStore) -> Self {
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
    pub fn resource_store(&self) -> Option<&crate::resource::ResourceStore> {
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
    pub fn into_parts(
        mut self,
    ) -> Result<
        (
            Option<crate::resource::ResourceStore>,
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
    pub fn into_resource_store(self) -> Result<Option<crate::resource::ResourceStore>, Self> {
        self.into_parts().map(|(records, _)| records)
    }

    /// The close reports, taken out on the launching thread.
    pub fn into_close_reports(self) -> Result<Vec<crate::application::ComponentCloseReport>, Self> {
        self.into_parts().map(|(_, reports)| reports)
    }
}

#[cfg(feature = "model")]
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

#[cfg(feature = "model")]
impl fmt::Debug for LaunchRecovery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LaunchRecovery")
            .field("reachable", &self.here())
            .field("close_report_count", &self.report_count)
            .finish_non_exhaustive()
    }
}

#[cfg(all(feature = "native", feature = "model"))]
impl From<::collaboration::NativeLaunchError> for LaunchError {
    fn from(error: ::collaboration::NativeLaunchError) -> Self {
        match error {
            ::collaboration::NativeLaunchError::Operation(message) => Self::Operation(message),
        }
    }
}
