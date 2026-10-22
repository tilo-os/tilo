//! The person's apps, every kind in one list, and the one way each leaves it.
//!
//! This is a view of the applications model search and the launcher already
//! read, not a second list: the installed Tilo packages
//! ([`crate::shell::launcher::gui_apps_snapshot`]) and the desktop entries
//! ([`crate::shell::host_apps`]), which is where Flatpaks, downloaded
//! programs and the system's menu entries all live. A desktop entry an
//! installed Tilo package answers for is the package's, as in search.
//!
//! Files' Applications place lists it ([`listed`], over the `tools` service or
//! the in-process route), and Uninstall is one action wherever it is asked
//! for: Files, search, the Store and an agent all reach [`ask`] (the person's
//! sheet, [`crate::shell::overlay_host`]) or [`uninstall`] (the work, on a
//! worker). What uninstalling does depends only on where the app came from
//! ([`AppSource::removal`]):
//!
//! - a Flatpak in the person's installation is uninstalled, and its data
//!   (`~/.var/app/<id>`) moves to the Trash unless the person keeps it;
//! - a downloaded program's file moves to the Trash and its entry goes;
//! - a Tilo app the person added is removed; its documents are workspace
//!   data and nothing here touches them;
//! - a part of the system can only be taken off the person's apps: a
//!   `Hidden` entry in their own applications directory hides it, and an
//!   entry file of their own moves to the Trash first.
//!
//! Nothing here deletes anything of the person's; what leaves goes to the
//! Trash. Every function that touches the disk runs on a worker.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use os_actions::{AppEntry, AppKind, AppRemoval};
use tilo_desktop::notifications::{Notification, NotificationSeverity};
use tilo_lock::MutexRecover as _;

use crate::shell::i18n::{t, t_args};

/// Output kept from an uninstall command, for its error message.
const COMMAND_OUTPUT_LIMIT: usize = 256 * 1024;
/// How long one uninstall command may run.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The edge an app's icon is looked up at for Files' grid.
const ICON_PX: u32 = 64;

/// Where an app came from, which decides what uninstalling it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppSource {
    /// A Tilo package. `added` when it is in the person's own store and
    /// the system image ships no copy of it.
    Tilo { added: bool },
    /// A Flatpak: `app_id` is what `flatpak` takes; `user` when it is in the
    /// person's own installation, which they may change.
    Flatpak { app_id: String, user: bool },
    /// A program the person downloaded: an `AppImage`, or a program from a
    /// tarball or a loose binary. `entry` is the file the app list holds it
    /// by, which Tilo wrote.
    Downloaded { program: PathBuf, entry: PathBuf },
    /// A menu entry the person put in their own applications directory.
    PersonEntry { path: PathBuf },
    /// A menu entry of the operating system.
    System,
}

impl AppSource {
    /// What uninstalling an app from here does.
    #[must_use]
    pub fn removal(&self) -> AppRemoval {
        match self {
            Self::Tilo { added: true } | Self::Downloaded { .. } => AppRemoval::Uninstall,
            Self::Tilo { added: false } => AppRemoval::None,
            Self::Flatpak { user: true, .. } => AppRemoval::UninstallWithData,
            Self::Flatpak { user: false, .. } | Self::PersonEntry { .. } | Self::System => {
                AppRemoval::RemoveFromApps
            }
        }
    }
}

/// One app in the person's list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppListItem {
    /// A Tilo package id or a desktop-file id: the address search uses for
    /// the same app.
    pub id: String,
    pub name: String,
    /// The icon file, when the app has one.
    pub icon: Option<PathBuf>,
    pub source: AppSource,
    /// What it takes on disk, once measured; `None` where that is unknown.
    pub size: Option<u64>,
}

impl AppListItem {
    #[must_use]
    pub fn removal(&self) -> AppRemoval {
        self.source.removal()
    }

    /// The Tilo package id, for a Tilo app.
    #[must_use]
    pub fn package(&self) -> Option<&str> {
        matches!(self.source, AppSource::Tilo { .. }).then_some(self.id.as_str())
    }

    /// Where it came from, as the person knows it.
    #[must_use]
    pub fn kind(&self) -> AppKind {
        match &self.source {
            AppSource::Tilo { .. } => AppKind::Tilo,
            AppSource::Flatpak { .. } => AppKind::Flatpak,
            AppSource::Downloaded { program, .. } => {
                if program
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("appimage"))
                {
                    AppKind::AppImage
                } else {
                    AppKind::Program
                }
            }
            AppSource::PersonEntry { .. } => AppKind::Program,
            AppSource::System => AppKind::System,
        }
    }

    /// The app as Files lists it.
    #[must_use]
    pub fn entry(&self) -> AppEntry {
        let program = match &self.source {
            AppSource::Downloaded { program, .. } => Some(program.display().to_string()),
            _ => None,
        };
        AppEntry {
            id: self.id.clone(),
            name: self.name.clone(),
            package: self.package().map(str::to_owned),
            icon: self.icon.as_ref().map(|path| path.display().to_string()),
            program,
            removal: self.removal(),
            kind: self.kind(),
            size: self.size,
        }
    }
}

/// A Tilo package as the list reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TiloApp {
    pub package_id: String,
    pub name: String,
    /// In the person's own store.
    pub in_user_store: bool,
    /// The system image ships a copy.
    pub in_image: bool,
}

/// A desktop entry as the list reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostEntry {
    pub desktop_id: String,
    pub name: String,
    /// The entry file.
    pub path: PathBuf,
    /// `X-Flatpak=`: the Flatpak it was exported by.
    pub flatpak: Option<String>,
    /// The program of a downloaded program's entry.
    pub downloaded: Option<PathBuf>,
}

/// Where the person's own things are, which tells their apps from the
/// system's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Places {
    /// `$XDG_DATA_HOME`.
    pub data_home: PathBuf,
}

impl Places {
    #[must_use]
    pub fn of_session() -> Option<Self> {
        tilo_paths::xdg::data_home().map(|data_home| Self { data_home })
    }

    /// The person's own applications directory, where an entry of theirs
    /// lives and a hidden one is written.
    #[must_use]
    pub fn applications(&self) -> PathBuf {
        self.data_home.join("applications")
    }

    fn source_of(&self, entry: &HostEntry) -> AppSource {
        if let Some(app_id) = &entry.flatpak {
            return AppSource::Flatpak {
                app_id: app_id.clone(),
                user: entry.path.starts_with(self.data_home.join("flatpak")),
            };
        }
        if let Some(program) = &entry.downloaded {
            return AppSource::Downloaded {
                program: program.clone(),
                entry: entry.path.clone(),
            };
        }
        if entry.path.starts_with(self.applications()) {
            return AppSource::PersonEntry {
                path: entry.path.clone(),
            };
        }
        AppSource::System
    }
}

/// The list, sorted by name: every Tilo app, then every desktop entry no
/// Tilo package answers for. Pure, so what each kind becomes is assertable.
#[must_use]
pub fn project(tilo: &[TiloApp], host: &[HostEntry], places: &Places) -> Vec<AppListItem> {
    let packages: std::collections::HashSet<String> = tilo
        .iter()
        .map(|app| app.package_id.to_ascii_lowercase())
        .collect();
    let mut items: Vec<AppListItem> = tilo
        .iter()
        .map(|app| AppListItem {
            id: app.package_id.clone(),
            name: app.name.clone(),
            icon: None,
            size: None,
            source: AppSource::Tilo {
                added: app.in_user_store && !app.in_image,
            },
        })
        .chain(
            host.iter()
                .filter(|entry| !packages.contains(&entry.desktop_id.to_ascii_lowercase()))
                .map(|entry| AppListItem {
                    id: entry.desktop_id.clone(),
                    name: entry.name.clone(),
                    icon: None,
                    size: None,
                    source: places.source_of(entry),
                }),
        )
        .collect();
    items.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    items
}

// ---------------------------------------------------------------------
// The session's list, kept current on a worker.
// ---------------------------------------------------------------------

type Generation = (u64, u64);

#[derive(Default)]
struct Cache {
    built_for: Option<Generation>,
    items: Arc<Vec<AppListItem>>,
}

static CACHE: std::sync::LazyLock<Mutex<Cache>> = std::sync::LazyLock::new(Mutex::default);
static REFRESHING: AtomicBool = AtomicBool::new(false);
static WAKE: Mutex<Option<crate::session_runtime::Wake>> = Mutex::new(None);

/// Wake the session's loop with `wake` when a question is asked, a read of
/// the list lands or an uninstall finishes.
pub(crate) fn set_wake(wake: crate::session_runtime::Wake) {
    *WAKE.lock_recover() = Some(wake);
}

fn wake() {
    let wake = WAKE.lock_recover().clone();
    if let Some(wake) = wake {
        wake();
    }
}

fn current_generation() -> Generation {
    (
        crate::shell::host_apps::host_apps_generation(),
        crate::shell::launcher::gui_apps_generation(),
    )
}

/// The person's apps as last read, never waiting on the disk: one whose
/// sources changed since is read again on a worker and served when it lands.
#[must_use]
pub fn listed() -> Arc<Vec<AppListItem>> {
    let (fresh, items) = {
        let cache = CACHE.lock_recover();
        (
            cache.built_for == Some(current_generation()),
            Arc::clone(&cache.items),
        )
    };
    if !fresh {
        refresh_in_background();
    }
    items
}

/// Whether a read of the list is under way, so an answer given now may be
/// missing an app on its way.
#[must_use]
pub fn settling() -> bool {
    REFRESHING.load(Ordering::Acquire)
}

/// The listed app `id`, by its id or its Flatpak id.
#[must_use]
pub fn find(id: &str) -> Option<AppListItem> {
    find_in(&listed(), id)
}

fn find_in(items: &[AppListItem], id: &str) -> Option<AppListItem> {
    items
        .iter()
        .find(|item| item.id == id)
        .or_else(|| {
            items.iter().find(
                |item| matches!(&item.source, AppSource::Flatpak { app_id, .. } if app_id == id),
            )
        })
        .cloned()
}

fn refresh_in_background() {
    if REFRESHING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("tilo-app-list".into())
        .spawn(|| {
            let (built_for, mut items, measures) = read();
            {
                let mut cache = CACHE.lock_recover();
                cache.built_for = Some(built_for);
                cache.items = Arc::new(items.clone());
            }
            REFRESHING.store(false, Ordering::Release);
            wake();
            // Sizes take a walk of each app's files: the list is served
            // first, and gains them once they are counted.
            for (item, measure) in items.iter_mut().zip(&measures) {
                item.size = measure.as_deref().and_then(size_on_disk);
            }
            let mut cache = CACHE.lock_recover();
            if cache.built_for == Some(built_for) {
                cache.items = Arc::new(items);
                drop(cache);
                wake();
            }
        });
    if spawned.is_err() {
        REFRESHING.store(false, Ordering::Release);
    }
}

/// Read the list from its sources, with where each app's size is measured.
/// Blocks: a worker's only.
fn read() -> (Generation, Vec<AppListItem>, Vec<Option<PathBuf>>) {
    let (installed_generation, installed) = crate::shell::launcher::gui_apps_snapshot();
    let (host_generation, scan) = desktop_entry::cache::cached_scan_snapshot();
    let catalog = package_store::Catalog::from_env().ok();
    let mut payloads = std::collections::HashMap::new();
    let tilo: Vec<TiloApp> = installed
        .iter()
        .map(|app| {
            let layers = catalog
                .as_ref()
                .and_then(|catalog| catalog.layers(&app.package_id).ok());
            if let Some(copy) = layers
                .as_ref()
                .and_then(|layers| layers.user.as_ref().or(layers.image.as_ref()))
            {
                payloads.insert(app.package_id.clone(), copy.payload_root.clone());
            }
            TiloApp {
                package_id: app.package_id.clone(),
                name: app
                    .name_for(&crate::shell::i18n::current_locale())
                    .to_owned(),
                in_user_store: layers.as_ref().is_some_and(|layers| layers.user.is_some()),
                // Unknown layers count as the image's: an app is never
                // offered for uninstalling on a guess.
                in_image: layers.as_ref().is_none_or(|layers| layers.image.is_some()),
            }
        })
        .collect();
    let host = host_entries(&scan);
    let generation = (host_generation, installed_generation);
    let Some(places) = Places::of_session() else {
        return (generation, Vec::new(), Vec::new());
    };
    let mut items = project(&tilo, &host, &places);
    let mut measures = Vec::with_capacity(items.len());
    for item in &mut items {
        let entry = scan.find(&item.id);
        let name = match &item.source {
            AppSource::Tilo { .. } => Some(item.id.clone()),
            _ => entry.and_then(|entry| entry.icon.clone()),
        };
        item.icon = name
            .and_then(|name| desktop_entry::icon_theme::lookup(&name, ICON_PX, 1))
            .map(|file| file.path);
        measures.push(match &item.source {
            AppSource::Tilo { .. } => payloads.get(&item.id).cloned(),
            AppSource::Flatpak { app_id, .. } => {
                entry.and_then(|entry| flatpak_files(&entry.path, app_id))
            }
            AppSource::Downloaded { program, .. } => Some(program.clone()),
            AppSource::PersonEntry { .. } | AppSource::System => None,
        });
    }
    (generation, items, measures)
}

/// The desktop entries the app list holds: the host apps search lists,
/// each with the entry it came from. The scan has already left out what no
/// menu shows (`NoDisplay`, `Hidden`, another desktop's, no `Exec`), so
/// search, the launcher and this list lose the same entries.
#[must_use]
pub fn host_entries(scan: &desktop_entry::Scan) -> Vec<HostEntry> {
    crate::shell::host_apps::deduplicate(
        scan.entries
            .iter()
            .map(crate::shell::host_apps::HostApp::from_entry)
            .collect(),
    )
    .iter()
    .filter_map(|app| scan.find(&app.desktop_id))
    .map(|entry| HostEntry {
        desktop_id: entry.id.clone(),
        name: entry.name.clone(),
        path: entry.path.clone(),
        flatpak: match &entry.source {
            desktop_entry::EntrySource::Flatpak { app_id } => Some(app_id.clone()),
            desktop_entry::EntrySource::Host => None,
        },
        downloaded: None,
    })
    .collect()
}

/// A Flatpak's installed files, from the entry its installation exported:
/// `<installation>/exports/share/applications/<id>.desktop` beside
/// `<installation>/app/<id>/current/active/files`.
#[must_use]
pub fn flatpak_files(entry: &Path, app_id: &str) -> Option<PathBuf> {
    let installation = entry
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "exports"))?
        .parent()?;
    Some(
        installation
            .join("app")
            .join(app_id)
            .join("current/active/files"),
    )
}

/// The bytes a file or a tree takes; `None` when there is nothing there.
fn size_on_disk(path: &Path) -> Option<u64> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    let size = if metadata.is_file() {
        metadata.len()
    } else {
        package_store::tree_size(path)
    };
    (size > 0).then_some(size)
}

/// Open the listed app `id` in workspace `room`, as search opens it.
///
/// # Errors
///
/// When the app is not in the list, or it could not be started.
pub fn open(id: &str, room: crate::shell::state::RoomId) -> Result<(), String> {
    let item = find(id).ok_or_else(|| t("app-list-gone-body"))?;
    match item.source {
        AppSource::Tilo { .. } => crate::shell::launcher::launch(&item.id, room)
            .map(drop)
            .map_err(|error| error.user_message()),
        _ => crate::shell::host_apps::launch(&item.id, Some(room.to_string()))
            .map(drop)
            .map_err(|error| error.user_message()),
    }
}

// ---------------------------------------------------------------------
// Asking the person.
// ---------------------------------------------------------------------

/// Apps the person asked to uninstall (or take off their apps), waiting for
/// the sheet ([`take_asked`]).
static ASKED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Ask the person whether to uninstall app `id`, in the shell's sheet. The
/// answer runs [`uninstall`]; nothing runs until it is given.
pub fn ask(id: &str) {
    let mut asked = ASKED.lock_recover();
    if !asked.iter().any(|queued| queued == id) {
        asked.push(id.to_owned());
    }
    drop(asked);
    // The list may not have it yet; reading it now is what lets the sheet
    // open.
    let _ = listed();
    wake();
}

/// The next app the sheet should ask about, once the list knows it. An app
/// the list does not have once it has settled is dropped with a notice.
pub(crate) fn take_asked() -> Option<Result<AppListItem, Notification>> {
    let id = ASKED.lock_recover().first()?.clone();
    let found = find_in(&listed(), &id);
    if found.is_none() && (settling() || CACHE.lock_recover().built_for.is_none()) {
        return None;
    }
    {
        let mut asked = ASKED.lock_recover();
        if asked.first() == Some(&id) {
            asked.remove(0);
        }
    }
    Some(found.ok_or_else(|| {
        Notification::new(t("app-list-gone-title"), t("app-list-gone-body"))
            .with_severity(NotificationSeverity::Warning)
            .with_app_name(t("app-list-app-name"))
    }))
}

// ---------------------------------------------------------------------
// Doing it.
// ---------------------------------------------------------------------

static NOTICES: Mutex<Vec<Notification>> = Mutex::new(Vec::new());

/// Notices from finished uninstalls since the last call.
pub fn drain_notices() -> Vec<Notification> {
    std::mem::take(&mut *NOTICES.lock_recover())
}

/// Uninstall `item` on a worker, as the person answered: `remove_data`
/// decides a Flatpak's data and means nothing for any other kind. An app
/// that can only be taken off the person's apps is taken off them. The
/// outcome becomes a notice ([`drain_notices`]).
///
/// # Errors
///
/// When the worker cannot start, or the app cannot be removed at all.
pub fn uninstall(item: AppListItem, remove_data: bool) -> Result<(), String> {
    if item.removal() == AppRemoval::None {
        return Err(t_args(
            "app-list-part-of-system",
            &[("name", item.name.as_str().into())],
        ));
    }
    let places = Places::of_session().ok_or_else(|| t("app-list-no-home"))?;
    std::thread::Builder::new()
        .name("tilo-uninstall".into())
        .spawn(move || {
            let notice = match run(&item, remove_data, &places) {
                Ok(outcome) => outcome.notice(&item.name),
                Err(detail) => Notification::new(
                    t_args(
                        if item.removal() == AppRemoval::RemoveFromApps {
                            "app-list-remove-failed"
                        } else {
                            "app-list-uninstall-failed"
                        },
                        &[("name", item.name.as_str().into())],
                    ),
                    detail,
                )
                .with_severity(NotificationSeverity::Error),
            };
            NOTICES
                .lock_recover()
                .push(notice.with_app_name(t("app-list-app-name")));
            crate::shell::launcher::invalidate_app_cache();
            wake();
        })
        .map(drop)
        .map_err(|error| error.to_string())
}

/// What an uninstall did, for the notice that says so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Uninstalled,
    /// A Flatpak's data went to the Trash with it.
    UninstalledDataInTrash,
    /// A downloaded program's file went to the Trash.
    UninstalledFileInTrash,
    RemovedFromApps,
}

impl Outcome {
    fn notice(&self, name: &str) -> Notification {
        let args = [("name", name.into())];
        let (title, body) = match self {
            Self::Uninstalled => ("app-list-uninstalled", None),
            Self::UninstalledDataInTrash => {
                ("app-list-uninstalled", Some("app-list-uninstalled-data"))
            }
            Self::UninstalledFileInTrash => {
                ("app-list-uninstalled", Some("app-list-uninstalled-file"))
            }
            Self::RemovedFromApps => ("app-list-removed", None),
        };
        Notification::new(t_args(title, &args), body.map(t).unwrap_or_default())
    }
}

/// One thing an uninstall does, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Run the `app` tool with these arguments (a Tilo app's removal).
    App(Vec<String>),
    /// Run `flatpak` with these arguments. A step that is not `required`
    /// may fail without failing the uninstall.
    Flatpak { args: Vec<String>, required: bool },
    /// Move this file or folder to the Trash, when it is there. Never a
    /// symlink's target: a link moves as the link.
    Trash(PathBuf),
    /// Write a `Hidden` entry here, which takes the app off the person's
    /// apps.
    Hide { at: PathBuf, name: String },
    /// Remove the entry Tilo wrote for a downloaded program. Only ever a
    /// file in the person's applications directory that Tilo wrote.
    ForgetEntry(PathBuf),
}

/// What uninstalling `item` does, step by step, and what to say when it is
/// done: pure, so what each kind touches is assertable. `home` is the
/// person's home folder, where a Flatpak keeps its data.
///
/// # Errors
///
/// When `item` cannot be uninstalled at all, or names something that is not
/// safe to act on.
pub fn plan(
    item: &AppListItem,
    remove_data: bool,
    places: &Places,
    home: &Path,
) -> Result<(Vec<Step>, Outcome), String> {
    let to_args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    match &item.source {
        AppSource::Tilo { added: true } => Ok((
            vec![Step::App(to_args(&["remove", "--", &item.id]))],
            Outcome::Uninstalled,
        )),
        AppSource::Flatpak { app_id, user: true } => {
            let commands = app_catalogue::flathub::uninstall_commands(app_id)
                .map_err(|_| t("app-list-invalid-flatpak"))?;
            let mut steps: Vec<Step> = commands
                .into_iter()
                .enumerate()
                .map(|(at, args)| Step::Flatpak {
                    args,
                    required: at == 0,
                })
                .collect();
            if !remove_data {
                return Ok((steps, Outcome::Uninstalled));
            }
            steps.push(Step::Trash(flatpak_data_dir(home, app_id)));
            Ok((steps, Outcome::UninstalledDataInTrash))
        }
        AppSource::Downloaded { program, entry } => {
            if !entry.starts_with(places.applications()) {
                return Err(t("app-list-invalid-entry"));
            }
            Ok((
                vec![
                    Step::Trash(program.clone()),
                    Step::ForgetEntry(entry.clone()),
                ],
                Outcome::UninstalledFileInTrash,
            ))
        }
        AppSource::PersonEntry { path } => {
            if !path.starts_with(places.applications()) {
                return Err(t("app-list-invalid-entry"));
            }
            // The person's own entry file: it goes to the Trash, from where
            // it can be put back.
            Ok((vec![Step::Trash(path.clone())], Outcome::RemovedFromApps))
        }
        AppSource::Flatpak { user: false, .. } | AppSource::System => {
            if item.id.is_empty() || item.id.contains(['/', '\\']) || item.id.starts_with('.') {
                return Err(t("app-list-invalid-entry"));
            }
            // A `Hidden` entry of the same id in the person's own
            // applications directory keeps every copy of it from showing:
            // the desktop entry specification's way, which every menu honours.
            Ok((
                vec![Step::Hide {
                    at: places.applications().join(format!("{}.desktop", item.id)),
                    name: item.name.clone(),
                }],
                Outcome::RemovedFromApps,
            ))
        }
        AppSource::Tilo { added: false } => Err(t_args(
            "app-list-part-of-system",
            &[("name", item.name.as_str().into())],
        )),
    }
}

/// Uninstall `item`. Blocks: a worker's only.
///
/// # Errors
///
/// What went wrong, in words for the person.
pub fn run(item: &AppListItem, remove_data: bool, places: &Places) -> Result<Outcome, String> {
    let home = tilo_paths::home().ok_or_else(|| t("app-list-no-home"))?;
    let (steps, outcome) = plan(item, remove_data, places, &home)?;
    let mut trashed_data = false;
    for step in &steps {
        let trashed = execute(step)?;
        trashed_data |= trashed && matches!(item.source, AppSource::Flatpak { .. });
    }
    // A Flatpak that kept no data has nothing in the Trash to mention.
    Ok(match outcome {
        Outcome::UninstalledDataInTrash if !trashed_data => Outcome::Uninstalled,
        outcome => outcome,
    })
}

/// Carry out one step. Returns whether something went to the Trash.
///
/// # Errors
///
/// What went wrong, in words for the person.
pub fn execute(step: &Step) -> Result<bool, String> {
    match step {
        Step::App(args) => {
            let bin = crate::shell::catalogue::resolve_catalogue_bin()
                .ok_or_else(|| t("app-list-no-app-tool"))?;
            let mut command = std::process::Command::new(bin);
            command.args(args);
            run_command(command).map(|()| false)
        }
        Step::Flatpak { args, required } => {
            let mut command = std::process::Command::new("flatpak");
            command.args(args);
            match run_command(command) {
                Ok(()) => Ok(false),
                Err(error) if !required => {
                    tracing::warn!(%error, "an optional Flatpak step failed");
                    Ok(false)
                }
                Err(error) => Err(error),
            }
        }
        Step::Trash(path) => {
            if std::fs::symlink_metadata(path).is_err() {
                return Ok(false);
            }
            host_fs::trash::trash(path).map(|_| true)
        }
        Step::Hide { at, name } => {
            if let Some(parent) = at.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            std::fs::write(at, hidden_entry(name))
                .map(|()| false)
                .map_err(|error| error.to_string())
        }
        Step::ForgetEntry(entry) => match std::fs::remove_file(entry) {
            Ok(()) => Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.to_string()),
        },
    }
}

/// Where Flatpak keeps `app_id`'s data in `home`.
#[must_use]
pub fn flatpak_data_dir(home: &Path, app_id: &str) -> PathBuf {
    home.join(".var").join("app").join(app_id)
}

/// The entry that hides an app from the person's apps.
#[must_use]
pub fn hidden_entry(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    format!("[Desktop Entry]\nType=Application\nName={name}\nHidden=true\n")
}

fn run_command(command: std::process::Command) -> Result<(), String> {
    let output =
        bounded_command::run_prepared(command, None, COMMAND_OUTPUT_LIMIT, COMMAND_TIMEOUT)
            .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let detail: String = String::from_utf8_lossy(&output.stderr)
        .chars()
        .filter(|c| !c.is_control() || *c == ' ')
        .take(512)
        .collect();
    Err(detail.trim().to_owned())
}

#[cfg(test)]
#[path = "app_list_tests.rs"]
mod tests;
