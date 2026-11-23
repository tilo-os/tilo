app so.tilo.calendar.test "Calendar tests"

/// One of the workspace's calendar accounts, kept in step with its server.
type Account (origin: caldav.account) {
    key: Text merge register
    address: Text merge register
    server: Text merge register
    read_only: Bool merge register
    last_synced: Time
    status: Text
}

/// One calendar: an account's, or one of the workspace's own.
type Calendar (origin: caldav.calendar) {
    key: Text merge register
    account: Text merge register
    name: Text
    color: Text
    writable: Bool
}

/// One entry of a calendar.
type Event (origin: caldav.event, title: title) {
    key: Text merge register
    href: Text merge register
    calendar: Text merge register
    title: Text
    start: Time
    end: Time
    all_day: Bool
    timezone: Text
    location: Text
    repeats: Text
    repeat_every: Int
    repeat_until: Text
    repeat_count: Int
    repeat_exdates: Set<Text>
    overrides: Text
    organizer: Text
    attendees: Text
    categories: Set<Text>
    notes: Text
}

type Calendars {
    accounts: Collection<Account>
    calendars: Collection<Calendar>
    events: Collection<Event>
}

root Calendars

default view main(c: Calendars) {
    page {
        for cal in c.calendars {
            text(cal.name)
        }
        for e in c.events {
            text(e.title)
        }
    }
}

/// A calendar of the workspace's own, which no server holds.
action add_calendar(c: Calendars, name: Text) -> Calendar {
    c.calendars.insert(Calendar(name: name))
}

action add_event(c: Calendars, calendar: Text, title: Text) -> Event {
    c.events.insert(Event(calendar: calendar, title: title))
}

action rename_event(e: Event, title: Text) {
    e.title = title
}

action move_event(e: Event, calendar: Text) {
    e.calendar = calendar
}

action reschedule(e: Event, start: Time, end: Time) {
    e.start = start
    e.end = end
}

action set_notes(e: Event, notes: Text) {
    e.notes = notes
}

destructive action remove_event(c: Calendars, e: Event) {
    c.events.remove(e)
}
