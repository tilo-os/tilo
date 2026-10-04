//! Device-local EPUB capability registry.
//!
//! A source path is a local portal grant, not collaborative content. Keep it
//! in the app's device-local settings so reopening a book or watching a
//! folder survives restart without putting a path into the Automerge
//! document.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value};

#[cfg(test)]
const APP_ID: &str = "so.tilo.ebook";
const BOOKS: &str = "books";
const WATCHED: &str = "watched_folders";
const MAX_ENTRIES: usize = 200;
const MAX_PATH_BYTES: usize = 4096;

/// The library as stored: the app's device-local settings, kept as the raw
/// keys [`LocalLibrary::from_section`] validates one by one.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub(crate) struct Stored(Map<String, Value>);

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
        let stored = Self::from_section(&store.get().0);
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
        if let Err(error) = store.set(&Stored(self.section())) {
            tracing::warn!(%error, "ebook: couldn't persist local library capabilities");
        }
    }

    fn from_section(section: &Map<String, Value>) -> Self {
        let mut out = Self::default();
        if let Some(Value::Object(books)) = section.get(BOOKS) {
            for (id, value) in books.iter().take(MAX_ENTRIES) {
                let Some(path) = value.as_str().map(PathBuf::from) else {
                    continue;
                };
                if valid_book_id(id) && valid_path(&path) {
                    out.books.insert(id.clone(), path);
                }
            }
        }
        if let Some(Value::Array(watched)) = section.get(WATCHED) {
            for value in watched.iter().take(MAX_ENTRIES) {
                let Some(path) = value.as_str().map(PathBuf::from) else {
                    continue;
                };
                if valid_path(&path) && !out.watched_folders.contains(&path) {
                    out.watched_folders.push(path);
                }
            }
        }
        out
    }

    fn section(&self) -> Map<String, Value> {
        let books = self
            .books
            .iter()
            .filter(|(id, path)| valid_book_id(id) && valid_path(path))
            .take(MAX_ENTRIES)
            .map(|(id, path)| {
                (
                    id.clone(),
                    Value::String(path.to_string_lossy().into_owned()),
                )
            })
            .collect();
        let watched_folders = self
            .watched_folders
            .iter()
            .filter(|path| valid_path(path))
            .take(MAX_ENTRIES)
            .map(|path| Value::String(path.to_string_lossy().into_owned()))
            .collect();
        [
            (BOOKS.to_string(), Value::Object(books)),
            (WATCHED.to_string(), Value::Array(watched_folders)),
        ]
        .into_iter()
        .collect()
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
                    .get(BOOKS)
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
            let section = serde_json::json!({
                BOOKS: {
                    "book:not-an-id": "/books/nope.epub",
                    "book:0123456789abcdef0123456789abcdef": "relative.epub"
                },
                WATCHED: [PathBuf::from("../private"), &safe, &safe]
            });
            let registry = LocalLibrary::from_section(section.as_object().unwrap());
            assert!(registry.books.is_empty());
            assert_eq!(registry.watched_folders, vec![safe]);
        }
    }
}
