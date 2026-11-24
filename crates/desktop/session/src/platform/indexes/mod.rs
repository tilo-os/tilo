//! What the person's files say, as this host indexes them.
//!
//! Search reads the bodies of the person's files from an index only a host with a disk keeps. A
//! page has none and answers with its own, so a role names this service and never the disk behind
//! it. What apps hold is the device's one search index, which its daemon keeps and answers
//! ([`crate::search::SearchOptions::index`]).

use std::sync::Arc;

use tilo_desktop_search::Held;

/// This device's indexes, read from its disk.
#[cfg(target_os = "linux")]
mod disk;
#[cfg(target_os = "linux")]
pub use disk::SessionIndexes;

/// The indexes this host offers search beyond the shell.
pub trait Indexes: Send + Sync {
    /// The indexes as `person` reads them.
    fn of(&self, person: &str) -> Arc<dyn Index>;
}

/// One session's reading of this host's indexes.
pub trait Index: Send + Sync {
    /// The person's files, by their names and what they hold, as a source; `None` where this host
    /// indexes no file.
    fn files(&self) -> Option<Held>;
}
