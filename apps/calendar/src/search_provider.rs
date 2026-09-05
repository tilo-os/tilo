//! Calendar events and tasks as a qOS global-search source.
//!
//! Every result addresses the resource tree's stable exported `open` view
//! action. The palette therefore uses the same brokered operation as agents,
//! accessibility and direct resource navigation; Calendar owns no parallel
//! activation-token namespace.

use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_INDEX_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult,
};

use crate::date;
use crate::model::{Event, Model, Task};

/// The kind label (and result-group header) Calendar produces.
pub const CALENDAR_KIND: &str = "Calendar";

/// A synchronous snapshot of the events and tasks currently in the model.
/// CalDAV work remains off the palette's query path.
pub struct CalendarSearchProvider {
    events: Vec<Event>,
    tasks: Vec<Task>,
}

impl CalendarSearchProvider {
    pub fn new(model: &Model) -> Self {
        let events = model
            .events
            .iter()
            .take(DEFAULT_SEARCH_INDEX_LIMIT)
            .cloned()
            .collect::<Vec<_>>();
        let remaining = DEFAULT_SEARCH_INDEX_LIMIT.saturating_sub(events.len());
        Self {
            events,
            tasks: model.tasks.iter().take(remaining).cloned().collect(),
        }
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
}

impl SearchProvider for CalendarSearchProvider {
    fn name(&self) -> &str {
        CALENDAR_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let query = query.to_lowercase();
        let events = self.events.iter().filter_map(|event| {
            (event.display_title().to_lowercase().contains(&query)
                || event.location.to_lowercase().contains(&query))
            .then(|| {
                SearchResult::for_action(
                    CALENDAR_KIND,
                    event.display_title(),
                    crate::actions::open_event_address(event.id),
                )
                .with_subtitle(Self::event_subtitle(event))
                .with_icon(Icon::Calendar)
            })
        });
        let tasks = self.tasks.iter().filter_map(|task| {
            let title = Self::task_title(task);
            title.to_lowercase().contains(&query).then(|| {
                SearchResult::for_action(
                    CALENDAR_KIND,
                    title,
                    crate::actions::open_task_address(task.id),
                )
                .with_subtitle(Self::task_subtitle(task))
                .with_icon(Icon::Check)
            })
        });
        events
            .chain(tasks)
            .take(limit.min(DEFAULT_SEARCH_RESULT_LIMIT))
            .collect()
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
    fn provider_finds_events_by_title_and_location() {
        let provider = CalendarSearchProvider::new(&seeded());
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
                .any(|hit| hit.title == "Dentist"),
            "an event remains discoverable by its location"
        );
    }

    #[test]
    fn provider_finds_tasks_and_addresses_their_open_actions() {
        let model = seeded();
        let task = model.tasks.first().expect("demo has tasks");
        let hits = CalendarSearchProvider::new(&model).search(&task.title);
        let hit = hits
            .iter()
            .find(|hit| hit.title == task.title)
            .expect("task is found");
        assert_eq!(
            hit.action_address(),
            Some(crate::actions::open_task_address(task.id))
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
            model.events.len() + model.tasks.len()
        );
        assert!(
            groups[0]
                .results
                .iter()
                .all(|hit| hit.action_address().is_some())
        );
    }
}
