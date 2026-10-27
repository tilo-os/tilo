//! Ranking and grouping the palette's search hits.
//!
//! Every provider's rows compete for the same overlay, so they are scored by
//! one shared comparator and bucketed by kind before the palette renders
//! them. Kept apart from the palette's dispatch flow because relevance is a
//! self-contained order over [`SearchResult`]s, not a step in resolving an
//! action.

use super::PER_GROUP_CAP;
use crate::search::{SearchGroup, SearchResult, result_relevance};

/// Rank `rows` by fit against `query` and bucket by kind. Uses the shared
/// [`result_relevance`] ranker so a hit from one source is comparable to a
/// hit from another. Caps each group so no single source floods the overlay.
pub(super) fn rank_and_group(query: &str, rows: Vec<SearchResult>) -> Vec<SearchGroup> {
    /// A provider bias notch is worth this many query-match points.
    const BIAS_WEIGHT: i32 = 4;
    let key = |r: &SearchResult| -> i32 {
        r.score
            .saturating_mul(BIAS_WEIGHT)
            .saturating_add(result_relevance(query, r))
    };
    let mut ranked = rows;
    ranked.sort_by_cached_key(|b| std::cmp::Reverse(key(b)));
    let mut groups: Vec<SearchGroup> = Vec::new();
    for r in ranked {
        if let Some(g) = groups.iter_mut().find(|g| g.kind == r.kind) {
            if g.results.len() < PER_GROUP_CAP {
                g.results.push(r);
            }
        } else {
            groups.push(SearchGroup {
                kind: r.kind.clone(),
                results: vec![r],
            });
        }
    }
    groups
}
