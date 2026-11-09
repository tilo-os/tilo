//! What a host lists as apps, and where a session's own question is answered.
//!
//! Search, the palette and the app sheets name apps and offer them as sources; which apps a host
//! has — the ones installed here, the machine's own, the ones a person can get — and what each is
//! called is the host's. On a Linux desktop that is the packaging this session runs
//! ([`crate::lists`]); on the web it is the registry the page reads. A role names this seam and
//! never the platform behind it.

use std::sync::Arc;

use tilo_desktop_search::providers::AppEntry;
use tilo_desktop_search::providers::host_apps::HostAppEntry;
use tilo_desktop_search::{Held, SearchPreferences};

/// The apps a host lists, and what it calls them.
pub trait Apps: Send + Sync {
    /// The apps as last read: one reading for as long as nothing changed, so a role keeps what it
    /// built from a reading until another arrives.
    fn reading(&self) -> Arc<dyn Reading>;
    /// What the app `id` is called to the person.
    fn name(&self, id: &str) -> String;
    /// The apps installed here, as a palette offers them.
    fn applications(&self) -> Vec<AppEntry>;
    /// How many apps the host knows at all: what a source switch offers.
    fn host_apps(&self) -> Vec<HostAppEntry>;
    /// Whether the app `id` stands here, as the person has it installed.
    fn installed(&self, id: &str) -> bool;
}

/// One reading of a host's app lists.
pub trait Reading: Send + Sync {
    /// What tells this reading from another one: when it changes, the host read its lists again.
    fn generation(&self) -> usize;
    /// The machine's own apps as a source, less the sources the person switched off.
    fn host_apps(&self, preferences: &SearchPreferences) -> Held;
    /// The apps a person can get, in `workspace`.
    fn catalogue(&self, workspace: &str) -> Held;
}

/// The settings this host offers search, as the pages the person's own settings are on.
pub trait Settings: Send + Sync {
    /// The pages of `workspace`, with the settings among `published` changed where they are.
    fn source(&self, workspace: &str, published: &[tilo_actions::ActionDescriptor]) -> Held;
}
