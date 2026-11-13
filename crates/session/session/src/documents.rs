//! Which app opens a file or a link here.
//!
//! Files' Open, a search row and a link all ask the same question, and it is answered once
//! ([`tilo_app_launch::routing`]): the app this workspace's rules or the person's own defaults
//! name for the type, else a Tilo app that claims it, else the app this machine opens the type
//! with, else nothing and the person is asked which app they want. What this module adds is
//! where those defaults are read from — the workspace's own record, the person's settings, and
//! this machine's `mimeapps.list` — so no caller assembles them itself.

use std::path::Path;
use std::sync::Arc;

use tilo_app_launch::Launch;
use tilo_app_launch::routing;
use tilo_desktop_surface::DesktopHandle;

use crate::lists::AppLists;

/// What this session decides an open with, read once: this device's apps, the defaults that
/// decide among them, and the machine's own lists.
struct Deciding {
    lists: Arc<AppLists>,
    personal: desktop_policy::PersonalDefaults,
    host: desktop_policy::MimeAppsChain,
    workspace: Option<desktop_policy::WorkspaceDefaults>,
}

impl Deciding {
    /// Read what decides an open in `workspace`: its own record when the desk shows it, the
    /// person's defaults as their settings hold them, and the machine's default applications.
    fn read(desk: Option<&DesktopHandle>, workspace: &str) -> Self {
        Self {
            // The lists search reads, kept current by their own worker: the same apps a row, a
            // card and this answer name.
            lists: crate::app_icons::app_lists().unwrap_or_default(),
            personal: desktop_policy::PersonalDefaults::load(),
            host: desktop_policy::mimeapps::session_chain(),
            workspace: desk.and_then(|desk| {
                desk.read(|tree| {
                    tree.workspaces
                        .iter()
                        .find(|shown| shown.id().to_string() == workspace)
                        .map(|shown| shown.default_apps.clone())
                })
            }),
        }
    }

    /// The defaults that decide among `self`'s apps, for `person`.
    fn defaults<'a>(&'a self, person: &'a str) -> routing::Defaults<'a> {
        routing::Defaults {
            person,
            personal: &self.personal,
            workspace: self.workspace.as_ref(),
            host: Some(&self.host),
        }
    }
}

/// Every app that opens the document at `path` in `workspace`, the one it opens with today first:
/// what Files' "Open with" lists, and what opening the file starts.
pub(crate) fn openers(
    desk: Option<&DesktopHandle>,
    workspace: &str,
    person: &str,
    path: &Path,
) -> Vec<Launch> {
    let deciding = Deciding::read(desk, workspace);
    tracing::debug!(
        path = %path.display(),
        candidates = deciding.lists.candidates.len(),
        handlers = deciding.personal.handlers.len(),
        workspace_defaults = deciding.workspace.as_ref().map_or(0, |ws| ws.policy.len()),
        "which apps open a file"
    );
    routing::documents(path, &deciding.lists.candidates, &deciding.defaults(person))
}

/// The app that opens `uri` in `workspace`; `None` when nothing here takes it.
pub(crate) fn opener(
    desk: Option<&DesktopHandle>,
    workspace: &str,
    person: &str,
    uri: &str,
) -> Option<Launch> {
    let deciding = Deciding::read(desk, workspace);
    routing::link(uri, &deciding.lists.candidates, &deciding.defaults(person))
}

/// The app `launch` starts, as a launch names it: a Tilo package's id, or a desktop-file id of one
/// of the machine's own apps.
pub(crate) fn app_of(launch: &Launch) -> Option<&str> {
    match launch {
        Launch::Package { app, .. } => Some(app),
        Launch::Host { desktop_id, .. } => Some(desktop_id),
        _ => None,
    }
}
