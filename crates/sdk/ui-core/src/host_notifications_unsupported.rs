//! Native notification scheduling where this build has none.
//!
//! Windows builds with the `notifications` feature deliver and schedule
//! toasts through the operating system. Everywhere else the same API exists so
//! apps compile unchanged; [`SUPPORTED`] is `false` and every operation fails
//! with [`Unsupported`], so callers skip native work up front.

use crate::time::SystemTime;

/// Whether this build delivers and schedules native notifications.
pub const SUPPORTED: bool = false;

/// The error every operation returns in a build without native notifications.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unsupported;

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("native notifications are not available in this build")
    }
}

impl std::error::Error for Unsupported {}

/// A native notification host; never constructed in this build.
pub struct HostNotifications {
    never: std::convert::Infallible,
}

/// A pending notification owned by this application.
#[derive(Debug)]
pub struct ScheduledNotice {
    pub id: String,
    pub delivery_time: SystemTime,
}

impl HostNotifications {
    /// # Errors
    /// Always [`Unsupported`].
    pub fn new(_app_id: &str) -> Result<Self, Unsupported> {
        Err(Unsupported)
    }

    /// # Errors
    /// Unreachable: no host exists in this build.
    pub fn with_protocol_activation(self, _uri: &str) -> Result<Self, Unsupported> {
        match self.never {}
    }

    /// # Errors
    /// Unreachable: no host exists in this build.
    pub fn show(&self, _tag: &str, _title: &str, _body: &str) -> Result<(), Unsupported> {
        match self.never {}
    }

    /// # Errors
    /// Unreachable: no host exists in this build.
    pub fn schedule(
        &self,
        _id: &str,
        _title: &str,
        _body: &str,
        _delivery_time: SystemTime,
    ) -> Result<(), Unsupported> {
        match self.never {}
    }

    /// # Errors
    /// Unreachable: no host exists in this build.
    pub fn scheduled(&self) -> Result<Vec<ScheduledNotice>, Unsupported> {
        match self.never {}
    }

    /// # Errors
    /// Unreachable: no host exists in this build.
    pub fn cancel_schedule(&self, _id: &str) -> Result<(), Unsupported> {
        match self.never {}
    }
}
