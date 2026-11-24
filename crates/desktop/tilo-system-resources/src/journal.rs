//! The `journal` resource: this computer's systemd journal as rows, the cursor a live tail
//! continues from, and whether this window follows it.
//!
//! What reads `journalctl` is the session's system service; this is the vocabulary an app, an
//! agent and a script read the journal by. The entries are this device's and are never shared:
//! only the filter people agree on lives in the workspace.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const JOURNAL: &str = "journal";

/// One field of a journal record the reading does not promote to a column.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct JournalField {
    pub name: String,
    pub value: String,
}

/// One record of the journal, as the window lists it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct JournalEntry {
    /// The journal's own realtime timestamp, in microseconds since the Unix epoch.
    pub realtime_us: u64,
    /// The unit that wrote it, or the identifier when it belonged to no unit.
    pub unit: String,
    /// The program that wrote it (`SYSLOG_IDENTIFIER`, else `_COMM`).
    pub identifier: String,
    /// The RFC 5424 severity, 0 (emergency) to 7 (debug).
    pub priority: u8,
    /// Its message, without the terminal escapes a program wrote for a console.
    pub message: String,
    pub pid: Option<u32>,
    pub hostname: String,
    pub boot_id: String,
    pub transport: String,
    /// The record's other fields, sorted by name.
    pub extra: Vec<JournalField>,
    /// Its time in the person's zone: `HH:MM:SS.mmm` today, `Mon DD HH:MM:SS.mmm` earlier.
    pub time: String,
    /// Its full local timestamp, `YYYY-MM-DD HH:MM:SS.mmm`.
    pub timestamp: String,
}

/// What this computer's journal reads as now.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Journal {
    /// Whether `journalctl` answers on this machine.
    #[serde(default)]
    pub serving: bool,
    /// Why the journal could not be read, when it could not.
    #[serde(default)]
    pub read_error: Option<String>,
    /// Why the last action did not happen, until the next one.
    #[serde(default)]
    pub problem: Option<String>,
    /// The records read, oldest first.
    #[serde(default)]
    pub entries: Vec<JournalEntry>,
    /// Where the next tail read continues from; empty until a read answered.
    #[serde(default)]
    pub cursor: String,
    /// Whether the journal is followed as new records arrive.
    #[serde(default)]
    pub following: bool,
    /// When the journal was last read, in milliseconds since the Unix epoch.
    #[serde(default)]
    pub read_at_ms: u64,
    /// Actions taken and not landed yet, by the action's id.
    #[serde(default)]
    pub pending: Vec<String>,
}

/// The `journal` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.journal")]
pub enum JournalAction {
    /// Read the most recent records.
    #[effect(kind = "read")]
    #[effect(
        own_device,
        icon = "rotate-cw",
        keywords = "refresh journal, journal neu lesen"
    )]
    Refresh {
        #[param("How many records to read")] limit: u32,
    },
    /// Read the records written after `cursor`.
    #[effect(kind = "read")]
    #[effect(
        own_device,
        icon = "arrow-down",
        keywords = "follow journal, journal folgen"
    )]
    ReadAfter {
        #[param("The cursor to read after")] cursor: String,
        #[param("How many records to read")] limit: u32,
    },
    /// Follow the journal as new records arrive, or stop following it.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "play",
        keywords = "follow live, live verfolgen"
    )]
    SetFollowing {
        #[param("Whether to follow new records")] on: bool,
    },
}
