//! Durable browser-library models: bookmarks, which the shared document keeps,
//! apart from browsing history, which stays private.

const MAX_BOOKMARKS: usize = crate::continuity::MAX_BUNDLE_ENTRIES;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
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

    pub fn contains(&self, url: &str) -> bool {
        let normalized = crate::actions::normalized_bookmark_url(url);
        let url = normalized.as_deref().unwrap_or(url);
        self.entries.iter().any(|entry| entry.url == url)
    }
}
