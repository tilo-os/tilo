//! The apps search lists of this device: the Tilo apps installed here, the machine's own, and the
//! apps a person can get. Read on a worker of their own, and again whenever a directory they are
//! read from changes (an install, a desktop entry added, a registry's or Flathub's catalogue
//! refreshed, a block made), never on the session's thread.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use tilo_broker_core::services::LaunchableApplication;
use tilo_desktop_search::providers::AppEntry;
use tilo_desktop_search::providers::catalogue::StoreCatalogue;
use tilo_desktop_search::providers::host_apps::HostAppEntry;
use tilo_desktop_search::providers::host_apps::HostApps;
use tilo_desktop_search::providers::settings::Settings;
use tilo_files::native::Watched;

use runtime_portal::tools::{
    AppEntry as ListedApp, AppKind as ListedKind, AppRemoval, MAX_LISTED_APPS,
};

/// The apps search lists, as last read.
#[derive(Clone, Default)]
pub struct AppLists {
    /// The Tilo apps installed here.
    pub applications: Vec<AppEntry>,
    /// The machine's own apps.
    pub host_apps: Vec<HostAppEntry>,
    /// The apps a person can get here: neither installed nor blocked.
    pub catalogue: Vec<app_catalogue::CatalogueEntry>,
    /// The icon file each installed app's signed manifest names, by package id: resolved here so a
    /// banner paints an app's artwork without reading its manifest on the letter's thread.
    pub icons: BTreeMap<String, PathBuf>,
    /// Every app that opens a kind of file or link here, and which kinds: what workspace settings
    /// offer as default openers.
    pub candidates: Vec<desktop_policy::AppCandidate>,
    /// What each installed app declared it may do on the action door under its own identity, by
    /// package id: the ceiling the broker admits an app's query and invoke against, never a grant.
    pub declared: BTreeMap<String, tilo_broker_core::access::AppActionAccess>,
    /// The apps a service can ask this session to open, as the action door lists them: each app's
    /// name, the actions its signed manifest offers while it is not running, and the capabilities a
    /// grant is decided on. What decides the reach the device's settings app has of the settings
    /// document.
    pub launchable: Vec<LaunchableApplication>,
    /// The person's apps as Files lists them through the `tools` service,
    /// every kind in one list: the same apps as above, with where each came
    /// from and so the one way it leaves the list.
    pub tools: Vec<ListedApp>,
}

impl AppLists {
    /// Read this device's lists: the installed Tilo apps, the machine's desktop entries, and the
    /// catalogue (the registries' listing as last accepted and Flathub's cached metadata) less what
    /// is here or blocked. Reads the disk only, never the network.
    #[must_use]
    pub fn read() -> Self {
        let installed = app_catalogue::apps::read_from_env().unwrap_or_else(|error| {
            tracing::warn!(%error, "the installed apps are not read");
            Vec::new()
        });
        // The person's interface language, as every name read here is in it.
        let locales = crate::app_names::entry_locales();
        let applications = installed
            .iter()
            .filter(|app| app.has_gui)
            .map(|app| AppEntry {
                id: app.package_id.clone(),
                // The name the person's interface language gives the app: a German machine lists
                // the app whose manifest says `[package.names] de = "Notizen"` as Notizen, and
                // every surface that names it — a tab, a start card, search — reads that name from
                // here.
                title: app.name_in(&locales).to_owned(),
                category: app.category.clone(),
                description: None,
            })
            .collect();
        let icons = installed
            .iter()
            .filter_map(|app| Some((app.package_id.clone(), app.icon_path()?)))
            .collect();
        let declared = declared(&installed);
        // The machine's entries through the memoised scan, which this read starts on the lists'
        // worker: a later ask for an entry's icon or name (`crate::app_icons::entry_icon`) finds it
        // read, or meets the read already under way rather than starting one.
        let scan = desktop_entry::cache::cached_scan(&locales);
        // An installed Tilo app may keep a desktop entry of its own, as its bundle does: that entry
        // is the same app, listed once.
        let tilo_apps =
            app_catalogue::Installed::from_ids(installed.iter().map(|app| app.package_id.clone()));
        // The entries Tilo keeps for the person's downloaded programs are search's own source
        // (`providers::downloaded`): it lists them with what the registry knows of them (their
        // kind, and a file that went away), which a host app row says nothing of.
        let host_apps = scan
            .entries
            .iter()
            .filter(|entry| machine_entry(entry, &tilo_apps))
            .map(|entry| HostAppEntry {
                desktop_id: entry.id.clone(),
                name: entry.name.clone(),
                generic_name: entry.generic_name.clone(),
                comment: entry.comment.clone(),
                keywords: entry.keywords.clone(),
            })
            .collect();
        let candidates: Vec<desktop_policy::AppCandidate> = installed
            .iter()
            .map(|app| desktop_policy::AppCandidate {
                package_id: app.package_id.clone(),
                display_name: app.display_name.clone(),
                resources: app
                    .opens_extensions
                    .iter()
                    .filter_map(|ext| desktop_policy::ResourceKind::extension(ext).ok())
                    .chain(
                        app.opens_mime_types
                            .iter()
                            .filter_map(|mime| desktop_policy::ResourceKind::mime(mime).ok()),
                    )
                    .chain(
                        app.opens_uri_schemes
                            .iter()
                            .filter_map(|scheme| desktop_policy::ResourceKind::scheme(scheme).ok()),
                    )
                    .collect::<std::collections::BTreeSet<_>>(),
                secondary: app.opens_secondary,
            })
            .chain(scan.entries.iter().filter_map(|entry| {
                let resources: std::collections::BTreeSet<_> = entry
                    .mime_types
                    .iter()
                    .filter_map(|mime| desktop_policy::ResourceKind::from_mimeapps_type(mime))
                    .collect();
                (!resources.is_empty() && !entry.no_display && !entry.hidden).then(|| {
                    desktop_policy::AppCandidate {
                        package_id: entry.id.clone(),
                        display_name: entry.name.clone(),
                        resources,
                        secondary: false,
                    }
                })
            }))
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
        let launchable = launchable(&installed, &scan.entries);
        let tools = listed_apps(&installed, &scan.entries, &icons);
        Self {
            applications,
            host_apps,
            catalogue,
            icons,
            candidates,
            declared,
            launchable,
            tools,
        }
    }

    /// The directories [`Self::read`] reads, whose entries change with what it would find.
    #[must_use]
    pub fn directories() -> Vec<PathBuf> {
        let mut directories = app_catalogue::apps::store_directories();
        directories.extend(desktop_entry::scan::application_dirs());
        directories.extend(app_cli::last_accepted_directories());
        directories.extend(app_catalogue::appstream::cache_directories());
        directories.extend(app_catalogue::BlockList::directories());
        directories.dedup();
        directories
    }
}

/// The apps a service can ask this session to open: this device's packages,
/// then the machine's own entries no package answers for. Only the
/// capabilities a grant is decided on are read, and only for the app the
/// image could grant one to: the manifest is read for that app alone.
fn launchable(
    installed: &[app_catalogue::apps::InstalledApp],
    entries: &[desktop_entry::entry::DesktopEntry],
) -> Vec<LaunchableApplication> {
    let packages: std::collections::HashSet<String> = installed
        .iter()
        .map(|app| app.package_id.to_ascii_lowercase())
        .collect();
    installed
        .iter()
        .filter(|app| app.has_gui)
        .map(|app| LaunchableApplication {
            capabilities: if settings_store::app_scope::SYSTEM_SETTINGS_APPS
                .contains(&app.package_id.as_str())
            {
                app.declared_capabilities()
                    .into_iter()
                    .filter(|capability| {
                        capability == settings_store::app_scope::SYSTEM_SETTINGS_CAPABILITY
                    })
                    .collect()
            } else {
                Vec::new()
            },
            package_id: app.package_id.clone(),
            display_name: app.display_name.clone(),
            unconfined: false,
            offers: app.offers.clone(),
        })
        .chain(
            entries
                .iter()
                .filter(|entry| !entry.no_display && !entry.hidden)
                .filter(|entry| !packages.contains(&entry.id.to_ascii_lowercase()))
                .map(|entry| LaunchableApplication {
                    capabilities: Vec::new(),
                    package_id: format!("{}{}", tilo_app_launch::launch::HOST_PREFIX, entry.id),
                    display_name: entry.name.clone(),
                    unconfined: true,
                    offers: Vec::new(),
                }),
        )
        .collect()
}

/// The person's apps as Files lists them, every kind in one list: the Tilo apps installed here,
/// then the machine's entries no package answers for. Where each came from says the one way it
/// leaves the list: a Tilo app the person added or a downloaded program is uninstalled, a Flatpak
/// of theirs with its data, and anything the system keeps is only taken off their apps.
fn listed_apps(
    installed: &[app_catalogue::apps::InstalledApp],
    entries: &[desktop_entry::entry::DesktopEntry],
    icons: &BTreeMap<String, PathBuf>,
) -> Vec<ListedApp> {
    // Which copy of each Tilo app runs: the system image's, the person's own,
    // or theirs rolled back below the image's.
    let from: std::collections::HashMap<String, String> = app_cli::installed_apps()
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "the installed app registry is not read");
            Vec::new()
        })
        .into_iter()
        .map(|app| (app.package_id.to_ascii_lowercase(), app.from))
        .collect();
    let packages: std::collections::HashSet<String> = installed
        .iter()
        .map(|app| app.package_id.to_ascii_lowercase())
        .collect();
    let person_applications = tilo_paths::xdg::data_home().map(|home| home.join("applications"));
    let person_flatpaks = tilo_paths::xdg::data_home().map(|home| home.join("flatpak"));
    let mut listed: Vec<ListedApp> = installed
        .iter()
        .filter(|app| app.has_gui)
        .map(|app| {
            let added = from
                .get(&app.package_id.to_ascii_lowercase())
                .is_some_and(|from| from == "user");
            ListedApp {
                id: app.package_id.clone(),
                name: app.display_name.clone(),
                package: Some(app.package_id.clone()),
                icon: icons
                    .get(&app.package_id)
                    .map(|icon| icon.display().to_string()),
                program: None,
                removal: if added {
                    AppRemoval::Uninstall
                } else {
                    AppRemoval::RemoveFromApps
                },
                kind: ListedKind::Tilo,
                size: None,
            }
        })
        .chain(
            entries
                .iter()
                .filter(|entry| !entry.no_display && !entry.hidden)
                .filter(|entry| !packages.contains(&entry.id.to_ascii_lowercase()))
                .map(|entry| {
                    let kind = entry.kind(person_applications.as_deref());
                    let of_person = person_flatpaks
                        .as_ref()
                        .is_some_and(|flatpaks| entry.path.starts_with(flatpaks));
                    let (kind, removal) = match kind {
                        desktop_entry::EntryKind::Flatpak if of_person => {
                            (ListedKind::Flatpak, AppRemoval::UninstallWithData)
                        }
                        // A Flatpak of the device's installation is uninstalled by whoever
                        // administers it; here it is taken off the person's apps.
                        desktop_entry::EntryKind::Flatpak => {
                            (ListedKind::Flatpak, AppRemoval::RemoveFromApps)
                        }
                        desktop_entry::EntryKind::AppImage => {
                            (ListedKind::AppImage, AppRemoval::Uninstall)
                        }
                        desktop_entry::EntryKind::Program if entry.downloaded.is_some() => {
                            (ListedKind::Program, AppRemoval::Uninstall)
                        }
                        desktop_entry::EntryKind::Program => {
                            (ListedKind::Program, AppRemoval::RemoveFromApps)
                        }
                        desktop_entry::EntryKind::System => {
                            (ListedKind::System, AppRemoval::RemoveFromApps)
                        }
                    };
                    ListedApp {
                        id: entry.id.clone(),
                        name: entry.name.clone(),
                        package: None,
                        icon: None,
                        program: entry
                            .downloaded
                            .as_ref()
                            .map(|program| program.program.display().to_string()),
                        removal,
                        kind,
                        size: None,
                    }
                }),
        )
        .filter(ListedApp::fits)
        .collect();
    listed.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    listed.truncate(MAX_LISTED_APPS);
    listed
}

/// Whether a desktop entry is one of the machine's own to list: it shows,
/// is not a program Tilo downloaded (search lists those itself), and is not
/// an installed Tilo app's own entry — that app is listed as itself.
fn machine_entry(
    entry: &desktop_entry::DesktopEntry,
    tilo_apps: &app_catalogue::Installed,
) -> bool {
    !entry.no_display
        && !entry.hidden
        && entry.downloaded.is_none()
        && !tilo_apps.contains(&entry.id)
}

/// Each app's declared reach of the action door, from its signed manifest: the ceiling the broker
/// admits its calls against.
fn declared(
    installed: &[app_catalogue::apps::InstalledApp],
) -> BTreeMap<String, tilo_broker_core::access::AppActionAccess> {
    installed
        .iter()
        .map(|app| {
            let declared = app.declared_actions();
            (
                app.package_id.clone(),
                tilo_broker_core::access::AppActionAccess {
                    query: declared.query,
                    invoke: declared.invoke,
                },
            )
        })
        .collect()
}

/// Where search takes the app lists from: lists fixed up front, or lists read again whenever their
/// directories change.
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

    /// `read` on a worker named `name`, as it starts, and again after any
    /// change in `directories`. Starting never waits for the disk: until the
    /// first read lands the lists are empty, and whoever shows them hears of
    /// the change ([`Watched::on_change`]).
    ///
    /// # Errors
    /// A directory cannot be watched, or the worker cannot start.
    pub fn follow(
        name: &str,
        directories: Vec<PathBuf>,
        read: impl Fn() -> AppLists + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        Watched::start_unread(name, directories, AppLists::default(), read)
            .map(|watched| Self(Held::Followed(watched)))
    }

    /// This device's lists ([`AppLists::read`]), following their directories; read once when they
    /// cannot be watched.
    #[must_use]
    pub fn of_device() -> Self {
        Self::follow("tilo-search-lists", AppLists::directories(), AppLists::read).unwrap_or_else(
            |error| {
                tracing::warn!(%error, "the app lists do not follow the device");
                Self::fixed(AppLists::read())
            },
        )
    }

    /// The lists as last read. Never touches the disk: the same value until a change was read.
    pub(crate) fn latest(&self) -> Arc<AppLists> {
        match &self.0 {
            Held::Fixed(lists) => Arc::clone(lists),
            Held::Followed(watched) => watched.get(),
        }
    }
}

impl crate::platform::apps::Apps for Lists {
    fn reading(&self) -> Arc<dyn crate::platform::apps::Reading> {
        Arc::new(Listed(self.latest()))
    }

    fn name(&self, id: &str) -> String {
        crate::app_names::app_name(id)
    }

    fn applications(&self) -> Vec<AppEntry> {
        self.latest().applications.clone()
    }

    fn host_apps(&self) -> Vec<HostAppEntry> {
        self.latest().host_apps.clone()
    }

    fn installed(&self, id: &str) -> bool {
        app_catalogue::apps::read_one_from_env(id).is_ok_and(|installed| installed.is_some())
    }
}

/// One reading of this device's lists, as search builds its sources from: the sources it keeps
/// until the worker reads another one.
struct Listed(Arc<AppLists>);

impl crate::platform::apps::Reading for Listed {
    fn generation(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }

    fn host_apps(
        &self,
        preferences: &tilo_desktop_search::SearchPreferences,
    ) -> Option<tilo_desktop_search::Held> {
        Some(tilo_desktop_search::Held::new(HostApps::new(
            self.0.host_apps.clone(),
            preferences,
        )))
    }

    fn catalogue(&self, workspace: &str) -> Option<tilo_desktop_search::Held> {
        Some(tilo_desktop_search::Held::new(StoreCatalogue::new(
            self.0.catalogue.clone(),
            workspace,
        )))
    }
}

/// The settings this desktop offers search: the pages of the person's own settings.
pub(crate) struct DesktopSettings;

impl crate::platform::apps::Settings for DesktopSettings {
    fn source(
        &self,
        workspace: &str,
        published: &[tilo_actions::ActionDescriptor],
    ) -> tilo_desktop_search::Held {
        // Settings the settings service changes in place are found as its actions.
        let in_place: std::collections::HashSet<String> = published
            .iter()
            .filter_map(|action| {
                tilo_settings_service::setting_of(action.address.resource.as_str())
                    .map(str::to_owned)
            })
            .collect();
        tilo_desktop_search::Held::new(Settings::changing_in_place(workspace, &|id| {
            in_place.contains(id)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> desktop_entry::DesktopEntry {
        desktop_entry::entry::parse(
            "[Desktop Entry]\nType=Application\nName=Notes\nExec=notes\n",
            id,
            std::path::Path::new("/usr/share/applications/notes.desktop"),
            &[],
        )
        .expect("a desktop entry")
    }

    #[test]
    fn an_installed_tilo_apps_own_entry_is_not_one_of_the_machines() {
        let tilo_apps = app_catalogue::Installed::from_ids(["so.tilo.notes"]);
        // The app is listed as itself; the desktop entry its bundle keeps is the same app, so
        // search names it once.
        assert!(!machine_entry(&entry("so.tilo.notes"), &tilo_apps));
        // Any other app's entry is one of the machine's.
        assert!(machine_entry(&entry("org.gnome.notes"), &tilo_apps));
    }

    /// The machine's own apps are named in the person's language, not in the machine's: on a German
    /// desktop with English chosen the list reads `Files` , and the same desktop with no language
    /// chosen reads `Dateien` .
    #[test]
    fn the_machines_apps_are_named_in_the_persons_language() {
        let home = tempfile::tempdir().unwrap();
        let data = home.path().join("data");
        let applications = data.join("applications");
        std::fs::create_dir_all(&applications).unwrap();
        std::fs::write(
            applications.join("org.example.Files.desktop"),
            "[Desktop Entry]\nType=Application\nName=Files\nName[de]=Dateien\n\
             Exec=/bin/true\n",
        )
        .unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", home.path());
        env.set("XDG_DATA_HOME", &data);
        env.set("XDG_DATA_DIRS", &data);
        env.set("LANG", "de_DE.UTF-8");
        for unset in ["LC_ALL", "LC_MESSAGES", "LANGUAGE"] {
            env.remove(unset);
        }
        let listed = || {
            AppLists::read()
                .host_apps
                .iter()
                .find(|app| app.desktop_id == "org.example.Files")
                .map(|app| app.name.clone())
        };

        // The person chose English, on a German machine.
        let english = serde_json::json!({"appearance": {"language": "en-US"}})
            .as_object()
            .cloned()
            .unwrap_or_default();
        let chosen = settings_store::scoped_source(Some(std::sync::Arc::new(
            settings_store::HostDocument::new(english, |_| Ok(())),
        )));
        assert_eq!(
            listed().as_deref(),
            Some("Files"),
            "the person's language names the machine's apps"
        );
        drop(chosen);

        // Nothing chosen: the machine's locale is the default, not a second source that survives a
        // choice.
        let none = settings_store::scoped_source(Some(std::sync::Arc::new(
            settings_store::HostDocument::new(serde_json::Map::new(), |_| Ok(())),
        )));
        assert_eq!(
            listed().as_deref(),
            Some("Dateien"),
            "with no choice the device's own locale decides"
        );
        drop(none);
    }

    #[test]
    fn the_lists_arrive_from_their_worker() {
        let dir = tempfile::tempdir().unwrap();
        let lists = Lists::follow(
            "tilo-search-lists-test",
            vec![dir.path().to_owned()],
            || AppLists {
                applications: vec![AppEntry {
                    id: "so.tilo.notes".to_owned(),
                    title: "Notes".to_owned(),
                    category: None,
                    description: None,
                }],
                ..AppLists::default()
            },
        )
        .unwrap();
        // The worker's own read is the one that counts: what this thread has asked for never does.
        tilo_test_env::wait_until("the lists' worker to read them", || {
            lists
                .latest()
                .applications
                .iter()
                .any(|app| app.id == "so.tilo.notes")
        });
    }
}
