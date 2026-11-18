//! The browser's bookmarks, apart from browsing history, which stays private.

use tilo_resource::{self, Model, Refusal};

pub const MAX_BOOKMARKS: usize = 10_000;

#[derive(Model, Clone, Debug, PartialEq, Eq)]
#[model(check = Bookmark::valid)]
pub struct Bookmark {
    /// Absolute http(s), normalized: `https://example.org` keeps its slash.
    #[model(readonly)]
    pub url: String,
    #[model(label)]
    pub title: String,
}

impl Bookmark {
    fn valid(&self) -> Result<(), Refusal> {
        if crate::urls::normalized_bookmark_url(&self.url).as_deref() != Some(&self.url) {
            return Err(Refusal::new("a bookmark is a normalized http(s) URL"));
        }
        if self.title.len() > crate::urls::MAX_BOOKMARK_TITLE_BYTES {
            return Err(Refusal::new("a bookmark's title is at most 4 KiB"));
        }
        Ok(())
    }
}
