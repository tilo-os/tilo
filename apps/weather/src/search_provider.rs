//! Saved cities as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: Weather declares its
//! searchable type by implementing [`SearchProvider`] over a snapshot of the
//! user's **saved** cities, so a saved location is findable from the unified
//! Ctrl+K palette by name, region, or country. Activating a row opens that
//! city's forecast.
//!
//! The provider is deliberately **library-scoped and synchronous**: it matches
//! only the cities already saved in memory (`WeatherModel::locations`), never
//! the geocoder — a Ctrl+K query stays instant while the "add a city" search
//! (which does real geocoding) lives in the app's own add-city field, off the
//! query path.

use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult};

use crate::model::{LocationId, SavedLocation};

/// The kind label (and result-group header) Weather produces.
pub const CITY_KIND: &str = "Cities";

/// The `action_id` prefix. A chosen row's `action_id` is `city:<uuid>`;
/// [`parse_action_id`] turns it back into a [`LocationId`].
const ACTION_PREFIX: &str = "city:";

/// A [`SearchProvider`] over a snapshot of the saved cities.
pub struct CitySearchProvider {
    cities: Vec<SavedLocation>,
}

impl CitySearchProvider {
    /// Snapshot the current saved cities into a provider.
    pub fn new(cities: &[SavedLocation]) -> Self {
        Self {
            cities: cities.to_vec(),
        }
    }

    /// The `action_id` for a saved city — what a chosen row hands back.
    pub fn action_id(id: LocationId) -> String {
        format!("{ACTION_PREFIX}{id}")
    }

    /// Resolve an `action_id` produced by this provider back into a
    /// [`LocationId`]. Returns `None` for an id this provider did not emit.
    pub fn parse_action_id(action_id: &str) -> Option<LocationId> {
        action_id
            .strip_prefix(ACTION_PREFIX)
            .and_then(|rest| rest.parse::<LocationId>().ok())
    }
}

impl SearchProvider for CitySearchProvider {
    fn name(&self) -> &str {
        CITY_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        let q = query.to_lowercase();
        self.cities
            .iter()
            .filter(|c| {
                c.name.to_lowercase().contains(&q)
                    || c.region.to_lowercase().contains(&q)
                    || c.country.to_lowercase().contains(&q)
            })
            .map(|c| {
                let mut r = SearchResult::new(CITY_KIND, c.name.clone(), Self::action_id(c.id))
                    .with_icon(Icon::Sun);
                let sub = c.subtitle();
                if !sub.is_empty() {
                    r = r.with_subtitle(sub);
                }
                r
            })
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
    fn action_id_round_trips_to_a_saved_city() {
        let cities = saved();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(CitySearchProvider::new(&cities)));
        let some = cities.first().unwrap().clone();
        let hit = registry
            .query(&some.name)
            .into_iter()
            .find(|r| r.action_id == CitySearchProvider::action_id(some.id))
            .expect("the city is findable through the registry");
        assert_eq!(
            CitySearchProvider::parse_action_id(&hit.action_id),
            Some(some.id)
        );
    }

    #[test]
    fn parse_action_id_rejects_foreign_ids() {
        assert!(CitySearchProvider::parse_action_id("file:/etc/passwd").is_none());
        assert!(CitySearchProvider::parse_action_id("city:not-a-uuid").is_none());
    }

    #[test]
    fn widget_hooks_find_then_open_the_city() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::WeatherApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        // Find Tokyo (not the default index-0 selection) through the real hook.
        let (action_id, id) = {
            let providers = app.search_providers();
            assert!(!providers.is_empty(), "weather publishes a search provider");
            let hit = providers[0]
                .search("tokyo")
                .into_iter()
                .next()
                .expect("Tokyo is a saved sample city");
            let id = CitySearchProvider::parse_action_id(&hit.action_id).unwrap();
            (hit.action_id, id)
        };
        assert_ne!(app.selected_city(), Some(id), "not already selected");

        app.activate_search_result(&action_id, &mut doc, &ROOT);
        assert_eq!(
            app.selected_city(),
            Some(id),
            "activation opened the chosen city"
        );
    }
}
