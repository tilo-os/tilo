app so.tilo.calendar.test "Calendar tests"

/// One calendar: an account's, or one of the workspace's own.
type Calendar (origin: caldav.calendar) {
    key: Text merge register
    account: Text merge register
    name: Text
    color: Text
    writable: Bool
    sync_token: Text merge register
}

/// Somebody an entry names.
type Attendee (origin: caldav.attendee) {
    name: Text
    email: Text
    status: Text
    role: Text
}

/// One entry of a calendar.
type Event (origin: caldav.event, title: title) {
    key: Text merge register
    href: Text merge register
    etag: Text merge register
    title: Text
    start: Text
    end: Text
    all_day: Bool merge register
    timezone: Text merge register
    day: Date
    end_day: Date
    location: Text
    notes: Text
    attendees: Collection<Attendee>
    categories: Set<Text>
    recurrence: Text
    recurrence_every: Int merge register
    recurrence_until: Option<Date> = .none
    recurrence_count: Option<Int> = .none
    exdates: Set<Text>
    color: Text
    organizer: Text merge register
    project: Text merge register
    cancelled: Bool merge register
    calendar: Option<Calendar> = .none
    recurrence_of: Option<Event> = .none
    original_start: Text merge register
    remind: Time = Time("1970-01-01T00:00:00Z")
}

type Calendars {
    calendars: Collection<Calendar>
    events: Collection<Event>
}

root Calendars

default view main for Calendars

view main(c: Calendars) {
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
    c.calendars.insert(Calendar(name: name, color: "#111111"))
}

action add_event(c: Calendars, calendar: Calendar, title: Text, start: Text, end: Text) -> Event {
    c.events.insert(Event(calendar: calendar, title: title, start: start, end: end))
}

action rename_event(e: Event, title: Text) {
    e.title = title
}

action move_event(e: Event, calendar: Calendar) {
    e.calendar = calendar
}

action reschedule(e: Event, start: Text, end: Text) {
    e.start = start
    e.end = end
}

action set_notes(e: Event, notes: Text) {
    e.notes = notes
}

action recolor_event(e: Event, color: Text) {
    e.color = color
}

action rename_calendar(c: Calendar, name: Text) {
    c.name = name
}

action recolor_calendar(c: Calendar, color: Text) {
    c.color = color
}

destructive action remove_event(c: Calendars, e: Event) {
    c.events.remove(e)
}
