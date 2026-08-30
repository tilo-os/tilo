//! Calendar events as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: Calendar declares its
//! searchable type by implementing [`SearchProvider`] over a snapshot of its
//! events, so every event becomes a typed [`SearchResult`] the unified Ctrl+K
//! palette can rank alongside every other provider's hits. Activating a row
//! hands back an opaque `action_id` the app resolves to an event [`Id`], jumps
//! its anchor date to, and selects.
//!
//! The provider is deliberately **library-scoped and synchronous**: it matches
//! the in-memory `Model::events` the views already render, never touching the
//! network (`CalDAV` pulls run on their own worker), so a Ctrl+K query never
//! blocks on a sync.

use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult};

use crate::date;
use crate::model::{Event, Id, Model};

/// The kind label (and result-group header) Calendar produces.
pub const EVENT_KIND: &str = "Calendar";

/// The `action_id` prefix. A chosen row's `action_id` is `event:<uuid>`;
/// [`parse_action_id`] turns it back into an event [`Id`].
const ACTION_PREFIX: &str = "event:";

/// A [`SearchProvider`] over a snapshot of the event list. Rebuild it (cheap)
/// whenever the search surface opens.
pub struct EventSearchProvider {
    events: Vec<Event>,
}

impl EventSearchProvider {
    /// Snapshot the current events into a provider.
    pub fn new(model: &Model) -> Self {
        Self {
            events: model.events.clone(),
        }
    }

    /// The `action_id` for an event — what a chosen row hands back.
    pub fn action_id(id: Id) -> String {
        format!("{ACTION_PREFIX}{id}")
    }

    /// Resolve an `action_id` produced by this provider back to an event
    /// [`Id`]. Returns `None` for an id this provider did not emit.
    pub fn parse_action_id(action_id: &str) -> Option<Id> {
        action_id
            .strip_prefix(ACTION_PREFIX)
            .and_then(|rest| rest.parse::<Id>().ok())
    }

    /// The "when" line shown under an event: the long day, plus the start time
    /// unless it is an all-day event.
    fn when_subtitle(ev: &Event) -> String {
        if ev.all_day {
            date::fmt_day_long(ev.start.date())
        } else {
            format!(
                "{} · {}",
                date::fmt_day_long(ev.start.date()),
                date::fmt_time(&ev.start)
            )
        }
    }
}

impl SearchProvider for EventSearchProvider {
    fn name(&self) -> &str {
        EVENT_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        // Match the display title and the location; the registry re-ranks the
        // survivors by `fuzzy_score` against the visible title. An empty query
        // browses the whole calendar.
        let q = query.to_lowercase();
        self.events
            .iter()
            .filter(|ev| {
                ev.display_title().to_lowercase().contains(&q)
                    || ev.location.to_lowercase().contains(&q)
            })
            .map(|ev| {
                SearchResult::new(EVENT_KIND, ev.display_title(), Self::action_id(ev.id))
                    .with_subtitle(Self::when_subtitle(ev))
                    .with_icon(Icon::Calendar)
            })
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
    fn provider_finds_a_real_event_by_title() {
        let model = seeded();
        let provider = EventSearchProvider::new(&model);
        let hits = provider.search("design");
        assert!(
            hits.iter().any(|r| r.title == "Design review"),
            "the provider surfaces the matching event, got {:?}",
            hits.iter().map(|r| &r.title).collect::<Vec<_>>()
        );
        assert!(hits.iter().all(|r| r.kind == EVENT_KIND));
        assert!(
            hits.iter().all(|r| r.subtitle.is_some()),
            "every event row carries a when-line"
        );
    }

    #[test]
    fn empty_query_browses_every_event() {
        let model = seeded();
        let provider = EventSearchProvider::new(&model);
        assert_eq!(provider.search("").len(), model.events.len());
    }

    #[test]
    fn action_id_round_trips_to_the_chosen_event() {
        let model = seeded();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(EventSearchProvider::new(&model)));

        let groups = registry.query_grouped("design");
        assert_eq!(groups[0].kind, EVENT_KIND);
        let top = &groups[0].results[0];

        let id = EventSearchProvider::parse_action_id(&top.action_id)
            .expect("action_id parses back to an event Id");
        let ev = model
            .event(id)
            .expect("the id resolves to a real stored event");
        assert_eq!(
            ev.display_title(),
            top.title,
            "activation lands on the right event"
        );
    }

    #[test]
    fn parse_action_id_rejects_foreign_ids() {
        assert!(EventSearchProvider::parse_action_id("file:/etc/passwd").is_none());
        assert!(EventSearchProvider::parse_action_id("event:not-a-uuid").is_none());
    }

    #[test]
    fn widget_hooks_find_then_navigate_to_the_event() {
        // Drive the exact pair the shell palette calls: collect the app's own
        // `search_providers()`, query one, then hand the chosen row's action_id
        // back through `activate_search_result` and prove the app navigated.
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::CalendarApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        let providers = app.search_providers();
        assert!(
            !providers.is_empty(),
            "the calendar publishes a search provider"
        );
        let hits = providers[0].search("dentist");
        let hit = hits
            .iter()
            .find(|r| r.title == "Dentist")
            .expect("the seeded 'Dentist' event is findable through the real hook");
        let id = EventSearchProvider::parse_action_id(&hit.action_id).unwrap();
        assert_ne!(
            app.selected_event(),
            Some(id),
            "not already selected before activation"
        );

        app.activate_search_result(&hit.action_id, &mut doc, &ROOT);
        assert_eq!(
            app.selected_event(),
            Some(id),
            "activation selected the chosen event"
        );
    }
}
