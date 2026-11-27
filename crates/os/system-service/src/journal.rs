//! The `journal` resource: what [`tilo_system_resources::journal`] says it is.
//!
//! [`read`] reads the most recent records of this computer's systemd journal, [`read_after`]
//! the records written since a cursor, and [`JournalAction`] offers both to a shell. The read is
//! the journal's own ([`tilo_system_resources::journal::read`]), so a device whose daemon serves
//! the journal with no session reads it the same way. A machine whose journal cannot be read says
//! so; it never answers with an empty journal.

use tilo_actions::ResourceSpec;
use tilo_service::offer;
use tilo_system_resources::journal::{JOURNAL, Journal, JournalAction};

use crate::i18n::name;

// The journal's own read, shared with a device whose daemon has no session to read it.
pub use tilo_system_resources::journal::{read, read_after};

/// The `journal` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.journal", label = "System journal")]
pub struct JournalReading;

/// The `journal` resource reading `journal`.
#[must_use]
pub fn resource(_journal: &Journal) -> ResourceSpec {
    offer::<JournalAction>(tilo_resource::typed_resource::<JournalReading>(
        JOURNAL,
        name("so.tilo.system.journal", "System journal"),
    ))
}

/// The `journal` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Journal, JournalAction>(JOURNAL, "System journal")
}

/// What `limit` means for a read that names none.
#[must_use]
pub const fn default_limit() -> u32 {
    500
}

/// The moment now, in milliseconds since the Unix epoch.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}
