//! Settings panes as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: Settings declares its
//! searchable type by implementing [`SearchProvider`] over the static list of
//! [`Category`] panes, so a settings screen is findable from the unified Ctrl+K
//! palette by its title, summary, or any of its built-in keyword synonyms
//! ("vpn", "night light", "wallpaper", …). Each result addresses the pane's
//! stable exported `open` View action, so search, agents, accessibility and
//! resource navigation all use the same brokered operation.
//!
//! This indexes **panes** (navigation), not individual toggles — those are
//! already exported to Ctrl+K as *actions* through the app's
//! [`ResourceProvider`](qos_actions::ResourceProvider); giving pane results a
//! distinct `Settings` kind keeps the two groups apart in the palette.

use ui::icons::Icon;
use ui::search::{DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult};

use crate::categories::Category;

/// The kind label (and result-group header) Settings produces.
pub const SETTINGS_KIND: &str = "Personal Settings";

/// A [`SearchProvider`] over the settings panes. Stateless — the pane set is
/// static — so it holds nothing.
pub struct SettingsSearchProvider;

impl SearchProvider for SettingsSearchProvider {
    fn name(&self) -> &str {
        SETTINGS_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        // `Category::matches` tests label + subtitle + the pane's keyword
        // synonyms, and returns true for every pane on an empty query — so a
        // blank query browses all of Settings.
        Category::ALL
            .iter()
            .copied()
            .filter(|c| c.matches(query))
            .map(|c| {
                SearchResult::for_action(
                    SETTINGS_KIND,
                    c.label(),
                    crate::actions::open_pane_address(c),
                )
                .with_subtitle(c.subtitle())
                .with_icon(Icon::Settings)
            })
            .take(limit.min(DEFAULT_SEARCH_RESULT_LIMIT))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchRegistry;

    #[test]
    fn provider_finds_a_pane_by_keyword() {
        let provider = SettingsSearchProvider;
        // "night light" is a Displays keyword synonym, not in the label.
        let hits = provider.search("night light");
        assert!(
            hits.iter().any(|r| r.action_address()
                == Some(crate::actions::open_pane_address(Category::Displays))),
            "a keyword synonym surfaces its pane, got {:?}",
            hits.iter().map(|r| &r.title).collect::<Vec<_>>()
        );
        assert!(hits.iter().all(|r| r.kind == SETTINGS_KIND));
    }

    #[test]
    fn empty_query_browses_every_pane() {
        let provider = SettingsSearchProvider;
        assert_eq!(provider.search("").len(), Category::ALL.len());
    }

    #[test]
    fn result_addresses_the_exported_open_action() {
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(SettingsSearchProvider));
        let hit = registry
            .query("appearance")
            .into_iter()
            .find(|r| {
                r.action_address() == Some(crate::actions::open_pane_address(Category::Appearance))
            })
            .expect("Appearance is findable");
        assert_eq!(
            hit.action_address(),
            Some(crate::actions::open_pane_address(Category::Appearance))
        );
    }

    #[test]
    fn broker_resolves_every_search_address_and_navigates_to_the_exact_pane() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::settings_app::SettingsApp::new();
        let providers = app.search_providers();
        assert!(
            !providers.is_empty(),
            "settings publishes a search provider"
        );
        let doc = AutoCommit::new();
        let resources = app.resources(&doc);
        let hits = providers[0].search("");
        assert_eq!(hits.len(), Category::ALL.len());

        for (category, hit) in Category::ALL.iter().copied().zip(hits) {
            let address = hit.action_address().expect("canonical action address");
            assert_eq!(address, crate::actions::open_pane_address(category));
            let spec = qos_actions::door::resolve(&resources, &address)
                .expect("the search address is exported by Settings");
            assert_eq!(spec.kind, qos_actions::OperationKind::View);
            assert_eq!(spec.contract, qos_actions::ActionContract::view());
            let kind = spec.kind;
            let encoded = qos_actions::door::encode(&resources, &address, None)
                .expect("the exported operation encodes");
            assert!(
                app.invoke_resource_operation(
                    kind,
                    &encoded.type_id,
                    &encoded.payload,
                    &doc,
                    &ROOT,
                )
                .expect("the brokered operation succeeds")
            );
            assert_eq!(
                app.selected(),
                category,
                "the broker selected the pane named by the result"
            );
        }
    }
}
