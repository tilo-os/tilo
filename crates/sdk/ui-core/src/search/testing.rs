//! Shared assertions for [`SearchProvider`] implementations.
//!
//! Every app provider owes the shell the same promise: a record that entered
//! the catalogue last is still found, and finding it costs work proportional
//! to the matches rather than to the corpus. [`assert_late_hit_reachable`]
//! checks that promise through the app's own provider, so an app test only
//! says how a row of text becomes one of its records.
//!
//! The toolkit's own catalogue test proves the index at 100,000 records. An
//! app's provider sits on that index, so a few thousand records are enough
//! to show it builds and queries it rather than scanning.

use super::{SearchProvider, SearchResult};

/// Text that only the corpus's last row contains.
pub const LATE_HIT_NEEDLE: &str = "cobalt-orchid-needle";

/// Work budget per page. A provider that scans instead of using the index
/// inspects ordinary rows and exceeds [`MAX_INSPECTED`] long before it
/// reaches the last one.
const PAGE_BUDGET: usize = 8;

/// Most candidates the whole search may inspect across all its pages.
const MAX_INSPECTED: usize = 64;

/// One row of the synthetic corpus handed to the provider builder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorpusRow {
    /// Position in the corpus, `0..n`. Unique, so it can seed record ids.
    pub index: usize,
    /// Searchable text for the record. Ordinary rows share their words with
    /// every other row; the last row alone adds [`LATE_HIT_NEEDLE`].
    pub text: String,
    /// Whether this is the last row, the one the search must find.
    pub is_target: bool,
}

/// The rows of an `n`-row corpus, in insertion order.
#[derive(Clone, Debug)]
pub struct Corpus {
    next: usize,
    len: usize,
}

impl Iterator for Corpus {
    type Item = CorpusRow;

    fn next(&mut self) -> Option<CorpusRow> {
        if self.next >= self.len {
            return None;
        }
        let index = self.next;
        self.next += 1;
        let is_target = index + 1 == self.len;
        let text = if is_target {
            format!("ordinary record {index} {LATE_HIT_NEEDLE}")
        } else {
            format!("ordinary record {index}")
        };
        Some(CorpusRow {
            index,
            text,
            is_target,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.len - self.next;
        (left, Some(left))
    }
}

impl ExactSizeIterator for Corpus {}

/// Build a provider over `n` records with `rows_fn`, then assert that a
/// search for [`LATE_HIT_NEEDLE`] returns the last record and nothing else,
/// inspecting at most a few dozen candidates across all pages.
///
/// `rows_fn` turns each [`CorpusRow`] into one of the app's records (put
/// `text` wherever the app indexes it, such as a subject or title) and
/// returns the provider. The provider and the hit are handed back so the
/// caller can check app-specific behaviour, such as resolving the hit's
/// action address to a resource.
///
/// # Panics
///
/// When the last record is not found, when anything else matches, or when
/// the search inspects more candidates than an index lookup needs.
pub fn assert_late_hit_reachable<P, F>(rows_fn: F, n: usize) -> (P, SearchResult)
where
    P: SearchProvider,
    F: FnOnce(Corpus) -> P,
{
    assert!(n > 0, "the corpus needs at least the target row");
    let provider = rows_fn(Corpus { next: 0, len: n });
    let mut results = Vec::new();
    let mut inspected = 0;
    let mut cursor = None;
    for page_number in 1.. {
        let page = provider.search_page(LATE_HIT_NEEDLE, cursor.as_ref(), PAGE_BUDGET, PAGE_BUDGET);
        inspected += page.inspected;
        results.extend(page.results);
        assert!(
            inspected <= MAX_INSPECTED && page_number <= MAX_INSPECTED,
            "{}: finding one record among {n} took {page_number} pages inspecting \
             {inspected} candidates; search work must follow the matches, not the corpus",
            provider.name()
        );
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(
        results.len(),
        1,
        "{}: exactly the last of {n} records contains the needle, found {:?}",
        provider.name(),
        results.iter().map(|hit| &hit.title).collect::<Vec<_>>()
    );
    let hit = results.remove(0);
    (provider, hit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{SearchCatalog, SearchCursor, SearchPage};
    use tilo_actions::ActionAddress;

    fn catalogue(corpus: Corpus, extra_text: &str) -> SearchCatalog {
        let catalog = SearchCatalog::new("Documents");
        for row in corpus {
            let id = format!("document:{}", row.index);
            let hit = SearchResult::for_action(
                "Documents",
                format!("Document {}", row.index),
                ActionAddress::new(&id, "open"),
            );
            catalog
                .upsert(&id, format!("{}{extra_text}", row.text), hit)
                .unwrap();
        }
        catalog
    }

    #[test]
    fn a_catalogue_backed_provider_finds_the_last_record() {
        let (catalog, hit) = assert_late_hit_reachable(|corpus| catalogue(corpus, ""), 3_000);
        assert_eq!(hit.title, "Document 2999");
        assert_eq!(catalog.stats().records, 3_000);
    }

    #[test]
    fn only_the_last_row_carries_the_needle() {
        let rows: Vec<_> = Corpus { next: 0, len: 4 }.collect();
        assert_eq!(rows.len(), 4);
        assert!(
            rows.iter()
                .take(3)
                .all(|row| !row.is_target && !row.text.contains(LATE_HIT_NEEDLE))
        );
        assert!(rows[3].is_target && rows[3].text.contains(LATE_HIT_NEEDLE));
    }

    #[test]
    #[should_panic(expected = "exactly the last of 20 records")]
    fn a_provider_that_matches_ordinary_rows_fails() {
        let needle = format!(" {LATE_HIT_NEEDLE}");
        assert_late_hit_reachable(|corpus| catalogue(corpus, &needle), 20);
    }

    #[test]
    #[should_panic(expected = "search work must follow the matches")]
    fn a_provider_that_scans_the_corpus_fails() {
        /// Finds the right record, but by reading every one of them.
        struct Scanning(SearchCatalog);
        impl SearchProvider for Scanning {
            fn name(&self) -> &str {
                "Scanning"
            }
            fn search(&self, _: &str) -> Vec<SearchResult> {
                Vec::new()
            }
            fn search_page(
                &self,
                query: &str,
                cursor: Option<&SearchCursor>,
                work_budget: usize,
                result_limit: usize,
            ) -> SearchPage {
                let mut page = self.0.query_page(query, cursor, work_budget, result_limit);
                page.inspected = self.0.stats().records;
                page
            }
        }
        assert_late_hit_reachable(|corpus| Scanning(catalogue(corpus, "")), 100);
    }
}
