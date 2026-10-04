//! Settings panes as a Tilo global-search source.
//!
//! Mirrors the reference wiring in `apps/contacts`: Settings declares its
//! searchable type by implementing `SearchProvider` over the static list of
//! [`Category`] panes, so a settings screen is findable from the unified Ctrl+K
//! palette by its title, summary, or any of its built-in keyword synonyms
//! ("vpn", "night light", "wallpaper", …). Each result addresses the pane's
//! stable exported `open` View action, so search, agents, accessibility and
//! resource navigation all use the same brokered operation.
//!
//! This indexes **panes** (navigation), not individual toggles — those are
//! already exported to Ctrl+K as *actions* through the app's
//! [`PublishesResources`](ui::resources::PublishesResources); giving pane results a
//! distinct `Settings` kind keeps the two groups apart in the palette.

use tilo_actions::{ActionAddress, Resource};
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};

use crate::categories::Category;

/// The kind label (and result-group header) Settings produces.
pub const SETTINGS_KIND: &str = "Personal Settings";

/// A `SearchProvider` over the complete Settings destination corpus.
///
/// The category list is small today, but it is still a durable namespace: an
/// old continuation must not silently skip a category when a future build
/// adds one.  Keep it in the toolkit's incremental catalogue rather than
/// rebuilding/filtering a `Vec` on each keystroke.  That gives the palette a
/// bounded cursor protocol now and keeps the implementation correct when the
/// category corpus grows or gains dynamically discovered aliases.
#[derive(Clone, Debug)]
pub struct SettingsSearchProvider {
    catalog: SearchCatalog,
}

impl Default for SettingsSearchProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsSearchProvider {
    #[must_use]
    pub fn new() -> Self {
        let catalog = SearchCatalog::new(SETTINGS_KIND);
        for category in Category::ALL.iter().copied() {
            // The index contains every phrase people can use to find the
            // destination.  The materialised hit stays intentionally lean;
            // it resolves through the canonical `open` action below.
            let label = category.label();
            let subtitle = category.subtitle();
            let searchable = [label.as_str(), subtitle.as_str()]
                .into_iter()
                .chain(category.keywords().iter().copied())
                .collect::<Vec<_>>()
                .join(" ");
            let result = SearchResult::for_action(
                SETTINGS_KIND,
                &label,
                crate::actions::open_pane_address(category),
            )
            .with_subtitle(&subtitle)
            .with_icon(category.icon());
            // Every input above is static, and the result uses precisely the
            // same stable resource id, so this can only fail if a future
            // edit violates the catalogue's canonical-address contract.
            debug_assert!(
                catalog
                    .upsert(
                        crate::actions::pane_resource_id(category),
                        searchable,
                        result
                    )
                    .is_ok(),
                "settings search records must use their own canonical resource id"
            );
        }
        Self { catalog }
    }

    /// Resolve a static Settings navigation resource without building the
    /// full, current resource tree.  Dynamic setting values remain in the
    /// ordinary tree, while a search hit only needs its canonical navigation
    /// door.  Unknown categories/actions are deliberately refused rather than
    /// falling back to a legacy navigation token.
    fn resolve_navigation(address: &ActionAddress) -> Option<Resource> {
        let category = Category::ALL
            .iter()
            .copied()
            .find(|category| crate::actions::pane_resource_id(*category) == address.resource)?;
        (address.action == crate::actions::OPEN_PANE_ACTION)
            .then(|| crate::actions::pane_navigation_resource(category))
    }
}

impl CatalogSearchProvider for SettingsSearchProvider {
    fn provider_name(&self) -> &str {
        SETTINGS_KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }

    fn resolve_action_address(&self, address: &ActionAddress) -> Option<Resource> {
        Self::resolve_navigation(address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::{SearchProvider, SearchRegistry};

    /// Keyword synonyms, browsing and budgeted paging over one provider.
    #[test]
    fn pane_search_finds_keywords_browses_and_pages_every_pane() {
        let provider = SettingsSearchProvider::new();
        // "night light" is a Displays keyword synonym, not in the label.
        let hits = provider.search("night light");
        assert!(
            hits.iter().any(|r| r.action_address()
                == Some(crate::actions::open_pane_address(Category::Displays))),
            "a keyword synonym surfaces its pane, got {:?}",
            hits.iter().map(|r| &r.title).collect::<Vec<_>>()
        );
        assert!(hits.iter().all(|r| r.kind == SETTINGS_KIND));

        // The empty query browses every pane.
        assert_eq!(provider.search("").len(), Category::ALL.len());

        // Paging reaches the complete corpus with a hard per-page budget.
        let mut cursor = None;
        let mut titles = Vec::new();
        loop {
            let page =
                provider.search_page(&ui::search::SearchQuery::new(""), cursor.as_ref(), 1, 1);
            assert!(page.inspected <= 1, "a page must respect its work budget");
            assert!(
                page.results.len() <= 1,
                "a page must respect its result limit"
            );
            titles.extend(page.results.into_iter().map(|result| result.title));
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(
            titles,
            Category::ALL
                .iter()
                .map(|category| category.label())
                .collect::<Vec<_>>(),
            "a continuation reaches late categories instead of truncating browse discovery"
        );
    }

    #[test]
    fn result_addresses_the_exported_open_action() {
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(SettingsSearchProvider::new()));
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
        let config = crate::prefs::test_env::ScratchConfig::new();

        let mut app = config.app();
        let providers = ui::search::capability::search_providers(&app);
        assert!(
            !providers.is_empty(),
            "settings publishes a search provider"
        );
        let mut doc = AutoCommit::new();
        let resources = ui::resources::resources(&app, &doc);
        let hits = providers[0].search("");
        assert_eq!(hits.len(), Category::ALL.len());

        for (category, hit) in Category::ALL.iter().copied().zip(hits) {
            let address = hit.action_address().expect("canonical action address");
            assert_eq!(address, crate::actions::open_pane_address(category));
            let spec = tilo_actions::door::resolve(&resources, &address)
                .expect("the search address is exported by Settings");
            assert_eq!(spec.kind, tilo_actions::OperationKind::View);
            assert_eq!(spec.contract, tilo_actions::ActionContract::view());
            let kind = spec.kind;
            let encoded = tilo_actions::door::encode(&resources, &address, None)
                .expect("the exported operation encodes");
            assert!(matches!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        kind,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .expect("the brokered operation succeeds"),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert_eq!(
                app.selected(),
                category,
                "the broker selected the pane named by the result"
            );
        }
    }

    #[test]
    fn runtime_lazy_resolution_only_accepts_current_canonical_open_addresses() {
        use automerge::AutoCommit;
        let config = crate::prefs::test_env::ScratchConfig::new();

        let app = config.app();
        let doc = AutoCommit::new();
        let address = crate::actions::open_pane_address(Category::Accessibility);
        let resource = ui::resources::resource(&app, &doc, &address.resource)
            .expect("the widget resolves a current canonical resource at activation time");
        assert_eq!(resource.id.as_str(), address.resource);
        let resources = [resource];
        let resolved = tilo_actions::door::resolve(&resources, &address)
            .expect("resolved resource exports the exact open action");
        assert_eq!(resolved.kind, tilo_actions::OperationKind::View);

        let stale_resource = ActionAddress::new("settings.section:removed-pane", "open");
        let stale_action = ActionAddress::new(
            crate::actions::pane_resource_id(Category::Accessibility),
            "removed-action",
        );
        assert!(ui::resources::resource(&app, &doc, &stale_resource.resource).is_none());
        let current_resource = ui::resources::resource(&app, &doc, &stale_action.resource)
            .expect("the current resource remains resolvable");
        assert!(tilo_actions::door::resolve(&[current_resource], &stale_action).is_none());
    }
}
