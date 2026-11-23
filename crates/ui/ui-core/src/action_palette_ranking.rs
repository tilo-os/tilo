//! Ranking and grouping the palette's search hits. Every provider's rows compete for the same overlay,
//! so they are scored by one shared comparator and bucketed by kind before the palette renders them.

use super::{ACTIONS_KIND, PER_GROUP_CAP};
use crate::search::{SearchGroup, SearchResult, result_relevance};

/// Rank `rows` by fit against `query` and bucket by kind.
pub(super) fn rank_and_group(query: &str, rows: Vec<SearchResult>) -> Vec<SearchGroup> {
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
            groups.push(SearchGroup::new(r.kind.clone(), vec![r]));
        }
    }
    let (actions, mut held): (Vec<SearchGroup>, Vec<SearchGroup>) = groups
        .into_iter()
        .partition(|group| group.kind == ACTIONS_KIND);
    held.extend(actions);
    held
}
