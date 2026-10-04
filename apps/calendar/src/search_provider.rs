//! Calendar events, tasks, and projects as a retained Tilo global-search source.
//!
//! The catalogue is keyed by each record's canonical resource id. It retains
//! the complete loaded Calendar corpus and delegates query paging to
//! [`SearchCatalog`], so typing never re-walks events and a match after a long
//! run of broad matches remains reachable through a cursor.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};

use tilo_actions::{ActionAddress, Resource};
use ui::icons::Icon;

use crate::actions::{
    CalendarCollectionResource, calendar_resource_id, event_resource_id, project_resource_id,
    task_resource_id,
};
use crate::date;
use crate::model::{Event, Model, Project, Task};

/// The kind label (and result-group header) Calendar produces.
pub const CALENDAR_KIND: &str = "Calendar";

/// A durable, complete catalogue over Calendar's loaded CRDT projection.
///
/// Clones retain the same `SearchCatalog`, which lets `CalendarApp` refresh it
/// on document revisions while palette instances keep inexpensive provider
/// handles. `indexed_ids` records the successfully-published revision so
/// removals and stale result addresses remain observable to diagnostics.
#[derive(Clone)]
pub struct CalendarSearchProvider {
    catalog: SearchCatalog,
    indexed_ids: Arc<RwLock<BTreeSet<String>>>,
    /// The search action directory resolves through this exact, retained map
    /// rather than walking the app's open-resource tree. Each value is the
    /// one-action resource for its canonical result address.
    resources: Arc<RwLock<BTreeMap<String, Resource>>>,
    last_refresh_error: Arc<RwLock<Option<String>>>,
}

struct CatalogRow {
    id: String,
    searchable: String,
    result: SearchResult,
    resource: Resource,
}

impl CalendarSearchProvider {
    /// Create a self-contained catalogue, primarily for small callers and
    /// tests. The live app keeps this instance and calls [`Self::refresh`] when
    /// its CRDT revision changes.
    pub fn new(model: &Model) -> Self {
        Self::new_with_notes(model, &std::collections::HashMap::new())
    }

    pub fn new_with_notes(
        model: &Model,
        notes: &std::collections::HashMap<String, String>,
    ) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new(CALENDAR_KIND),
            indexed_ids: Arc::new(RwLock::new(BTreeSet::new())),
            resources: Arc::new(RwLock::new(BTreeMap::new())),
            last_refresh_error: Arc::new(RwLock::new(None)),
        };
        // Keep construction total for app startup. The error is retained and
        // surfaced by the live app on its next refresh instead of quietly
        // presenting a partial index.
        if let Err(error) = provider.refresh(model, notes) {
            *provider
                .last_refresh_error
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.to_string());
        }
        provider
    }

    fn index_calendar(calendar: &CalendarCollectionResource) -> CatalogRow {
        let result = SearchResult::for_action(
            CALENDAR_KIND,
            &calendar.label,
            crate::actions::open_calendar_address(&calendar.id),
        )
        .with_subtitle(if calendar.visible {
            "Calendar"
        } else {
            "Hidden calendar"
        })
        .with_icon(Icon::Calendar);
        let id = calendar_resource_id(&calendar.id);
        let searchable = format!("{}\0{}", calendar.label, calendar.id);
        CatalogRow {
            id,
            searchable,
            result,
            resource: crate::actions::calendar_collection_resource(calendar),
        }
    }

    /// When the event happens: the one fact a search hit needs to tell two
    /// events with the same title apart.
    fn event_subtitle(event: &Event) -> String {
        if event.all_day {
            date::fmt_day_long(event.start.date())
        } else {
            format!(
                "{} at {}",
                date::fmt_day_long(event.start.date()),
                date::fmt_time(&event.start)
            )
        }
    }

    fn task_title(task: &Task) -> &str {
        let title = task.title.trim();
        if title.is_empty() {
            "Untitled task"
        } else {
            title
        }
    }

    fn task_subtitle(task: &Task) -> String {
        if task.done {
            "Completed task".to_string()
        } else {
            task.due.map_or_else(
                || "Task".to_string(),
                |due| format!("Due {}", date::fmt_day_long(due)),
            )
        }
    }

    fn index_project(project: &Project) -> CatalogRow {
        let result = SearchResult::for_action(
            CALENDAR_KIND,
            &project.name,
            crate::actions::open_project_address(project.id),
        )
        .with_subtitle("Task project")
        .with_icon(Icon::Check);
        CatalogRow {
            id: project_resource_id(project.id),
            searchable: project.name.clone(),
            result,
            resource: crate::actions::search_project_resource(project),
        }
    }

    fn index_event(event: &Event, note: Option<&String>) -> CatalogRow {
        let result = SearchResult::for_action(
            CALENDAR_KIND,
            event.display_title(),
            crate::actions::open_event_address(event.id),
        )
        .with_subtitle(Self::event_subtitle(event))
        .with_icon(Icon::Calendar);
        let attendees = event
            .attendees
            .iter()
            .map(|attendee| {
                format!(
                    "{} {} {}",
                    attendee.name,
                    attendee.email,
                    attendee.status.label()
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        let searchable = format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            event.display_title(),
            event.location,
            event.calendar_id.as_deref().unwrap_or(""),
            event.timezone,
            attendees,
            note.map_or("", String::as_str),
        );
        CatalogRow {
            id: event_resource_id(event.id),
            searchable,
            result,
            resource: crate::actions::search_event_resource(event),
        }
    }

    /// A hidden collection is a visibility boundary, not merely a paint-time
    /// preference. In particular, its event titles, locations, attendees and
    /// notes must not escape through the global search catalogue. Collections
    /// absent from this configuration snapshot remain searchable: they may be
    /// local data imported before an account is configured, and treating an
    /// unknown owner as hidden would make durable records unreachable.
    fn event_visible_to_search(event: &Event, calendars: &[CalendarCollectionResource]) -> bool {
        let owner = event.calendar_id.as_deref().unwrap_or("local");
        calendars
            .iter()
            .find(|calendar| calendar.id == owner)
            .is_none_or(|calendar| calendar.visible)
    }

    fn index_task(task: &Task, note: Option<&String>) -> CatalogRow {
        let result = SearchResult::for_action(
            CALENDAR_KIND,
            Self::task_title(task),
            crate::actions::open_task_address(task.id),
        )
        .with_subtitle(Self::task_subtitle(task))
        .with_icon(Icon::Check);
        let searchable = format!(
            "{}\0{}",
            Self::task_title(task),
            note.map_or("", String::as_str)
        );
        CatalogRow {
            id: task_resource_id(task.id),
            searchable,
            result,
            resource: crate::actions::search_task_resource(task),
        }
    }

    /// Atomically replace the retained snapshot from the complete loaded model.
    /// A successful rebuild publishes one new generation; an invalid row leaves
    /// the old index and its exact action resolver intact.
    ///
    /// # Errors
    ///
    /// Returns a catalogue validation or generation error without publishing a
    /// partial snapshot.
    pub fn refresh(
        &self,
        model: &Model,
        notes: &std::collections::HashMap<String, String>,
    ) -> Result<(), ui::search::SearchCatalogError> {
        self.refresh_with_calendars(model, notes, &[])
    }

    /// Revision callers provide the live account projection too, so local and
    /// remote collections participate in the same durable search namespace as
    /// their events.
    ///
    /// # Errors
    ///
    /// Returns a catalogue validation or generation error without replacing
    /// the previously retained resolver/index revision.
    pub fn refresh_with_calendars(
        &self,
        model: &Model,
        notes: &std::collections::HashMap<String, String>,
        calendars: &[CalendarCollectionResource],
    ) -> Result<(), ui::search::SearchCatalogError> {
        let mut rows = Vec::with_capacity(
            calendars.len() + model.projects.len() + model.events.len() + model.tasks.len(),
        );
        for calendar in calendars {
            rows.push(Self::index_calendar(calendar));
        }
        for project in &model.projects {
            rows.push(Self::index_project(project));
        }
        for event in &model.events {
            if Self::event_visible_to_search(event, calendars) {
                rows.push(Self::index_event(event, notes.get(&event.id.to_string())));
            }
        }
        for task in &model.tasks {
            rows.push(Self::index_task(task, notes.get(&task.id.to_string())));
        }
        // `replace_all` validates every canonical address before publishing a
        // new generation. On error the prior catalogue and resolver map stay
        // intact, so failed indexing can neither leak a partial corpus nor
        // silently leave mismatched result/resource state behind.
        if let Err(error) = self.catalog.replace_all(
            rows.iter()
                .map(|row| (row.id.clone(), row.searchable.clone(), row.result.clone())),
        ) {
            *self
                .last_refresh_error
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.to_string());
            return Err(error);
        }
        let current = rows.iter().map(|row| row.id.clone()).collect();
        let resources = rows.into_iter().map(|row| (row.id, row.resource)).collect();
        *self
            .indexed_ids
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = current;
        *self
            .resources
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = resources;
        *self
            .last_refresh_error
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        Ok(())
    }

    /// The last failed snapshot rebuild, if any. Callers can surface this in
    /// the UI instead of behaving as though every durable record was indexed.
    pub fn last_refresh_error(&self) -> Option<String> {
        self.last_refresh_error
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[cfg(test)]
    fn stats(&self) -> ui::search::SearchCatalogStats {
        self.catalog.stats()
    }
}

impl CatalogSearchProvider for CalendarSearchProvider {
    fn provider_name(&self) -> &str {
        CALENDAR_KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }

    fn resolve_action_address(&self, address: &ActionAddress) -> Option<Resource> {
        if address.action != "open" {
            return None;
        }
        let resource = self
            .resources
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&address.resource)
            .cloned()?;
        resource
            .actions
            .iter()
            .any(|action| action.id == address.action)
            .then_some(resource)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use ui::search::SearchRegistry;
    use ui::search::testing::{LATE_HIT_NEEDLE, assert_late_hit_reachable};
    use ui::search::{DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider};

    fn seeded() -> Model {
        crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap())
    }

    #[test]
    fn provider_finds_events_and_calendars_at_canonical_resource_addresses() {
        // Case `provider_finds_events_by_title_location_and_notes`.
        {
            let model = seeded();
            let mut notes = std::collections::HashMap::new();
            let event = &model.events[0];
            notes.insert(event.id.to_string(), "private launch dossier".into());
            let provider = CalendarSearchProvider::new_with_notes(&model, &notes);
            assert!(
                provider
                    .search("design")
                    .iter()
                    .any(|hit| hit.title == "Design review")
            );
            assert!(
                provider
                    .search("bright smiles")
                    .iter()
                    .any(|hit| hit.title == "Dentist")
            );
            assert!(
                provider
                    .search("launch dossier")
                    .iter()
                    .any(|hit| hit.title == event.display_title())
            );
        }
        // Case `every_hit_has_a_canonical_resource_action`.
        {
            let model = seeded();
            let mut registry = SearchRegistry::new();
            registry.register(Box::new(CalendarSearchProvider::new(&model)));
            let groups = registry.query_grouped("");
            assert_eq!(groups.len(), 1);
            assert_eq!(
                groups[0].results.len(),
                model.projects.len() + model.events.len() + model.tasks.len()
            );
            assert!(
                groups[0]
                    .results
                    .iter()
                    .all(|hit| hit.action_address().is_some())
            );
        }
        // Case `configured_calendars_are_searchable_at_their_canonical_open_addresses`.
        {
            let model = seeded();
            let provider = CalendarSearchProvider::new(&model);
            let calendars = vec![crate::actions::CalendarCollectionResource {
                id: "account/team".into(),
                label: "Work · Team schedule".into(),
                color: Some(0),
                visible: true,
            }];
            provider
                .refresh_with_calendars(&model, &std::collections::HashMap::new(), &calendars)
                .expect("canonical rows rebuild the search catalogue");
            let hit = provider
                .search("team schedule")
                .into_iter()
                .find(|hit| hit.title == "Work · Team schedule")
                .expect("configured collection is searchable");
            assert_eq!(
                hit.action_address(),
                Some(crate::actions::open_calendar_address("account/team"))
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn paging_bounds_work_and_results_while_keeping_exact_stable_addresses() {
        // Case `unique_late_record_is_findable_without_a_corpus_scan`.
        {
            let mut records = 0;
            let (provider, hit) = assert_late_hit_reachable(
                |corpus| {
                    let mut model = seeded();
                    let template = model.events[0].clone();
                    model.events.extend(corpus.map(|row| {
                        let mut event = template.clone();
                        event.id = uuid::Uuid::from_u128(row.index as u128 + 1);
                        event.title = row.text;
                        event
                    }));
                    records = model.projects.len() + model.events.len() + model.tasks.len();
                    CalendarSearchProvider::new(&model)
                },
                3_000,
            );
            assert_eq!(hit.title, format!("ordinary record 2999 {LATE_HIT_NEEDLE}"));
            let page =
                provider.search_page(&ui::search::SearchQuery::new(LATE_HIT_NEEDLE), None, 4, 4);
            assert_eq!(page.inspected, 1);
            let address = hit
                .action_address()
                .expect("a late hit retains its canonical address");
            let resource = provider
                .resolve_action_resource(&address)
                .expect("the retained provider materializes the exact late resource");
            assert_eq!(resource.id.as_str(), address.resource);
            assert!(
                tilo_actions::door::resolve(&[resource], &address).is_some(),
                "the exact open action exists without constructing the resource tree"
            );
            assert_eq!(provider.stats().records, records);
        }
        // Case `pages_bound_work_and_results_without_losing_broad_matches`.
        {
            let mut model = seeded();
            let template = model.events[0].clone();
            model.events = (0..257u128)
                .map(|index| {
                    let mut event = template.clone();
                    event.id = uuid::Uuid::from_u128(index + 1);
                    event.title = format!("common calendar event {index}");
                    event
                })
                .collect();
            model.tasks.clear();
            model.projects.clear();
            let provider = CalendarSearchProvider::new(&model);
            let mut cursor = None;
            let mut seen = BTreeSet::new();
            loop {
                let page = provider.search_page(
                    &ui::search::SearchQuery::new("common"),
                    cursor.as_ref(),
                    17,
                    5,
                );
                assert!(page.inspected <= 17);
                assert!(page.results.len() <= 5);
                seen.extend(page.results.iter().map(|result| result.action_id.clone()));
                let Some(next) = page.next else { break };
                cursor = Some(next);
            }
            assert_eq!(seen.len(), 257);
        }
        // Case `oversized_result_requests_stay_within_the_shared_display_cap`.
        {
            let mut model = seeded();
            let template = model.events[0].clone();
            model.events = (0..(DEFAULT_SEARCH_RESULT_LIMIT as u128 + 32))
                .map(|index| {
                    let mut event = template.clone();
                    event.id = uuid::Uuid::from_u128(index + 1);
                    event.title = format!("common capped event {index}");
                    event
                })
                .collect();
            model.tasks.clear();
            model.projects.clear();
            let provider = CalendarSearchProvider::new(&model);

            assert_eq!(
                provider.search_limited("common", usize::MAX).len(),
                DEFAULT_SEARCH_RESULT_LIMIT
            );
            let page = provider.search_page(
                &ui::search::SearchQuery::new("common"),
                None,
                1_024,
                usize::MAX,
            );
            assert_eq!(page.results.len(), DEFAULT_SEARCH_RESULT_LIMIT);
            assert!(page.inspected <= 1_024);
        }
        // Case `stale_cursor_and_colliding_titles_keep_exact_stable_addresses`.
        {
            let mut model = seeded();
            let template = model.events[0].clone();
            let first_id = uuid::Uuid::from_u128(90_001);
            let second_id = uuid::Uuid::from_u128(90_002);
            for id in [first_id, second_id] {
                let mut event = template.clone();
                event.id = id;
                event.title = "Same visible title".into();
                model.events.push(event);
            }
            let provider = CalendarSearchProvider::new(&model);
            let hits = provider.search_limited("same visible", 8);
            let addresses: Vec<_> = hits
                .iter()
                .filter_map(SearchResult::action_address)
                .filter(|address| {
                    address.resource == event_resource_id(first_id)
                        || address.resource == event_resource_id(second_id)
                })
                .collect();
            assert_eq!(
                addresses.len(),
                2,
                "equal labels cannot collide in identity"
            );
            assert_ne!(addresses[0].resource, addresses[1].resource);
            for address in &addresses {
                let resource = provider
                    .resolve_action_resource(address)
                    .expect("each exact collision-safe address resolves");
                assert_eq!(resource.id.as_str(), address.resource);
                assert!(tilo_actions::door::resolve(&[resource], address).is_some());
            }

            let first_page =
                provider.search_page(&ui::search::SearchQuery::new("same visible"), None, 4, 1);
            let cursor = first_page.next.expect("more collision-safe rows remain");
            model.events[0].title = "revision invalidates cursors".into();
            provider
                .refresh(&model, &std::collections::HashMap::new())
                .expect("a canonical revision rebuilds atomically");
            assert!(
                !provider.action_directory_cursor_is_current(&cursor),
                "a cursor from the prior catalogue generation is stale"
            );
        }
    }

    #[test]
    fn index_withdraws_deleted_and_hidden_calendar_resources() {
        // Case `refresh_replaces_changed_rows_and_withdraws_deleted_resources`.
        {
            let mut model = seeded();
            let provider = CalendarSearchProvider::new(&model);
            let event = model.events[0].id;
            model.events[0].title = "revised calendar event".into();
            provider
                .refresh(&model, &std::collections::HashMap::new())
                .expect("canonical rows rebuild the search catalogue");
            assert!(
                provider
                    .search("revised")
                    .iter()
                    .any(|result| result.action_address()
                        == Some(crate::actions::open_event_address(event)))
            );
            model.events.retain(|candidate| candidate.id != event);
            provider
                .refresh(&model, &std::collections::HashMap::new())
                .expect("canonical rows rebuild the search catalogue");
            assert!(provider.search("revised").is_empty());
        }
        // Case `hidden_calendar_events_and_notes_do_not_enter_global_search`.
        {
            let mut model = seeded();
            let event = &mut model.events[0];
            event.title = "Private board strategy".into();
            event.location = "Secret annex".into();
            event.calendar_id = Some("work/private".into());
            let mut notes = std::collections::HashMap::new();
            notes.insert(event.id.to_string(), "restricted launch plan".into());
            let provider = CalendarSearchProvider::new(&model);
            let calendars = vec![crate::actions::CalendarCollectionResource {
                id: "work/private".into(),
                label: "Private work".into(),
                color: None,
                visible: false,
            }];

            provider
                .refresh_with_calendars(&model, &notes, &calendars)
                .expect("canonical rows rebuild the search catalogue");
            for query in ["board strategy", "secret annex", "restricted launch"] {
                assert!(
                    provider.search(query).is_empty(),
                    "hidden event content escaped through query {query:?}"
                );
            }

            // The collection remains discoverable so the user can re-enable it,
            // but its private event records stay absent.
            assert!(
                provider
                    .search("private work")
                    .iter()
                    .any(|hit| hit.title == "Private work")
            );
        }
    }
}
