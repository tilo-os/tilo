//! The page's palette is the session's search: the one role a native desktop runs stands here too,
//! over what this page publishes.
//!
//! [`tilo_session::search::SessionSearch`] is the desktop's search beyond its own windows and
//! commands — the apps this host has, what stands over the desk as published actions, the question
//! the person types, and the sources they switched off — and the rows a host alone can carry out.
//! The desk draws the palette and the row the person chose ([`tilo_desktop_surface::SearchHost`]);
//! this page hands over what only it knows and takes the choices back to the desk it arranges.

use std::rc::Rc;
use std::sync::Arc;

use tilo_actions::DoorError;
use tilo_desktop_search::{InvokeTarget, SearchPreferences, Target};
use tilo_desktop_surface::DesktopHandle;
use tilo_session::platform::apps::Apps;
use tilo_session::search::{SearchOptions, SessionSearch};

/// The session's search over this page's desk, mounted as a native desktop mounts it: the bundle's
/// apps as the apps installed here.
///
/// A page runs no model gateway the session's ask can reach — the page's own service keeps its
/// models on the page's thread — so its palette offers no question until the ask runs here too.
pub(crate) fn mount(
    handle: &DesktopHandle,
    person: &str,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Rc<SessionSearch> {
    let search = Rc::new(SessionSearch::new(
        SearchOptions {
            preferences: SearchPreferences::default(),
            ..SearchOptions::default()
        },
        Some(Arc::new(crate::page_apps::PageApps) as Arc<dyn Apps>),
        person,
        None,
        Some(wake),
        None,
    ));
    handle.set_search_host(Rc::clone(&search) as Rc<dyn tilo_desktop_surface::SearchHost>);
    search
}

/// The rows the person chose since the last call, carried out as the session carries them out: the
/// desk's own published actions go through `own`, and a row that could not be carried out is handed
/// back for the palette to say why on it.
pub(crate) fn chosen(
    handle: &DesktopHandle,
    search: &SessionSearch,
    workspace: &str,
    own: &mut dyn FnMut(&InvokeTarget) -> Result<(), DoorError>,
) -> Vec<(Target, DoorError)> {
    let mut refused = Vec::new();
    for target in handle.take_search_choices() {
        if let Err(error) = search.perform(workspace, target.clone(), own) {
            refused.push((target, error));
        }
    }
    refused
}
