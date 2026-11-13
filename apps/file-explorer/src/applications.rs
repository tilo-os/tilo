//! The Applications place: every app the person has, and the files of `~/Applications`.

use std::path::{Path, PathBuf};

use runtime_portal::tools::{AppEntry, AppKind, AppRemoval};

use crate::model::Entry;

pub const APPLICATIONS_LOCATION: &str = "applications://";

pub const APP_MIME: &str = "application/x-desktop";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppFacts {
    pub icon: Option<PathBuf>,
    pub kind: AppKind,
    pub size: Option<u64>,
}

#[must_use]
pub fn kind_label(kind: AppKind) -> String {
    crate::i18n::t(match kind {
        AppKind::Tilo => "files-app-kind-tilo",
        AppKind::Flatpak => "files-app-kind-flatpak",
        AppKind::AppImage => "files-app-kind-appimage",
        AppKind::Program => "files-app-kind-program",
        AppKind::System => "files-app-kind-system",
    })
}

#[must_use]
pub fn location() -> PathBuf {
    PathBuf::from(APPLICATIONS_LOCATION)
}

#[must_use]
pub fn is_location(path: &Path) -> bool {
    path.as_os_str() == APPLICATIONS_LOCATION
}

#[must_use]
pub fn folder(home: &Path) -> PathBuf {
    home.join("Applications")
}

#[must_use]
pub fn app_path(id: &str) -> PathBuf {
    PathBuf::from(format!("{APPLICATIONS_LOCATION}{id}"))
}

#[must_use]
pub fn app_id(path: &Path) -> Option<&str> {
    path.to_str()?
        .strip_prefix(APPLICATIONS_LOCATION)
        .filter(|id| !id.is_empty())
}

#[must_use]
pub fn is_app(path: &Path) -> bool {
    app_id(path).is_some()
}

#[must_use]
pub fn app_entry(app: &AppEntry) -> Entry {
    Entry {
        name: app.name.clone(),
        path: app_path(&app.id),
        is_dir: false,
        size: app.size.unwrap_or(0),
        modified: None,
        mime: APP_MIME.to_owned(),
        sync_state: None,
        content_hash: None,
        record_id: None,
        availability: None,
        history: None,
        rich_metadata: None,
        observed: None,
        app: Some(AppFacts {
            icon: app.icon.as_ref().map(PathBuf::from),
            kind: app.kind,
            size: app.size,
        }),
    }
}

#[must_use]
pub fn listing(apps: &[AppEntry], folder_files: &[Entry]) -> Vec<Entry> {
    let programs: Vec<&Path> = apps
        .iter()
        .filter_map(|app| app.program.as_deref().map(Path::new))
        .collect();
    apps.iter()
        .map(app_entry)
        .chain(
            folder_files
                .iter()
                .filter(|file| !programs.contains(&file.path.as_path()))
                .cloned(),
        )
        .collect()
}

#[must_use]
pub fn removal_of(apps: &[AppEntry], path: &Path) -> AppRemoval {
    app_id(path)
        .and_then(|id| apps.iter().find(|app| app.id == id))
        .map_or(AppRemoval::None, |app| app.removal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, program: Option<&str>) -> AppEntry {
        AppEntry {
            id: id.into(),
            name: id.to_uppercase(),
            package: None,
            icon: None,
            program: program.map(str::to_owned),
            removal: AppRemoval::Uninstall,
            kind: AppKind::AppImage,
            size: None,
        }
    }

    fn file(path: &str) -> Entry {
        let mut entry = app_entry(&app("x", None));
        entry.path = PathBuf::from(path);
        entry.name = path.rsplit('/').next().unwrap_or_default().to_owned();
        entry.mime = "application/octet-stream".into();
        entry.app = None;
        entry
    }

    #[test]
    fn an_app_item_names_its_app_and_nothing_else_does() {
        assert_eq!(
            app_id(&app_path("org.gnome.Chess")),
            Some("org.gnome.Chess")
        );
        assert_eq!(app_id(&location()), None);
        assert_eq!(
            app_id(Path::new("/home/me/Applications/Foo.AppImage")),
            None
        );
        assert!(is_location(&location()));
    }

    #[test]
    fn a_moved_program_shows_once_as_its_app_and_other_files_as_files() {
        let apps = [app("foo", Some("/home/me/Applications/Foo.AppImage"))];
        let files = [
            file("/home/me/Applications/Foo.AppImage"),
            file("/home/me/Applications/notes.txt"),
        ];
        let items = listing(&apps, &files);
        let paths: Vec<_> = items.iter().map(|item| item.path.clone()).collect();
        assert_eq!(
            paths,
            [
                app_path("foo"),
                PathBuf::from("/home/me/Applications/notes.txt")
            ]
        );
    }
}
