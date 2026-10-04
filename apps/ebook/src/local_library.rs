//! Device-local EPUB capability registry.
//!
//! A source path is a local portal grant, not collaborative content. Keep it
//! in the app's device-local settings so reopening a book or watching a
//! folder survives restart without putting a path into the Automerge
//! document.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};

#[cfg(test)]
const APP_ID: &str = "so.tilo.ebook";
const MAX_ENTRIES: usize = 200;
const MAX_PATH_BYTES: usize = 4096;

/// The library as stored in the app's device-local settings: where each
/// book's file is on this device, and the folders it watches. Read back
/// through [`LocalLibrary::from_stored`], which keeps only what is valid.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Stored {
    #[serde(default)]
    books: BTreeMap<String, PathBuf>,
    #[serde(default)]
    watched_folders: Vec<PathBuf>,
}

impl ui::storage::SettingsSchema for Stored {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::DeviceLocal;
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LocalLibrary {
    /// Where the grants are kept: the app's device-local settings in the
    /// host's settings document. `None` keeps them in memory only.
    store: Option<ui::storage::Settings<Stored>>,
    pub(crate) books: HashMap<String, PathBuf>,
    pub(crate) watched_folders: Vec<PathBuf>,
}

impl LocalLibrary {
    /// The grants kept in `store`, which [`Self::save`] writes back to.
    pub(crate) fn load(store: ui::storage::Settings<Stored>) -> Self {
        let stored = Self::from_stored(&store.get());
        Self {
            store: Some(store),
            ..stored
        }
    }

    pub(crate) fn remember_book(&mut self, id: &str, path: &Path) {
        if valid_book_id(id)
            && valid_path(path)
            && (self.books.len() < MAX_ENTRIES || self.books.contains_key(id))
        {
            self.books.insert(id.to_string(), path.to_path_buf());
        }
    }

    pub(crate) fn forget_book(&mut self, id: &str) {
        self.books.remove(id);
    }

    pub(crate) fn toggle_watch(&mut self, path: &Path) {
        if !valid_path(path) {
            return;
        }
        if let Some(position) = self.watched_folders.iter().position(|item| item == path) {
            self.watched_folders.remove(position);
        } else if self.watched_folders.len() < MAX_ENTRIES {
            self.watched_folders.push(path.to_path_buf());
        }
    }

    pub(crate) fn save(&self) {
        let Some(store) = &self.store else {
            return;
        };
        if let Err(error) = store.set(&self.stored()) {
            tracing::warn!(%error, "ebook: couldn't persist local library capabilities");
        }
    }

    fn from_stored(stored: &Stored) -> Self {
        let mut out = Self::default();
        for (id, path) in stored.books.iter().take(MAX_ENTRIES) {
            if valid_book_id(id) && valid_path(path) {
                out.books.insert(id.clone(), path.clone());
            }
        }
        for path in stored.watched_folders.iter().take(MAX_ENTRIES) {
            if valid_path(path) && !out.watched_folders.contains(path) {
                out.watched_folders.push(path.clone());
            }
        }
        out
    }

    fn stored(&self) -> Stored {
        Stored {
            books: self
                .books
                .iter()
                .filter(|(id, path)| valid_book_id(id) && valid_path(path))
                .take(MAX_ENTRIES)
                .map(|(id, path)| (id.clone(), path.clone()))
                .collect(),
            watched_folders: self
                .watched_folders
                .iter()
                .filter(|path| valid_path(path))
                .take(MAX_ENTRIES)
                .cloned()
                .collect(),
        }
    }

    #[cfg(test)]
    fn in_file(path: &Path) -> ui::storage::Settings<Stored> {
        ui::storage::Settings::for_app(APP_ID).in_file(path)
    }
}

fn valid_book_id(id: &str) -> bool {
    id.strip_prefix("book:").is_some_and(|digest| {
        digest.len() == 32 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn valid_path(path: &Path) -> bool {
    let rendered = path.to_string_lossy();
    path.is_absolute()
        && !rendered.is_empty()
        && rendered.len() <= MAX_PATH_BYTES
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paths round-trip only in private settings, and hostile paths and ids fail closed.
    #[test]
    fn local_library_settings_stay_private_and_fail_closed() {
        // --- phase: paths_round_trip_only_in_private_settings ---
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("settings.json");
            let books = directory.path().join("books");
            let book = books.join("a.epub");
            let mut registry = LocalLibrary::load(LocalLibrary::in_file(&path));
            registry.remember_book("book:0123456789abcdef0123456789abcdef", &book);
            registry.toggle_watch(&books);
            assert_eq!(registry.books.len(), 1);
            registry.save();

            assert!(
                ui::storage::settings_at(&path)["app_device_local"][APP_ID]
                    .get("books")
                    .is_some(),
                "the paths are the device's own"
            );

            let restored = LocalLibrary::load(LocalLibrary::in_file(&path));
            assert_eq!(
                restored.books.get("book:0123456789abcdef0123456789abcdef"),
                Some(&book)
            );
            assert_eq!(restored.watched_folders, vec![books]);
        }

        // --- phase: hostile_paths_and_ids_fail_closed ---
        {
            let directory = tempfile::tempdir().unwrap();
            let safe = directory.path().join("safe");
            let stored: Stored = serde_json::from_value(serde_json::json!({
                "books": {
                    "book:not-an-id": "/books/nope.epub",
                    "book:0123456789abcdef0123456789abcdef": "relative.epub"
                },
                "watched_folders": [PathBuf::from("../private"), &safe, &safe]
            }))
            .unwrap();
            let registry = LocalLibrary::from_stored(&stored);
            assert!(registry.books.is_empty());
            assert_eq!(registry.watched_folders, vec![safe]);
        }
    }
}
