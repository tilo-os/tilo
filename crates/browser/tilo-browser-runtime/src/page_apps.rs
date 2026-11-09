//! The apps this page has, as a host lists them: the bundle's own catalogue, which is every app a
//! window here can show. A session role that offers an app by name — the roster's card offering to
//! write to a person in Chat — asks this and never the bundle.
//!
//! The lists a palette searches are the page's own desk to answer; these say what stands here, and
//! offer the bundle's apps as one source, as a host's own apps are offered on a desktop.

use std::sync::Arc;

use tilo_desktop_search::ResourceKind;
use tilo_desktop_search::model::{Hit, Source};
use tilo_desktop_search::model::LaunchPlace;
use tilo_desktop_search::Target;
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

    fn applications(&self) -> Vec<AppEntry> {
        Vec::new()
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

    fn host_apps(&self, _preferences: &SearchPreferences) -> Held {
        Held::new(Bundle)
    }

    fn catalogue(&self, _workspace: &str) -> Held {
        Held::new(Bundle)
    }
}

/// The bundle's apps, as search offers them: each by the title it shows.
struct Bundle;

impl Source for Bundle {
    fn id(&self) -> &'static str {
        "page-apps"
    }

    fn name(&self) -> String {
        String::new()
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        let needle = needle.to_lowercase();
        crate::desktop::known_applications()
            .into_iter()
            .filter(|(_, title)| needle.is_empty() || title.to_lowercase().contains(&needle))
            .map(|(id, title)| Hit {
                row: None,
                preview: None,
                faces: Vec::new(),
                outcome: None,
                kind: ResourceKind::Application,
                provider: "page-apps".to_owned(),
                id: id.to_owned(),
                title: title.to_owned(),
                subtitle: None,
                facts: Vec::new(),
                score: 0,
                icon: None,
                target: Target::Launch {
                    app: id.to_owned(),
                    place: LaunchPlace::Focus,
                },
            })
            .collect()
    }
}
