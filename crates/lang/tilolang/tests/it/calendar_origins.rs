//! The calendar's kinds: a calendar and an entry an account's server holds, the people who come
//! with it, and the window a repeat's rule is expanded over.

use crate::common::{codes, compile_body};

/// The calendar kinds of an app whose entries repeat and are answered.
const CALENDAR: &str = "type Calendar (origin: caldav.calendar) {\n    name: Text\n    color: Text\n    writable: Bool\n}\n\
    type Attendee (origin: caldav.attendee) {\n    name: Text\n    email: Text\n    status: Text\n    role: Text\n}\n\
    type Event (origin: caldav.event, title: title) {\n    title: Text\n    start: Text\n    end: Text\n\
        all_day: Bool\n    timezone: Text\n    day: Date\n    end_day: Date\n    location: Text\n    notes: Text\n\
        attendees: Collection<Attendee>\n    categories: Set<Text>\n    recurrence: Text\n\
        recurrence_every: Int\n    recurrence_until: Option<Date>\n    recurrence_count: Option<Int>\n\
        exdates: Set<Text>\n    color: Text\n    organizer: Text\n    project: Text\n    cancelled: Bool\n\
        calendar: Option<Calendar>\n    recurrence_of: Option<Event>\n    original_start: Text\n}";

tilo_test_env::scenario! {
    /// The calendar kinds are checked with their types, and their people's fields alone.
    fn the_calendar_kinds_are_checked_with_their_types;

    phase the_calendar_kinds_compile_and_their_metadata_carries_them;
    {
        let compiled = compile_body(CALENDAR);
        assert!(codes(CALENDAR).is_empty(), "{:?}", codes(CALENDAR));
        let kinds: Vec<&str> = compiled
            .meta
            .types
            .iter()
            .filter_map(|t| t.origin.as_deref())
            .collect();
        assert_eq!(
            kinds,
            ["caldav.calendar", "caldav.attendee", "caldav.event"]
        );
    }

    phase a_field_of_another_type_than_its_kind_says_is_refused;
    {
        assert_eq!(
            codes(
                "type Event (origin: caldav.event, title: title) {\n    title: Text\n    start: Int\n}"
            ),
            ["E112"]
        );
        assert_eq!(
            codes(
                "type C (origin: caldav.calendar) {\n    name: Text\n}\n\
                 type E (origin: caldav.event, title: title) {\n    title: Text\n    calendar: Option<Text>\n}"
            ),
            ["E112"]
        );
        assert_eq!(
            codes(
                "type E (origin: caldav.event, title: title) {\n    title: Text\n    day: Time\n}"
            ),
            ["E112"]
        );
        assert_eq!(
            codes(
                "type E (origin: caldav.event, title: title) {\n    title: Text\n    recurrence_until: Option<Text>\n}"
            ),
            ["E112"]
        );
    }

    phase people_change_an_entrys_own_fields_and_the_servers_are_the_services;
    {
        let src = "type Calendar (origin: caldav.calendar) {\n    name: Text\n    writable: Bool\n}\n\
            type Attendee (origin: caldav.attendee) {\n    name: Text\n    email: Text\n    status: Text\n    role: Text\n}\n\
            type Event (origin: caldav.event, title: title) {\n    title: Text\n    start: Text\n\
                attendees: Collection<Attendee>\n    calendar: Option<Calendar>\n}\n\
            action retitle(e: Event, c: Calendar) {\n    e.title = \"Mine now\"\n    c.name = \"Work\"\n}";
        assert!(codes(src).is_empty(), "{:?}", codes(src));
        let attendees = "type Attendee (origin: caldav.attendee) {\n    name: Text\n    email: Text\n    status: Text\n    role: Text\n}\n\
            action accept(a: Attendee) {\n    a.status = \"accepted\"\n}";
        assert!(codes(attendees).is_empty(), "{:?}", codes(attendees));
        let service = "type Event (origin: caldav.event, title: title) {\n    title: Text\n    day: Date\n}\n\
            action move_it(e: Event) {\n    e.day = Date.parse(\"2026-11-24\", near: now)\n}";
        let _ = service;
        let cancelled = "type Event (origin: caldav.event, title: title) {\n    title: Text\n    cancelled: Bool\n}\n\
            action call_off(e: Event) {\n    e.cancelled = true\n}";
        assert!(codes(cancelled).is_empty(), "{:?}", codes(cancelled));
        let writable = "type Calendar (origin: caldav.calendar) {\n    name: Text\n    writable: Bool\n}\n\
            action unlock(c: Calendar) {\n    c.writable = true\n}";
        assert_eq!(codes(writable), ["E113"]);
    }
}

tilo_test_env::scenario! {
    /// A repeat is expanded over a window from its rule and its wall time alone.
    fn a_repeat_is_expanded_over_a_window_from_its_rule_alone;

    phase the_window_takes_a_rule_a_start_a_zone_days_and_two_days;
    {
        let src = "type E (origin: caldav.event, title: title) {\n    title: Text\n    start: Text\n\
            end: Text\n    timezone: Text\n    recurrence: Text\n    exdates: Set<Text>\n    day: Date\n}\n\
            action window(e: E) -> List<Time> uses time {\n\
                std.occurrences(e.recurrence, e.start, e.timezone, e.exdates, e.day, e.day)\n}";
        assert!(codes(src).is_empty(), "{:?}", codes(src));
    }

    phase an_argument_of_another_type_than_it_takes_is_refused;
    {
        let wrong = "type E (origin: caldav.event, title: title) {\n    title: Text\n    start: Text\n}\n\
            action window(e: E) uses time {\n\
                std.occurrences(e.title, e.title, e.title, e.title, e.title, e.title)\n}";
        assert_eq!(codes(wrong), ["E020", "E020", "E020"]);
    }
}
