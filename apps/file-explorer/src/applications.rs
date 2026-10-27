//! The Applications place: every app in the person's app list, as items.
//!
//! The list is the shell's applications model, not one of Files' own: Files
//! asks for it (`ToolIntent::Applications`), opens an app through the shell
//! (`ToolIntent::OpenApplication`) and hands Uninstall to the shell's one
//! Uninstall action (`ToolIntent::UninstallApplication`), which asks the
//! person in its own sheet. Files never removes an app itself.
//!
//! `~/Applications`, where an `AppImage` is moved to, is part of the same
//! place: going there opens this place, and a file in it that is not one of
//! the listed apps is shown here too, as the file it is.

use std::path::{Path, PathBuf};

use runtime_portal::tools::{AppEntry, AppKind, AppRemoval};

use crate::model::Entry;

/// The pseudo-location the Applications place navigates to. Like Recent's,
/// it is relative and scheme-like, so it never names a real directory.
pub const APPLICATIONS_LOCATION: &str = "applications://";

/// The type an app item carries, so views and rows tell it from a file.
pub const APP_MIME: &str = "application/x-desktop";

/// What an app item shows beyond its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppFacts {
    /// Its icon file.
    pub icon: Option<PathBuf>,
    pub kind: AppKind,
    /// Bytes on disk, where the applications model knows them.
    pub size: Option<u64>,
}

/// The kind column's words for `kind`.
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

/// The folder in the person's files that belongs to this place.
#[must_use]
pub fn folder(home: &Path) -> PathBuf {
    home.join("Applications")
}

/// The item path of the app with this id.
#[must_use]
pub fn app_path(id: &str) -> PathBuf {
    PathBuf::from(format!("{APPLICATIONS_LOCATION}{id}"))
}

/// The app id an item path names, when it names one.
#[must_use]
pub fn app_id(path: &Path) -> Option<&str> {
    path.to_str()?
        .strip_prefix(APPLICATIONS_LOCATION)
        .filter(|id| !id.is_empty())
}

/// Whether `path` is an app item rather than a file.
#[must_use]
pub fn is_app(path: &Path) -> bool {
    app_id(path).is_some()
}

/// One app as an item of the listing.
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
        share: None,
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

/// The place's items: every app, then each file of `~/Applications` that is
/// not one of those apps' programs.
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

/// What "remove" means for the app at `path` among `apps`: Uninstall, Remove
/// from apps, or nothing.
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
