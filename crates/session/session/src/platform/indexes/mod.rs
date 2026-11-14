//! What apps hold and what the person's files say, as this host indexes them.
//!
//! Search reads two indexes that only a host with a disk keeps: what apps published of their
//! content (a note, a message, a login) and the bodies of the person's files. A page has neither
//! and answers with its own, so a role names this seam and never the disk behind it.

use std::sync::Arc;

use tilo_desktop_search::Held;
use ui::floating_surface::Recipient;

/// This device's indexes, read from its disk.
#[cfg(target_os = "linux")]
mod disk;
#[cfg(target_os = "linux")]
pub use disk::SessionIndexes;

/// The indexes this host offers search beyond the desk.
pub trait Indexes: Send + Sync {
    /// The indexes as `person` reads them: rows about what apps hold say who wrote them.
    fn of(&self, person: &str) -> Arc<dyn Index>;
}

/// One session's reading of this host's indexes.
pub trait Index: Send + Sync {
    /// What the apps of this device published, as a source.
    fn content(&self) -> Held;
    /// What `app` alone published, as a source: what a window's search is over,
    /// whether or not the app is running. `None` where this host keeps no index.
    fn content_for(&self, _app: &str) -> Option<Held> {
        None
    }
    /// The person's files, by their names and what they hold, as a source; `None` where this host
    /// indexes no file.
    fn files(&self) -> Option<Held>;
    /// The people, agents and conversations a message can go to, the most recent first.
    fn recipients(&self) -> Vec<Recipient>;
    /// A number that moves whenever what [`Self::recipients`] lists may have.
    fn generation(&self) -> u64;
}
