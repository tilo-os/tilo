//! Device-local EPUB capability registry.
//!
//! A source path is a local portal grant, not collaborative content. Keep it
//! in qOS' private settings store so reopening a book or watching a folder
//! survives restart without putting a path into the Automerge document.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value};

const SECTION: &str = "ebook_local_library";
const BOOKS: &str = "books";
const WATCHED: &str = "watched_folders";
const MAX_ENTRIES: usize = 200;
const MAX_PATH_BYTES: usize = 4096;

#[derive(Clone, Debug, Default)]
pub(crate) struct LocalLibrary {
    pub(crate) books: HashMap<String, PathBuf>,
    pub(crate) watched_folders: Vec<PathBuf>,
}

impl LocalLibrary {
    pub(crate) fn load() -> Self {
        Self::from_section(&settings_store::load_section(SECTION))
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
        if let Err(error) = settings_store::update_section(SECTION, self.section()) {
            tracing::warn!(%error, "ebook: couldn't persist local library capabilities");
        }
    }

    fn from_section(section: &Map<String, Value>) -> Self {
        let mut out = Self::default();
        let Some(Value::Object(books)) = section.get(BOOKS) else {
            return out;
        };
        for (id, value) in books.iter().take(MAX_ENTRIES) {
            let Some(path) = value.as_str().map(PathBuf::from) else {
                continue;
            };
            if valid_book_id(id) && valid_path(&path) {
                out.books.insert(id.clone(), path);
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
    fn from_path(path: &Path) -> Self {
        let document = settings_store::load_at(path);
        let section = document
            .get(SECTION)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        Self::from_section(&section)
    }

    #[cfg(test)]
    fn save_at(&self, path: &Path) -> std::io::Result<()> {
        settings_store::update_section_at(path, SECTION, self.section())
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

    #[test]
    fn paths_round_trip_only_in_private_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut registry = LocalLibrary::default();
        registry.remember_book(
            "book:0123456789abcdef0123456789abcdef",
            Path::new("/books/a.epub"),
        );
        registry.toggle_watch(Path::new("/books"));
        assert_eq!(registry.books.len(), 1);
        registry.save_at(&path).unwrap();

        assert!(settings_store::load_at(&path).contains_key(SECTION));

        let restored = LocalLibrary::from_path(&path);
        assert_eq!(
            restored.books.get("book:0123456789abcdef0123456789abcdef"),
            Some(&PathBuf::from("/books/a.epub"))
        );
        assert_eq!(restored.watched_folders, vec![PathBuf::from("/books")]);
    }

    #[test]
    fn hostile_paths_and_ids_fail_closed() {
        let section = serde_json::json!({
            BOOKS: {
                "book:not-an-id": "/books/nope.epub",
                "book:0123456789abcdef0123456789abcdef": "relative.epub"
            },
            WATCHED: ["../private", "/safe", "/safe"]
        });
        let registry = LocalLibrary::from_section(section.as_object().unwrap());
        assert!(registry.books.is_empty());
        assert_eq!(registry.watched_folders, vec![PathBuf::from("/safe")]);
    }
}
