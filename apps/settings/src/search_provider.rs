//! Settings panes as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: Settings declares its
//! searchable type by implementing [`SearchProvider`] over the static list of
//! [`Category`] panes, so a settings screen is findable from the unified Ctrl+K
//! palette by its title, summary, or any of its built-in keyword synonyms
//! ("vpn", "night light", "wallpaper", …). Activating a row navigates straight
//! to that pane.
//!
//! This indexes **panes** (navigation), not individual toggles — those are
//! already exported to Ctrl+K as *actions* through the app's
//! [`ResourceProvider`](qos_actions::ResourceProvider); giving pane results a
//! distinct `Settings` kind keeps the two groups apart in the palette.

use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult};

use crate::categories::Category;

/// The kind label (and result-group header) Settings produces.
pub const SETTINGS_KIND: &str = "Settings";

/// The `action_id` prefix. A chosen row's `action_id` is
/// `settings.pane:<slug>`; [`parse_action_id`] turns it back into a
/// [`Category`].
const ACTION_PREFIX: &str = "settings.pane:";

/// A [`SearchProvider`] over the settings panes. Stateless — the pane set is
/// static — so it holds nothing.
pub struct SettingsSearchProvider;

impl SettingsSearchProvider {
    /// The `action_id` for a pane — what a chosen row hands back.
    pub fn action_id(category: Category) -> String {
        format!("{ACTION_PREFIX}{}", category.slug())
    }

    /// Resolve an `action_id` produced by this provider back into a
    /// [`Category`]. Returns `None` for an id this provider did not emit.
    pub fn parse_action_id(action_id: &str) -> Option<Category> {
        let slug = action_id.strip_prefix(ACTION_PREFIX)?;
        Category::ALL.iter().copied().find(|c| c.slug() == slug)
    }
}

impl SearchProvider for SettingsSearchProvider {
    fn name(&self) -> &str {
        SETTINGS_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        // `Category::matches` tests label + subtitle + the pane's keyword
        // synonyms, and returns true for every pane on an empty query — so a
        // blank query browses all of Settings.
        Category::ALL
            .iter()
            .copied()
            .filter(|c| c.matches(query))
            .map(|c| {
                SearchResult::new(SETTINGS_KIND, c.label(), Self::action_id(c))
                    .with_subtitle(c.subtitle())
                    .with_icon(Icon::Settings)
            })
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
            hits.iter()
                .any(|r| SettingsSearchProvider::parse_action_id(&r.action_id)
                    == Some(Category::Displays)),
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
    fn action_id_round_trips_to_the_category() {
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(SettingsSearchProvider));
        let hit = registry
            .query("appearance")
            .into_iter()
            .find(|r| {
                SettingsSearchProvider::parse_action_id(&r.action_id) == Some(Category::Appearance)
            })
            .expect("Appearance is findable");
        assert_eq!(
            SettingsSearchProvider::parse_action_id(&hit.action_id),
            Some(Category::Appearance)
        );
    }

    #[test]
    fn parse_action_id_rejects_foreign_ids() {
        assert!(SettingsSearchProvider::parse_action_id("file:/etc/passwd").is_none());
        assert!(SettingsSearchProvider::parse_action_id("settings.pane:not-a-pane").is_none());
    }

    #[test]
    fn widget_hooks_find_then_navigate_to_the_pane() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::settings_app::SettingsApp::new();
        assert_ne!(
            app.selected(),
            Category::Displays,
            "not already on Displays"
        );

        let providers = app.search_providers();
        assert!(
            !providers.is_empty(),
            "settings publishes a search provider"
        );
        let hit = providers[0]
            .search("night light")
            .into_iter()
            .find(|r| {
                SettingsSearchProvider::parse_action_id(&r.action_id) == Some(Category::Displays)
            })
            .expect("the Displays pane is findable through the real hook");

        let mut doc = AutoCommit::new();
        app.activate_search_result(&hit.action_id, &mut doc, &ROOT);
        assert_eq!(
            app.selected(),
            Category::Displays,
            "activation navigated to the pane"
        );
    }
}
