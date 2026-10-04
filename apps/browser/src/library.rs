//! Durable browser-library models. Bookmarks are intentionally separate from
//! browsing history: history is private activity, while bookmarks are explicit
//! user-created resources that may be shared through the app document.

const MAX_BOOKMARKS: usize = 10_000;
#[cfg(test)]
const MAX_BOOKMARK_URL_BYTES: usize = 8 * 1024;
#[cfg(test)]
const MAX_BOOKMARK_TITLE_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
    /// CRDT add-tags still present for this URL; a remove tombstones exactly
    /// these, so a concurrent add survives it.
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

    /// Whether `url` is bookmarked: the document keeps bookmarks under their
    /// normalized URL, so `https://example.org` is `https://example.org/`.
    pub fn contains(&self, url: &str) -> bool {
        let normalized = crate::actions::normalized_bookmark_url(url);
        let url = normalized.as_deref().unwrap_or(url);
        self.entries.iter().any(|entry| entry.url == url)
    }

    /// Monotonic-in-practice change token for cached projections.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    #[cfg(test)]
    pub(crate) fn add(&mut self, url: &str, title: &str) -> bool {
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

    #[cfg(test)]
    pub(crate) fn remove(&mut self, url: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.url != url);
        let changed = before != self.entries.len();
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }
}

#[cfg(test)]
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
