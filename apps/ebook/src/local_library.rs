//! Device-local EPUB capability registry.
//!
//! A source path is a local portal grant, not collaborative content. Keep it
//! in Tilo' private settings store so reopening a book or watching a folder
//! survives restart without putting a path into the Automerge document.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value};

const SECTION: &str = "ebook_local_library";
const BOOKS: &str = "books";
const WATCHED: &str = "watched_folders";
const SHORTCUTS: &str = "shortcuts";
const MAX_ENTRIES: usize = 200;
const MAX_PATH_BYTES: usize = 4096;

#[derive(Clone, Debug, Default)]
pub(crate) struct LocalLibrary {
    pub(crate) books: HashMap<String, PathBuf>,
    pub(crate) watched_folders: Vec<PathBuf>,
    /// Reader bindings are intentionally local: a collaborator's preferred
    /// physical keys must never change another reader's device.
    pub(crate) shortcuts: HashMap<String, String>,
}

impl LocalLibrary {
    pub(crate) fn load() -> Self {
        Self::from_section(&ui::storage::SettingsSection::named(SECTION).load())
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

    pub(crate) fn shortcut(&self, action: &str, default: &str) -> String {
        self.shortcuts
            .get(action)
            .filter(|key| valid_shortcut(key) && !reserved_reader_key(key))
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    /// Set one reader key. Duplicate bindings are refused so pressing a key
    /// always has one predictable meaning in the reader.
    pub(crate) fn set_shortcut(&mut self, action: &str, key: &str) -> bool {
        if !valid_shortcut(key)
            || reserved_reader_key(key)
            || self
                .shortcuts
                .iter()
                .any(|(other, bound)| other != action && bound.eq_ignore_ascii_case(key))
        {
            return false;
        }
        self.shortcuts
            .insert(action.to_string(), key.to_lowercase());
        true
    }

    pub(crate) fn save(&self) {
        if let Err(error) = ui::storage::SettingsSection::named(SECTION).update(self.section()) {
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
        if let Some(Value::Object(shortcuts)) = section.get(SHORTCUTS) {
            for (action, value) in shortcuts.iter().take(MAX_ENTRIES) {
                let Some(key) = value.as_str() else {
                    continue;
                };
                if valid_shortcut(key)
                    && !out
                        .shortcuts
                        .values()
                        .any(|bound| bound.eq_ignore_ascii_case(key))
                {
                    out.shortcuts.insert(action.clone(), key.to_lowercase());
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
        let shortcuts = self
            .shortcuts
            .iter()
            .filter(|(_, key)| valid_shortcut(key))
            .take(MAX_ENTRIES)
            .map(|(action, key)| (action.clone(), Value::String(key.clone())))
            .collect();
        [
            (BOOKS.to_string(), Value::Object(books)),
            (WATCHED.to_string(), Value::Array(watched_folders)),
            (SHORTCUTS.to_string(), Value::Object(shortcuts)),
        ]
        .into_iter()
        .collect()
    }

    #[cfg(test)]
    fn from_path(path: &Path) -> Self {
        Self::from_section(
            &ui::storage::SettingsSection::named(SECTION)
                .in_file(path)
                .load(),
        )
    }

    #[cfg(test)]
    fn save_at(&self, path: &Path) -> std::io::Result<()> {
        ui::storage::SettingsSection::named(SECTION)
            .in_file(path)
            .update(self.section())
    }
}

/// These reader commands deliberately remain fixed. They are either global
/// mode controls or selection-sensitive actions, so allowing a configurable
/// binding to shadow them would make the keyboard contract ambiguous.
pub(crate) fn reserved_reader_key(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    matches!(
        lowered.as_str(),
        "t" | "n" | "f" | "c" | "+" | "=" | "-" | "_" | "−" | "[" | "]" | "1" | "2" | "3" | "4"
    )
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

fn valid_shortcut(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(key) = chars.next() else {
        return false;
    };
    chars.next().is_none() && !key.is_whitespace() && !key.is_control()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paths round-trip only in private settings, hostile paths and ids fail closed, and shortcuts are local and validated.
    #[test]
    fn local_library_settings_stay_private_and_fail_closed() {
        // --- phase: paths_round_trip_only_in_private_settings ---
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("settings.json");
            let books = directory.path().join("books");
            let book = books.join("a.epub");
            let mut registry = LocalLibrary::default();
            registry.remember_book("book:0123456789abcdef0123456789abcdef", &book);
            registry.toggle_watch(&books);
            assert_eq!(registry.books.len(), 1);
            registry.save_at(&path).unwrap();

            assert!(ui::storage::settings_at(&path).contains_key(SECTION));

            let restored = LocalLibrary::from_path(&path);
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

        // --- phase: shortcuts_are_local_validated_and_do_not_require_book_entries ---
        {
            let directory = tempfile::tempdir().unwrap();
            let safe = directory.path().join("safe");
            let section = serde_json::json!({
                SHORTCUTS: { "next-page": "J", "bookmark": "\n", "other": "j" },
                WATCHED: [&safe]
            });
            let mut registry = LocalLibrary::from_section(section.as_object().unwrap());
            assert_eq!(registry.watched_folders, vec![safe]);
            assert_eq!(registry.shortcut("next-page", "."), "j");
            assert_eq!(registry.shortcut("bookmark", "b"), "b");
            assert!(!registry.set_shortcut("bookmark", "j"));
            assert!(!registry.set_shortcut("bookmark", "t"));
            assert!(!registry.set_shortcut("bookmark", "−"));
            assert!(registry.set_shortcut("bookmark", "x"));
            assert_eq!(registry.shortcut("bookmark", "b"), "x");
        }
    }
}
