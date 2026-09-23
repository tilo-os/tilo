//! Saved cities as a Tilo global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: Weather declares its
//! searchable type by implementing [`SearchProvider`] over a snapshot of the
//! user's **saved** cities, so a saved location is findable from the unified
//! Ctrl+K palette by name, region, or country. Every row addresses the saved
//! location resource's exported `open` View action, so search, accessibility
//! and agents all use the same brokered route to its forecast.
//!
//! The provider is deliberately **library-scoped and synchronous**: it indexes
//! only the cities already saved in memory (`WeatherModel::locations`), never
//! the geocoder — a Ctrl+K query stays instant while the "add a city" search
//! (which does real geocoding) lives in the app's own add-city field, off the
//! query path. [`SearchCatalog`] owns the complete durable namespace and
//! exposes bounded continuation pages, so a location after the first 100,000
//! rows remains discoverable without turning a broad query into one long UI
//! frame.

use ui::icons::Icon;
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};

use crate::model::SavedLocation;

/// The kind label (and result-group header) Weather produces.
pub const CITY_KIND: &str = "Cities";

/// A complete, bounded-query catalogue of saved-location resources.
///
/// This stores no visual-list prefix: every current saved city is indexed by
/// its canonical resource id. Cloning the provider for the palette is cheap
/// because [`SearchCatalog`] is an `Arc` handle internally.
#[derive(Clone)]
pub struct CitySearchProvider {
    catalog: SearchCatalog,
}

impl CitySearchProvider {
    /// Build a catalogue for every currently saved city.
    pub fn new(cities: &[SavedLocation]) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new(CITY_KIND),
        };
        for city in cities {
            provider.index_city(city);
        }
        provider
    }

    fn index_city(&self, city: &SavedLocation) {
        let mut result = SearchResult::for_action(
            CITY_KIND,
            city.name.clone(),
            crate::actions::open_location_address(city.id),
        )
        .with_icon(Icon::Sun);
        let subtitle = city.subtitle();
        if !subtitle.is_empty() {
            result = result.with_subtitle(subtitle);
        }
        let Some(address) = result.action_address() else {
            return;
        };
        // `for_action` above is deliberately coupled to this key. The shared
        // catalogue verifies that every searchable record addresses its own
        // durable resource, rather than silently admitting an opaque target.
        let _ = self.catalog.upsert(
            address.resource,
            format!(
                "{}\0{}\0{}\0{}",
                city.name, city.region, city.country, city.country_code
            ),
            result,
        );
    }

    #[cfg(test)]
    fn stats(&self) -> ui::search::SearchCatalogStats {
        self.catalog.stats()
    }
}

impl CatalogSearchProvider for CitySearchProvider {
    fn provider_name(&self) -> &str {
        CITY_KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::{SearchProvider, SearchRegistry};

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
    fn catalog_preserves_unicode_casefolded_substrings() {
        let city = SavedLocation::new(
            "München",
            "Bayern",
            "Deutschland",
            "DE",
            48.1372,
            11.5756,
            "Europe/Berlin",
        );
        let page = CitySearchProvider::new(&[city]).search_page("CHEN", None, 8, 8);
        assert_eq!(page.results.len(), 1);
        assert_eq!(page.results[0].title, "München");
        assert!(page.inspected <= 8);
    }

    #[test]
    fn unique_late_city_remains_findable_beyond_one_hundred_thousand_rows() {
        let template = saved().remove(0);
        let mut cities = (0..100_001u128)
            .map(|index| {
                let mut city = template.clone();
                city.name = format!("City {index}");
                city.id = uuid::Uuid::from_u128(index + 1);
                city
            })
            .collect::<Vec<_>>();
        cities.last_mut().expect("large corpus is nonempty").name = "Late city needle".into();
        let provider = CitySearchProvider::new(&cities);
        let page = provider.search_page("city needle", None, 4, 4);
        assert_eq!(page.inspected, 1, "rare posting avoids a corpus scan");
        assert_eq!(page.results.len(), 1);
        assert_eq!(page.results[0].title, "Late city needle");
        assert_eq!(provider.stats().records, 100_001);
    }

    #[test]
    fn paged_search_bounds_work_and_materialisation_without_losing_broad_hits() {
        let template = saved().remove(0);
        let cities = (0..257u128)
            .map(|index| {
                let mut city = template.clone();
                city.id = uuid::Uuid::from_u128(index + 1);
                city.name = format!("Common City {index}");
                city
            })
            .collect::<Vec<_>>();
        let provider = CitySearchProvider::new(&cities);
        let mut cursor = None;
        let mut seen = 0;
        loop {
            let page = provider.search_page("common", cursor.as_ref(), 17, 5);
            assert!(page.inspected <= 17, "query work must be bounded");
            assert!(page.results.len() <= 5, "result materialisation is paged");
            seen += page.results.len();
            let Some(next) = page.next else {
                break;
            };
            cursor = Some(next);
        }
        assert_eq!(seen, 257);
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
        let spec = tilo_actions::door::resolve(&resources, &address)
            .expect("search address resolves against the live resource tree");
        let encoded = match &spec.input {
            tilo_actions::ActionInput::Immediate(encoded) => encoded,
            tilo_actions::ActionInput::Prompted { .. } => panic!("open takes no argument"),
        };
        assert_eq!(spec.kind, tilo_actions::OperationKind::View);
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

    #[test]
    fn stale_search_target_is_refused_by_the_current_canonical_resource_tree() {
        use automerge::{AutoCommit, ROOT};
        use tilo_actions::Action;
        use ui::widget::Widget;

        let mut app = crate::app::WeatherApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let city = crate::doc::WeatherDoc::read_model(&doc).locations[0].clone();
        let provider = CitySearchProvider::new(std::slice::from_ref(&city));
        let address = provider
            .search(&city.name)
            .into_iter()
            .next()
            .and_then(|hit| hit.action_address())
            .expect("saved city produces a canonical open address");

        // A palette result is only a reference. Once another collaborator (or
        // this user in another surface) removes the resource, the live tree
        // does not resolve it and the local operation refuses the stale id.
        crate::actions::WeatherAction::RemoveLocation {
            id: city.id.to_string(),
        }
        .apply(&mut doc)
        .unwrap();
        doc.commit();
        app.sync(&mut doc, &ROOT);
        let resources = Widget::resources(&app, &doc);
        assert!(tilo_actions::door::resolve(&resources, &address).is_none());
        let payload = serde_json::to_vec(&city.id.to_string()).unwrap();
        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                tilo_actions::OperationKind::View,
                crate::actions::OPEN_LOCATION_OPERATION,
                &payload,
                &doc,
                &ROOT,
            )
            .is_err()
        );
    }
}
