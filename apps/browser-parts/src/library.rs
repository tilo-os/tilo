//! Durable browser-library models. Bookmarks are intentionally separate from
//! browsing history: history is private activity, while bookmarks are explicit
//! user-created resources that may be shared through the app document.

use ui::tilo_resource::{self, Collection, Id, Model, Refusal};

pub const MAX_BOOKMARKS: usize = 10_000;

/// One bookmarked page, an item of the browser's bookmarks.
#[derive(Model, Clone, Debug, PartialEq, Eq)]
#[model(check = Bookmark::valid)]
pub struct Bookmark {
    /// Address: one absolute http(s) URL in its normalized form, so
    /// `https://example.org` is `https://example.org/`.
    #[model(readonly)]
    pub url: String,
    /// Title
    #[model(label)]
    pub title: String,
}

impl Bookmark {
    /// A bookmark names one web page, under a title that fits a row.
    fn valid(&self) -> Result<(), Refusal> {
        if crate::actions::normalized_bookmark_url(&self.url).as_deref() != Some(&self.url) {
            return Err(Refusal::new("a bookmark is a normalized http(s) URL"));
        }
        if self.title.len() > crate::actions::MAX_BOOKMARK_TITLE_BYTES {
            return Err(Refusal::new("a bookmark's title is at most 4 KiB"));
        }
        Ok(())
    }
}

/// One bookmarked page as the chrome lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookmarkEntry {
    /// The bookmark's item in the document.
    pub id: Id,
    pub url: String,
    pub title: String,
}

/// The bookmarks the chrome shows: one per page, sorted by address. Two
/// people bookmarking one page at once keep two items; the page is listed
/// once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bookmarks {
    entries: Vec<BookmarkEntry>,
}

impl Bookmarks {
    /// The bookmarks `collection` holds.
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

    /// Whether `url` is bookmarked: the document keeps bookmarks under their
    /// normalized URL, so `https://example.org` is `https://example.org/`.
    pub fn contains(&self, url: &str) -> bool {
        let normalized = crate::actions::normalized_bookmark_url(url);
        let url = normalized.as_deref().unwrap_or(url);
        self.entries.iter().any(|entry| entry.url == url)
    }
}
