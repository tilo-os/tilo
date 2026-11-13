//! The apps this page has, as a host lists them: the bundle's own catalogue, which is every app a
//! window here can show. A session role that offers an app by name — the roster's card offering to
//! write to a person in Chat — asks this and never the bundle.
//!
//! The apps a palette offers to open are these, as a host's installed apps are its own: the bundle
//! is what this page has, and it has no machine apps or store catalogue beside them.

use std::sync::Arc;

use tilo_desktop_search::providers::AppEntry;
use tilo_desktop_search::providers::host_apps::HostAppEntry;
use tilo_desktop_search::{Held, SearchPreferences};
use tilo_session::platform::apps::{Apps, Reading};

pub(crate) struct PageApps;

impl Apps for PageApps {
    /// The bundle's apps, as one reading: they are the same for as long as the page runs.
    fn reading(&self) -> Arc<dyn Reading> {
        Arc::new(Shipped)
    }

    fn name(&self, id: &str) -> String {
        crate::desktop::known_application(id).map_or_else(String::new, str::to_owned)
    }

    /// The bundle's apps, each a launch here: what a palette or a new tab offers to open, and the
    /// apps installed on this host's own terms.
    fn applications(&self) -> Vec<AppEntry> {
        crate::desktop::known_applications()
            .into_iter()
            .filter(|(id, _)| tilo_desktop_search::availability::runs_on(id, &["web"]))
            .map(|(id, title)| AppEntry {
                id: id.into(),
                title: title.into(),
                category: None,
                description: None,
            })
            .collect()
    }

    fn host_apps(&self) -> Vec<HostAppEntry> {
        Vec::new()
    }

    fn installed(&self, id: &str) -> bool {
        crate::desktop::known_application(id).is_some()
    }
}

/// The one reading of the bundle's apps.
struct Shipped;

impl Reading for Shipped {
    fn generation(&self) -> usize {
        0
    }

    /// The machine's own apps: a page runs none, so its search offers none beyond the bundle's own
    /// list ([`Apps::applications`]).
    fn host_apps(&self, _preferences: &SearchPreferences) -> Option<Held> {
        None
    }

    /// The store a person gets apps from: a page has none of its own.
    fn catalogue(&self, _workspace: &str) -> Option<Held> {
        None
    }
}
