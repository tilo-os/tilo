//! The [`SearchRegistry`] — apps populate it with [`SearchProvider`]s; it
//! queries them all, merges the hits, and ranks them on a shared footing so the
//! best matches surface regardless of which provider produced them.

use super::provider::{SearchProvider, SearchResult, fuzzy_score};

/// Weight applied to a provider's own [`SearchResult::score`] bias relative
/// to the registry's query-match bonus: high enough that a pinned result
/// leads for a weak query, low enough that an exact title match still wins.
const PROVIDER_BIAS_WEIGHT: i32 = 4;

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

    /// The provider's bias plus the query-match bonus against the title
    /// (falling back to the subtitle).
    fn rank_key(query: &str, r: &SearchResult) -> i32 {
        let bonus = fuzzy_score(query, &r.title)
            .or_else(|| r.subtitle.as_deref().and_then(|s| fuzzy_score(query, s)))
            .unwrap_or(0);
        r.score
            .saturating_mul(PROVIDER_BIAS_WEIGHT)
            .saturating_add(bonus)
    }

    /// Sorted by descending [`rank_key`](Self::rank_key); ties keep provider
    /// registration order (a stable sort), so ranking is deterministic.
    pub fn query(&self, query: &str) -> Vec<SearchResult> {
        let mut all: Vec<SearchResult> = Vec::new();
        for provider in &self.providers {
            all.extend(provider.search(query));
        }
        all.sort_by(|a, b| Self::rank_key(query, b).cmp(&Self::rank_key(query, a)));
        all
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
        let mut groups = self.query_grouped(query);
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
    fn empty_registry_returns_nothing() {
        let reg = SearchRegistry::new();
        assert!(reg.query("anything").is_empty());
        assert_eq!(reg.provider_count(), 0);
    }

    #[test]
    fn merges_results_from_multiple_providers() {
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

    #[test]
    fn ranks_stronger_title_match_first_across_providers() {
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

    #[test]
    fn provider_bias_floats_pinned_results() {
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

    #[test]
    fn grouping_orders_groups_by_best_result() {
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

    #[test]
    fn grouping_keeps_ranked_order_within_a_group() {
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

    #[test]
    fn per_group_cap_limits_flood() {
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

    #[test]
    fn struct_provider_registers_and_queries() {
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
