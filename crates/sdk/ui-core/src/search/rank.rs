//! Merging hits from different sources.
//!
//! Each source finds and ranks its own hits (through a [`SearchCatalog`]
//! or a [`ListIndex`]); a surface that shows several sources in one list
//! merges them by their provider bias and how the query meets the title
//! each hit shows, so an exact visible title beats a weak match from
//! another source.
//!
//! [`SearchCatalog`]: super::SearchCatalog
//! [`ListIndex`]: tilo_search::ListIndex

use super::provider::SearchResult;
use tilo_search::fold;

pub use tilo_search::{FieldWeight, MatchTier};

/// The tier at which folded `query` meets folded `text`, if it occurs:
/// exact, prefix, a later word start, or anywhere.
#[must_use]
pub fn match_tier(query: &str, text: &str) -> Option<MatchTier> {
    if query.is_empty() {
        return None;
    }
    if text == query {
        return Some(MatchTier::Exact);
    }
    if text.starts_with(query) {
        return Some(MatchTier::Prefix);
    }
    let mut found = false;
    for (at, _) in text.match_indices(query) {
        found = true;
        let starts_word = text
            .get(..at)
            .and_then(|before| before.chars().next_back())
            .is_none_or(|previous| !previous.is_alphanumeric());
        if starts_word {
            return Some(MatchTier::WordPrefix);
        }
    }
    found.then_some(MatchTier::Contains)
}

/// Points one rank step is worth when merging, so a tier outweighs a
/// provider bias notch.
const RANK_POINTS: i32 = 20;

/// How the query meets one displayed hit, for merging sources: the tier in
/// its title, else in its subtitle at subtitle weight.
#[must_use]
pub fn result_relevance(query: &str, result: &SearchResult) -> i32 {
    let query = fold(query.trim());
    let rank = match_tier(&query, &fold(&result.title))
        .and_then(|tier| tier.rank_in(FieldWeight::TITLE))
        .or_else(|| {
            let subtitle = fold(result.subtitle.as_deref()?);
            match_tier(&query, &subtitle)?.rank_in(FieldWeight::SUBTITLE)
        })
        .unwrap_or(0);
    rank.saturating_mul(RANK_POINTS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_order_exact_prefix_word_and_infix_matches() {
        assert_eq!(match_tier("dune", "dune"), Some(MatchTier::Exact));
        assert_eq!(match_tier("dune", "dune notes"), Some(MatchTier::Prefix));
        assert_eq!(
            match_tier("dune", "the-dune-notes.md"),
            Some(MatchTier::WordPrefix)
        );
        assert_eq!(match_tier("dune", "redundant"), None);
        assert_eq!(match_tier("dun", "redundant"), Some(MatchTier::Contains));
        assert_eq!(match_tier("", "anything"), None);
        // A later word-start occurrence wins over an earlier infix one.
        assert_eq!(
            match_tier("dun", "redundant dune"),
            Some(MatchTier::WordPrefix)
        );
    }

    #[test]
    fn relevance_prefers_a_title_hit_over_a_subtitle_hit() {
        let title = SearchResult::new("Apps", "Calculator", "a");
        let subtitle =
            SearchResult::new("Files", "Draft", "f").with_subtitle("Quarterly calculator notes");
        assert!(result_relevance("calculator", &title) > result_relevance("calculator", &subtitle));
        assert!(result_relevance("calculator", &subtitle) > 0);
        assert_eq!(result_relevance("", &title), 0);
        assert_eq!(
            result_relevance("CRÈME", &SearchResult::new("Notes", "Crème brûlée", "n")),
            result_relevance("creme", &SearchResult::new("Notes", "Crème brûlée", "n"))
        );
    }
}
