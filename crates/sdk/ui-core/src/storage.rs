//! Where an app keeps what is not a document: one seam for app files,
//! preferences and secrets, the same on Linux, macOS and the web.
//!
//! | What | Seam | Backed by |
//! |---|---|---|
//! | Documents (collaborative, synced) | resources and records; [`crate::persist`] for a single-document app | Automerge records |
//! | App files (caches, imports, databases) | [`AppFiles`], [`HostContext::files`] | `tilo_files::Dir` in `tilo_paths::AppPaths`: the per-user directories natively, the origin-private file system in a page |
//! | Preferences | [`Settings`], [`HostContext::settings`] | one section of the desktop settings document (`settings-store`), or the host's settings source |
//! | Secrets | [`secrets`], [`HostContext::secrets`] | `tilo-secrets`: the Secret Service, the Keychain, the page's wrapped store |
//!
//! Code with a [`HostContext`] uses its methods, which are scoped to the app
//! the host prepared. Code without one (a model loading its preferences
//! before the first frame) names the app or section itself. The older
//! entry points (`settings_store`, `xdg-paths`, `tilo_files::native::atomic`, `app-state`
//! from an app) are internals of these; apps other than Settings, which owns
//! the settings document, have no use for them.
//!
//! [`HostContext`]: crate::application::HostContext
//! [`HostContext::files`]: crate::application::HostContext::files
//! [`HostContext::settings`]: crate::application::HostContext::settings
//! [`HostContext::secrets`]: crate::application::HostContext::secrets

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

pub use tilo_files::{Dir, FsError};
pub use tilo_paths::{AppPaths, Location};

/// One app's private file areas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppFiles {
    paths: AppPaths,
}

impl AppFiles {
    /// The areas of the app with this id (its reverse-DNS id).
    #[must_use]
    pub fn for_app(app_id: &str) -> Self {
        Self {
            paths: AppPaths::for_app(app_id),
        }
    }

    /// Areas below `paths` instead (tests, hosts that sandbox each app).
    #[must_use]
    pub fn at(paths: AppPaths) -> Self {
        Self { paths }
    }

    /// Where each area is, for code that has to show or hand one on.
    #[must_use]
    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// Durable files the app owns: imports, databases, downloads it manages.
    ///
    /// # Errors
    ///
    /// When the area cannot be opened or created.
    pub fn data(&self) -> Result<Dir, FsError> {
        Dir::open(self.paths.data())
    }

    /// Configuration files. Preferences belong in [`Settings`].
    ///
    /// # Errors
    ///
    /// When the area cannot be opened or created.
    pub fn config(&self) -> Result<Dir, FsError> {
        Dir::open(self.paths.config())
    }

    /// Files the app can rebuild, which the system may delete.
    ///
    /// # Errors
    ///
    /// When the area cannot be opened or created.
    pub fn cache(&self) -> Result<Dir, FsError> {
        Dir::open(self.paths.cache())
    }

    /// State worth keeping across restarts but not worth backing up.
    ///
    /// # Errors
    ///
    /// When the area cannot be opened or created.
    pub fn state(&self) -> Result<Dir, FsError> {
        Dir::open(self.paths.state())
    }
}

/// One section of the desktop settings document: an app's preferences, or a
/// shared section another owner writes (`locale`, `appearance`) that the app
/// reads. Reads never fail; a missing, unreadable or malformed document reads
/// as empty. Writes merge the given keys into the section and keep every
/// other section and key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    section: String,
    file: Option<PathBuf>,
}

impl Settings {
    /// The section named `name`.
    #[must_use]
    pub fn section(name: &str) -> Self {
        Self {
            section: name.to_owned(),
            file: None,
        }
    }

    /// The app's own section, named by its id.
    #[must_use]
    pub fn for_app(app_id: &str) -> Self {
        Self::section(app_id)
    }

    /// The same section in the document at `path` instead of the user's
    /// (tests, tools that edit another profile).
    #[must_use]
    pub fn in_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.file = Some(path.into());
        self
    }

    /// The section's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.section
    }

    /// Where the document is, when it is a file.
    #[must_use]
    pub fn path(&self) -> Option<PathBuf> {
        self.file.clone().or_else(settings_store::settings_path)
    }

    /// The whole desktop settings document.
    #[must_use]
    pub fn document(&self) -> Map<String, Value> {
        match &self.file {
            Some(path) => settings_store::load_at(path),
            None => settings_store::load(),
        }
    }

    /// The section's keys.
    #[must_use]
    pub fn load(&self) -> Map<String, Value> {
        match &self.file {
            Some(path) => settings_store::load_section_at(path, &self.section),
            None => settings_store::load_section(&self.section),
        }
    }

    /// One key, when present and of this type.
    #[must_use]
    pub fn bool(&self, key: &str) -> Option<bool> {
        self.load().get(key).and_then(Value::as_bool)
    }

    /// One string key, when present.
    #[must_use]
    pub fn string(&self, key: &str) -> Option<String> {
        self.load()
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// Merge `values` into the section.
    ///
    /// # Errors
    ///
    /// When there is no document to write or the write fails.
    pub fn update(&self, values: Map<String, Value>) -> std::io::Result<()> {
        match &self.file {
            Some(path) => settings_store::update_section_at(path, &self.section, values),
            None => settings_store::update_section(&self.section, values),
        }
    }
}

/// Parse settings-document text the way every reader does: anything
/// malformed reads as an empty document.
#[must_use]
pub fn parse_settings(text: &str) -> Map<String, Value> {
    settings_store::parse(text)
}

/// The document at `path`, for code that compares two profiles.
#[must_use]
pub fn settings_at(path: &Path) -> Map<String, Value> {
    settings_store::load_at(path)
}

/// This app's secrets in the store the current host provides.
#[must_use]
pub fn secrets(app_id: &str) -> crate::secrets::SecretStore {
    crate::host::current().secrets.store(app_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_section_update_keeps_other_sections_and_keys() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        let appearance = Settings::section("appearance").in_file(&file);
        appearance
            .update(
                [("theme".to_owned(), Value::from("dark"))]
                    .into_iter()
                    .collect(),
            )
            .unwrap();
        let mine = Settings::for_app("so.tilo.example").in_file(&file);
        mine.update(
            [("loop".to_owned(), Value::from(true))]
                .into_iter()
                .collect(),
        )
        .unwrap();
        mine.update([("speed".to_owned(), Value::from(2))].into_iter().collect())
            .unwrap();
        assert_eq!(appearance.string("theme").as_deref(), Some("dark"));
        assert_eq!(mine.bool("loop"), Some(true));
        assert_eq!(mine.load().get("speed"), Some(&Value::from(2)));
        assert_eq!(mine.path(), Some(file.clone()));
        assert!(settings_at(&file).contains_key("appearance"));
    }

    #[test]
    fn a_malformed_document_reads_as_empty() {
        assert!(parse_settings("{not json").is_empty());
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(&file, "[1, 2]").unwrap();
        assert!(Settings::section("locale").in_file(&file).load().is_empty());
    }

    #[test]
    fn app_files_live_below_the_apps_paths() {
        let dir = tempfile::tempdir().unwrap();
        let files = AppFiles::at(AppPaths::under(dir.path(), "so.tilo.example"));
        let data = files.data().unwrap();
        tilo_files::blocking::write_atomic(&data, &tilo_files::VPath::new("a.txt").unwrap(), b"x")
            .unwrap();
        assert!(dir.path().join("so.tilo.example/data/a.txt").is_file());
        assert_eq!(
            AppFiles::for_app("so.tilo.example").paths().id(),
            "so.tilo.example"
        );
    }
}
