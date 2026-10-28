//! What the shared result surface shows of the row a person is on.
//!
//! A provider says what is worth knowing about a hit before opening it — a
//! feed's site, unread count and latest entries; an article's source, date
//! and first lines; what a folder holds — and the platform lays it out in
//! the preview band under the list, the same way in every palette. Data,
//! never drawing code. A hit with nothing worth previewing carries none, and
//! the band is not drawn for it.

/// The preview of one search hit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchPreview {
    /// What the preview is of, as its heading.
    pub title: Option<String>,
    /// Short facts shown side by side in columns: a site, a count, a date.
    pub facts: Vec<String>,
    /// Items it holds or lists, one per line: a feed's latest entries, a
    /// folder's feeds.
    pub lines: Vec<String>,
    /// The start of its text, wrapped over a few lines.
    pub body: Option<String>,
}

/// How many [`SearchPreview::lines`] the band shows.
pub const PREVIEW_LINES: usize = 3;

/// How many lines the band wraps a [`SearchPreview::body`] over.
pub const PREVIEW_BODY_LINES: usize = 2;

impl SearchPreview {
    /// A preview headed `title`.
    #[must_use]
    pub fn titled(title: impl Into<String>) -> Self {
        Self {
            title: Some(title.into()),
            ..Self::default()
        }
    }

    /// One more fact on its facts line; an empty one adds nothing.
    #[must_use]
    pub fn fact(mut self, fact: impl Into<String>) -> Self {
        let fact = fact.into();
        if !fact.trim().is_empty() {
            self.facts.push(fact);
        }
        self
    }

    /// One more item line; an empty one adds nothing.
    #[must_use]
    pub fn line(mut self, line: impl Into<String>) -> Self {
        let line = line.into();
        if !line.trim().is_empty() {
            self.lines.push(line);
        }
        self
    }

    /// The start of its text; an empty one sets none.
    #[must_use]
    pub fn body(mut self, body: impl Into<String>) -> Self {
        let body = body.into();
        self.body = (!body.trim().is_empty()).then_some(body);
        self
    }

    /// Whether it says anything at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.facts.is_empty()
            && self.lines.is_empty()
            && self.body.is_none()
    }

    /// The whole preview as one sentence for a screen reader.
    #[must_use]
    pub fn spoken(&self) -> String {
        self.title
            .iter()
            .cloned()
            .chain(self.facts.iter().cloned())
            .chain(self.lines.iter().take(PREVIEW_LINES).cloned())
            .chain(self.body.iter().cloned())
            .collect::<Vec<_>>()
            .join(". ")
    }
}

/// Plain text alone previews as its body.
impl From<String> for SearchPreview {
    fn from(body: String) -> Self {
        Self::default().body(body)
    }
}

impl From<&str> for SearchPreview {
    fn from(body: &str) -> Self {
        Self::from(body.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preview_keeps_only_what_says_something() {
        let preview = SearchPreview::titled("Rust Blog")
            .fact("blog.rust-lang.org")
            .fact("  ")
            .fact("1 unread")
            .line("Announcing Rust 1.96.1")
            .line("")
            .body(" ");
        assert_eq!(preview.facts, ["blog.rust-lang.org", "1 unread"]);
        assert_eq!(preview.lines, ["Announcing Rust 1.96.1"]);
        assert_eq!(preview.body, None);
        assert_eq!(
            preview.spoken(),
            "Rust Blog. blog.rust-lang.org. 1 unread. Announcing Rust 1.96.1"
        );
        assert!(SearchPreview::from("   ").is_empty());
        assert_eq!(SearchPreview::from("Sunny").body.as_deref(), Some("Sunny"));
    }
}
