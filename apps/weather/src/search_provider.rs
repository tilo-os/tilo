//! Saved cities as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: Weather declares its
//! searchable type by implementing [`SearchProvider`] over a snapshot of the
//! user's **saved** cities, so a saved location is findable from the unified
//! Ctrl+K palette by name, region, or country. Every row addresses the saved
//! location resource's exported `open` View action, so search, accessibility
//! and agents all use the same brokered route to its forecast.
//!
//! The provider is deliberately **library-scoped and synchronous**: it matches
//! only the cities already saved in memory (`WeatherModel::locations`), never
//! the geocoder — a Ctrl+K query stays instant while the "add a city" search
//! (which does real geocoding) lives in the app's own add-city field, off the
//! query path.

use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_INDEX_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult,
};

use crate::model::SavedLocation;

/// The kind label (and result-group header) Weather produces.
pub const CITY_KIND: &str = "Cities";

/// A [`SearchProvider`] over a snapshot of the saved cities.
pub struct CitySearchProvider {
    cities: Vec<SavedLocation>,
}

impl CitySearchProvider {
    /// Snapshot the current saved cities into a provider.
    pub fn new(cities: &[SavedLocation]) -> Self {
        Self {
            cities: cities
                .iter()
                .take(DEFAULT_SEARCH_INDEX_LIMIT)
                .cloned()
                .collect(),
        }
    }
}

impl SearchProvider for CitySearchProvider {
    fn name(&self) -> &str {
        CITY_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let q = query.to_lowercase();
        self.cities
            .iter()
            .filter(|c| {
                c.name.to_lowercase().contains(&q)
                    || c.region.to_lowercase().contains(&q)
                    || c.country.to_lowercase().contains(&q)
            })
            .map(|c| {
                let mut r = SearchResult::for_action(
                    CITY_KIND,
                    c.name.clone(),
                    crate::actions::open_location_address(c.id),
                )
                .with_icon(Icon::Sun);
                let sub = c.subtitle();
                if !sub.is_empty() {
                    r = r.with_subtitle(sub);
                }
                r
            })
            .take(limit.min(DEFAULT_SEARCH_RESULT_LIMIT))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchRegistry;

    fn saved() -> Vec<SavedLocation> {
        crate::sample::sample_model().locations
    }

    #[test]
    fn provider_finds_a_saved_city_by_name() {
        let cities = saved();
        let provider = CitySearchProvider::new(&cities);
        let hits = provider.search("tokyo");
        assert!(
            hits.iter().any(|r| r.title == "Tokyo"),
            "the provider surfaces the saved city"
        );
        assert!(hits.iter().all(|r| r.kind == CITY_KIND));
    }

    #[test]
    fn empty_query_browses_every_saved_city() {
        let cities = saved();
        let provider = CitySearchProvider::new(&cities);
        assert_eq!(provider.search("").len(), cities.len());
    }

    #[test]
    fn large_city_corpus_bounds_snapshot_and_hit_materialisation() {
        let template = saved().remove(0);
        let cities = (0..(DEFAULT_SEARCH_INDEX_LIMIT + 500))
            .map(|index| {
                let mut city = template.clone();
                city.name = format!("City {index}");
                city
            })
            .collect::<Vec<_>>();
        let provider = CitySearchProvider::new(&cities);
        assert_eq!(provider.cities.len(), DEFAULT_SEARCH_INDEX_LIMIT);
        assert_eq!(provider.search_limited("City", 7).len(), 7);
        assert!(provider.search_limited("City", 0).is_empty());
    }

    #[test]
    fn result_addresses_the_exported_open_action() {
        let cities = saved();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(CitySearchProvider::new(&cities)));
        let some = cities.first().unwrap().clone();
        let hit = registry
            .query(&some.name)
            .into_iter()
            .find(|r| r.title == some.name)
            .expect("the city is findable through the registry");
        assert_eq!(
            hit.action_address(),
            Some(crate::actions::open_location_address(some.id))
        );
    }

    #[test]
    fn canonical_search_hit_resolves_and_opens_through_the_broker() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::WeatherApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        // Find Tokyo (not the default index-0 selection) through the real hook.
        let (address, id) = {
            let providers = app.search_providers();
            assert!(!providers.is_empty(), "weather publishes a search provider");
            let hit = providers[0]
                .search("tokyo")
                .into_iter()
                .next()
                .expect("Tokyo is a saved sample city");
            let address = hit.action_address().expect("canonical action address");
            let id = address
                .resource
                .strip_prefix("weather.location:")
                .and_then(|raw| raw.parse().ok())
                .expect("location resource carries its stable id");
            (address, id)
        };
        assert_ne!(app.selected_city(), Some(id), "not already selected");

        let resources = Widget::resources(&app, &doc);
        let spec = qos_actions::door::resolve(&resources, &address)
            .expect("search address resolves against the live resource tree");
        let encoded = match &spec.input {
            qos_actions::ActionInput::Immediate(encoded) => encoded,
            qos_actions::ActionInput::Prompted { .. } => panic!("open takes no argument"),
        };
        assert_eq!(spec.kind, qos_actions::OperationKind::View);
        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                spec.kind,
                &encoded.type_id,
                &encoded.payload,
                &doc,
                &ROOT,
            )
            .expect("brokered open succeeds")
        );
        assert_eq!(
            app.selected_city(),
            Some(id),
            "activation opened the chosen city"
        );
    }
}
