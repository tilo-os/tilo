//! Durable browser-library models. Bookmarks are intentionally separate from
//! browsing history: history is private activity, while bookmarks are explicit
//! user-created resources that may be shared through the app document.

const MAX_BOOKMARKS: usize = crate::continuity::MAX_BUNDLE_ENTRIES;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
    /// CRDT add-tags still present for this URL; a remove tombstones exactly
    /// these, so a concurrent add survives it.
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bookmarks {
    entries: Vec<Bookmark>,
}

impl Bookmarks {
    pub(crate) fn from_entries(mut entries: Vec<Bookmark>) -> Self {
        entries.truncate(MAX_BOOKMARKS);
        Self { entries }
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
}
