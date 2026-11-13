//! The search index the page loads the first time someone searches: per
//! page its title, section, address, headings and prose.

use serde::Serialize;

use crate::markdown::Heading;

#[derive(Debug, Serialize)]
pub struct Entry<'a> {
    pub url: &'a str,
    pub title: &'a str,
    pub section: &'a str,
    pub description: &'a str,
    /// `[id, text]` per heading, so a hit can open at its heading.
    pub headings: Vec<(&'a str, &'a str)>,
    pub text: &'a str,
}

impl<'a> Entry<'a> {
    #[must_use]
    pub fn headings_of(headings: &'a [Heading]) -> Vec<(&'a str, &'a str)> {
        headings
            .iter()
            .map(|heading| (heading.id.as_str(), heading.text.as_str()))
            .collect()
    }
}

#[must_use]
pub fn index(entries: &[Entry<'_>]) -> String {
    #[expect(
        clippy::expect_used,
        reason = "serialising strings and tuples of strings cannot fail"
    )]
    serde_json::to_string(entries).expect("search entries serialise")
}
