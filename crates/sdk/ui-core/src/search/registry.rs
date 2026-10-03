//! The [`SearchRegistry`] — apps populate it with [`SearchProvider`]s; it
//! queries them all, merges the hits, and ranks them on a shared footing so the
//! best matches surface regardless of which provider produced them.

use super::provider::{SearchProvider, SearchResult};
use super::rank::result_relevance;
use std::collections::HashSet;

/// Weight applied to a provider's own [`SearchResult::score`] bias relative
/// to the registry's query-match bonus: high enough that a pinned result
/// leads for a weak query, low enough that an exact title match still wins.
const PROVIDER_BIAS_WEIGHT: i32 = 4;
const MAX_PROVIDER_MERGE_RESULTS: usize = 256;

/// A ranked bucket of results that share a [`SearchResult::kind`]. The results
/// overlay renders one section per group, headed by `kind`.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchGroup {
    pub kind: String,
    pub results: Vec<SearchResult>,
}

/// A [`SearchProvider`] built from a closure plus its kind label — the
/// convenient way to register a source without declaring a struct. Created via
/// [`SearchRegistry::register_fn`].
pub struct FnProvider<F> {
    name: String,
    f: F,
}

impl<F> FnProvider<F> {
    pub fn new(name: impl Into<String>, f: F) -> Self {
        Self {
            name: name.into(),
            f,
        }
    }
}

impl<F> SearchProvider for FnProvider<F>
where
    F: Fn(&str) -> Vec<SearchResult>,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn search(&self, query: &str) -> Vec<SearchResult> {
        (self.f)(query)
    }
}

/// The set of providers a search surface draws from. One registry can back a
/// single app (its own providers) or, in the shell, every app's providers at
/// once for a global Ctrl+K.
#[derive(Default)]
pub struct SearchRegistry {
    providers: Vec<Box<dyn SearchProvider>>,
}

impl SearchRegistry {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    pub fn register(&mut self, provider: Box<dyn SearchProvider>) -> &mut Self {
        self.providers.push(provider);
        self
    }

    pub fn register_fn<F>(&mut self, name: impl Into<String>, f: F) -> &mut Self
    where
        F: Fn(&str) -> Vec<SearchResult> + 'static,
    {
        self.providers.push(Box::new(FnProvider::new(name, f)));
        self
    }

    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    /// In registration order.
    pub fn provider_names(&self) -> Vec<&str> {
        self.providers.iter().map(|p| p.name()).collect()
    }

    /// The provider's bias plus the shared ranker's query relevance.
    fn rank_key(query: &str, r: &SearchResult) -> i32 {
        r.score
            .saturating_mul(PROVIDER_BIAS_WEIGHT)
            .saturating_add(result_relevance(query, r))
    }

    fn dedupe_key(result: &SearchResult) -> String {
        result.action_address().map_or_else(
            || format!("legacy\0{}\0{}", result.kind, result.action_id),
            |address| format!("action\0{}", address.encode()),
        )
    }

    fn rank_and_dedupe(
        query: &str,
        mut results: Vec<SearchResult>,
        limit: usize,
    ) -> Vec<SearchResult> {
        results.sort_by_cached_key(|result| std::cmp::Reverse(Self::rank_key(query, result)));
        let mut seen = HashSet::new();
        results.retain(|result| seen.insert(Self::dedupe_key(result)));
        results.truncate(limit);
        results
    }

    /// Sorted by descending `rank_key`; ties keep provider
    /// registration order (a stable sort), so ranking is deterministic.
    pub fn query(&self, query: &str) -> Vec<SearchResult> {
        let mut all: Vec<SearchResult> = Vec::new();
        for provider in &self.providers {
            all.extend(provider.search(query));
        }
        Self::rank_and_dedupe(query, all, usize::MAX)
    }

    /// Bounded cross-provider merge for interactive global search. Providers
    /// stop materialising once their share is full, canonical action aliases
    /// collapse to one row, and only the best `limit` hits survive.
    pub fn query_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        if limit == 0 {
            return Vec::new();
        }
        let provider_limit = limit
            .saturating_mul(4)
            .min(MAX_PROVIDER_MERGE_RESULTS)
            .max(limit.min(MAX_PROVIDER_MERGE_RESULTS));
        let mut all = Vec::new();
        for provider in &self.providers {
            all.extend(provider.search_limited(query, provider_limit));
        }
        Self::rank_and_dedupe(query, all, limit)
    }

    /// Groups appear in the order their **best** result ranks; within a
    /// group, results stay in ranked order.
    pub fn query_grouped(&self, query: &str) -> Vec<SearchGroup> {
        let ranked = self.query(query);
        let mut groups: Vec<SearchGroup> = Vec::new();
        for r in ranked {
            if let Some(g) = groups.iter_mut().find(|g| g.kind == r.kind) {
                g.results.push(r);
            } else {
                groups.push(SearchGroup {
                    kind: r.kind.clone(),
                    results: vec![r],
                });
            }
        }
        groups
    }

    /// Caps each group to `per_group_cap` results, so no single busy provider
    /// floods the overlay. `0` means uncapped.
    pub fn query_grouped_capped(&self, query: &str, per_group_cap: usize) -> Vec<SearchGroup> {
        let merge_limit = if per_group_cap == 0 {
            usize::MAX
        } else {
            per_group_cap
                .saturating_mul(self.providers.len().max(1))
                .saturating_mul(4)
        };
        let ranked = if merge_limit == usize::MAX {
            self.query(query)
        } else {
            self.query_limited(query, merge_limit)
        };
        let mut groups: Vec<SearchGroup> = Vec::new();
        for result in ranked {
            if let Some(group) = groups.iter_mut().find(|group| group.kind == result.kind) {
                group.results.push(result);
            } else {
                groups.push(SearchGroup {
                    kind: result.kind.clone(),
                    results: vec![result],
                });
            }
        }
        if per_group_cap > 0 {
            for g in &mut groups {
                g.results.truncate(per_group_cap);
            }
        }
        groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(kind: &str, title: &str, id: &str) -> SearchResult {
        SearchResult::new(kind, title, id)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn the_registry_merges_ranks_and_budgets_provider_results() {
        // phase: empty_registry_returns_nothing
        {
            let reg = SearchRegistry::new();
            assert!(reg.query("anything").is_empty());
            assert_eq!(reg.provider_count(), 0);
        }
        // phase: merges_results_from_multiple_providers
        {
            let mut reg = SearchRegistry::new();
            reg.register_fn("Contacts", |_q| vec![r("Contacts", "Alice", "c:1")]);
            reg.register_fn("Files", |_q| vec![r("Files", "alice.txt", "f:1")]);
            assert_eq!(reg.provider_count(), 2);
            let out = reg.query("alice");
            assert_eq!(out.len(), 2, "both providers contribute");
            let titles: Vec<&str> = out.iter().map(|r| r.title.as_str()).collect();
            assert!(titles.contains(&"Alice"));
            assert!(titles.contains(&"alice.txt"));
        }
        // phase: ranks_stronger_title_match_first_across_providers
        {
            let mut reg = SearchRegistry::new();
            // A weak match from the first-registered provider, an exact-prefix match from the second.
            reg.register_fn("Files", |_q| vec![r("Files", "critical-log.txt", "f:1")]);
            reg.register_fn("Apps", |_q| vec![r("Apps", "Calculator", "a:1")]);
            let out = reg.query("cal");
            assert_eq!(
                out[0].title, "Calculator",
                "prefix match outranks scattered, across providers"
            );
        }
        // phase: provider_bias_floats_pinned_results
        {
            let mut reg = SearchRegistry::new();
            reg.register_fn("Contacts", |_q| {
                vec![
                    r("Contacts", "Zed Zimmer", "c:z").with_score(10), // pinned/favourite
                    r("Contacts", "Zed Zephyr", "c:y"),                // same query strength
                ]
            });
            let out = reg.query("zed");
            assert_eq!(out[0].action_id, "c:z", "the biased result leads");
        }
        // phase: exact_title_beats_provider_bias_and_subtitle_only_matches
        {
            let mut reg = SearchRegistry::new();
            reg.register_fn("Files", |_q| {
                vec![
                    r("Files", "Draft", "f:1")
                        .with_subtitle("Quarterly calculator notes")
                        .with_score(20),
                    r("Apps", "Calculator", "a:1"),
                ]
            });
            assert_eq!(reg.query("calculator")[0].title, "Calculator");
        }
        // phase: canonical_action_aliases_collapse_across_providers
        {
            let address = tilo_actions::ActionAddress::new("notes.note:1", "open");
            let mut reg = SearchRegistry::new();
            let first = SearchResult::for_action("Notes", "Launch brief", &address);
            let second = SearchResult::for_action("Recent", "Launch brief", &address);
            reg.register_fn("Notes", move |_q| vec![first.clone()]);
            reg.register_fn("Recent", move |_q| vec![second.clone()]);
            let results = reg.query("launch");
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].action_address(), Some(address));
        }
        // phase: limited_query_pushes_a_materialisation_budget_into_each_provider
        {
            use std::cell::Cell;
            use std::rc::Rc;

            struct Bounded {
                observed: Rc<Cell<usize>>,
            }
            impl SearchProvider for Bounded {
                fn name(&self) -> &'static str {
                    "Bounded"
                }
                fn search(&self, _query: &str) -> Vec<SearchResult> {
                    panic!("interactive bounded merge must not call unbounded search")
                }
                fn search_limited(&self, _query: &str, limit: usize) -> Vec<SearchResult> {
                    self.observed.set(limit);
                    (0..limit)
                        .map(|index| r("Rows", &format!("row {index}"), &format!("row:{index}")))
                        .collect()
                }
            }

            let observed = Rc::new(Cell::new(0));
            let mut reg = SearchRegistry::new();
            reg.register(Box::new(Bounded {
                observed: observed.clone(),
            }));
            assert_eq!(reg.query_limited("row", 3).len(), 3);
            assert_eq!(observed.get(), 12);
        }
        // phase: struct_provider_registers_and_queries
        {
            struct Fixed;
            impl SearchProvider for Fixed {
                fn name(&self) -> &'static str {
                    "Fixed"
                }
                fn search(&self, _q: &str) -> Vec<SearchResult> {
                    vec![r("Fixed", "one", "1")]
                }
            }
            let mut reg = SearchRegistry::new();
            reg.register(Box::new(Fixed));
            assert_eq!(reg.provider_names(), vec!["Fixed"]);
            assert_eq!(reg.query("o")[0].title, "one");
        }
    }

    #[test]
    fn grouping_orders_groups_by_best_result_and_caps_floods() {
        // phase: grouping_orders_groups_by_best_result
        {
            let mut reg = SearchRegistry::new();
            // Files only weakly matches ("cal" scattered through the name); Apps
            // has the strong prefix hit.
            reg.register_fn("Files", |_q| vec![r("Files", "technical-notes.md", "f:1")]);
            reg.register_fn("Apps", |_q| vec![r("Apps", "Calculator", "a:1")]);
            let groups = reg.query_grouped("cal");
            assert_eq!(groups.len(), 2);
            assert_eq!(groups[0].kind, "Apps", "the group with the best hit leads");
            assert_eq!(groups[1].kind, "Files");
        }
        // phase: grouping_keeps_ranked_order_within_a_group
        {
            let mut reg = SearchRegistry::new();
            reg.register_fn("Apps", |_q| {
                vec![
                    r("Apps", "Terminal Calculator", "a:2"), // scattered
                    r("Apps", "Calculator", "a:1"),          // prefix
                ]
            });
            let groups = reg.query_grouped("cal");
            assert_eq!(groups.len(), 1);
            assert_eq!(
                groups[0].results[0].title, "Calculator",
                "best-in-group first"
            );
            assert_eq!(groups[0].results[1].title, "Terminal Calculator");
        }
        // phase: per_group_cap_limits_flood
        {
            let mut reg = SearchRegistry::new();
            reg.register_fn("Files", |_q| {
                (0..20)
                    .map(|i| r("Files", &format!("file{i}.txt"), &format!("f:{i}")))
                    .collect()
            });
            let groups = reg.query_grouped_capped("file", 5);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].results.len(), 5, "group capped to 5");
        }
    }
}
