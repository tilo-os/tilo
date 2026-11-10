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

use tilo_desktop_search::SearchPreferences;
use tilo_desktop_surface::DesktopHandle;
use tilo_session::platform::apps::Apps;
use tilo_session::search::{SearchOptions, SessionSearch};

/// The session's search over this page's desk, mounted as a native desktop mounts it: the bundle's
/// apps as the apps installed here, the records the workspace's indexes name as `store` holds them
/// (what the page's replication took in, noted in its one stream of record changes), and the page's
/// own model service as the gateway its questions run on.
pub(crate) fn mount(
    handle: &DesktopHandle,
    person: &str,
    store: tilo_session::platform::Store,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Rc<SessionSearch> {
    let search = Rc::new(SessionSearch::new(
        SearchOptions {
            preferences: SearchPreferences::default(),
            records: Some(tilo_session::records::Records::over(
                store,
                crate::replication::changes(),
            )),
            ..SearchOptions::default()
        },
        Some(Arc::new(crate::page_apps::PageApps) as Arc<dyn Apps>),
        person,
        None,
        Some(wake),
        Some(Arc::new(PageGateway) as Arc<dyn tilo_session::platform::Models>),
    ));
    handle.set_search_host(Rc::clone(&search) as Rc<dyn tilo_desktop_surface::SearchHost>);
    search
}

/// The model service this page runs, as the session's ask reaches it: its models hold the page's
/// own handles, so each question builds the capability where it is asked.
struct PageGateway;

impl tilo_session::platform::Models for PageGateway {
    fn models(&self) -> Option<ui::models::Models> {
        Some(crate::host_services::page_models())
    }
}
