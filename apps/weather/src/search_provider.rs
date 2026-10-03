//! Saved cities as a Tilo global-search source.
//!
//! Mirrors the reference wiring in `apps/contacts`: Weather declares its
//! searchable type by implementing `SearchProvider` over a snapshot of the
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
    use ui::search::testing::{LATE_HIT_NEEDLE, assert_late_hit_reachable};
    use ui::search::{SearchProvider, SearchRegistry};

    fn saved() -> Vec<SavedLocation> {
        crate::sample::sample_model().locations
    }

    #[test]
    fn saved_city_search_matches_browses_and_pages() {
        // Case `provider_finds_a_saved_city_by_name`.
        {
            let cities = saved();
            let provider = CitySearchProvider::new(&cities);
            let hits = provider.search("tokyo");
            assert!(
                hits.iter().any(|r| r.title == "Tokyo"),
                "the provider surfaces the saved city"
            );
            assert!(hits.iter().all(|r| r.kind == CITY_KIND));
        }
        // Case `empty_query_browses_every_saved_city`.
        {
            let cities = saved();
            let provider = CitySearchProvider::new(&cities);
            assert_eq!(provider.search("").len(), cities.len());
        }
        // Case `catalog_preserves_unicode_casefolded_substrings`.
        {
            let city = SavedLocation::new(
                "München",
                "Bayern",
                "Deutschland",
                "DE",
                48.1372,
                11.5756,
                "Europe/Berlin",
            );
            let page = CitySearchProvider::new(&[city]).search_page(
                &ui::search::SearchQuery::new("CHEN"),
                None,
                8,
                8,
            );
            assert_eq!(page.results.len(), 1);
            assert_eq!(page.results[0].title, "München");
            assert!(page.inspected <= 8);
        }
        // Case `unique_late_city_remains_findable_among_many_saved_cities`.
        {
            let (provider, hit) = assert_late_hit_reachable(
                |corpus| {
                    let template = saved().remove(0);
                    let cities = corpus
                        .map(|row| {
                            let mut city = template.clone();
                            city.name = row.text;
                            city.id = uuid::Uuid::from_u128(row.index as u128 + 1);
                            city
                        })
                        .collect::<Vec<_>>();
                    CitySearchProvider::new(&cities)
                },
                3_000,
            );
            assert_eq!(hit.title, format!("ordinary record 2999 {LATE_HIT_NEEDLE}"));
            let page =
                provider.search_page(&ui::search::SearchQuery::new(LATE_HIT_NEEDLE), None, 4, 4);
            assert_eq!(page.inspected, 1, "rare posting avoids a corpus scan");
            assert_eq!(provider.stats().records, 3_000);
        }
        // Case `paged_search_bounds_work_and_materialisation_without_losing_broad_hits`.
        {
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
                let page = provider.search_page(
                    &ui::search::SearchQuery::new("common"),
                    cursor.as_ref(),
                    17,
                    5,
                );
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
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn search_hits_open_through_current_canonical_resources() {
        // Case `result_addresses_the_exported_open_action`.
        {
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
        // Case `canonical_search_hit_resolves_and_opens_through_the_broker`.
        {
            use automerge::{AutoCommit, ROOT};
            use ui::widget::Widget;

            let mut app = crate::app::WeatherApp::new();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &ROOT);

            // Find Tokyo (not the default index-0 selection) through the real hook.
            let (address, id) = {
                let providers = ui::search::capability::search_providers(&app);
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

            let resources = ui::resources::resources(&app, &doc);
            let spec = tilo_actions::door::resolve(&resources, &address)
                .expect("search address resolves against the live resource tree");
            let encoded = match &spec.input {
                tilo_actions::ActionInput::Immediate(encoded) => encoded,
                tilo_actions::ActionInput::Prompted { .. }
                | tilo_actions::ActionInput::Deferred { .. } => {
                    panic!("open takes no argument")
                }
            };
            assert_eq!(spec.kind, tilo_actions::OperationKind::View);
            assert!(matches!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        spec.kind,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .expect("brokered open succeeds"),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert_eq!(
                app.selected_city(),
                Some(id),
                "activation opened the chosen city"
            );
        }
        // Case `stale_search_target_is_refused_by_the_current_canonical_resource_tree`.
        {
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
            let resources = ui::resources::resources(&app, &doc);
            assert!(tilo_actions::door::resolve(&resources, &address).is_none());
            let payload = serde_json::to_vec(&city.id.to_string()).unwrap();
            assert!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::View,
                        crate::actions::OPEN_LOCATION_OPERATION,
                        &payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .is_err()
            );
        }
    }
}
