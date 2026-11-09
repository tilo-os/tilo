//! What apps hold and what the person's files say, as this host indexes them.
//!
//! Search reads two indexes that only a host with a disk keeps: what apps published of their
//! content (a note, a message, a login) and the bodies of the person's files. A page has neither
//! and answers with its own, so a role names this seam and never the disk behind it.

use std::sync::Arc;

use tilo_desktop_search::Held;
use ui::floating_surface::Recipient;

/// The indexes this host offers search beyond the desk.
pub trait Indexes: Send + Sync {
    /// The indexes as `person` reads them: rows about what apps hold say who wrote them.
    fn of(&self, person: &str) -> Arc<dyn Index>;
}

/// One session's reading of this host's indexes.
pub trait Index: Send + Sync {
    /// What the apps of this device published, as a source.
    fn content(&self) -> Held;
    /// The person's files, by their names and what they hold, as a source; `None` where this host
    /// indexes no file.
    fn files(&self) -> Option<Held>;
    /// The people, agents and conversations `query` names, as a window's box lists whom a message
    /// could go to.
    fn recipients(&self, query: &str) -> Vec<Recipient>;
}
