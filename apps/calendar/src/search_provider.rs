//! Calendar events, tasks, and projects as a retained qOS global-search source.
//!
//! The catalogue is keyed by each record's canonical resource id. It retains
//! the complete loaded Calendar corpus and delegates query paging to
//! [`SearchCatalog`], so typing never re-walks events and a match after a long
//! run of broad matches remains reachable through a cursor.

use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};

use crate::actions::CalendarCollectionResource;
use crate::date;
use crate::model::{Event, Model, Project, Task};

/// The kind label (and result-group header) Calendar produces.
pub const CALENDAR_KIND: &str = "Calendar";

/// A durable, complete catalogue over Calendar's loaded CRDT projection.
///
/// Clones retain the same `SearchCatalog`, which lets `CalendarApp` refresh it
/// on document revisions while palette instances keep inexpensive provider
/// handles. `indexed_ids` lets Calendar remove records deleted by a peer or an
/// undo without reconstructing the catalogue.
#[derive(Clone)]
pub struct CalendarSearchProvider {
    catalog: SearchCatalog,
    indexed_ids: Arc<RwLock<BTreeSet<String>>>,
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
        };
        provider.refresh(model, notes);
        provider
    }

    fn index_calendar(
        &self,
        calendar: &CalendarCollectionResource,
        current: &mut BTreeSet<String>,
    ) {
        let result = SearchResult::for_action(
            CALENDAR_KIND,
            &calendar.label,
            crate::actions::open_calendar_address(&calendar.id),
        )
        .with_subtitle(if calendar.visible {
            "Calendar"
        } else {
            "Calendar · hidden"
        })
        .with_icon(Icon::Calendar);
        let Some(address) = result.action_address() else {
            return;
        };
        let id = address.resource;
        let searchable = format!("{}\0{}", calendar.label, calendar.id);
        let _ = self.catalog.upsert(id.clone(), searchable, result);
        current.insert(id);
    }

    fn event_subtitle(event: &Event) -> String {
        if event.all_day {
            date::fmt_day_long(event.start.date())
        } else {
            format!(
                "{} · {}",
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
        let state = if task.done { "Completed" } else { "Task" };
        task.due.map_or_else(
            || state.to_string(),
            |due| format!("{state} · due {}", date::fmt_day_long(due)),
        )
    }

    fn index_project(&self, project: &Project, current: &mut BTreeSet<String>) {
        let result = SearchResult::for_action(
            CALENDAR_KIND,
            &project.name,
            crate::actions::open_project_address(project.id),
        )
        .with_subtitle("Task project")
        .with_icon(Icon::Check);
        let Some(address) = result.action_address() else {
            return;
        };
        let id = address.resource;
        let _ = self.catalog.upsert(id.clone(), &project.name, result);
        current.insert(id);
    }

    fn index_event(&self, event: &Event, note: Option<&String>, current: &mut BTreeSet<String>) {
        let result = SearchResult::for_action(
            CALENDAR_KIND,
            event.display_title(),
            crate::actions::open_event_address(event.id),
        )
        .with_subtitle(Self::event_subtitle(event))
        .with_icon(Icon::Calendar);
        let Some(address) = result.action_address() else {
            return;
        };
        let id = address.resource;
        let searchable = format!(
            "{}\0{}\0{}\0{}",
            event.display_title(),
            event.location,
            event.calendar_id.as_deref().unwrap_or(""),
            note.map_or("", String::as_str),
        );
        let _ = self.catalog.upsert(id.clone(), searchable, result);
        current.insert(id);
    }

    fn index_task(&self, task: &Task, note: Option<&String>, current: &mut BTreeSet<String>) {
        let result = SearchResult::for_action(
            CALENDAR_KIND,
            Self::task_title(task),
            crate::actions::open_task_address(task.id),
        )
        .with_subtitle(Self::task_subtitle(task))
        .with_icon(Icon::Check);
        let Some(address) = result.action_address() else {
            return;
        };
        let id = address.resource;
        let searchable = format!(
            "{}\0{}",
            Self::task_title(task),
            note.map_or("", String::as_str)
        );
        let _ = self.catalog.upsert(id.clone(), searchable, result);
        current.insert(id);
    }

    /// Incrementally reconcile the complete loaded model into the retained
    /// catalogue. Changed records replace only their row; absent stable ids are
    /// removed so stale search addresses cannot outlive a peer deletion/undo.
    pub fn refresh(&self, model: &Model, notes: &std::collections::HashMap<String, String>) {
        self.refresh_with_calendars(model, notes, &[]);
    }

    /// Revision callers provide the live account projection too, so local and
    /// remote collections participate in the same durable search namespace as
    /// their events.
    pub fn refresh_with_calendars(
        &self,
        model: &Model,
        notes: &std::collections::HashMap<String, String>,
        calendars: &[CalendarCollectionResource],
    ) {
        let mut current = BTreeSet::new();
        for calendar in calendars {
            self.index_calendar(calendar, &mut current);
        }
        for project in &model.projects {
            self.index_project(project, &mut current);
        }
        for event in &model.events {
            self.index_event(event, notes.get(&event.id.to_string()), &mut current);
        }
        for task in &model.tasks {
            self.index_task(task, notes.get(&task.id.to_string()), &mut current);
        }
        let mut indexed = self
            .indexed_ids
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for removed in indexed.difference(&current) {
            self.catalog.remove(removed);
        }
        *indexed = current;
    }

    #[cfg(test)]
    fn stats(&self) -> ui::search::SearchCatalogStats {
        self.catalog.stats()
    }
}

impl SearchProvider for CalendarSearchProvider {
    fn name(&self) -> &str {
        CALENDAR_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.catalog.search_limited(query, limit)
    }

    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        self.catalog
            .query_page(query, cursor, work_budget, result_limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use ui::search::SearchRegistry;

    fn seeded() -> Model {
        crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap())
    }

    #[test]
    fn provider_finds_events_by_title_location_and_notes() {
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

    #[test]
    fn every_hit_has_a_canonical_resource_action() {
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

    #[test]
    fn unique_late_record_is_findable_without_a_corpus_scan() {
        let mut model = seeded();
        let template = model.events[0].clone();
        model.events.extend((0..100_001u128).map(|index| {
            let mut event = template.clone();
            event.id = uuid::Uuid::from_u128(index + 1);
            event.title = format!("ordinary event {index}");
            event
        }));
        model.events.last_mut().unwrap().title = "late calendar needle".into();
        let provider = CalendarSearchProvider::new(&model);
        let page = provider.search_page("calendar needle", None, 4, 4);
        assert_eq!(page.inspected, 1);
        assert_eq!(page.results.len(), 1);
        assert_eq!(page.results[0].title, "late calendar needle");
        assert_eq!(
            provider.stats().records,
            model.projects.len() + model.events.len() + model.tasks.len()
        );
    }

    #[test]
    fn pages_bound_work_and_results_without_losing_broad_matches() {
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
            let page = provider.search_page("common", cursor.as_ref(), 17, 5);
            assert!(page.inspected <= 17);
            assert!(page.results.len() <= 5);
            seen.extend(page.results.iter().map(|result| result.action_id.clone()));
            let Some(next) = page.next else { break };
            cursor = Some(next);
        }
        assert_eq!(seen.len(), 257);
    }

    #[test]
    fn refresh_replaces_changed_rows_and_withdraws_deleted_resources() {
        let mut model = seeded();
        let provider = CalendarSearchProvider::new(&model);
        let event = model.events[0].id;
        model.events[0].title = "revised calendar event".into();
        provider.refresh(&model, &std::collections::HashMap::new());
        assert!(provider.search("revised").iter().any(
            |result| result.action_address() == Some(crate::actions::open_event_address(event))
        ));
        model.events.retain(|candidate| candidate.id != event);
        provider.refresh(&model, &std::collections::HashMap::new());
        assert!(provider.search("revised").is_empty());
    }

    #[test]
    fn configured_calendars_are_searchable_at_their_canonical_open_addresses() {
        let model = seeded();
        let provider = CalendarSearchProvider::new(&model);
        let calendars = vec![crate::actions::CalendarCollectionResource {
            id: "account/team".into(),
            label: "Work · Team schedule".into(),
            color: Some(0),
            visible: true,
        }];
        provider.refresh_with_calendars(&model, &std::collections::HashMap::new(), &calendars);
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
