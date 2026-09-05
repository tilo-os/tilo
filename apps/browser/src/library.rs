//! Durable browser-library models. Bookmarks are intentionally separate from
//! browsing history: history is private activity, while bookmarks are explicit
//! user-created resources that may be shared through the app document.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bookmarks {
    entries: Vec<Bookmark>,
}

impl Bookmarks {
    pub fn entries(&self) -> &[Bookmark] {
        &self.entries
    }

    pub fn contains(&self, url: &str) -> bool {
        self.entries.iter().any(|entry| entry.url == url)
    }

    pub fn add(&mut self, url: &str, title: &str) -> bool {
        let url = url.trim();
        if url.is_empty() || self.contains(url) {
            return false;
        }
        self.entries.push(Bookmark {
            url: url.to_string(),
            title: if title.trim().is_empty() {
                url.to_string()
            } else {
                title.trim().to_string()
            },
        });
        true
    }

    pub fn remove(&mut self, url: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.url != url);
        before != self.entries.len()
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map_or_else(Self::default, |text| Self::from_json(&text))
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no browser data directory")
        })?;
        self.save_to(&path)
    }

    fn path() -> Option<PathBuf> {
        dirs::data_dir().map(|dir| dir.join("qos-browser").join("bookmarks.json"))
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
        let entries = serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|row| {
                let url = row.get("url")?.as_str()?.trim().to_string();
                if url.is_empty() {
                    return None;
                }
                Some(Bookmark {
                    title: row
                        .get("title")
                        .and_then(serde_json::Value::as_str)
                        .filter(|title| !title.trim().is_empty())
                        .unwrap_or(&url)
                        .to_string(),
                    url,
                })
            })
            .take(10_000)
            .collect();
        Self { entries }
    }

    pub(crate) fn to_json(&self) -> String {
        let rows: Vec<_> = self
            .entries
            .iter()
            .map(|entry| serde_json::json!({ "url": entry.url, "title": entry.title }))
            .collect();
        serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmark_store_deduplicates_and_round_trips() {
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

    #[test]
    fn malformed_or_huge_store_is_bounded() {
        assert!(Bookmarks::from_json("not json").entries().is_empty());
        let rows = (0..10_500)
            .map(|index| serde_json::json!({"url": format!("https://{index}.test")}))
            .collect::<Vec<_>>();
        let loaded = Bookmarks::from_json(&serde_json::to_string(&rows).unwrap());
        assert_eq!(loaded.entries().len(), 10_000);
    }
}
