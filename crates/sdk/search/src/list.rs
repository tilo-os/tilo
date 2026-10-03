//! [`ListIndex`]: ranking a short list its owner rebuilds freely.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use crate::index::{Document, Page, SearchIndex};
use crate::query::Matching;
use crate::weight::FieldWeight;

/// Ranks the rows of a list against a query, for surfaces that rebuild
/// their rows on every query or change (commands, windows, actions).
///
/// The rows' text is indexed once and kept until it changes, so asking
/// again with the same rows only searches. Rows are identified by position.
pub struct ListIndex {
    matching: Matching,
    cached: Mutex<Option<(u64, SearchIndex)>>,
}

impl std::fmt::Debug for ListIndex {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ListIndex")
            .field("matching", &self.matching)
            .finish_non_exhaustive()
    }
}

/// Names in a short list are what a person half remembers, so the default
/// also matches words spread over fields and typos.
impl Default for ListIndex {
    fn default() -> Self {
        Self::new(Matching::LOOSE)
    }
}

impl ListIndex {
    #[must_use]
    pub const fn new(matching: Matching) -> Self {
        Self {
            matching,
            cached: Mutex::new(None),
        }
    }

    /// Each row's rank for `query`, or `None` where it does not match. An
    /// empty query matches every row at rank 0.
    pub fn ranks<'a, R>(&self, query: &str, rows: impl IntoIterator<Item = R>) -> Vec<Option<i32>>
    where
        R: IntoIterator<Item = (&'a str, FieldWeight)>,
    {
        let rows: Vec<Vec<(&'a str, FieldWeight)>> = rows
            .into_iter()
            .map(|row| row.into_iter().collect())
            .collect();
        if query.trim().is_empty() {
            return vec![Some(0); rows.len()];
        }
        let mut hasher = DefaultHasher::new();
        rows.hash(&mut hasher);
        let fingerprint = hasher.finish();
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cached.as_ref().is_none_or(|(held, _)| *held != fingerprint) {
            let mut index = SearchIndex::new(self.matching);
            index.replace_all(rows.iter().enumerate().map(|(at, fields)| {
                fields
                    .iter()
                    .fold(Document::new(at as u64), |document, (text, weight)| {
                        document.field(*text, *weight)
                    })
            }));
            if let Err(error) = index.commit() {
                tracing::warn!(%error, "could not index a list");
            }
            *cached = Some((fingerprint, index));
        }
        let mut ranks = vec![None; rows.len()];
        if let Some((_, index)) = cached.as_ref() {
            for hit in index.search(query, Page::first(rows.len())).hits {
                if let Some(rank) = usize::try_from(hit.key)
                    .ok()
                    .and_then(|at| ranks.get_mut(at))
                {
                    *rank = hit.rank;
                }
            }
        }
        drop(cached);
        ranks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MatchTier;

    #[test]
    fn ranks_rows_and_reindexes_only_on_change() {
        let list = ListIndex::new(Matching::LOOSE);
        let rows = [
            ["Rename Workspace", "Give the workspace a new name"],
            ["Hotel", ""],
            ["Bravo", ""],
        ];
        let fields = |rows: &[[&'static str; 2]]| {
            rows.iter()
                .map(|[label, description]| {
                    [
                        (*label, FieldWeight::TITLE),
                        (*description, FieldWeight::SUBTITLE),
                    ]
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(list.ranks("", fields(&rows)), [Some(0); 3]);
        assert_eq!(
            list.ranks("ren", fields(&rows)),
            [Some(MatchTier::Prefix.percent()), None, None]
        );
        assert!(list.ranks("new name", fields(&rows))[0].is_some());
        assert!(list.ranks("rename space", fields(&rows))[0].is_some());
        assert!(list.ranks("hotle", fields(&rows))[1].is_some());
        assert_eq!(list.ranks("xyz", fields(&rows)), [None; 3]);
        let changed = [["Charlie", ""], ["Hotel", ""]];
        assert_eq!(
            list.ranks("char", fields(&changed)),
            [Some(MatchTier::Prefix.percent()), None]
        );
    }
}
