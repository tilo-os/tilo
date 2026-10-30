//! The apps search lists of this device: the Tilo apps installed here, the
//! machine's own, and the apps a person can get. Read once at startup and
//! again on a worker of their own whenever a directory they are read from
//! changes (an install, a desktop entry added, a registry's or Flathub's
//! catalogue refreshed, a block made), never on the session's thread.

use std::path::PathBuf;
use std::sync::Arc;

use tilo_desktop_search::providers::AppEntry;
use tilo_desktop_search::providers::host_apps::HostAppEntry;
use tilo_files::native::Watched;

/// The apps search lists, as last read.
#[derive(Clone, Default)]
pub struct AppLists {
    /// The Tilo apps installed here.
    pub applications: Vec<AppEntry>,
    /// The machine's own apps.
    pub host_apps: Vec<HostAppEntry>,
    /// The apps a person can get here: neither installed nor blocked.
    pub catalogue: Vec<app_catalogue::CatalogueEntry>,
}

impl AppLists {
    /// Read this device's lists: the installed Tilo apps, the machine's
    /// desktop entries, and the catalogue (the registries' listing as last
    /// accepted and Flathub's cached metadata) less what is here or
    /// blocked. Reads the disk only, never the network.
    #[must_use]
    pub fn read() -> Self {
        let installed = tilo_installed_apps::read_from_env().unwrap_or_else(|error| {
            tracing::warn!(%error, "the installed apps are not read");
            Vec::new()
        });
        let applications = installed
            .iter()
            .filter(|app| app.has_gui)
            .map(|app| AppEntry {
                id: app.package_id.clone(),
                title: app.display_name.clone(),
                category: app.category.clone(),
                description: None,
            })
            .collect();
        let scan = desktop_entry::scan::scan();
        let host_apps = scan
            .entries
            .iter()
            .filter(|entry| !entry.no_display && !entry.hidden)
            .map(|entry| HostAppEntry {
                desktop_id: entry.id.clone(),
                name: entry.name.clone(),
                generic_name: entry.generic_name.clone(),
                comment: entry.comment.clone(),
                keywords: entry.keywords.clone(),
            })
            .collect();
        let present = app_catalogue::Installed::from_ids(
            installed
                .iter()
                .map(|app| app.package_id.clone())
                .chain(scan.entries.iter().map(|entry| entry.id.clone())),
        );
        let catalogue = app_catalogue::Catalogue::from_sources(
            app_cli::last_accepted_offers()
                .iter()
                .map(app_catalogue::tilo::entry)
                .collect(),
            app_catalogue::appstream::load_cached("en"),
        );
        let blocks = app_catalogue::BlockList::load();
        let catalogue = catalogue.offered(&present, &blocks).cloned().collect();
        Self {
            applications,
            host_apps,
            catalogue,
        }
    }

    /// The directories [`Self::read`] reads, whose entries change with
    /// what it would find.
    #[must_use]
    pub fn directories() -> Vec<PathBuf> {
        let mut directories = tilo_installed_apps::store_directories();
        directories.extend(desktop_entry::scan::application_dirs());
        directories.extend(app_cli::last_accepted_directories());
        directories.extend(app_catalogue::appstream::cache_directories());
        directories.extend(app_catalogue::BlockList::directories());
        directories.dedup();
        directories
    }
}

/// Where search takes the app lists from: lists fixed up front, or lists
/// read again whenever their directories change.
#[derive(Clone)]
pub struct Lists(Held);

#[derive(Clone)]
enum Held {
    Fixed(Arc<AppLists>),
    Followed(Watched<AppLists>),
}

impl Default for Lists {
    fn default() -> Self {
        Self::fixed(AppLists::default())
    }
}

impl Lists {
    /// Lists that never change.
    #[must_use]
    pub fn fixed(lists: AppLists) -> Self {
        Self(Held::Fixed(Arc::new(lists)))
    }

    /// `read` now, and again on a worker named `name` after any change in
    /// `directories`.
    ///
    /// # Errors
    /// A directory cannot be watched, or the worker cannot start.
    pub fn follow(
        name: &str,
        directories: Vec<PathBuf>,
        read: impl Fn() -> AppLists + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        Watched::start(name, directories, read).map(|watched| Self(Held::Followed(watched)))
    }

    /// This device's lists ([`AppLists::read`]), following their
    /// directories; read once when they cannot be watched.
    #[must_use]
    pub fn of_device() -> Self {
        Self::follow("tilo-search-lists", AppLists::directories(), AppLists::read).unwrap_or_else(
            |error| {
                tracing::warn!(%error, "the app lists do not follow the device");
                Self::fixed(AppLists::read())
            },
        )
    }

    /// The lists as last read. Never touches the disk: the same value until
    /// a change was read.
    pub(crate) fn latest(&self) -> Arc<AppLists> {
        match &self.0 {
            Held::Fixed(lists) => Arc::clone(lists),
            Held::Followed(watched) => watched.get(),
        }
    }
}
