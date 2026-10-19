//! The session shell's choosers where no Tilo session launches apps
//! (Windows, a browser page): no process is ever handed a connection, so
//! [`ShellLink::inherited`] is always `None` and portable callers take their
//! fallback without selecting a target themselves.

use std::sync::Arc;

use crate::{FilePicker, UriOpener};

/// A connection to the session shell; none exists on this target.
#[derive(Clone)]
pub struct ShellLink(Never);

#[derive(Clone, Copy)]
enum Never {}

impl ShellLink {
    /// Always `None`: no session shell hands this process a connection.
    pub fn inherited() -> Option<Self> {
        None
    }

    /// The shell's choosers (unreachable: no link exists here).
    #[must_use]
    pub fn picker(&self, _unattended: Option<Arc<dyn FilePicker>>) -> Arc<dyn FilePicker> {
        match self.0 {}
    }

    /// Links opened by the shell (unreachable: no link exists here).
    #[must_use]
    pub fn opener(&self, _elsewhere: Arc<dyn UriOpener>) -> Arc<dyn UriOpener> {
        match self.0 {}
    }

    /// Hear when the workspace's identity is selected (unreachable: no link
    /// exists here).
    pub fn await_identity(&self, _selected: impl FnOnce(Result<(), String>) + Send + 'static) {
        match self.0 {}
    }

    /// Show the shell's identity chooser (unreachable: no link exists here).
    pub fn choose_identity(&self, _answered: impl FnOnce(Result<(), String>) + Send + 'static) {
        match self.0 {}
    }
}

/// Always `None`: no session shell hands this process a connection.
pub fn inherited_picker(_unattended: Option<Arc<dyn FilePicker>>) -> Option<Arc<dyn FilePicker>> {
    None
}

/// Always `None`: no session shell hands this process a connection.
pub fn inherited_opener(_elsewhere: Arc<dyn UriOpener>) -> Option<Arc<dyn UriOpener>> {
    None
}
