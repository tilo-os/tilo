//! Narrowing a widget's own list as the person types.

use std::collections::HashSet;

use tilo_search::{Document, FieldWeight, Matching, Page, SearchIndex};

/// Which items of a list match a query, for widgets that keep their own
/// order and only hide what doesn't match. Items are indexed once per list;
/// a name matches by substring, by words spread over its fields, or with a
/// typo.
pub(crate) struct ListFilter {
    index: SearchIndex,
    len: usize,
}

impl ListFilter {
    /// Index `items`, each a list of `(text, weight)` fields.
    pub(crate) fn new<'a, I, F>(items: I) -> Self
    where
        I: IntoIterator<Item = F>,
        F: IntoIterator<Item = (&'a str, FieldWeight)>,
    {
        let mut index = SearchIndex::new(Matching::LOOSE);
        let mut len = 0usize;
        index.replace_all(items.into_iter().enumerate().map(|(at, fields)| {
            len = at.saturating_add(1);
            fields
                .into_iter()
                .fold(Document::new(at as u64), |document, (text, weight)| {
                    document.field(text, weight)
                })
        }));
        if let Err(error) = index.commit() {
            tracing::warn!(%error, "could not index a list for filtering");
        }
        Self { index, len }
    }

    /// Positions of the items matching `query`; `None` when every item
    /// matches (an empty query).
    pub(crate) fn matching(&self, query: &str) -> Option<HashSet<usize>> {
        if query.trim().is_empty() {
            return None;
        }
        Some(
            self.index
                .search(query, Page::first(self.len))
                .hits
                .iter()
                .filter_map(|hit| usize::try_from(hit.key).ok())
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrows_by_substring_spread_words_and_typos() {
        let items = [
            ("Rename Workspace", "Give the workspace a new name"),
            ("Hotel", ""),
            ("Bravo", ""),
        ];
        let filter = ListFilter::new(items.iter().map(|(label, description)| {
            [
                (*label, FieldWeight::TITLE),
                (*description, FieldWeight::SUBTITLE),
            ]
        }));
        assert_eq!(filter.matching(""), None);
        assert_eq!(filter.matching("ren"), Some(HashSet::from([0])));
        assert_eq!(filter.matching("new name"), Some(HashSet::from([0])));
        assert_eq!(filter.matching("rename space"), Some(HashSet::from([0])));
        assert_eq!(filter.matching("hotle"), Some(HashSet::from([1])));
        assert_eq!(filter.matching("xyz"), Some(HashSet::new()));
    }
}
