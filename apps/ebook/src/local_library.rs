//! Where each device opens the library's books from.
//!
//! A source path is a local portal grant on one device, not part of the
//! shared library: each device keeps its grants and watched folders as its
//! own entry in a record beside the library ([`BookFiles`]), so reopening a
//! book or watching a folder survives restart without putting a path into
//! the library's document.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};

use ui::framework::PersonRecord;
use ui::tilo_resource::{self, Model, Refusal, resource};

const MAX_ENTRIES: usize = 200;
const MAX_PATH_BYTES: usize = 4096;

/// One device's grants as stored: where each book's file is, and the
/// folders it watches. Read back through [`LocalLibrary::from_stored`],
/// which keeps only what is valid.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Stored {
    #[serde(default)]
    books: BTreeMap<String, PathBuf>,
    #[serde(default)]
    watched_folders: Vec<PathBuf>,
}

tilo_resource::json_model!(Stored);

/// Where each device opens the library's books from, by device: a record
/// Books keeps beside the library in the workspace. A path names a file on
/// one device, so each device reads and writes only its own entry.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct BookFiles {
    pub devices: BTreeMap<String, Stored>,
}

#[resource(kind = "ebook.files", label = "Book files")]
impl BookFiles {
    /// `device`'s grants and watched folders are now `kept`.
    #[action(housekeeping)]
    pub fn keep(
        &mut self,
        device: String,
        #[param("The device's book files and watched folders, as it stores them")]
        kept: serde_json::Value,
    ) -> Result<(), Refusal> {
        let kept: Stored = serde_json::from_value(kept)
            .map_err(|error| Refusal::new(format!("not a device's book files: {error}")))?;
        self.devices
            .insert(device, LocalLibrary::from_stored(&kept).stored());
        Ok(())
    }
}

#[derive(Clone, Default)]
pub(crate) struct LocalLibrary {
    /// Where the grants are kept: the [`BookFiles`] record and this
    /// device's id. `None` keeps them in memory only.
    store: Option<(PersonRecord<BookFiles>, String)>,
    pub(crate) books: HashMap<String, PathBuf>,
    pub(crate) watched_folders: Vec<PathBuf>,
}

impl std::fmt::Debug for LocalLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalLibrary")
            .field("books", &self.books)
            .field("watched_folders", &self.watched_folders)
            .finish_non_exhaustive()
    }
}

impl LocalLibrary {
    /// `device`'s grants in `record`, which [`Self::save`] writes back to.
    pub(crate) fn load(record: PersonRecord<BookFiles>, device: String) -> Self {
        let stored = record.with(|files| {
            Self::from_stored(&files.devices.get(&device).cloned().unwrap_or_default())
        });
        Self {
            store: Some((record, device)),
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
        let Some((record, device)) = &self.store else {
            return;
        };
        let stored = self.stored();
        if record.with(|files| files.devices.get(device) == Some(&stored)) {
            return;
        }
        let keep = serde_json::to_value(&stored)
            .map_err(|error| error.to_string())
            .and_then(|kept| {
                record
                    .invoke(&BookFiles::KEEP.with((device.clone(), kept)))
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = keep {
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

    /// Hostile paths and ids fail closed.
    #[test]
    fn local_library_fails_closed() {
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
