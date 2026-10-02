//! Durable browser-library models. Bookmarks are intentionally separate from
//! browsing history: history is private activity, while bookmarks are explicit
//! user-created resources that may be shared through the app document.

use std::path::{Path, PathBuf};

const MAX_BOOKMARKS: usize = 10_000;
const MAX_BOOKMARK_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_BOOKMARK_URL_BYTES: usize = 8 * 1024;
const MAX_BOOKMARK_TITLE_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
    /// CRDT add-tags still present for this URL. File-cache rows leave this
    /// empty; remove is published from the document projection.
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Bookmarks {
    entries: Vec<Bookmark>,
    /// Changes only on corpus mutations; used to avoid rebuilding search on
    /// every frame.
    revision: u64,
}

// The revision is a cache-invalidation implementation detail, never part of
// the bookmark model's durable value equality.
impl PartialEq for Bookmarks {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl Eq for Bookmarks {}

impl Bookmarks {
    pub(crate) fn from_entries(mut entries: Vec<Bookmark>) -> Self {
        entries.truncate(MAX_BOOKMARKS);
        Self {
            entries,
            revision: 0,
        }
    }

    pub fn entries(&self) -> &[Bookmark] {
        &self.entries
    }

    pub fn contains(&self, url: &str) -> bool {
        self.entries.iter().any(|entry| entry.url == url)
    }

    /// Monotonic-in-practice change token for cached projections.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn add(&mut self, url: &str, title: &str) -> bool {
        let url = url.trim();
        if url.is_empty()
            || url.len() > MAX_BOOKMARK_URL_BYTES
            || self.entries.len() >= MAX_BOOKMARKS
            || self.contains(url)
        {
            return false;
        }
        self.entries.push(Bookmark {
            url: url.to_string(),
            title: if title.trim().is_empty() {
                url.to_string()
            } else {
                bounded_text(title.trim(), MAX_BOOKMARK_TITLE_BYTES)
            },
            tags: Vec::new(),
        });
        self.revision = self.revision.wrapping_add(1);
        true
    }

    pub fn remove(&mut self, url: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.url != url);
        let changed = before != self.entries.len();
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    pub fn load() -> Self {
        Self::path().map_or_else(Self::default, |path| {
            std::fs::metadata(&path)
                .ok()
                .filter(|metadata| metadata.len() <= MAX_BOOKMARK_FILE_BYTES as u64)
                .and_then(|_| std::fs::read_to_string(path).ok())
                .map_or_else(Self::default, |text| Self::from_json(&text))
        })
    }

    /// Saves the bookmark collection in the platform browser-data directory.
    ///
    /// # Errors
    ///
    /// Returns an error when no platform data directory is available or when
    /// the collection cannot be written there.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no browser data directory")
        })?;
        self.save_to(&path)
    }

    fn path() -> Option<PathBuf> {
        crate::storage::data_dir().map(|dir| dir.join("bookmarks.json"))
    }

    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let rows: Vec<_> = self
            .entries
            .iter()
            .map(|entry| serde_json::json!({ "url": entry.url, "title": entry.title }))
            .collect();
        let bytes = serde_json::to_vec_pretty(&rows)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        std::fs::write(path, bytes)
    }

    pub(crate) fn from_json(text: &str) -> Self {
        if text.len() > MAX_BOOKMARK_FILE_BYTES {
            return Self::default();
        }
        let entries = serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|row| {
                let url = row.get("url")?.as_str()?.trim();
                if url.is_empty() || url.len() > MAX_BOOKMARK_URL_BYTES {
                    return None;
                }
                Some(Bookmark {
                    title: row
                        .get("title")
                        .and_then(serde_json::Value::as_str)
                        .filter(|title| !title.trim().is_empty())
                        .map_or_else(
                            || url.to_string(),
                            |title| bounded_text(title.trim(), MAX_BOOKMARK_TITLE_BYTES),
                        ),
                    url: url.to_string(),
                    tags: Vec::new(),
                })
            })
            .take(MAX_BOOKMARKS)
            .collect();
        Self {
            entries,
            revision: 0,
        }
    }
}

fn bounded_text(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value.get(..end).unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bookmark store persistence and bounds.
    #[test]
    fn bookmark_store_dedups_round_trips_and_bounds_hostile_input() {
        // Phase `bookmark_store_deduplicates_and_round_trips`.
        {
            let mut bookmarks = Bookmarks::default();
            assert!(bookmarks.add("https://example.test", "Example"));
            assert!(!bookmarks.add("https://example.test", "Duplicate"));
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("bookmarks.json");
            bookmarks.save_to(&path).unwrap();
            let loaded = Bookmarks::from_json(&std::fs::read_to_string(path).unwrap());
            assert_eq!(loaded, bookmarks);
            assert!(bookmarks.remove("https://example.test"));
        }

        // Phase `malformed_or_huge_store_is_bounded`.
        {
            assert!(Bookmarks::from_json("not json").entries().is_empty());
            let rows = (0..10_500)
                .map(|index| serde_json::json!({"url": format!("https://{index}.test")}))
                .collect::<Vec<_>>();
            let loaded = Bookmarks::from_json(&serde_json::to_string(&rows).unwrap());
            assert_eq!(loaded.entries().len(), 10_000);
        }

        // Phase `hostile_rows_cannot_create_unbounded_library_labels`.
        {
            let too_long_url = format!(
                "https://example.test/{}",
                "x".repeat(MAX_BOOKMARK_URL_BYTES)
            );
            let json = serde_json::json!([
                { "url": too_long_url, "title": "ignored" },
                {
                    "url": "https://kept.example.test",
                    "title": "t".repeat(MAX_BOOKMARK_TITLE_BYTES + 64),
                }
            ]);
            let loaded = Bookmarks::from_json(&json.to_string());
            assert_eq!(loaded.entries().len(), 1);
            assert_eq!(loaded.entries()[0].title.len(), MAX_BOOKMARK_TITLE_BYTES);
            assert!(
                Bookmarks::from_json(&" ".repeat(MAX_BOOKMARK_FILE_BYTES + 1))
                    .entries()
                    .is_empty()
            );
        }
    }
}
