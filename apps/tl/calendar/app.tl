app so.tilo.calendar "Calendar" (
    icon: .calendar,
    category: .office,
    tagline: "The workspace's events: the week, the month, and who is coming",
    keywords: ["calendar", "events", "schedule", "week", "month", "agenda", "appointments", "meetings", "invitations", "caldav", "ics", "shared calendar"],
)

use so.tilo.mail as mail

/// Someone invited to an event, and what they answered: `needs-action` until they say.
type Attendee (origin: caldav.attendee, title: email) {
    name: Text
    /// Their email address, where the invitation goes, and what names them here.
    email: Text
    /// What they answered: `needs-action`, `accepted`, `declined` or `tentative`.
    status: Text = "needs-action"
    /// What part they take: `required`, `optional` or `chair`.
    role: Text = "required"
}

/// How much a task matters, most urgent last.
enum Priority {
    /// No priority
    none
    /// Low
    low
    /// Medium
    medium
    /// High
    high
}

/// The colours a calendar, an event or a project can take, each a place in the theme.
enum Color {
    indigo
    emerald
    amber
    red
    blue
    violet
    slate
}

/// A project: what a set of events and tasks is for, and the colour it takes.
type Project (title: name) {
    name: Text
    color: Color = .indigo
    /// Where it lists among the projects.
    order: Int = 0
}

/// A task: what to do, by when, and in which project.
type Task (title: title, untitled: "New task") {
    title: Text
    done: Bool = false
    /// The day it is due by, if any.
    due: Option<Date> = .none
    /// When to tell whoever is doing it that its moment came, if a reminder is set.
    remind: Option<Time> = .none
    /// Anything worth remembering about it.
    notes: RichText
    priority: Priority = .none
    /// The project it belongs to; none while it waits in the inbox.
    project: Option<Project> = .none
    /// The task it is a step of; none for a task of its own.
    parent: Option<Task> = .none

    search {
        filter "Inbox" = project == .none and due == .none and not done (icon: .inbox)
        filter "Today" = not done and due <= today (icon: .star)
        filter "Upcoming" = due > today and not done (icon: .calendar)
        filter "Done" = done (icon: .circle_check)
        filter due
        filter project
        filter priority
        filter parent
    }
}

/// Something happening on a day: what, when, where, and who is coming.
///
/// Its calendar belongs to the workspace, so the workspace's own calendar is one of this type too,
/// an ordinary record no account holds; an event of an account's calendar is kept in step with that
/// account's server by the calendar service, which leaves the workspace's own alone.
type Event (origin: caldav.event, title: title, untitled: "New event") {
    /// The entry's id: its `UID`, the name its server and every client know it by.
    key: Text merge register
    /// Where its server keeps it; empty for an entry no server holds.
    href: Text merge register
    /// The version its server last gave it, so a change is written only over what was read.
    etag: Text merge register
    title: Text
    /// The wall time it starts at, as a calendar writes it: `2026-11-24T09:00:00`.
    start: Text
    /// The wall time it ends at, the same way.
    end: Text
    /// Whether it takes the whole day rather than the hours it names.
    all_day: Bool merge register = false
    /// The zone its wall times are read in: empty or `floating` for the same time of day
    /// everywhere, else `UTC` or an IANA zone such as `Europe/Berlin`. An instant is derived from
    /// the wall and the zone where it is read.
    timezone: Text = "floating"
    location: Text = ""
    /// What the server says of it: its `DESCRIPTION`, as text.
    notes: Text = ""
    /// What the person writes of it, in rich text.
    body: RichText
    /// Who is coming, and what each answered.
    attendees: Collection<Attendee>
    /// The person's own words for what kind of event it is.
    categories: Set<Text>
    /// Its frequency, in iCalendar's words (`FREQ=WEEKLY`); empty for one that does not repeat.
    /// The interval, the end and the days it comes on stand in their own fields.
    recurrence: Text = ""
    /// How it repeats: which unit the rule counts in.
    repeat: Repeat = .none
    /// How often the repeat comes, in its own unit: every 2 weeks is 2.
    recurrence_every: Int merge register = 1
    /// The last day it repeats by, if it ends on a day.
    recurrence_until: Option<Date> = .none
    /// How many times it repeats in all, if it ends after a count.
    recurrence_count: Option<Int> = .none
    /// The days of the week it repeats on, as `MO`..`SU`; empty for the day it stands on.
    recurrence_weekdays: Set<Text>
    /// The days its series leaves out, each as a day's text.
    exdates: Set<Text>
    /// The day it stands on: kept with its moments so a window finds it by a day of its own.
    day: Date = Date("1970-01-01")
    /// The day it ends on, kept with `end` as `day` is kept with `start`: a day past the first
    /// stands for every day it runs through. A whole-day event ends at the next midnight, so its
    /// last day is the one before.
    end_day: Date = Date("1970-01-01")
    /// The colour it takes: the name of one of the palette's places, empty for its calendar's.
    color: Text = ""
    /// What the calendar service files it under; the server holds nothing of it.
    project: Text merge register = ""
    /// The project the person files it under.
    filed_under: Option<Project> = .none
    /// The address of whoever organises it; empty while this workspace does.
    organizer: Text merge register = ""
    /// Whether one date of a series is cancelled rather than moved.
    cancelled: Bool merge register = false
    /// The calendar it belongs to: the workspace's own while none.
    calendar: Option<Calendar> = .none
    /// When to tell the people coming that it starts, if anyone is to be told: 1970 for none.
    remind: Time = Time("1970-01-01T00:00:00Z")
    /// The event this one stands apart from, when it is one date of a series changed on its
    /// own; none for an event of its own and for the series itself.
    recurrence_of: Option<Event> = .none
    /// The date of the series this one stands in for, when it is one date changed on its own.
    /// The wall time this date of a series would have started at, as its series writes it.
    original_start: Text merge register = ""

    search {
        filter "Upcoming" = end_day >= today (icon: .calendar)
        filter "Coming" = end_day >= today or recurrence != ""
        filter "Today" = day == today (icon: .star)
        filter "Repeating" = recurrence != "" (icon: .repeat)
        filter day
        filter calendar
        filter recurrence_of
        sort start a_z
        hide timezone
        hide recurrence
        hide exdates
    }
}

/// A calendar: the color-coded place events are kept. The workspace's own has no account and is an
/// ordinary record; one of an account's is kept in step with that account's server.
type Calendar (origin: caldav.calendar, title: name) {
    /// The calendar's id: the address its server keeps it at, or one given to a calendar of the
    /// workspace's own.
    key: Text merge register
    /// The key of the account whose server holds it; empty for one of the workspace's own.
    account: Text merge register
    /// How far the server's changes were read the last time, so a restart asks only for more.
    sync_token: Text merge register
    name: Text
    /// The colour it is drawn in: the name of one of the palette's places.
    color: Text = ""
    /// Whether its server lets this workspace add and change its events.
    writable: Bool = true
}

/// The workspace's events and its calendars.
type Calendars {
    events: Collection<Event>
    calendars: Collection<Calendar>
    /// Every task the calendar keeps, in the order people put them.
    tasks: Collection<Task>
    projects: Collection<Project>
    /// The moment the window stands on: the day it shows, at that day's midnight where the person
    /// is, so the timeline's anchor is the day itself whatever their zone. This week when they
    /// open the calendar.
    anchor_at: Time = Time("1970-01-01T00:00:00Z")
}

root calendar: Calendars

default view window for Calendars

default view event_page for Event

default view task_page for Task

default view moment_page for Moment

row view event_row for Event

/// What a workspace's calendar starts with: its own calendar, which holds events an account does
/// not, so an event can be made the moment the calendar opens.
initial action own(c: Calendars) {
    c.calendars.insert(Calendar(name: std.say("My calendar")))
}

/// Opening the calendar shows this week.
opened action land(c: Calendars) uses time {
    let day = midnight(today, now)
    if c.anchor_at != day {
        c.anchor_at = day
    }
}

/// Starts an event on a day the person points at, at nine in the morning for an hour, or at
/// the moment the words name.
primary action new_event(
    c: Calendars,
    title: Text = "",
    day: Option<Date> = .none,
    at: Option<Time> = .none,
    minutes: Option<Int> = .none,
) -> Event uses time (icon: .plus, label: "New event", key: "Ctrl+N") {
    let d = day ?? today
    // A line that names only a day makes an event of the whole day, as the Rust app's quick
    // add did; a day's own entry says the time of day it starts at, and the bar's button starts
    // one at the next quarter hour. What is written is the wall time where the person is, as a
    // floating time: nine is nine wherever it is read.
    let all_day = at == .none and day != .none
    let midnight = Time.parse("{d.iso()}T00:00:00", near: now) ?? now
    let moment = if let given = at {
        given
    } else if day == .none { next_quarter(now) } else { midnight }
    let end = if all_day { moment + 1d } else { moment + Duration((minutes ?? 60) * 60) }
    let start_wall = moment.wall("floating") ?? "{d.iso()}T00:00:00"
    let end_wall = end.wall("floating") ?? start_wall
    c.events.insert(
        Event(
            title: title,
            start: start_wall,
            end: end_wall,
            all_day: all_day,
            day: d,
            end_day: end_day_of(all_day, Date.parse(end_wall, near: now) ?? today),
            timezone: "floating",
            calendar: c.calendars.first(),
        ),
    )
}

/// Makes an event of the hours dragged across, from where the drag began to where it ended.
action new_between(c: Calendars, start: Time, end: Time) -> Event uses time (label: "New event") {
    require end > start "A drag goes forward in time"
    let minutes = (end - start).seconds() / 60
    new_event(c, at: .some(start), minutes: .some(if minutes < 15 { 15 } else { minutes }))
}

/// Moves an event where it was dropped: a day chip moves to that day keeping its time of day,
/// and an hour moves to the hour it was dropped at. A repeating event waits for its own answer.
action dropped(e: Event, start: Time, place: Text) uses time (label: "Move") {
    if e.recurrence == "" {
        if place == "month" { move_to_day(e, start.day()) } else { change_start(e, start) }
    }
}

/// Ends an event where its edge was dropped. A repeating event waits for its own answer.
action ended(e: Event, end_at: Time) uses time (label: "Resize") {
    if e.recurrence == "" { change_end(e, end_at) }
}

/// Files an event under a project, or files it under none.
action file_event(e: Event, p: Option<Project>) (label: "Project", icon: .folder) {
    e.filed_under = p
}

/// Sets the colour an event is drawn in.
action set_event_color(e: Event, color: Color) (icon: .palette, label: "Colour") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    e.color = color_name(color)
}

/// Takes an event's own colour off: it is drawn in its calendar's.
action clear_event_color(e: Event) (icon: .x, label: "Its calendar's colour") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    e.color = ""
}

/// Sets the colour a calendar is drawn in.
action set_calendar_color(k: Calendar, color: Color) (icon: .palette, label: "Colour") {
    k.color = color_name(color)
}

/// Sets the categories an event is filed under in its person's words.
action set_categories(e: Event, categories: List<Text>) (label: "Categories", icon: .tag) {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    for old in e.categories.map(c -> c) {
        e.categories.remove(old)
    }
    for c in categories {
        e.categories.add(c)
    }
}

/// Sets the zone an event's wall times are read in: `Europe/Berlin`, `UTC`, or `floating` for
/// the same time of day everywhere.
action set_zone(e: Event, zone: Text) uses time (icon: .globe, label: "Time zone") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require zone.trim() != "" "A zone is floating, UTC, or a name such as Europe/Berlin"
    require Time.from_wall(zone.trim(), "1970-01-01T00:00:00") != .none "That is not a time zone this build knows"
    e.timezone = zone.trim()
}

/// Files an event under one more category.
action add_category(e: Event, category: Text) (icon: .tag, label: "Add a category") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require category.trim() != "" "A category is a few words"
    e.categories.add(category.trim())
}

/// Takes a category off an event.
action remove_category(e: Event, category: Text) (icon: .x, label: "Remove a category") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    e.categories.remove(category)
}

/// Makes someone the organiser of an event.
action organize(e: Event, who: Text) (label: "Organiser", icon: .user) {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    e.organizer = who
}

/// Reminds the people coming when the event starts.
action remind_at_start(e: Event) (label: "At the start", icon: .bell, group: "Remind") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    e.remind = starts(e)
}

/// Reminds them a given number of minutes before it starts.
action remind_before(e: Event, minutes: Int) (label: "Before", icon: .bell, group: "Remind") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require minutes >= 0 "A reminder is before the start"
    e.remind = starts(e) - Duration(minutes * 60)
}

/// Tells nobody, once more.
action remind_off(e: Event) (label: "No reminder", icon: .bell_off, group: "Remind") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require e.remind != Time("1970-01-01T00:00:00Z") "It has no reminder"
    e.remind = Time("1970-01-01T00:00:00Z")
}

/// Repeats an event, its rule in iCalendar's words, how often, and where it ends.
action repeat_rule(
    e: Event,
    how: Repeat,
    every: Int = 1,
    weekdays: List<Text> = [],
    until: Option<Date> = .none,
    count: Option<Int> = .none,
) (icon: .repeat, label: "Repeat") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    e.repeat = how
    e.recurrence_every = every
    for old in e.recurrence_weekdays.map(d -> d) {
        e.recurrence_weekdays.remove(old)
    }
    for d in weekdays {
        e.recurrence_weekdays.add(d)
    }
    e.recurrence_until = until
    e.recurrence_count = count
    e.recurrence = frequency_of(e)
}

/// Repeats an event on chosen days of the week from the day it stands on.
action repeat_weekdays(e: Event, days: List<Text>) (icon: .repeat, label: "Every week on") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require not days.is_empty() "Choose the days it repeats on"
    repeat_rule(e, .weekdays, weekdays: days)
}

/// Stops an event repeating from the day it stands on.
action repeat_off(e: Event) (icon: .x, label: "Does not repeat") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require e.recurrence != "" "It does not repeat"
    e.recurrence = ""
    e.repeat = .none
    e.recurrence_every = 1
    for d in e.recurrence_weekdays.map(x -> x) {
        e.recurrence_weekdays.remove(d)
    }
    e.recurrence_until = .none
    e.recurrence_count = .none
}

/// Skips one date of a repeating event.
action skip_date(e: Event, day: Date) (icon: .calendar, label: "Skip this date") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require e.recurrence != "" "It does not repeat"
    e.exdates.add(day.iso())
}

/// Gives a skipped date of a repeating event back.
action restore_date(e: Event, day: Date) (icon: .calendar, label: "Restore this date") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    e.exdates.remove(day.iso())
}

/// Adds a task to the inbox or to a project.
action new_task(
    c: Calendars,
    p: Option<Project> = .none,
    due: Option<Date> = .none,
    title: Text = "",
) -> Task (icon: .plus, label: "New task", key: "Ctrl+Shift+N") {
    let task = c.tasks.insert(Task(title: title, due: due, project: p))
    task
}

/// Checks a task off, or opens it again.
action check_task(t: Task, done: Bool = not t.done) (
    label: if done { "Complete" } else { "Reopen" },
    icon: .check,
    on: t.done,
) {
    t.done = done
}

/// Sets the day a task is due by.
action due_task(t: Task, due: Option<Date>) uses time (label: "Pick date", icon: .calendar) {
    t.due = due
}

/// Makes a task due today.
action due_task_today(t: Task) uses time (label: "Today", icon: .star, group: "Due") {
    t.due = today
}

/// Makes a task due tomorrow.
action due_task_tomorrow(t: Task) uses time (label: "Tomorrow", icon: .calendar, group: "Due") {
    t.due = today + 1d
}

/// Makes a task due at the start of next week.
action due_task_next_week(t: Task) uses time (label: "Next week", icon: .calendar, group: "Due") {
    t.due = today.start_of_week() + 1w
}

/// Clears the day a task is due by.
action due_task_clear(t: Task) (label: "No date", group: "Due") {
    require t.due != .none "It has no date"
    t.due = .none
}

/// Sets how much a task matters.
action set_task_priority(t: Task, priority: Priority) (icon: .flag, label: "Priority") {
    t.priority = priority
}

/// Files a task under a project, or under none for the inbox.
action file_task(t: Task, p: Option<Project>) (label: "Project", icon: .folder) {
    t.project = p
}

/// Adds a step under a task, in its project, and opens the task if it was done: there is work
/// under it now.
action add_step(c: Calendars, t: Task, title: Text) -> Task (
    label: "Add a step",
    icon: .list_tree,
) {
    let made = c.tasks.insert(Task(title: title, project: t.project, parent: t))
    if t.done {
        t.done = false
    }
    made
}

/// Deletes a task and the steps under it.
destructive action remove_task(c: Calendars, t: Task) (icon: .trash, label: "Delete task") {
    for x in c.tasks.descendants(t, .parent) {
        c.tasks.remove(x)
    }
    c.tasks.remove(t)
}

/// Starts a project.
action new_project(c: Calendars) -> Project (icon: .folder_plus, label: "New project") {
    c.projects.insert(Project(name: std.say("New project"), order: c.projects.len()))
}

/// Sets the colour a project is drawn in.
action set_project_color(p: Project, color: Color) (icon: .palette, label: "Colour") {
    p.color = color
}

/// Moves a project one place earlier or later among the projects.
action move_project(c: Calendars, p: Project, up: Bool) (icon: .arrow_up, label: "Move") {
    let ordered = c.projects.sort_by(x -> x.order)
    let at = ordered.index_of(p) ?? 0
    let other_at = if up { at - 1 } else { at + 1 }
    if let other = ordered.at(other_at) {
        let mine = p.order
        p.order = other.order
        other.order = mine
    }
}

/// Renames a project.
action rename_project(p: Project, name: Text) {
    p.name = name
}

/// Deletes a project; its events and tasks go back to none.
destructive action remove_project(c: Calendars, p: Project) (
    icon: .trash,
    label: "Delete project",
) {
    for e in c.events.filter(e -> e.filed_under == .some(p)) {
        e.filed_under = .none
    }
    for t in c.tasks.filter(t -> t.project == .some(p)) {
        t.project = .none
    }
    c.projects.remove(p)
}

/// The date of a series that stands apart, if one does: the event filed under the series for
/// that date.
fn apart(e: Event, at: Time) -> Option<Event> {
    let wall = at.wall(zone_of(e)) ?? ""
    calendar.events.matching(Event.recurrence_of(e)).find(o -> o.original_start == wall)
}

/// Moves one date of a repeating event: an event of its own stands in its place and the series
/// keeps every other date.
action move_occurrence(e: Event, at: Time, start: Time, end: Time) uses time (
    icon: .calendar,
    label: "Only this event",
) {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require e.recurrence != "" "It does not repeat"
    let zone = zone_of(e)
    let day = start.day()
    let start_wall = start.wall(zone) ?? e.start
    let end_wall = end.wall(zone) ?? e.end
    let from_wall = at.wall(zone) ?? e.start
    if let o = apart(e, at) {
        o.start = start_wall
        o.end = end_wall
        o.day = day
        o.end_day = end_day_of(e.all_day, Date.parse(end_wall, near: now) ?? today)
    } else {
        let made = calendar.events.insert(
            Event(
                title: e.title,
                start: start_wall,
                end: end_wall,
                all_day: e.all_day,
                timezone: e.timezone,
                location: e.location,
                notes: e.notes,
                calendar: e.calendar,
                color: e.color,
                filed_under: e.filed_under,
                recurrence_of: .some(e),
                original_start: from_wall,
                day: day,
                end_day: end_day_of(e.all_day, Date.parse(end_wall, near: now) ?? today),
            ),
        )
        for c in e.categories.map(x -> x) {
            made.categories.add(c)
        }
    }
}

/// Cancels one date of a repeating event.
action cancel_occurrence(e: Event, at: Time) uses time (icon: .x, label: "Cancel this date") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require e.recurrence != "" "It does not repeat"
    let zone = zone_of(e)
    let wall = at.wall(zone) ?? e.start
    let from_wall = wall
    if let o = apart(e, at) {
        o.cancelled = true
    } else {
        calendar.events.insert(
            Event(
                title: e.title,
                start: wall,
                end: wall,
                timezone: e.timezone,
                calendar: e.calendar,
                recurrence_of: .some(e),
                original_start: from_wall,
                cancelled: true,
                day: Date.parse(wall, near: now) ?? today,
                end_day: Date.parse(wall, near: now) ?? today,
            ),
        )
    }
}

/// Gives a cancelled or moved date of a series back.
destructive action restore_occurrence(e: Event, at: Time) (
    icon: .undo_2,
    label: "Restore this date",
) {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    if let o = apart(e, at) { calendar.events.remove(o) }
}

/// Moves a repeating event from one date on: the series ends the day before, and a new one
/// begins where it was moved to, with every date after it.
action move_following(e: Event, at: Time, start: Time, end: Time) uses time (
    icon: .arrow_right,
    label: "This and following events",
) {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require e.recurrence != "" "It does not repeat"
    let zone = zone_of(e)
    let start_wall = start.wall(zone) ?? e.start
    let end_wall = end.wall(zone) ?? e.end
    let cut = at.day() - 1d
    let last = e.recurrence_until
    e.recurrence_until = .some(cut)
    e.recurrence_count = .none
    let made = calendar.events.insert(
        Event(
            title: e.title,
            start: start_wall,
            end: end_wall,
            all_day: e.all_day,
            timezone: e.timezone,
            location: e.location,
            notes: e.notes,
            recurrence: e.recurrence,
            recurrence_every: e.recurrence_every,
            recurrence_until: last,
            calendar: e.calendar,
            color: e.color,
            filed_under: e.filed_under,
            organizer: e.organizer,
            day: start.day(),
            end_day: end_day_of(e.all_day, Date.parse(end_wall, near: now) ?? today),
        ),
    )
    for c in e.categories.map(x -> x) {
        made.categories.add(c)
    }
    for d in e.recurrence_weekdays.map(x -> x) {
        made.recurrence_weekdays.add(d)
    }
    // The tail is the new series, so the date it was moved to is not also the old one's.
    skip_date(e, at.day())
}

/// Stands the window on the day a moment names.
action go_to_day(c: Calendars, at: Time) uses time (icon: .calendar, label: "Show this day") {
    c.anchor_at = at
}

/// Shows the day before.
action previous_day(c: Calendars) (icon: .chevron_left, label: "Previous day", group: "Window") {
    c.anchor_at = c.anchor_at - 1d
}

/// Shows the day after.
action next_day(c: Calendars) (icon: .chevron_right, label: "Next day", group: "Window") {
    c.anchor_at = c.anchor_at + 1d
}

/// Shows today.
action this_day(c: Calendars) uses time (icon: .calendar, label: "Today", group: "Window") {
    c.anchor_at = midnight(today, now)
}

/// Asks each account's server for its calendars now, so what it holds comes in as it does when
/// the server is heard from.
action sync(c: Calendars) (icon: .refresh, label: "Sync calendars") {
    for k in c.calendars {
        if k.account != "" { outside("caldav.check", k.account) }
    }
}

/// Adds a calendar of the workspace's own.
action new_calendar(c: Calendars) -> Calendar (icon: .calendar, label: "New calendar") {
    c.calendars.insert(Calendar(name: std.say("New calendar")))
}

/// Renames a calendar.
action rename_calendar(k: Calendar, name: Text) {
    k.name = name
}

/// Deletes an event.
destructive action remove(c: Calendars, e: Event) (
    icon: .trash,
    label: "Delete event",
    key: "Delete",
) {
    c.events.remove(e)
}

/// Moves an event to a day, keeping the time of day it starts and how long it lasts, and its
/// reminder at the same distance from its start.
action move_to_day(e: Event, day: Date) uses time {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require e.day != day "It is on that day already"
    let zone = zone_of(e)
    let by = day - starts(e).day()
    if e.remind != Time("1970-01-01T00:00:00Z") {
        e.remind = e.remind + by
    }
    e.start = wall_moved(e.start, zone, day)
    e.end = wall_moved(e.end, zone, day)
    e.day = day
    e.end_day = end_day_of(e.all_day, Date.parse(e.end, near: now) ?? today)
}

/// Moves an event to the day a moment names, keeping the time of day it starts.
action change_day(e: Event, at: Time) uses time {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    move_to_day(e, at.day())
}

/// Sets the time of day an event starts, its end keeping how long it lasts. A day the moment
/// names is the day it moves to.
action change_start(e: Event, at: Time) uses time {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    let zone = zone_of(e)
    let length = ends(e) - starts(e)
    if e.remind == starts(e) {
        e.remind = at
    }
    e.start = at.wall(zone) ?? e.start
    e.end = (at + length).wall(zone) ?? e.end
    // The day is the wall's own, where the event's zone reads it.
    e.day = Date.parse(e.start, near: now) ?? today
    e.end_day = end_day_of(e.all_day, Date.parse(e.end, near: now) ?? today)
}

/// Sets when an event ends.
action change_end(e: Event, at: Time) uses time {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require at > starts(e) "An event ends after it starts"
    e.end = at.wall(zone_of(e)) ?? e.end
    e.end_day = end_day_of(e.all_day, Date.parse(e.end, near: now) ?? today)
}

/// Repeats an event every day, week, month or year from the day it stands on, on the days
/// chosen of a week, and no longer on the days it named before.
action repeat(e: Event, how: Repeat) (icon: .repeat, label: "Repeat") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    repeat_rule(e, how)
}

/// How often it repeats, in the unit it repeats in: every two weeks is 2.
action repeat_every(e: Event, every: Int) (icon: .repeat, label: "How often") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require every >= 1 "It repeats at least once"
    repeat_rule(e, e.repeat, every: every)
}

/// The last day it repeats by, or none for a series that does not end.
action repeat_until(e: Event, until: Option<Date>) (icon: .calendar, label: "Repeat until") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    repeat_rule(e, e.repeat, until: until)
}

/// The last day it repeats by, as the day a moment names.
action repeat_until_at(e: Event, at: Time) uses time (icon: .calendar, label: "Repeat until") {
    repeat_until(e, .some(at.day()))
}

/// How many times it repeats in all, or none for a series that does not end.
action repeat_count(e: Event, count: Option<Int>) (icon: .hash, label: "Times") {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    repeat_rule(e, e.repeat, count: count)
}

/// Files an event under a calendar the person picks.
action file_in(e: Event, k: Calendar) (label: "Calendar", icon: .calendar) {
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require e.calendar != .some(k) "It is in that calendar already"
    e.calendar = k
}

/// Invites someone to an event, by the name and address the person gives.
action invite(e: Event, name: Text, email: Text) -> Attendee (icon: .user_plus, label: "Invite") {
    require email.trim().contains("@") "That is not an email address"
    require not e.attendees.any(g -> g.email == email.trim()) "They are coming already"
    e.attendees.insert(Attendee(name: name.trim(), email: email.trim()))
}

/// Records someone's answer to an invitation: `accepted`, `declined` or `tentative`.
action answer(e: Event, g: Attendee, status: Text) (icon: .check, label: "Answer") {
    require status == "accepted" or status == "declined" or status == "tentative" "An answer is accepted, declined or maybe"
    g.status = status
}

/// Takes someone off an event's guests: nothing is sent to them.
action uninvite(e: Event, g: Attendee) (icon: .x, label: "Remove guest") {
    e.attendees.remove(g)
}

/// Starts a message inviting a guest, the event's calendar invitation carried beside it: what Mail
/// shows the person, to read and send. Nothing is sent by this.
action invite_by_mail(e: Event, g: Attendee) (icon: .mail, label: "Invite by mail") {
    mail.prepare_calendar_invitation(
        mail.mail,
        g.email,
        std.say("Invitation: {e.title}"),
        std.say(
            "You are invited to {e.title} on {e.day} at {e.start}. The attached calendar invitation holds the details.",
        ),
        invitation(e, g),
    )
}

/// Tells the people coming that an event starts, once.
trigger ring(e: Event) at e.remind: ring_due(e)

/// Rings an event's reminder when its moment comes, and nothing for an event that has none.
action ring_due(e: Event) uses notify {
    if e.remind != Time("1970-01-01T00:00:00Z") { remind_now(e) }
}

/// Tells the people coming that an event starts, once, and clears its reminder.
action remind_now(e: Event) uses notify {
    require e.remind != Time("1970-01-01T00:00:00Z") "It has no reminder"
    e.remind = Time("1970-01-01T00:00:00Z")
    for who in workspace.members {
        notify(to: who, title: e.title, body: std.say("Starts {e.start}"), opens: e)
    }
}

/// Goes to the week an event is on.
action go_to(e: Event) uses time (icon: .calendar, label: "Show in calendar") {
    calendar.anchor_at = midnight(e.day.start_of_week(), now)
}

/// The window shows the week before.
action previous_week(c: Calendars) (icon: .chevron_left, label: "Previous week", group: "Window") {
    c.anchor_at = c.anchor_at - 1w
}

/// The window shows the week after.
action next_week(c: Calendars) (icon: .chevron_right, label: "Next week", group: "Window") {
    c.anchor_at = c.anchor_at + 1w
}

/// The window shows the month before.
action previous_month(c: Calendars) uses time (
    icon: .chevron_left,
    label: "Previous month",
    group: "Window",
) {
    let first = first_of_month(c.anchor_at.day())
    c.anchor_at = midnight(first_of_month(first - 1d), now)
}

/// The window shows the month after.
action next_month(c: Calendars) uses time (
    icon: .chevron_right,
    label: "Next month",
    group: "Window",
) {
    let next = first_of_month(first_of_month(c.anchor_at.day()) + 32d)
    c.anchor_at = midnight(next, now)
}

/// The window shows this week.
action this_week(c: Calendars) uses time (icon: .calendar, label: "Today", group: "Window") {
    c.anchor_at = midnight(today.start_of_week(), now)
}

/// How an event repeats: every day, week, month or year, or on chosen days of the week.
enum Repeat {
    none
    /// Every day
    daily
    /// Every week
    weekly
    /// Every month
    monthly
    /// Every year
    yearly
    /// The days chosen of every week
    weekdays
}

/// A palette place as a calendar server writes it: its name.
fn color_name(c: Color) -> Text {
    if c == .indigo {
        "indigo"
    } else if c == .emerald {
        "emerald"
    } else if c == .amber {
        "amber"
    } else if c == .red {
        "red"
    } else if c == .blue { "blue" } else if c == .violet { "violet" } else { "slate" }
}

/// Whether an event may be changed: its calendar's server keeps some for reading only.
fn writable(e: Event) -> Bool {
    if let k = e.calendar { k.writable } else { true }
}

/// The palette place a colour's name stands for, as a calendar server writes the name.
fn color_of(name: Text) -> Color {
    if name == "emerald" {
        .emerald
    } else if name == "amber" {
        .amber
    } else if name == "red" {
        .red
    } else if name == "blue" {
        .blue
    } else if name == "violet" { .violet } else if name == "slate" { .slate } else { .indigo }
}

/// A palette place in the person's words.
fn color_label(name: Text) -> Text {
    if name == "emerald" {
        std.say("Emerald")
    } else if name == "amber" {
        std.say("Amber")
    } else if name == "red" {
        std.say("Red")
    } else if name == "blue" {
        std.say("Blue")
    } else if name == "violet" {
        std.say("Violet")
    } else if name == "slate" { std.say("Slate") } else { std.say("Indigo") }
}

/// The zone an event's wall times are read in: `floating`, `UTC` or an IANA name.
fn zone_of(e: Event) -> Text {
    e.timezone
}

/// The moment an event starts: its wall time where the zone it names says, or where the reader
/// is for a floating one.
fn starts(e: Event) -> Time {
    Time.from_wall(zone_of(e), e.start) ?? Time("1970-01-01T00:00:00Z")
}

/// The moment an event ends, the same way.
fn ends(e: Event) -> Time {
    Time.from_wall(zone_of(e), e.end) ?? Time("1970-01-01T00:00:00Z")
}

/// A wall time moved to be on `day`, keeping its time of day.
fn wall_moved(wall: Text, zone: Text, day: Date) -> Text {
    if let at = Time.from_wall(zone, wall) {
        (at + (day - at.day())).wall(zone) ?? wall
    } else { wall }
}

/// The midnight of a day where the person is, as the action reading the clock resolves it: the
/// moment the window stands on for that day.
fn midnight(d: Date, now: Time) -> Time {
    Time.parse("{d.iso()}T00:00:00", near: now) ?? now
}

/// The last day an event is on: its end's day, or for a whole-day event the midnight before.
fn end_day_of(all_day: Bool, last: Date) -> Date {
    if all_day { last - 1d } else { last }
}

/// The next quarter hour after `at`, on the clock: what New event starts one at.
fn next_quarter(at: Time) -> Time {
    let rest = at.time_of_day().seconds() % 900
    at + Duration(if rest == 0 { 0 } else { 900 - rest })
}

/// The rule an event's repeat is, in iCalendar's words: what the shared expander reads.
/// The frequency an event repeats at, as a calendar writes it; empty for one that does not.
fn frequency_of(e: Event) -> Text {
    if e.repeat == .daily {
        "FREQ=DAILY"
    } else if e.repeat == .weekly or e.repeat == .weekdays {
        "FREQ=WEEKLY"
    } else if e.repeat == .monthly {
        "FREQ=MONTHLY"
    } else if e.repeat == .yearly { "FREQ=YEARLY" } else { "" }
}

/// One part of an iCalendar rule, by its name (`INTERVAL`), or empty where it names none.
fn rule_part(rule: Text, name: Text) -> Text {
    let part = rule.split(";").find(p -> p.starts_with("{name}=")) ?? ""
    if part == "" {
        ""
    } else {
        part.slice(name.len() + 1, part.len()).trim()
    }
}

/// The frequency of an iCalendar rule, as an event keeps it: `FREQ=WEEKLY`.
fn frequency_in(rule: Text) -> Text {
    let value = rule_part(rule, "FREQ").upper()
    if value == "" { "" } else { "FREQ={value}" }
}

/// How an event repeats, read from its rule's frequency.
fn repeat_in(rule: Text) -> Repeat {
    let value = rule_part(rule, "FREQ").upper()
    if value == "DAILY" {
        .daily
    } else if value == "WEEKLY" {
        if rule_part(rule, "BYDAY") == "" { .weekly } else { .weekdays }
    } else if value == "MONTHLY" { .monthly } else if value == "YEARLY" { .yearly } else { .none }
}

/// The whole rule an event's repeat is, in iCalendar's words: what the shared expander reads
/// and what a calendar file carries.
fn rule_of(e: Event) -> Text {
    let freq = e.recurrence
    if freq == "" {
        ""
    } else {
        let parts: List<Text> = [
            freq,
            if e.recurrence_every > 1 { "INTERVAL:{e.recurrence_every}" } else { "" },
            if let u = e.recurrence_until { "UNTIL:{stamp_day(u)}" } else { "" },
            if let n = e.recurrence_count { "COUNT:{n}" } else { "" },
            if e.recurrence_weekdays.is_empty() {
                ""
            } else {
                "BYDAY:{e.recurrence_weekdays.sort_by(d -> d).join(",")}"
            },
        ]
        parts.filter(x -> x != "").join(";")
    }
}

/// The first day of the month `d` is in.
fn first_of_month(d: Date) -> Date {
    d - Duration((d.day() - 1) * 86400)
}

/// The events running through any day from `first` to `last`, both included, in the calendar
/// chosen when one is.
fn between(
    c: Calendars,
    k: Option<Calendar>,
    first: Date,
    last: Date,
    hidden: Text,
) -> List<Event> {
    let within: List<Event> = if let chosen = k {
        c.events.matching(Event.calendar(chosen)).map(e -> e)
    } else { c.events.filter(e -> true) }
    within.filter(e -> e.day <= last and e.end_day >= first and not e.cancelled and kept(e, hidden))
}

/// Whether an event answers the window's search.
fn named(e: Event, q: Text) -> Bool {
    q.is_empty() or e.title.lower().contains(q.lower()) or e.location.lower().contains(q.lower())
}

/// When a repeating event comes between two days, both included, as the shared expander reads
/// its rule: a date that stands apart is an event of its own filed under the series, so the
/// series leaves that date to it.
fn meets(e: Event, from: Date, to: Date) -> List<Time> {
    if e.recurrence == "" {
        []
    } else {
        std
            .occurrences(rule_of(e), e.start, read_in(e), e.exdates, from, to)
            .map(at -> placed(e, at))
            .filter(at -> apart(e, at) == .none)
    }
}

/// What a day holds of a repeating event: its occurrence on that day, unless that day is the
/// series' own first, which its own event stands for.
fn meets_on(e: Event, d: Date) -> List<Time> {
    meets(e, d, d).filter(at -> at != starts(e))
}

/// An event at one of its moments: the time it is shown at and the day that time is on in the
/// event's own zone, in order with every other.
type Moment {
    event: Event
    at: Time
    day: Date
}

/// The zone the shared expander reads an event's rule in: the event's own, and UTC for a wall
/// time written nowhere in particular, which the expander places there and this app places where
/// the reader is.
fn read_in(e: Event) -> Text {
    if zone_of(e) == "floating" { "UTC" } else { zone_of(e) }
}

/// An occurrence's moment where the reader is: a wall time written nowhere in particular is
/// placed in their zone, as the event itself is.
fn placed(e: Event, at: Time) -> Time {
    if zone_of(e) == "floating" {
        if let wall = at.wall("UTC") { Time.from_wall("floating", wall) ?? at } else { at }
    } else { at }
}

/// The day a moment stands on in an event's calendar: the day people in its zone read, or the
/// person's own for a time that keeps its wall clock everywhere.
fn day_of(e: Event, at: Time) -> Date {
    at.day_in(zone_of(e))
}

/// The moments an event is shown at between two days, both included: its own start for an event
/// that does not repeat, and the occurrences of one that does.
fn moments(e: Event, from: Date, to: Date) -> List<Time> {
    if e.recurrence == "" { [starts(e)] } else { meets(e, from, to) }
}

/// Whether two events take any of the same time: an event that repeats stands on every date it
/// meets between the other's days.
fn clashes_with(o: Event, e: Event) -> Bool {
    if o.all_day or e.all_day {
        false
    } else if o.recurrence == "" {
        starts(o) < ends(e) and ends(o) > starts(e)
    } else {
        meets(o, e.day, e.end_day)
            .any(at -> at < ends(e) and at + (ends(o) - starts(o)) > starts(e))
    }
}

/// The other events in the same calendar that take any of an event's time, by their titles.
fn clashing(c: Calendars, e: Event) -> List<Text> {
    c.events
        .filter(
            o -> o != e and o.calendar == e.calendar and not o.cancelled and clashes_with(o, e),
        )
        .map(o -> o.title)
}

/// Whether a moment shown answers what the window's search holds.
fn found(r: Moment, q: Text) -> Bool {
    q.is_empty()
        or r.event.title.lower().contains(q.lower())
        or r.event.location.lower().contains(q.lower())
}

/// The events that repeat, in the calendar chosen when one is.
fn repeating_in(c: Calendars, k: Option<Calendar>, hidden: Text) -> List<Event> {
    if let chosen = k {
        c.events.matching(Event.repeating, Event.calendar(chosen)).filter(e -> kept(e, hidden))
    } else {
        c.events.matching(Event.repeating).filter(e -> kept(e, hidden))
    }
}

/// The calendars the window keeps out of it, as their ids.
fn hidden_ids(hidden: Text) -> List<Text> {
    hidden.split(",").filter(id -> id != "")
}

/// Whether an event is in a calendar the window shows: an event of the workspace's own always is.
fn kept(e: Event, hidden: Text) -> Bool {
    if let k = e.calendar { not hidden_ids(hidden).contains(k.id()) } else { true }
}

/// The calendars the window keeps out, with one more kept out.
fn kept_out(hidden: Text, id: Text) -> Text {
    let kept = hidden_ids(hidden).filter(x -> x != id).join(",")
    if kept == "" { id } else { "{kept},{id}" }
}

/// The calendars the window keeps out, with one shown again.
fn shown_again(hidden: Text, id: Text) -> Text {
    hidden_ids(hidden).filter(x -> x != id).join(",")
}

/// What a task list is called, in the window's title and its empty state.
fn task_list_name(smart: Text) -> Text {
    if smart == "inbox" {
        std.say("Inbox")
    } else if smart == "upcoming" {
        std.say("Upcoming")
    } else if smart == "done" { std.say("Done") } else { std.say("Today") }
}

/// Repeats an event how often the person typed, in the unit it repeats in: 2 for every other.
action repeat_every_text(e: Event, typed: Text) (icon: .repeat, label: "How often") {
    require typed.trim().to_int() != .none "How often it repeats is a number: 2 for every other"
    repeat_every(e, typed.trim().to_int() ?? 1)
}

/// Repeats an event as many times as the person typed.
action repeat_count_text(e: Event, typed: Text) (icon: .hash, label: "Times") {
    require typed.trim().to_int() != .none "How many times it repeats is a number"
    repeat_count(e, .some(typed.trim().to_int() ?? 1))
}

/// An event's invitation as a calendar server writes one: the event, who it is for, and no line
/// that would have a server write to anyone.
fn invitation(e: Event, g: Attendee) -> Text {
    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Tilo//Calendar//EN\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:{e.id()}\r\nDTSTAMP:{stamp(starts(e))}\r\nDTSTART:{stamp(starts(e))}\r\nDTEND:{stamp(ends(e))}\r\nSUMMARY:{e.title}\r\nLOCATION:{e.location}\r\nATTENDEE;CN={g.name};PARTSTAT=NEEDS-ACTION;RSVP=TRUE;SCHEDULE-AGENT=CLIENT:mailto:{g.email}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
}

/// What an event says, as a calendar writes it: the person's own words, else the server's.
fn describe(e: Event) -> Text {
    if e.body.text.trim() != "" { e.body.text } else { e.notes }
}

/// One event as a calendar server writes it, its lines joined.
fn ics_event(e: Event) -> Text {
    let times: List<Text> = if e.all_day {
        ["DTSTART;VALUE=DATE:{stamp_day(e.day)}", "DTEND;VALUE=DATE:{stamp_day(e.end_day + 1d)}"]
    } else {
        [time_line("DTSTART", e.start, zone_of(e)), time_line("DTEND", e.end, zone_of(e))]
    }
    let skipped: List<Text> = if e.exdates.is_empty() {
        []
    } else {
        [e.exdates.sort_by(d -> d).map(d -> "EXDATE;VALUE=DATE:{d.replace("-", "")}").join("\r\n")]
    }
    let alarm: List<Text> = if e.remind != Time("1970-01-01T00:00:00Z") {
        [
            [
                "BEGIN:VALARM",
                "ACTION:DISPLAY",
                "TRIGGER:-PT{(starts(e) - e.remind).seconds() / 60}M",
                "END:VALARM",
            ]
                .join("\r\n"),
        ]
    } else { [] }
    let groups: List<Text> = [
        [
            "BEGIN:VEVENT",
            "UID:{e.id()}",
            "X-TILO-ID:{e.id()}",
            "DTSTAMP:{stamp(starts(e))}",
            "SUMMARY:{e.title}",
            "LOCATION:{e.location}",
            "DESCRIPTION:{describe(e)}",
            if e.color == "" { "" } else { "X-TILO-COLOR:{e.color}" },
            if e.organizer == "" { "" } else { "ORGANIZER:mailto:{e.organizer}" },
        ]
            .filter(x -> x != "")
            .join("\r\n"),
        times.join("\r\n"),
        if e.recurrence == "" { "" } else { "RRULE:{rule_of(e)}" },
        skipped.join("\r\n"),
        e.attendees
            .map(
                g -> "ATTENDEE;CN={g.name};ROLE={g.role};PARTSTAT={status_ical(g.status)};RSVP=TRUE;SCHEDULE-AGENT=CLIENT:mailto:{g.email}",
            )
            .join("\r\n"),
        e.categories.map(c -> "CATEGORIES:{c}").join("\r\n"),
        alarm.join("\r\n"),
        "END:VEVENT",
    ]
    groups.filter(g -> g != "").join("\r\n")
}

/// One task as a calendar server writes it, its lines joined.
fn ics_task(t: Task) -> Text {
    let groups: List<Text> = [
        [
            "BEGIN:VTODO",
            "UID:{t.id()}",
            "SUMMARY:{t.title}",
            "DESCRIPTION:{t.notes.text}",
            "PRIORITY:{priority_ical(t.priority)}",
            "STATUS:{task_status(t.done)}",
        ]
            .join("\r\n"),
        if let due = t.due { "DUE;VALUE=DATE:{stamp_day(due)}" } else { "" },
        "END:VTODO",
    ]
    groups.filter(g -> g != "").join("\r\n")
}

/// The calendar — every event and task it holds, or only one event — as an iCalendar file, for
/// a person to save or an agent to hand on. Nothing is written by this.
action export_calendar(c: Calendars, e: Option<Event> = .none) -> File (
    icon: .download,
    label: "Export calendar",
) {
    let events = if let one = e { [one] } else { c.events.map(x -> x) }
    let tasks: List<Task> = if e == .none { c.tasks.map(x -> x) } else { [] }
    let text = [
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        "PRODID:-//Tilo//Calendar//EN",
        events.map(x -> ics_event(x)).join("\r\n"),
        tasks.map(x -> ics_task(x)).join("\r\n"),
        "END:VCALENDAR",
    ]
        .filter(x -> x != "")
        .join("\r\n")
    File.of_text("calendar.ics", text)
}

/// Imports the events and tasks of an iCalendar file, and answers how many it took: what a
/// person's own file offers, and what another calendar exported. A line naming a zone this
/// build cannot read is read in the person's zone.
action import_ics(c: Calendars, file: File) -> Text uses time (
    icon: .upload,
    label: "Import calendar",
    opens: ["text/calendar", .ics, .ical],
) {
    let read = match file.text() {
        .ok(text) => text,
        .err(why) => {
            return std.say("Could not read {file.name}: {why}")
        }
    }
    // A calendar's parts, each a block between its BEGIN and END: what one event or task says,
    // read by its own lines. An event's own blocks (an alarm) end without each being one.
    let events = read.split("END:VEVENT").filter(x -> x.contains("BEGIN:VEVENT"))
    for segment in events {
        let chunk = segment.split("BEGIN:VEVENT").last() ?? ""
        let lines = chunk.split("\n")
        let title = field(lines, "SUMMARY")
        let all_day = chunk.find("DTSTART;VALUE=DATE") != .none
        let zone = import_zone(lines, "DTSTART")
        let start_wall = wall_text(field(lines, "DTSTART"))
        if start_wall != "" {
            let end_wall = if field(lines, "DTEND") == "" {
                start_wall
            } else { wall_text(field(lines, "DTEND")) }
            let rule = field(lines, "RRULE")
            let every = rule_part(rule, "INTERVAL")
            let count = rule_part(rule, "COUNT")
            let until = rule_part(rule, "UNTIL")
            let made = c.events.insert(
                Event(
                    title: title,
                    start: start_wall,
                    end: end_wall,
                    all_day: all_day,
                    timezone: zone,
                    location: field(lines, "LOCATION"),
                    body: RichText(field(lines, "DESCRIPTION")),
                    recurrence: frequency_in(rule),
                    repeat: repeat_in(rule),
                    recurrence_every: if every == "" { 1 } else { every.to_int() ?? 1 },
                    recurrence_until: if until == "" {
                        .none
                    } else { Date.parse(wall_text(until), near: now) },
                    recurrence_count: if count == "" { .none } else { count.to_int() },
                    color: field(lines, "X-TILO-COLOR"),
                    organizer: ics_mailto(field(lines, "ORGANIZER")),
                    day: Date.parse(start_wall, near: now) ?? today,
                    end_day: end_day_of(all_day, Date.parse(end_wall, near: now) ?? today),
                    calendar: c.calendars.first(),
                ),
            )
            for day in rule_part(rule, "BYDAY").split(",") {
                if day.trim() != "" {
                    made.recurrence_weekdays.add(day.trim().upper())
                }
            }
            for line in lines.filter(x -> x.starts_with("EXDATE")) {
                for part in (line.split(":").last() ?? "").split(",") {
                    if let d = Date.parse(wall_text(part), near: now) { made.exdates.add(d.iso()) }
                }
            }
            for line in lines.filter(x -> x.starts_with("ATTENDEE")) {
                if let g = ics_guest(line) { made.attendees.insert(g) }
            }
            if let told = ics_remind(field(lines, "TRIGGER")) {
                if let start = Time.from_wall(zone, start_wall) {
                    made.remind = start - Duration(told * 60)
                }
            }
        }
    }
    let tasks = read.split("END:VTODO").filter(x -> x.contains("BEGIN:VTODO"))
    for segment in tasks {
        let chunk = segment.split("BEGIN:VTODO").last() ?? ""
        let lines = chunk.split("\n")
        let task = c.tasks.insert(
            Task(
                title: field(lines, "SUMMARY"),
                done: field(lines, "STATUS") == "COMPLETED",
                due: Date.parse(wall_text(field(lines, "DUE")), near: now),
                notes: RichText(field(lines, "DESCRIPTION")),
            ),
        )
        task.priority = priority_of(field(lines, "PRIORITY"))
    }
    std.say("Imported {events.len()} events and {tasks.len()} tasks")
}

/// The zone a time line names: its `TZID`, `UTC` for a `Z`, floating where it names neither.
fn import_zone(lines: List<Text>, name: Text) -> Text {
    let named = ics_zone(lines, name)
    if named != "" {
        named
    } else if field(lines, name).trim().ends_with("Z") { "UTC" } else { "floating" }
}

/// The zone a line names in its `TZID` property (`TZID=Europe/Berlin`), or empty where it
/// names none.
fn ics_zone(lines: List<Text>, name: Text) -> Text {
    let line = lines.find(x -> x.starts_with("{name};") and x.contains("TZID=")) ?? ""
    if let at = line.find("TZID=") {
        let rest = line.slice(at + 5, line.len())
        if let colon = rest.find(":") { rest.slice(0, colon).trim() } else { rest.trim() }
    } else { "" }
}

/// A wall time as a calendar line: `DTSTART:20261124T090000` for a floating one, with `Z` for
/// UTC's, and `DTSTART;TZID=Europe/Berlin:20261124T090000` for a named zone's.
fn time_line(name: Text, wall: Text, zone: Text) -> Text {
    let digits = wall.replace("-", "").replace(":", "")
    if zone == "floating" {
        "{name}:{digits}"
    } else if zone == "UTC" { "{name}:{digits}Z" } else { "{name};TZID={zone}:{digits}" }
}

/// The wall time a calendar time line holds, as its own text: `20261124T090000` is
/// `2026-11-24T09:00:00`.
fn wall_text(value: Text) -> Text {
    let trimmed = value.trim()
    let digits = if trimmed.ends_with("Z") { trimmed.slice(0, trimmed.len() - 1) } else { trimmed }
    if digits.len() >= 15 {
        "{digits.slice(0, 4)}-{digits.slice(4, 6)}-{digits.slice(6, 8)}T{digits.slice(9, 11)}:{digits.slice(11, 13)}:{digits.slice(13, 15)}"
    } else if digits.len() >= 8 {
        "{digits.slice(0, 4)}-{digits.slice(4, 6)}-{digits.slice(6, 8)}T00:00:00"
    } else { "" }
}

/// The address a line's `mailto:` names, or empty.
fn ics_mailto(value: Text) -> Text {
    if let at = value.find("mailto:") {
        value.slice(at + 7, value.len()).trim()
    } else { value.trim() }
}

/// One guest as a line names them: `ATTENDEE;CN=Ada;ROLE=required;PARTSTAT=NEEDS-ACTION:mailto:…`.
fn ics_guest(line: Text) -> Option<Attendee> {
    let email = ics_mailto(line)
    if email == "" {
        .none
    } else {
        let name = ics_parameter(line, "CN=")
        let role = ics_parameter(line, "ROLE=")
        return .some(
            Attendee(
                name: if name == "" { email } else { name },
                email: email,
                status: ics_parameter(line, "PARTSTAT=").lower().replace("-", "_"),
                role: if role == "" { "required" } else { role.lower() },
            ),
        )
    }
}

/// The value of the property `parameter` (with its `=`) on a line, empty where the line does
/// not name it.
fn ics_parameter(line: Text, parameter: Text) -> Text {
    if let at = line.find(parameter) {
        let rest = line.slice(at + parameter.len(), line.len())
        let end = if let semicolon = rest.find(";") {
            semicolon
        } else if let colon = rest.find(":") { colon } else { rest.len() }
        rest.slice(0, end).trim()
    } else { "" }
}

/// The minutes before a start a `TRIGGER` line names (`-PT30M`), or none.
fn ics_remind(value: Text) -> Option<Int> {
    let trigger = value.trim()
    if trigger.starts_with("-PT") and trigger.ends_with("M") {
        trigger.slice(3, trigger.len() - 1).to_int()
    } else { .none }
}

/// The value of a line named `name`, without the properties after its name: `SUMMARY:Standup`
/// is `Standup`, and a line the block does not hold is empty.
fn field(lines: List<Text>, name: Text) -> Text {
    let line = lines.find(x -> x.starts_with("{name}:") or x.starts_with("{name};")) ?? ""
    if let at = line.find(":") { line.slice(at + 1, line.len()).trim() } else { "" }
}

/// A task's priority as a calendar server numbers it: 1 high, 5 medium, 9 low, 0 none.
fn priority_ical(p: Priority) -> Text {
    if p == .high { "1" } else if p == .medium { "5" } else if p == .low { "9" } else { "0" }
}

/// The priority a calendar server's number names.
fn priority_of(text: Text) -> Priority {
    let value = text.trim()
    if value == "1" or value == "2" or value == "3" {
        .high
    } else if value == "4" or value == "5" or value == "6" {
        .medium
    } else if value == "7" or value == "8" or value == "9" { .low } else { .none }
}

/// A task's state as a calendar server writes it.
fn task_status(done: Bool) -> Text {
    if done { "COMPLETED" } else { "NEEDS-ACTION" }
}

/// A guest's answer as a calendar server writes it.
fn status_ical(status: Text) -> Text {
    if status == "accepted" {
        "ACCEPTED"
    } else if status == "tentative" {
        "TENTATIVE"
    } else if status == "declined" { "DECLINED" } else { "NEEDS-ACTION" }
}

/// A day as iCalendar writes it: `20261123`.
fn stamp_day(d: Date) -> Text {
    d.iso().replace("-", "")
}

/// A moment as iCalendar writes it: `20261123T090000Z`.
fn stamp(at: Time) -> Text {
    at.iso().replace("-", "").replace(":", "")
}

/// The calendar's window: the week, the month and what is coming, in the sidebar, over the events
/// of the calendars chosen there.
view window(c: Calendars) {
    state place: Text = "month"
    state chosen: Option<Calendar> = .none
    state searched: Text = ""
    // The calendars this person keeps out of the window, as their ids: a display choice of
    // theirs, kept with the window and not the workspace.
    state hidden: Text = ""
    // The task list the window shows, and the project it is narrowed to when one is chosen.
    state smart: Text = "today"
    state task_project: Option<Project> = .none
    // The day the sidebar's own calendar shows ringed, which is where the window goes.
    state picked: Option<Date> = .none
    // A window asked to show an event shows the week it stands on and opens it.
    on show(e: Event): set(place, "week").then(go_to(e)).then(open(e))
    let day = c.anchor_at.day()
    let week_start = day.start_of_week()
    let period = if place == "month" {
        let first = first_of_month(day)
        "{first} – {first_of_month(first + 32d) - 1d}"
    } else if place == "day" {
        "{day}"
    } else if place == "agenda" {
        "{day} – {day + 20d}"
    } else if place == "tasks" {
        if let p = task_project { p.name } else { task_list_name(smart) }
    } else { "{week_start} – {week_start + 6d}" }
    let title = period
    ui.NavShell(content_kind: .page, title: title, sidebar_title: "Calendar") {
        on search(q: Text): set(searched, q)
        actions: {
            ui.SearchButton()
        }
        sidebar: {
            stack {
                ui.NavList(selected: place) {
                    ui.NavItem("week", std.say("Week"), icon: .columns_2) {
                        on select: set(place, "week")
                    }
                    ui.NavItem("month", std.say("Month"), icon: .calendar) {
                        on select: set(place, "month")
                    }
                    ui.NavItem("agenda", std.say("Agenda"), icon: .list) {
                        on select: set(place, "agenda")
                    }
                    ui.NavItem("day", std.say("Day"), icon: .calendar) {
                        on select: set(place, "day")
                    }
                    ui.NavItem("tasks", std.say("Tasks"), icon: .list_checks) {
                        on select: set(place, "tasks")
                    }
                }
                if place == "tasks" {
                    ui.NavSection(std.say("Task lists")) {
                        ui.NavItem("inbox", std.say("Inbox"), icon: .inbox) {
                            on select: set(smart, "inbox").then(set(task_project, .none))
                        }
                        ui.NavItem("today", std.say("Today"), icon: .star) {
                            on select: set(smart, "today").then(set(task_project, .none))
                        }
                        ui.NavItem("upcoming", std.say("Upcoming"), icon: .calendar) {
                            on select: set(smart, "upcoming").then(set(task_project, .none))
                        }
                        ui.NavItem("done", std.say("Done"), icon: .circle_check) {
                            on select: set(smart, "done").then(set(task_project, .none))
                        }
                    }
                    ui.NavSection(std.say("Projects"), add: new_project(c)) {
                        for p in c.projects.sort_by(x -> x.order) {
                            ui.NavItem(p.id(), p.name, icon: .folder, item: p) {
                                on select: set(task_project, .some(p))
                            }
                        }
                    }
                }
                ui.DatePicker(
                    bind: picked,
                    label: std.say("Pick a day"),
                    marked: c.events.filter(e -> not e.cancelled).map(e -> e.day.iso()),
                ) {
                    on change(at): go_to_day(c, at)
                }
                ui.NavSection(std.say("Calendars"), add: new_calendar(c)) {
                    ui.NavItem("all", std.say("All calendars"), icon: .calendar) {
                        on select: set(chosen, .none)
                    }
                    for k in c.calendars {
                        ui.NavItem(k.id(), k.name, icon: .calendar, item: k) {
                            on select: set(chosen, .some(k))
                        }
                        if hidden_ids(hidden).contains(k.id()) {
                            ui.NavItem("{k.id()}-show", std.say("Show {k.name}"), icon: .eye) {
                                on select: set(hidden, shown_again(hidden, k.id()))
                            }
                        } else {
                            ui.NavItem("{k.id()}-hide", std.say("Hide {k.name}"), icon: .eye_off) {
                                on select: set(hidden, kept_out(hidden, k.id()))
                            }
                        }
                    }
                }
            }
        }
        content: {
            if place == "agenda" {
                agenda(c, chosen, searched, hidden)
            } else if place == "tasks" {
                tasks(c, smart, task_project, searched)
            } else {
                timeline(c, chosen, searched, hidden, place)
            }
        }
    }
}

/// The timeline: the week's seven days or one day, its events at their hours and what is all
/// day over them, moved and resized where they stand, and a press on a free hour making one.
///
/// A repeating event that is moved or resized asks, where it was dropped, whether the change is
/// this date's alone or the rest of the series'.
view timeline(
    c: Calendars,
    chosen: Option<Calendar>,
    searched: Text,
    hidden: Text,
    place: State<Text>,
) {
    state zoom: Float = 1.0
    // A repeating event that is moved or resized asks where it was dropped whether the change is
    // that date's alone or the rest of the series'.
    state asked: Option<Event> = .none
    state asked_from: Option<Time> = .none
    state asked_to: Option<Time> = .none
    state asked_end: Option<Time> = .none
    let day = c.anchor_at.day()
    let first = if place == "day" {
        day
    } else if place == "month" { first_of_month(day).start_of_week() } else { day.start_of_week() }
    let last = if place == "day" {
        day
    } else if place == "month" {
        first_of_month(day).start_of_week() + 41d
    } else { day.start_of_week() + 6d }
    let shown = between(c, chosen, first, last, hidden).filter(e -> named(e, searched))
    let repeats = repeating_in(c, chosen, hidden).filter(e -> named(e, searched))
    stack(gap: .group, fill: true) {
        row(gap: .tight) {
            if place == "day" {
                ui.Button(
                    "",
                    icon: .chevron_left,
                    icon_only: true,
                    accessible_name: std.say("Previous day"),
                ) {
                    on press: previous_day(c)
                }
                ui.Button(std.say("Today"), icon: .calendar) {
                    on press: this_day(c)
                }
                ui.Button(
                    "",
                    icon: .chevron_right,
                    icon_only: true,
                    accessible_name: std.say("Next day"),
                ) {
                    on press: next_day(c)
                }
            } else if place == "month" {
                ui.Button(
                    "",
                    icon: .chevron_left,
                    icon_only: true,
                    accessible_name: std.say("Previous month"),
                ) {
                    on press: previous_month(c)
                }
                ui.Button(std.say("Today"), icon: .calendar) {
                    on press: this_week(c)
                }
                ui.Button(
                    "",
                    icon: .chevron_right,
                    icon_only: true,
                    accessible_name: std.say("Next month"),
                ) {
                    on press: next_month(c)
                }
            } else {
                ui.Button(
                    "",
                    icon: .chevron_left,
                    icon_only: true,
                    accessible_name: std.say("Previous week"),
                ) {
                    on press: previous_week(c)
                }
                ui.Button(std.say("Today"), icon: .calendar) {
                    on press: this_week(c)
                }
                ui.Button(
                    "",
                    icon: .chevron_right,
                    icon_only: true,
                    accessible_name: std.say("Next week"),
                ) {
                    on press: next_week(c)
                }
            }
        }
        // The timeline draws the hours, the all-day lane, the month's bars and the now line.
        ui.Timeline(
            label: std.say("Calendar"),
            range: if place == "day" { .day } else if place == "month" { .month } else { .week },
            start: c.anchor_at,
            zoom: zoom,
        ) {
            on created(start: Time, end: Time): new_between(c, start, end)
            on open_day(day: Time): set(place, "day").then(go_to_day(c, day))
            on zoomed(z: Float): set(zoom, z)
            for e in shown {
                timeline_item(e, place, asked, asked_from, asked_to, asked_end)
            }
            for e in repeats {
                for at in meets(e, first, last).filter(t -> t != starts(e)) {
                    timeline_occurrence(e, at, asked, asked_from, asked_to, asked_end)
                }
            }
        }
        if let e = asked {
            if e.recurrence != "" {
                let from = asked_from ?? starts(e)
                let to = asked_to ?? starts(e)
                let end = asked_end ?? to + (ends(e) - starts(e))
                ui.Popover(label: std.say("{e.title} repeats"), open: true, side: .below) {
                    on dismiss: set(asked, .none)
                    stack(gap: .related) {
                        ui.Button(std.say("Only this date"), icon: .calendar) {
                            on press: move_occurrence(e, from, to, end).then(set(asked, .none))
                        }
                        ui.Button(std.say("This and all following"), icon: .arrow_right) {
                            on press: move_following(e, from, to, end).then(set(asked, .none))
                        }
                    }
                }
            }
        }
    }
}

/// One event as the timeline draws it: where it starts and ends, its colour as the theme's own
/// series tone, and what pressing, moving or resizing it does.
template view timeline_item(
    e: Event,
    place: State<Text>,
    asked: State<Option<Event>>,
    asked_from: State<Option<Time>>,
    asked_to: State<Option<Time>>,
    asked_end: State<Option<Time>>,
) {
    ui.TimelineItem(
        e,
        title: e.title,
        start: starts(e),
        end: if e.all_day { ends(e) - 1d } else { ends(e) },
        all_day: e.all_day,
        location: e.location,
        tone: if e.color == "indigo" {
            .first
        } else if e.color == "emerald" {
            .second
        } else if e.color == "amber" {
            .third
        } else if e.color == "red" {
            .fourth
        } else if e.color == "blue" {
            .fifth
        } else if e.color == "violet" {
            .sixth
        } else if e.color == "slate" { .seventh } else { .neutral },
    ) {
        on pressed(item: Event): open(item)
        on moved(item: Event, start: Time): dropped(item, start, place)
            .then(set(asked, item))
            .then(set(asked_to, start))
        on resized(item: Event, end_at: Time): ended(item, end_at)
            .then(set(asked, item))
            .then(set(asked_end, end_at))
    }
}

/// One date a series meets, as the timeline draws it: the series at that date's time, and the
/// question its move or resize asks.
template view timeline_occurrence(
    e: Event,
    at: Time,
    asked: State<Option<Event>>,
    asked_from: State<Option<Time>>,
    asked_to: State<Option<Time>>,
    asked_end: State<Option<Time>>,
) {
    ui.TimelineItem(
        e,
        title: e.title,
        start: at,
        end: at + (ends(e) - starts(e)),
        location: e.location,
        tone: if e.color == "indigo" {
            .first
        } else if e.color == "emerald" {
            .second
        } else if e.color == "amber" {
            .third
        } else if e.color == "red" {
            .fourth
        } else if e.color == "blue" {
            .fifth
        } else if e.color == "violet" {
            .sixth
        } else if e.color == "slate" { .seventh } else { .neutral },
    ) {
        on pressed(item: Event): open(item)
        on moved(item: Event, start: Time): set(asked, item)
            .then(set(asked_from, at))
            .then(set(asked_to, start))
        on resized(item: Event, end_at: Time): set(asked, item)
            .then(set(asked_from, at))
            .then(set(asked_end, end_at))
    }
}

/// What is coming, by day, each event opening where it stands, with the entry that adds one at
/// the foot.
view agenda(c: Calendars, chosen: Option<Calendar>, searched: Text, hidden: Text) {
    // The next three weeks, from the day the window stands on.
    let from = c.anchor_at.day()
    let to = from + 20d
    let coming = if let k = chosen {
        c.events.matching(Event.coming, Event.calendar(k)).filter(e -> kept(e, hidden))
    } else {
        c.events.matching(Event.coming).filter(e -> kept(e, hidden))
    }
    let rows = coming.flat_map(
        e -> moments(e, from, to).map(at -> Moment(event: e, at: at, day: day_of(e, at))),
    )
    let shown = rows.filter(r -> found(r, searched))
    ui.Docked {
        foot: {
            stack {
                std.add_entry(std.say("New event"), new_event(c))
            }
        }
        if shown.is_empty() {
            ui.EmptyState(std.say("Nothing scheduled in the next three weeks"), icon: .calendar)
        } else {
            // One list by time, under the day bands of the event's own zone, so a repeating
            // meeting stands between the one-off events of the day it meets. A date of a series
            // is its own row, at its own moment, opening the event it stands for.
            ui.VirtualList(selection_follows_focus: true, in_column: true, narrow: space.compact) {
                on select(r: Event): open(r, front: false)
                on activate(r: Event): open(r)
                for r in shown.sort_by(.at) {
                    ui.ListItem(item: r.event, group: r.day, opens: false) {
                        ui.ListRow(
                            title: r.event.title,
                            secondary: r.event.location,
                            item: r.event,
                            icon: .calendar,
                            motion: .brisk,
                        ) {
                            trailing: {
                                ui.Time(r.at, role: .caption, secondary: true, clock: true)
                            }
                        }
                        detail: {
                            event_body(r.event)
                        }
                    }
                }
            }
        }
    }
}

/// The tasks: the list chosen in the sidebar, its entry at the foot, each opening where it
/// stands.
view tasks(c: Calendars, smart: Text, project: Option<Project>, searched: Text) {
    let found = if let p = project {
        c.tasks.matching(Task.project(p))
    } else if smart == "inbox" {
        c.tasks.matching(Task.inbox)
    } else if smart == "upcoming" {
        c.tasks.matching(Task.upcoming)
    } else if smart == "done" { c.tasks.matching(Task.done) } else { c.tasks.matching(Task.today) }
    let listed = if searched.is_empty() { found } else { found.matching(searched) }
    let title = if let p = project { p.name } else { task_list_name(smart) }
    let placeholder = if let p = project { "New task in {p.name}" } else { "New task" }
    stack(gap: .group, fill: true) {
        ui.Text(title, role: .title)
        ui.Docked {
            foot: {
                stack {
                    std.add_entry(placeholder, new_task(c, p: project))
                }
            }
            if listed.is_empty() {
                ui.EmptyState(std.say("Nothing in {title}"), icon: .list_checks)
            } else {
                std.list(listed.sort_by(.due)) { (t, who) ->
                    ui.ListItem(item: t, presence: who, done: t.done, opens: false) {
                        task_row(t, project)
                        detail: {
                            task_body(c, t)
                        }
                    }
                }
            }
        }
    }
}

/// A task as a row: its box, its title, and what it waits on.
view task_row(t: Task, project: Option<Project>) {
    row(
        t.title,
        checkbox: t.done,
        check: check_task(t),
        secondary: if t.project != project { t.project?.name },
        note: if let p = t.parent { std.say("A step of {p.title}") },
        meta: t.due,
        mark: if t.priority != .none { .flag },
    )
}

/// A task where it stands: what it is, when it is due, how much it matters, its project, and
/// the steps under it.
template view task_body(c: Calendars, t: Task) {
    state step: Text = ""
    stack(gap: .group) {
        stack(gap: .related) {
            row(gap: .related, align: .start) {
                ui.Checkbox(bind: t.done) {
                    on check: check_task(t)
                }
                ui.TextInput(
                    bind: t.title,
                    placeholder: std.say("What to do"),
                    title: true,
                    fill: 1.0,
                )
            }
            stack(lead: 1) {
                std.origin(t)
                ui.BlockEditor(
                    bind: t.notes,
                    placeholder: std.say("Anything worth remembering about it."),
                )
            }
        }
        std.body() {
            facts(t) {
                ui.MenuButton(t.project?.name ?? std.say("Inbox"), icon: .folder) {
                    ui.MenuItem(std.say("Inbox"), icon: .inbox, checked: t.project == .none) {
                        on choose: file_task(t, .none)
                    }
                    for p in c.projects {
                        ui.MenuItem(p.name, icon: .folder, checked: t.project == .some(p)) {
                            on choose: file_task(t, .some(p))
                        }
                    }
                }
                ui.MenuButton(std.say("Due"), icon: .calendar) {
                    ui.MenuItem(std.say("Today"), icon: .star) {
                        on choose: due_task_today(t)
                    }
                    ui.MenuItem(std.say("Tomorrow"), icon: .calendar) {
                        on choose: due_task_tomorrow(t)
                    }
                    ui.MenuItem(std.say("Next week"), icon: .calendar) {
                        on choose: due_task_next_week(t)
                    }
                    ui.MenuItem(std.say("No date"), icon: .x) {
                        on choose: due_task_clear(t)
                    }
                }
                ui.MenuButton(std.say("Priority"), icon: .flag) {
                    ui.MenuItem(std.say("No priority"), icon: .flag, checked: t.priority == .none) {
                        on choose: set_task_priority(t, .none)
                    }
                    ui.MenuItem(std.say("Low"), icon: .flag, checked: t.priority == .low) {
                        on choose: set_task_priority(t, .low)
                    }
                    ui.MenuItem(std.say("Medium"), icon: .flag, checked: t.priority == .medium) {
                        on choose: set_task_priority(t, .medium)
                    }
                    ui.MenuItem(std.say("High"), icon: .flag, checked: t.priority == .high) {
                        on choose: set_task_priority(t, .high)
                    }
                }
            }
            section(std.say("Steps")) {
                for x in c.tasks.filter(x -> x.parent == t) {
                    row(gap: .related) {
                        ui.Checkbox(bind: x.done) {
                            on check: check_task(x)
                        }
                        ui.Text(x.title, fill: 1.0)
                        if let due = x.due {
                            ui.Text("{due}", role: .caption, secondary: true)
                        }
                    }
                }
                ui.TextInput(bind: step, placeholder: std.say("Add a step")) {
                    on submit: add_step(c, t, step).then(set(step, ""))
                }
            }
            std.after_body {
                std.linked(t)
                std.history(t)
            }
        }
    }
}

/// An event in a list as it stands on its own: what it is called, where, and the time it starts
/// at.
view event_row(e: Event) {
    ui.ListRow(title: e.title, secondary: e.location, item: e, icon: .calendar, motion: .brisk) {
        trailing: {
            ui.Time(starts(e), role: .caption, secondary: true, clock: true)
        }
    }
}

/// A task as a page of its own, where a list opens it in front of the window.
view task_page(t: Task) {
    std.document(t) {
        task_body(calendar, t)
    }
}

/// One date of a series as a page of its own: the event it stands for.
view moment_page(r: Moment) {
    std.document(r.event) {
        event_body(r.event)
    }
}

/// An event as a page of its own.
view event_page(e: Event) {
    std.document(e) {
        event_body(e)
    }
}

/// An event where it stands: what it is, when it is, who is coming, and what was written of it.
template view event_body(e: Event) {
    state guest: Text = ""
    state every: Text = ""
    state until: Option<Date> = .none
    state times: Text = ""
    state category: Text = ""
    state organiser: Text = ""
    state zone: Text = ""
    state at_time: Option<Time> = .none
    state to_time: Option<Time> = .none
    stack(gap: .group) {
        stack(gap: .related) {
            ui.TextInput(bind: e.title, placeholder: std.say("Event title"), title: true)
            stack(lead: 1) {
                std.origin(e)
                ui.BlockEditor(bind: e.body, placeholder: std.say("What is it about?"))
                if e.notes != "" {
                    ui.Text(e.notes, secondary: true)
                }
            }
        }
        std.body() {
            facts(e) {
                ui.DateField(bind: e.day, label: std.say("Day")) {
                    on change(at): change_day(e, at)
                }
                ui.TimePicker(bind: at_time, label: std.say("Starts"), step: 15) {
                    on changed(at): change_start(e, at)
                }
                ui.TimePicker(bind: to_time, label: std.say("Ends"), step: 15) {
                    on changed(at): change_end(e, at)
                }
                ui.TextInput(bind: e.location, placeholder: std.say("Add a place"))
                ui.TextInput(bind: zone, placeholder: "{e.timezone}", lines: 1) {
                    on submit: set_zone(e, zone).then(set(zone, ""))
                }
                ui.Toggle(bind: e.all_day, label: std.say("All day"))
                if let other = clashing(calendar, e).first() {
                    ui.Text(std.say("Overlaps {other}"), role: .caption, secondary: true)
                }
                ui.MenuButton(e.calendar?.name ?? std.say("My calendar"), icon: .calendar) {
                    for k in calendar.calendars {
                        ui.MenuItem(k.name, icon: .calendar, checked: e.calendar == .some(k)) {
                            on choose: file_in(e, k)
                        }
                    }
                }
                ui.MenuButton(std.say("Colour"), icon: .palette) {
                    for tone in ["", "indigo", "emerald", "amber", "red", "blue", "violet", "slate"] {
                        ui.MenuItem(
                            if tone == "" {
                                std.say("Its calendar's colour")
                            } else { color_label(tone) },
                            icon: if tone == "" { .x } else { .palette },
                            checked: e.color == tone,
                        ) {
                            on choose: if tone == "" {
                                clear_event_color(e)
                            } else { set_event_color(e, color_of(tone)) }
                        }
                    }
                }
                ui.MenuButton(e.filed_under?.name ?? std.say("No project"), icon: .folder) {
                    ui.MenuItem(std.say("No project"), icon: .x, checked: e.filed_under == .none) {
                        on choose: file_event(e, .none)
                    }
                    for p in calendar.projects.sort_by(x -> x.order) {
                        ui.MenuItem(p.name, icon: .folder, checked: e.filed_under == .some(p)) {
                            on choose: file_event(e, .some(p))
                        }
                    }
                }
                ui.MenuButton(std.say("Move"), icon: .arrow_right) {
                    ui.MenuItem(std.say("Tomorrow"), icon: .arrow_right) {
                        on choose: move_to_day(e, e.day + 1d)
                    }
                    ui.MenuItem(std.say("Next week"), icon: .arrow_right) {
                        on choose: move_to_day(e, e.day + 1w)
                    }
                }
                ui.MenuButton(std.say("Repeat"), icon: .repeat) {
                    ui.MenuItem(std.say("Every day"), icon: .repeat, checked: e.repeat == .daily) {
                        on choose: repeat(e, .daily)
                    }
                    ui.MenuItem(
                        std.say("Every week"),
                        icon: .repeat,
                        checked: e.repeat == .weekly,
                    ) {
                        on choose: repeat(e, .weekly)
                    }
                    ui.MenuItem(
                        std.say("Every month"),
                        icon: .repeat,
                        checked: e.repeat == .monthly,
                    ) {
                        on choose: repeat(e, .monthly)
                    }
                    ui.MenuItem(
                        std.say("Every year"),
                        icon: .repeat,
                        checked: e.repeat == .yearly,
                    ) {
                        on choose: repeat(e, .yearly)
                    }
                    ui.MenuItem(std.say("Does not repeat"), icon: .x) {
                        on choose: repeat_off(e)
                    }
                }
                ui.MenuButton(
                    if e.remind == Time("1970-01-01T00:00:00Z") {
                        std.say("Remind")
                    } else { std.say("Reminder set") },
                    icon: .bell,
                ) {
                    ui.MenuItem(
                        std.say("At the start"),
                        icon: .bell,
                        checked: e.remind == starts(e),
                    ) {
                        on choose: remind_at_start(e)
                    }
                    ui.MenuItem(std.say("5 minutes before"), icon: .bell) {
                        on choose: remind_before(e, 5)
                    }
                    ui.MenuItem(std.say("10 minutes before"), icon: .bell) {
                        on choose: remind_before(e, 10)
                    }
                    ui.MenuItem(std.say("30 minutes before"), icon: .bell) {
                        on choose: remind_before(e, 30)
                    }
                    ui.MenuItem(std.say("An hour before"), icon: .bell) {
                        on choose: remind_before(e, 60)
                    }
                    ui.MenuItem(std.say("A day before"), icon: .bell) {
                        on choose: remind_before(e, 1440)
                    }
                    ui.MenuItem(
                        std.say("No reminder"),
                        icon: .bell_off,
                        checked: e.remind == Time("1970-01-01T00:00:00Z"),
                    ) {
                        on choose: remind_off(e)
                    }
                }
            }
            if e.recurrence != "" {
                section(std.say("Repeat")) {
                    row(gap: .tight) {
                        ui.Text(std.say("How often"), role: .caption, secondary: true)
                        ui.TextInput(bind: every, placeholder: "{e.recurrence_every}", lines: 1) {
                            on submit: repeat_every_text(e, every).then(set(every, ""))
                        }
                    }
                    row(gap: .tight) {
                        ui.Text(std.say("Until"), role: .caption, secondary: true)
                        ui.DateField(bind: until, label: std.say("Repeat until")) {
                            on change(at): repeat_until_at(e, at)
                        }
                        ui.Button(
                            std.say("Clear"),
                            icon: .x,
                            icon_only: true,
                            accessible_name: std.say("No end"),
                        ) {
                            on press: repeat_until(e, .none)
                        }
                    }
                    row(gap: .tight) {
                        ui.Text(std.say("Times"), role: .caption, secondary: true)
                        ui.TextInput(
                            bind: times,
                            placeholder: "{e.recurrence_count ?? 0}",
                            lines: 1,
                        ) {
                            on submit: repeat_count_text(e, times).then(set(times, ""))
                        }
                    }
                }
            }
            if e.recurrence != "" {
                section(std.say("Occurrences")) {
                    for at in meets(e, calendar.anchor_at.day(), calendar.anchor_at.day() + 30d)
                        .take(6) {
                        let day = day_of(e, at)
                        let cancelled = if let o = apart(e, at) { o.cancelled } else { false }
                        ui.ListRow(
                            "{day}",
                            secondary: if cancelled { std.say("Cancelled") } else { "" },
                        ) {
                            trailing: {
                                ui.MenuButton(std.say("This date"), icon: .calendar) {
                                    if cancelled {
                                        ui.MenuItem(std.say("Restore this date"), icon: .undo_2) {
                                            on choose: restore_occurrence(e, at)
                                        }
                                    } else {
                                        ui.MenuItem(std.say("Cancel this date"), icon: .x) {
                                            on choose: cancel_occurrence(e, at)
                                        }
                                    }
                                    ui.MenuItem(std.say("Skip this date"), icon: .calendar) {
                                        on choose: skip_date(e, day)
                                    }
                                }
                            }
                        }
                    }
                }
            }
            section(std.say("Filed under")) {
                for c in e.categories {
                    ui.ListRow(c, icon: .tag) {
                        trailing: {
                            ui.Button(
                                "",
                                icon: .x,
                                icon_only: true,
                                accessible_name: std.say("Remove a category"),
                            ) {
                                on press: remove_category(e, c)
                            }
                        }
                    }
                }
                ui.TextInput(bind: category, placeholder: std.say("Add a category")) {
                    on submit: add_category(e, category).then(set(category, ""))
                }
                ui.TextInput(bind: organiser, placeholder: std.say("Organiser's email address")) {
                    on submit: organize(e, organiser).then(set(organiser, ""))
                }
            }
            section(std.say("Guests")) {
                for g in e.attendees {
                    let said = if g.status == "accepted" {
                        std.say("Accepted")
                    } else if g.status == "tentative" {
                        std.say("Maybe")
                    } else if g.status == "declined" {
                        std.say("Declined")
                    } else { std.say("Awaiting response") }
                    ui.ListRow(g.name, secondary: g.email, meta: said) {
                        trailing: {
                            row(gap: .tight) {
                                ui.MenuButton(std.say("Answer"), icon: .check) {
                                    ui.MenuItem(
                                        std.say("Accepted"),
                                        icon: .check,
                                        checked: g.status == "accepted",
                                    ) {
                                        on choose: answer(e, g, "accepted")
                                    }
                                    ui.MenuItem(
                                        std.say("Declined"),
                                        icon: .x,
                                        checked: g.status == "declined",
                                    ) {
                                        on choose: answer(e, g, "declined")
                                    }
                                    ui.MenuItem(
                                        std.say("Maybe"),
                                        icon: .circle_help,
                                        checked: g.status == "tentative",
                                    ) {
                                        on choose: answer(e, g, "tentative")
                                    }
                                }
                                ui.Button(
                                    "",
                                    icon: .mail,
                                    icon_only: true,
                                    accessible_name: std.say("Invite by mail"),
                                ) {
                                    on press: invite_by_mail(e, g)
                                }
                                ui.Button(
                                    "",
                                    icon: .x,
                                    icon_only: true,
                                    accessible_name: std.say("Remove guest"),
                                ) {
                                    on press: uninvite(e, g)
                                }
                            }
                        }
                    }
                }
                ui.TextInput(bind: guest, placeholder: std.say("Invite by email address")) {
                    on submit: invite(e, guest, guest).then(set(guest, ""))
                }
            }
            std.after_body {
                std.linked(e)
                std.history(e)
            }
        }
    }
}
