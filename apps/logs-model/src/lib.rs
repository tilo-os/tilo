//! The `logs.journal` resource: the journal filter a workspace shares, as the
//! app that draws it and an agent that reads it both see it.
//!
//! The journal itself is this device's and stays in the app; only the filter
//! people agree on travels in the workspace.

use tilo_resource::{self, Atom, Model, resource};

/// The eight syslog severities, ordered most-severe (`Emerg`, level 0) to least-severe
/// (`Debug`, level 7). Level numbers match RFC 5424 and the integer in the journal's
/// `PRIORITY` field.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, PartialOrd, Ord, serde::Serialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Emerg,
    Alert,
    Crit,
    Err,
    Warning,
    Notice,
    Info,
    #[default]
    Debug,
}

impl Priority {
    pub const ALL: [Priority; 8] = [
        Priority::Emerg,
        Priority::Alert,
        Priority::Crit,
        Priority::Err,
        Priority::Warning,
        Priority::Notice,
        Priority::Info,
        Priority::Debug,
    ];

    /// Map a journal `PRIORITY` integer to a [`Priority`]. Anything out of the 0..=7 range
    /// clamps to `Debug` (the most verbose bucket), which is the safe default for an
    /// unknown resource.
    #[must_use]
    pub fn from_level(n: i64) -> Priority {
        match n {
            0 => Priority::Emerg,
            1 => Priority::Alert,
            2 => Priority::Crit,
            3 => Priority::Err,
            4 => Priority::Warning,
            5 => Priority::Notice,
            6 => Priority::Info,
            _ => Priority::Debug,
        }
    }

    /// The RFC 5424 level number (0 = emerg … 7 = debug).
    #[must_use]
    pub fn level(self) -> i64 {
        match self {
            Priority::Emerg => 0,
            Priority::Alert => 1,
            Priority::Crit => 2,
            Priority::Err => 3,
            Priority::Warning => 4,
            Priority::Notice => 5,
            Priority::Info => 6,
            Priority::Debug => 7,
        }
    }

    /// The severity's name, the same in the list and the detail.
    #[must_use]
    pub fn long(self) -> &'static str {
        match self {
            Priority::Emerg => "Emergency",
            Priority::Alert => "Alert",
            Priority::Crit => "Critical",
            Priority::Err => "Error",
            Priority::Warning => "Warning",
            Priority::Notice => "Notice",
            Priority::Info => "Info",
            Priority::Debug => "Debug",
        }
    }

    /// The threshold this severity sets, as the severity filter names it.
    #[must_use]
    pub fn threshold_label(self) -> &'static str {
        match self {
            Priority::Emerg => "Emergency only",
            Priority::Alert => "Alert and above",
            Priority::Crit => "Critical and above",
            Priority::Err => "Error and above",
            Priority::Warning => "Warning and above",
            Priority::Notice => "Notice and above",
            Priority::Info => "Info and above",
            Priority::Debug => "All levels",
        }
    }
}

/// The shared journal filter.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Logs {
    /// The least severe priority shown.
    pub severity: Priority,
    /// Exact unit, or empty for every unit.
    pub unit: Atom,
    /// Exact boot id, or empty for every boot.
    pub boot: Atom,
    /// Text to find in messages, units or identifiers.
    pub search: String,
}

impl Logs {
    /// What the resource holds: the shared journal filter: severity, unit, boot and search
    /// text.
    ///
    /// # Errors
    /// When the fields cannot be read from the document.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(kind = "logs.journal", read_content = Logs::read_document)]
impl Logs {
    /// Clear every journal filter.
    #[action]
    pub fn clear_filters(&mut self) {
        *self = Self::default();
    }
}
