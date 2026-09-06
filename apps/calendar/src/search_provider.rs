//! Calendar events and tasks as a qOS global-search source.
//!
//! Every result addresses the resource tree's stable exported `open` view
//! action. The palette therefore uses the same brokered operation as agents,
//! accessibility and direct resource navigation; Calendar owns no parallel
//! activation-token namespace.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use ui::icons::Icon;
use ui::search::{DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult};

use crate::date;
use crate::model::{Event, Model, Project, Task};

/// The kind label (and result-group header) Calendar produces.
pub const CALENDAR_KIND: &str = "Calendar";

/// A long query may still have a very common three-character fragment. Bound
/// materialization after index intersection; exact/specific queries normally
/// reduce to just their matching records before this limit applies.
const QUERY_CANDIDATE_LIMIT: usize = 128;

#[derive(Clone, Copy)]
enum Record {
    Project(usize),
    Event(usize),
    Task(usize),
}

/// A synchronous, complete snapshot of the model plus an inverted n-gram
/// index. `CalDAV` work remains off the palette's query path: all I/O happens
/// before this provider is constructed, never while typing in search.
pub struct CalendarSearchProvider {
    projects: Vec<Project>,
    events: Vec<Event>,
    tasks: Vec<Task>,
    event_notes: Vec<String>,
    task_notes: Vec<String>,
    records: Vec<Record>,
    /// Normalized one-, two-, and three-character grams mapped to complete
    /// snapshot record ids. The bounded query path intersects these compact
    /// postings rather than re-reading every event/task on each keystroke.
    grams: HashMap<String, Vec<usize>>,
    /// Exact normalized words retain late, specific findability without ever
    /// walking a long substring posting on the query path.
    words: HashMap<String, Vec<usize>>,
    /// Test-only observable query budget; `Cell` keeps search's public
    /// synchronous `&self` contract intact.
    candidate_inspections: Cell<usize>,
}

impl CalendarSearchProvider {
    pub fn new(model: &Model) -> Self {
        let projects = model.projects.clone();
        let events = model.events.clone();
        let tasks = model.tasks.clone();
        let event_len = events.len();
        let task_len = tasks.len();
        let mut records = Vec::with_capacity(projects.len() + events.len() + tasks.len());
        records.extend((0..projects.len()).map(Record::Project));
        records.extend((0..events.len()).map(Record::Event));
        records.extend((0..tasks.len()).map(Record::Task));
        let mut provider = Self {
            projects,
            events,
            tasks,
            event_notes: vec![String::new(); event_len],
            task_notes: vec![String::new(); task_len],
            records,
            grams: HashMap::new(),
            words: HashMap::new(),
            candidate_inspections: Cell::new(0),
        };
        provider.index_complete_snapshot();
        provider
    }

    /// The note projection joins the normal resource/search snapshot without
    /// changing activation: a note hit still opens its owning event or task.
    pub fn new_with_notes(model: &Model, notes: &HashMap<String, String>) -> Self {
        let mut provider = Self::new(model);
        provider.event_notes = provider
            .events
            .iter()
            .map(|event| {
                notes
                    .get(&event.id.to_string())
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();
        provider.task_notes = provider
            .tasks
            .iter()
            .map(|task| notes.get(&task.id.to_string()).cloned().unwrap_or_default())
            .collect();
        provider.grams.clear();
        provider.words.clear();
        provider.index_complete_snapshot();
        provider
    }

    fn index_complete_snapshot(&mut self) {
        for index in 0..self.records.len() {
            let haystack = self.record_haystack(index);
            let mut unique_grams = HashSet::new();
            for gram in grams(&haystack) {
                if unique_grams.insert(gram.clone()) {
                    self.grams.entry(gram).or_default().push(index);
                }
            }
            let mut unique_words = HashSet::new();
            for word in words(&haystack) {
                if unique_words.insert(word.clone()) {
                    self.words.entry(word).or_default().push(index);
                }
            }
        }
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "record ids are constructed only from these immutable vectors in new() and are never externally supplied"
    )]
    fn record_haystack(&self, index: usize) -> String {
        match self.records[index] {
            Record::Project(index) => self.projects[index].name.to_lowercase(),
            Record::Event(index) => {
                let event = &self.events[index];
                format!(
                    "{} {} {}",
                    event.display_title(),
                    event.location,
                    self.event_notes[index]
                )
                .to_lowercase()
            }
            Record::Task(index) => format!(
                "{} {}",
                Self::task_title(&self.tasks[index]),
                self.task_notes[index]
            )
            .to_lowercase(),
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

    fn indexed_candidates(&self, query: &str) -> Vec<usize> {
        self.candidate_inspections.set(0);
        let query_words = words(query);
        if query_words.is_empty() {
            return (0..self.records.len())
                .take(DEFAULT_SEARCH_RESULT_LIMIT)
                .collect();
        }
        let word_postings = query_words
            .iter()
            .map(|word| self.words.get(word))
            .collect::<Option<Vec<_>>>()
            .filter(|postings| postings.len() == query_words.len());
        if let Some(postings) = word_postings {
            return self.bounded_intersection(postings);
        }
        let grams = grams(query);
        let postings = grams
            .iter()
            .map(|gram| self.grams.get(gram))
            .collect::<Option<Vec<_>>>()
            .filter(|postings| postings.len() == grams.len());
        postings.map_or_else(Vec::new, |postings| self.bounded_intersection(postings))
    }

    fn bounded_intersection(&self, mut postings: Vec<&Vec<usize>>) -> Vec<usize> {
        postings.sort_by_key(|posting| posting.len());
        let Some(rarest) = postings.first() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for candidate in rarest.iter().take(QUERY_CANDIDATE_LIMIT) {
            self.candidate_inspections
                .set(self.candidate_inspections.get().saturating_add(1));
            if postings
                .iter()
                .skip(1)
                .all(|posting| posting.binary_search(candidate).is_ok())
            {
                out.push(*candidate);
            }
        }
        out
    }

    #[cfg(test)]
    fn candidate_inspections(&self) -> usize {
        self.candidate_inspections.get()
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "query candidates are postings generated from the same immutable record vector"
    )]
    fn result_for(&self, index: usize) -> SearchResult {
        match self.records[index] {
            Record::Project(index) => {
                let project = &self.projects[index];
                SearchResult::for_action(
                    CALENDAR_KIND,
                    &project.name,
                    crate::actions::open_project_address(project.id),
                )
                .with_subtitle("Task project")
                .with_icon(Icon::Check)
            }
            Record::Event(index) => {
                let event = &self.events[index];
                SearchResult::for_action(
                    CALENDAR_KIND,
                    event.display_title(),
                    crate::actions::open_event_address(event.id),
                )
                .with_subtitle(Self::event_subtitle(event))
                .with_icon(Icon::Calendar)
            }
            Record::Task(index) => {
                let task = &self.tasks[index];
                SearchResult::for_action(
                    CALENDAR_KIND,
                    Self::task_title(task),
                    crate::actions::open_task_address(task.id),
                )
                .with_subtitle(Self::task_subtitle(task))
                .with_icon(Icon::Check)
            }
        }
    }
}

fn grams(value: &str) -> Vec<String> {
    let chars = value.chars().collect::<Vec<_>>();
    let width = chars.len().clamp(1, 3);
    chars
        .windows(width)
        .map(|gram| gram.iter().collect())
        .collect()
}

fn words(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
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
        self.indexed_candidates(&query)
            .into_iter()
            .filter(|index| self.record_haystack(*index).contains(&query))
            .take(limit.min(DEFAULT_SEARCH_RESULT_LIMIT))
            .map(|index| self.result_for(index))
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
    fn exact_query_finds_a_record_beyond_the_old_scan_cutoff() {
        let mut model = seeded();
        let template = model.events[0].clone();
        model
            .events
            .extend((0..(QUERY_CANDIDATE_LIMIT + 8)).map(|index| {
                let mut event = template.clone();
                event.title = format!("ordinary event {index}");
                event
            }));
        let target = model.events.last_mut().expect("appended event");
        target.title = "needle beyond sixty-four".into();
        let provider = CalendarSearchProvider::new(&model);
        assert!(
            provider
                .search("needle beyond sixty-four")
                .iter()
                .any(|hit| hit.title == "needle beyond sixty-four"),
            "the complete snapshot is indexed; record order cannot hide a specific result"
        );
        assert!(
            provider.candidate_inspections() <= QUERY_CANDIDATE_LIMIT,
            "the index lookup inspected only its fixed candidate budget"
        );
    }

    #[test]
    fn broad_query_never_evaluates_more_than_the_candidate_budget() {
        let mut model = seeded();
        let template = model.events[0].clone();
        model
            .events
            .extend((0..(QUERY_CANDIDATE_LIMIT * 4)).map(|index| {
                let mut event = template.clone();
                event.title = format!("common item {index}");
                event
            }));
        let provider = CalendarSearchProvider::new(&model);
        let _ = provider.search("common");
        assert!(provider.candidate_inspections() <= QUERY_CANDIDATE_LIMIT);
    }
}
