//! The browser document's bookmarks, apart from browsing history, which stays
//! private.

use ui::tilo_resource::{Collection, Id};

pub use browser_model::library::{Bookmark, MAX_BOOKMARKS};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookmarkEntry {
    pub id: Id,
    pub url: String,
    pub title: String,
}

/// The bookmarks the chrome shows: one per page, sorted by address; two people
/// bookmarking one page at once keep two items.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bookmarks {
    entries: Vec<BookmarkEntry>,
}

impl Bookmarks {
    pub fn of(collection: &Collection<Bookmark>) -> Self {
        let mut entries: Vec<BookmarkEntry> = collection
            .iter()
            .map(|(id, bookmark)| BookmarkEntry {
                id: id.clone(),
                url: bookmark.url.clone(),
                title: bookmark.title.clone(),
            })
            .collect();
        entries.sort_by(|a, b| {
            a.url
                .cmp(&b.url)
                .then_with(|| a.id.as_str().cmp(b.id.as_str()))
        });
        entries.dedup_by(|later, first| later.url == first.url);
        Self::from_entries(entries)
    }

    pub fn from_entries(mut entries: Vec<BookmarkEntry>) -> Self {
        entries.truncate(MAX_BOOKMARKS);
        Self { entries }
    }

    pub fn entries(&self) -> &[BookmarkEntry] {
        &self.entries
    }

    pub fn contains(&self, url: &str) -> bool {
        let normalized = crate::actions::normalized_bookmark_url(url);
        let url = normalized.as_deref().unwrap_or(url);
        self.entries.iter().any(|entry| entry.url == url)
    }
}
