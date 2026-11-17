//! Readings of the resources a provider publishes itself, for the programs its API runs.
//!
//! A session publishes resources that no record of an app holds: the device's sound, network,
//! Bluetooth, battery, power and displays, its windows, the tray, the agents, the styles, and
//! Settings' sections, each read from the session's own services
//! (`crates/session/service`, `service::reading`). The shell's parts read them as the session
//! hands them in, and a task's program reads the same ones: one reading serves every reader,
//! and the reading's shape is the one the shell's views are given, so a program and a view read
//! one resource alike.
//!
//! A program reads a reading within the program it runs, so the ask is answered synchronously
//! (unlike an action's, which is followed with [`super::ActionProvider::observe`]): the task's provider
//! meets a resource it has no reading of, asks its API for the newest one, and goes on with it.

use super::{ActionCallError, Caller};

/// A provider's readings of the resources it publishes itself, for the programs its API runs.
pub trait Readings: Send + Sync {
    /// The newest reading of the resource `resource` the app `app` publishes, as the JSON its
    /// service reads: `Some` once a service answers, `None` for a resource the provider publishes
    /// nothing of (an older build's, a device without that service).
    ///
    /// # Errors
    ///
    /// [`ActionCallError::Unavailable`] when the session cannot answer now (it is frozen), and
    /// [`ActionCallError::NotPermitted`] for a caller the reading is not theirs to see.
    fn reading(
        &self,
        caller: &Caller,
        app: &str,
        resource: &str,
    ) -> Result<Option<serde_json::Value>, ActionCallError>;
}
