//! [`ListIndex`]: ranking a short list its owner rebuilds freely.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use crate::index::{Document, SearchIndex};
use crate::query::Matching;
use crate::weight::{FieldWeight, Ranks};
use tilo_lock::MutexRecover as _;

/// Ranks the rows of a list against a query, for surfaces that rebuild
/// their rows on every query or change (commands, windows, actions).
///
/// The rows' text is indexed once and kept until it changes, so asking
/// again with the same rows only searches. Rows are identified by position.
pub struct ListIndex {
    matching: Matching,
    ranks: Option<Ranks>,
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
            ranks: None,
            cached: Mutex::new(None),
        }
    }

    /// Rank tiers with `ranks` instead of [`MatchTier::rank_in`](crate::MatchTier::rank_in).
    #[must_use]
    pub fn with_ranks(mut self, ranks: Ranks) -> Self {
        self.ranks = Some(ranks);
        self
    }

    /// Each row's rank for `query`, or `None` where it does not match. An
    /// empty query matches every row at rank 0.
    pub fn ranks<'a, R>(&self, query: &str, rows: impl IntoIterator<Item = R>) -> Vec<Option<i32>>
    where
        R: IntoIterator<Item = (&'a str, FieldWeight)>,
    {
        let one_word = query.split_whitespace().nth(1).is_none();
        let rows = RowFields::new(rows, one_word);
        if query.trim().is_empty() {
            return vec![Some(0); rows.len()];
        }
        let mut hasher = DefaultHasher::new();
        rows.hash(&mut hasher);
        let fingerprint = hasher.finish();
        let mut cached = self.cached.lock_recover();
        if cached.as_ref().is_none_or(|(held, _)| *held != fingerprint) {
            let mut index = SearchIndex::new(self.matching);
            if let Some(ranks) = self.ranks {
                index = index.with_ranks(ranks);
            }
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
            for (key, rank) in index.ranks(query) {
                if let Some(slot) = usize::try_from(key).ok().and_then(|at| ranks.get_mut(at)) {
                    *slot = rank;
                }
            }
        }
        drop(cached);
        ranks
    }
}

/// Borrowed fields for one query, staged in two flat buffers rather than
/// allocating a separate vector for every row on every keystroke.
#[derive(Hash)]
struct RowFields<'a> {
    fields: Vec<(&'a str, FieldWeight)>,
    ends: Vec<usize>,
}

impl<'a> RowFields<'a> {
    fn new<R>(rows: impl IntoIterator<Item = R>, one_word: bool) -> Self
    where
        R: IntoIterator<Item = (&'a str, FieldWeight)>,
    {
        let rows = rows.into_iter();
        let mut fields = Vec::new();
        let mut ends = Vec::with_capacity(rows.size_hint().0);
        for row in rows {
            fields.extend(
                row.into_iter()
                    .filter(|(text, weight)| !(one_word && is_phrase_keyword(text, *weight))),
            );
            ends.push(fields.len());
        }
        Self { fields, ends }
    }

    fn len(&self) -> usize {
        self.ends.len()
    }

    fn iter(&self) -> impl Iterator<Item = &[(&'a str, FieldWeight)]> {
        let mut start = 0;
        let mut fields = self.fields.as_slice();
        self.ends.iter().map(move |&end| {
            // Boundaries come only from extending this buffer in `new`.
            let (row, remaining) = fields.split_at(end.saturating_sub(start));
            fields = remaining;
            start = end;
            row
        })
    }
}

/// A keyword of several words is one name for the row, matched as a phrase:
/// a query of one word never matches it. "rechner" is not asking for
/// "rechner neu starten" (Restart), and "bildschirm" not for "bildschirm
/// sperren" (Lock); "rechner neu" is.
fn is_phrase_keyword(text: &str, weight: FieldWeight) -> bool {
    weight == FieldWeight::KEYWORD && text.split_whitespace().nth(1).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MatchTier;

    fn reference_ranks(query: &str, rows: &[Vec<(&str, FieldWeight)>]) -> Vec<Option<i32>> {
        if query.trim().is_empty() {
            return vec![Some(0); rows.len()];
        }
        let one_word = query.split_whitespace().nth(1).is_none();
        let mut index = SearchIndex::new(Matching::LOOSE);
        index.replace_all(rows.iter().enumerate().map(|(at, fields)| {
            fields
                .iter()
                .filter(|(text, weight)| !(one_word && is_phrase_keyword(text, *weight)))
                .fold(Document::new(at as u64), |document, (text, weight)| {
                    document.field(*text, *weight)
                })
        }));
        index.commit().expect("the reference list commits");
        let mut ranks = vec![None; rows.len()];
        for hit in index.search(query, crate::Page::first(rows.len())).hits {
            *ranks
                .get_mut(usize::try_from(hit.key).expect("the row key fits usize"))
                .expect("the reference answers a row it indexed") = hit.rank;
        }
        ranks
    }

    #[test]
    fn flat_staging_preserves_rows_fields_weights_and_phrase_changes() {
        let list = ListIndex::default();
        let title = FieldWeight::TITLE;
        let keyword = FieldWeight::KEYWORD;
        let versions = [
            vec![vec![("Hotel", title)], vec![]],
            vec![vec![], vec![("Hotel", title)]],
            vec![vec![("Hotel", title), ("Rename", keyword)]],
            vec![vec![("Hotel", title)], vec![("Rename", keyword)]],
            vec![vec![("Hotel", FieldWeight::SUBTITLE)]],
            vec![vec![("Restart", title), ("rechner neu starten", keyword)]],
            vec![vec![("Écran", title)], vec![("世界", title)], vec![]],
            Vec::new(),
        ];
        for rows in versions {
            for query in [
                "",
                "  ",
                "hotel",
                "hotle",
                "rename",
                "rechner",
                "rechner neu",
                "écr",
                "世界",
                "zzq",
            ] {
                // Filtering removes an exact size hint, as lazy callers may do.
                let actual = list.ranks(
                    query,
                    rows.iter().filter(|_| true).map(|row| row.iter().copied()),
                );
                assert_eq!(actual, reference_ranks(query, &rows), "{query:?}: {rows:?}");
            }
        }
    }

    #[test]
    fn flat_staging_preserves_the_large_list_engine_path() {
        let list = ListIndex::default();
        let mut rows = vec![vec![("Hotel", FieldWeight::TITLE)]; crate::direct::DIRECT_LIMIT + 1];
        rows.push(Vec::new());
        for query in ["", "hotel", "hotle", "zzq"] {
            assert_eq!(
                list.ranks(query, rows.iter().map(|row| row.iter().copied())),
                reference_ranks(query, &rows),
                "{query:?}"
            );
        }
    }

    #[test]
    fn a_keyword_phrase_needs_more_than_its_first_word() {
        let list = ListIndex::new(Matching::LOOSE);
        let rows = [
            vec![
                ("Restart", FieldWeight::TITLE),
                ("neustart", FieldWeight::KEYWORD),
                ("rechner neu starten", FieldWeight::KEYWORD),
            ],
            vec![
                ("Calculator", FieldWeight::TITLE),
                ("rechner", FieldWeight::KEYWORD),
            ],
        ];
        let ranks = |query| list.ranks(query, rows.iter().map(|row| row.iter().copied()));
        assert_eq!(ranks("rechner")[0], None, "one word never matches a phrase");
        assert!(ranks("rechner")[1].is_some());
        assert!(ranks("rechner neu")[0].is_some(), "the phrase, typed on");
        assert!(
            ranks("neustart")[0].is_some(),
            "a one-word keyword still matches"
        );
    }

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
