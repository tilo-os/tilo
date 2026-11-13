//! The session shell's choosers where no Tilo session launches apps
//! (Windows, a browser page): no process is ever handed a connection, so
//! [`ShellLink::inherited`] is always `None` and portable callers take their
//! fallback without selecting a target themselves.

use std::sync::Arc;

use crate::{FilePicker, UriOpener};

#[derive(Clone)]
pub struct ShellLink(Never);

#[derive(Clone, Copy)]
enum Never {}

impl ShellLink {
    pub fn inherited() -> Option<Self> {
        None
    }

    #[must_use]
    pub fn picker(&self, _unattended: Option<Arc<dyn FilePicker>>) -> Arc<dyn FilePicker> {
        match self.0 {}
    }

    #[must_use]
    pub fn opener(&self, _elsewhere: Arc<dyn UriOpener>) -> Arc<dyn UriOpener> {
        match self.0 {}
    }

    pub fn choose_app(
        &self,
        _purpose: &str,
        _chosen: impl FnOnce(Result<Option<ChosenApp>, String>) + Send + 'static,
    ) {
        match self.0 {}
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChosenApp {
    pub id: String,
    pub name: String,
    pub window_ids: Vec<String>,
}

pub fn inherited_picker(_unattended: Option<Arc<dyn FilePicker>>) -> Option<Arc<dyn FilePicker>> {
    None
}

pub fn inherited_opener(_elsewhere: Arc<dyn UriOpener>) -> Option<Arc<dyn UriOpener>> {
    None
}
