//! The Applications place in the window: the apps the shell lists, opened and
//! uninstalled through it.
#[cfg(test)]
#[path = "applications_tests.rs"]
mod tests;

#[allow(
    clippy::wildcard_imports,
    reason = "this file continues the parent module's widget impl; it and the tests under it use the parent's items as the parent does, under every cfg"
)]
use super::*;
use runtime_portal::tools::{AppEntry, AppKind, AppRemoval, ToolIntent};
use std::sync::Mutex;

const SETTLING_POLL: Duration = Duration::from_secs(1);

pub(super) type AppLister = fn() -> Result<Vec<AppEntry>, String>;
pub(super) type AppRequest = fn(ToolIntent, Option<[u8; 16]>) -> Result<(), String>;

struct Listed {
    apps: Vec<AppEntry>,
    folder_files: Vec<Entry>,
}

pub(super) struct Applications {
    pub(super) shown: Vec<AppEntry>,
    folder_files: Vec<Entry>,
    listed: Arc<Mutex<Option<Listed>>>,
    refused: Arc<Mutex<Option<String>>>,
    listing: bool,
    wanted: bool,
    next_poll: Option<Instant>,
    stale: bool,
    lister: Option<AppLister>,
    request: AppRequest,
}

impl Default for Applications {
    fn default() -> Self {
        Self {
            shown: Vec::new(),
            folder_files: Vec::new(),
            listed: Arc::default(),
            refused: Arc::default(),
            listing: false,
            wanted: false,
            next_poll: None,
            stale: false,
            // Unit tests never reach a shell; they inject what they need.
            lister: (!cfg!(test)).then_some(runtime_portal::tools::list_applications as AppLister),
            request: runtime_portal::tools::open_tool,
        }
    }
}

impl Applications {
    pub(super) fn next_wake(&self) -> Option<Instant> {
        if self.listed.lock().is_ok_and(|slot| slot.is_some()) {
            return Some(Instant::now());
        }
        if self.refused.lock().is_ok_and(|slot| slot.is_some()) {
            return self.stale.then(Instant::now);
        }
        if !self.wanted || self.lister.is_none() {
            return None;
        }
        if self.listing {
            return None;
        }
        if !self.stale && self.next_poll.is_none() {
            return None;
        }
        Some(self.next_poll.unwrap_or_else(Instant::now))
    }

    pub(super) fn event(&mut self, event: &ui::collaboration::CollabEvent) {
        if matches!(event, ui::collaboration::CollabEvent::ApplicationsChanged) {
            self.stale = true;
        }
    }
}

impl FilesState {
    pub(super) fn is_applications_view(&self) -> bool {
        crate::applications::is_location(&self.current_path)
    }

    pub(super) fn place_of(&mut self, path: PathBuf) -> PathBuf {
        let folder = self.home_path.as_deref().map(crate::applications::folder);
        let path = if folder.as_deref() == Some(path.as_path()) {
            crate::applications::location()
        } else {
            path
        };
        let wanted = crate::applications::is_location(&path);
        if wanted && !self.applications.wanted {
            self.applications.next_poll = None;
            self.applications.stale = true;
        }
        self.applications.wanted = wanted;
        path
    }

    pub(super) fn applications_entries(&self) -> Vec<Entry> {
        crate::applications::listing(&self.applications.shown, &self.applications.folder_files)
    }

    pub(super) fn app_removal(&self, path: &Path) -> AppRemoval {
        crate::applications::removal_of(&self.applications.shown, path)
    }

    pub(super) fn poll_applications(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if let Some(listed) = self
            .applications
            .listed
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
        {
            self.applications.listing = false;
            let settling = listed.apps.is_empty();
            self.applications.next_poll = settling.then(|| now + SETTLING_POLL);
            if listed.apps != self.applications.shown
                || listed.folder_files != self.applications.folder_files
            {
                self.applications.shown = listed.apps;
                self.applications.folder_files = listed.folder_files;
                if self.is_applications_view() {
                    changed |= self.refresh_metadata_preserving_view();
                }
            }
        }
        if let Some(refusal) = self
            .applications
            .refused
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
        {
            self.status = Some(t_args(
                "files-applications-failed",
                &[arg("error", refusal)],
            ));
            changed = true;
        }
        let Some(lister) = self.applications.lister else {
            return changed;
        };
        if !self.applications.wanted
            || self.applications.listing
            || self.applications.next_poll.is_some_and(|due| now < due)
        {
            return changed;
        }
        self.applications.stale = false;
        self.applications.next_poll = None;
        self.applications.listing = true;
        let folder = self.home_path.as_deref().map(crate::applications::folder);
        let slot = Arc::clone(&self.applications.listed);
        let spawned = std::thread::Builder::new()
            .name("files-applications".into())
            .spawn(move || {
                // No session to ask is no apps, not an error to show.
                let apps = lister().unwrap_or_default();
                let folder_files = folder
                    .and_then(|folder| crate::model::read_dir_sync(&folder).ok())
                    .unwrap_or_default();
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some(Listed { apps, folder_files });
                }
            });
        if spawned.is_err() {
            self.applications.listing = false;
        }
        changed
    }

    fn ask_shell(&self, intents: Vec<ToolIntent>) {
        let request = self.applications.request;
        let refused = Arc::clone(&self.applications.refused);
        let on_behalf_of = ui::invocation_scope::delivered_invocation().map(|id| id.0.into_bytes());
        let _ = std::thread::Builder::new()
            .name("files-applications-ask".into())
            .spawn(move || {
                for intent in intents {
                    if let Err(error) = request(intent, on_behalf_of)
                        && let Ok(mut slot) = refused.lock()
                    {
                        *slot = Some(error);
                    }
                }
            });
    }

    pub(super) fn open_app(&self, app: String) {
        self.ask_shell(vec![ToolIntent::OpenApplication { app }]);
    }

    pub(super) fn uninstall_apps(&self, paths: &[PathBuf]) {
        let intents: Vec<ToolIntent> = paths
            .iter()
            .filter(|path| self.app_removal(path) != AppRemoval::None)
            .filter_map(|path| crate::applications::app_id(path))
            .map(|app| ToolIntent::UninstallApplication {
                app: app.to_owned(),
            })
            .collect();
        if !intents.is_empty() {
            self.ask_shell(intents);
        }
    }

    fn selected_app_paths(&self) -> Vec<PathBuf> {
        self.selected_paths()
            .into_iter()
            .filter(|path| crate::applications::is_app(path))
            .collect()
    }

    pub(super) fn uninstall_selected_apps(&self) -> bool {
        let apps = self.selected_app_paths();
        self.uninstall_apps(&apps);
        true
    }

    pub(super) fn refuses_in_applications(&mut self, cmd: &FileCommand) -> bool {
        if matches!(
            cmd,
            FileCommand::NewFolder
                | FileCommand::NewFromTemplate(_)
                | FileCommand::Paste
                | FileCommand::OpenTerminal
                | FileCommand::ExtractHere
                | FileCommand::ExtractTo
                | FileCommand::CompressSelection(_)
        ) {
            self.status = Some(t("files-applications-not-a-folder"));
            return true;
        }
        let file_only = matches!(
            cmd,
            FileCommand::Copy
                | FileCommand::Cut
                | FileCommand::Rename
                | FileCommand::RenameCommit
                | FileCommand::OpenWithMenu
                | FileCommand::OpenWithOther
                | FileCommand::OpenNearbySend
                | FileCommand::OpenShortcutsMenu
        );
        if file_only && !self.selected_app_paths().is_empty() {
            self.status = Some(t("files-applications-not-a-file"));
            return true;
        }
        false
    }

    pub(super) fn applications_menu_spec(&self) -> Option<Vec<(String, Option<FileCommand>)>> {
        if !self.is_applications_view() {
            return None;
        }
        let apps = self.selected_app_paths();
        if apps.is_empty() {
            return self.selection.is_empty().then(Vec::new);
        }
        let mut spec = vec![
            (
                t("files-menu-open"),
                self.selection.primary().map(FileCommand::Activate),
            ),
            (String::new(), None),
        ];
        let removals: Vec<AppRemoval> = apps.iter().map(|path| self.app_removal(path)).collect();
        if removals.iter().any(|removal| {
            matches!(
                removal,
                AppRemoval::UninstallWithData | AppRemoval::Uninstall
            )
        }) {
            spec.push((t("files-menu-uninstall"), Some(FileCommand::Uninstall)));
        } else if removals.contains(&AppRemoval::RemoveFromApps) {
            spec.push((
                t("files-menu-remove-from-apps"),
                Some(FileCommand::Uninstall),
            ));
        }
        Some(tidy_menu(spec))
    }

    #[doc(hidden)]
    pub fn debug_show_applications(&mut self, apps: Vec<AppEntry>) {
        self.applications.lister = None;
        self.applications.shown = apps;
        self.applications.folder_files = Vec::new();
        if self.is_applications_view() {
            self.refresh_metadata_preserving_view();
        }
        self.structure_dirty = true;
    }

    #[cfg(test)]
    pub(super) fn set_app_request(&mut self, request: AppRequest) {
        self.applications.request = request;
    }

    #[cfg(test)]
    pub(super) fn set_app_lister(&mut self, lister: AppLister) {
        self.applications.lister = Some(lister);
    }

    #[cfg(test)]
    pub(super) fn take_apps_event(&mut self, event: &ui::collaboration::CollabEvent) {
        self.applications.event(event);
    }

    pub(super) fn debug_show_sample_applications(&mut self) {
        let art = self.current_path.join(".sample-app-icons");
        let mut apps = sample_apps();
        if std::fs::create_dir_all(&art).is_ok() {
            for (app, (fill, shape)) in apps.iter_mut().zip(SAMPLE_ART) {
                let path = art.join(format!("{}.svg", app.id));
                let svg = format!(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64"><rect x="4" y="4" width="56" height="56" rx="14" fill="{fill}"/>{shape}</svg>"#
                );
                if std::fs::write(&path, svg).is_ok() {
                    app.icon = Some(path.to_string_lossy().into_owned());
                }
            }
        }
        self.debug_show_applications(apps);
        self.navigate(crate::applications::location());
    }
}

const SAMPLE_ART: [(&str, &str); 4] = [
    (
        "#f5c542",
        r##"<rect x="18" y="16" width="28" height="32" rx="3" fill="#fff"/>"##,
    ),
    (
        "#3b7d4f",
        r##"<circle cx="32" cy="26" r="9" fill="#fff"/><rect x="20" y="38" width="24" height="8" rx="2" fill="#fff"/>"##,
    ),
    (
        "#7a4fd6",
        r##"<path d="M18 46 L32 16 L46 46 Z" fill="#fff"/>"##,
    ),
    (
        "#5a6270",
        r##"<rect x="16" y="20" width="32" height="24" rx="3" fill="none" stroke="#fff" stroke-width="4"/>"##,
    ),
];

pub(super) fn sample_apps() -> Vec<AppEntry> {
    let app =
        |id: &str, name: &str, removal: AppRemoval, kind: AppKind, size: Option<u64>| AppEntry {
            id: id.into(),
            name: name.into(),
            package: None,
            icon: None,
            program: (kind == AppKind::AppImage).then(|| {
                format!(
                    "/home/you/Applications/{}-5.2.AppImage",
                    name.to_lowercase()
                )
            }),
            removal,
            kind,
            size,
        };
    vec![
        app(
            "so.tilo.notes",
            "Notes",
            AppRemoval::Uninstall,
            AppKind::Tilo,
            Some(18_400_000),
        ),
        app(
            "org.gnome.Chess",
            "Chess",
            AppRemoval::UninstallWithData,
            AppKind::Flatpak,
            Some(6_200_000),
        ),
        app(
            "tilo-downloaded-krita",
            "Krita",
            AppRemoval::Uninstall,
            AppKind::AppImage,
            Some(312_000_000),
        ),
        app(
            "org.gnome.TextEditor",
            "Text Editor",
            AppRemoval::RemoveFromApps,
            AppKind::System,
            None,
        ),
    ]
}
