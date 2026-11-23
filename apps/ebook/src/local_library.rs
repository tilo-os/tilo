//! Where this device opens the library's books from. Paths and watched folders
//! stay with the device that holds their portal grants.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};

use settings_model::{Scope, SettingsSchema};
use ui::framework::ViewCx;
use ui::storage::Settings;

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

#[derive(Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct DeviceBookFiles {
    #[serde(default)]
    by_workspace: BTreeMap<String, Stored>,
}

impl SettingsSchema for DeviceBookFiles {
    const SCOPE: Scope = Scope::DeviceLocal;
}

#[derive(Clone, Default)]
pub(crate) struct LocalLibrary {
    /// The device settings and workspace where the paths were granted.
    store: Option<(Settings<DeviceBookFiles>, String)>,
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
    pub(crate) fn open(cx: &ViewCx<crate::resource::Ebook>) -> Self {
        let workspace = cx.workspace().unwrap_or_default();
        let settings = cx.settings::<DeviceBookFiles>();
        Self::load(settings.settings().clone(), workspace)
    }

    pub(crate) fn load(settings: Settings<DeviceBookFiles>, workspace: String) -> Self {
        let stored = settings
            .get()
            .by_workspace
            .get(&workspace)
            .map_or_else(Self::default, Self::from_stored);
        Self {
            store: Some((settings, workspace)),
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
        let Some((settings, workspace)) = &self.store else {
            return;
        };
        let stored = self.stored();
        if settings.get().by_workspace.get(workspace) == Some(&stored) {
            return;
        }
        if let Err(error) = settings.update(|state| {
            state.by_workspace.insert(workspace.clone(), stored);
        }) {
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
