app so.tilo.calendar "Calendar" (
    icon: .calendar,
    category: .office,
    tagline: "The workspace's events: the week, the month, and who is coming",
    keywords: ["calendar", "events", "schedule", "week", "month", "agenda", "appointments", "meetings", "invitations", "caldav", "ics", "shared calendar"],
)

use so.tilo.mail as mail
use so.tilo.contacts as contacts
use tilo.workspace as workspace

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

/// One of the workspace's calendar accounts, and how its server answers: the window says a
/// server that cannot be reached, a change it will not take, and when its calendars last came.
/// The account itself, its address and servers, is the workspace settings'; the system keeps
/// this in step with what its server says.
type Account (origin: caldav.account) {
    /// The account's id, as the workspace settings name it.
    key: Text merge register
    /// The name it signs in with.
    address: Text
    /// What it is called.
    name: Text
    /// The server address the person typed.
    server: Text
    /// Whether nothing is ever written to its server.
    read_only: Bool
    /// What the account's server said when it could not be reached, in its words: empty while
    /// it stands well.
    trouble: Text
    /// Whether the server answers.
    online: Bool
    /// Whether the server is being asked now, so the window says what is under way.
    checking: Bool
    /// When the account's calendars were last taken in from its server, as the clock the person
    /// read it at: 1970 when they never were.
    fetched: Time = Time("1970-01-01T00:00:00Z")
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
    /// What the server says of it: its description, as text.
    notes: Text = ""
    /// What the person writes of it, in rich text.
    body: RichText
    /// Who is coming, and what each answered.
    attendees: Collection<Attendee>
    /// The person's own words for what kind of event it is.
    categories: Set<Text>
    /// Its frequency, in iCalendar's words (for example, weekly); empty for one that does not repeat.
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
    /// The calendar accounts the workspace settings hold, each saying how its server answers.
    accounts: Collection<Account>
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
) -> Event uses time (icon: .plus, label: "New event") {
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

/// Makes an event of one line a person types: "Lunch with Sam friday 1pm" starts lunch with Sam
/// at one on Friday, "Workshop tomorrow at 10 for 90 min" runs ninety minutes from ten tomorrow,
/// "Trip on 2026-11-03 at 8:00" lands on that day, and "Dinner tonight" is at eight. The words
/// that said when are the line's alone; everything else is the title. A line that names no day
/// lands on the day the window is looking at, and one that names no time starts on the next
/// quarter hour, a hour long.
action quick_add(c: Calendars, line: Text) -> Event uses time (icon: .plus, label: "Quick add") {
    let anchor = quick_anchor(c, now)
    let parts = quick_parts(line, now, anchor)
    let title = parts.at(0) ?? ""
    let start_wall = parts.at(1) ?? ""
    let end_wall = parts.at(2) ?? ""
    let day = Date.parse(start_wall, near: now) ?? anchor
    c.events.insert(
        Event(
            title: title,
            start: start_wall,
            end: end_wall,
            all_day: false,
            day: day,
            end_day: Date.parse(end_wall, near: now) ?? day,
            timezone: "floating",
            calendar: c.calendars.first(),
        ),
    )
}

/// What a quick-add line says it will write, in the person's words: its title, the day and the
/// hours, so a line that lands elsewhere says so before anything is written.
fn quick_said(line: Text, now: Time, anchor: Date) -> Text {
    let parts = quick_parts(line, now, anchor)
    let title = parts.at(0) ?? ""
    let start = Time.parse(parts.at(1) ?? "", near: now)
    let end = Time.parse(parts.at(2) ?? "", near: now)
    let name = if title == "" { "New event" } else { title }
    if let s = start, e = end {
        let day = (s.wall("floating") ?? "").slice(0, 10)
        let when = if day == anchor.iso() {
            "today"
        } else if day == (anchor + 1d).iso() { "tomorrow" } else { day }
        "{name}, {when}, {s.clock()} – {e.clock()}"
    } else { name }
}

/// The day a quick-add line that names none lands on: the day the window stands on, or today
/// when the window has not said.
fn quick_anchor(c: Calendars, now: Time) -> Date {
    let day = c.anchor_at.day()
    if day.year() < 2000 { now.day() } else { day }
}

/// The quick-add line read into `[title, start wall time, end wall time]`, the wall times in the
/// form a calendar writes. The day, the clock and the length are each read once, from the first
/// words that name them; those words leave the title.
fn quick_parts(line: Text, now: Time, anchor: Date) -> List<Text> {
    let words = line.split(" ")
    let said = words.map(w -> w.lower())
    let taken = Map<Bool>()
    let at = words.map((w, i) -> i)
    // The day: today, tonight, tomorrow, a weekday (with `next` before it), or a day as written.
    let chosen = Map<Text>()
    let tonight = Map<Bool>()
    for i in at {
        if chosen.get("d") == .none {
            let w = said.at(i) ?? ""
            let next = if i > 0 { said.at(i - 1) ?? "" } else { "" }
            if w == "today" {
                chosen.put("d", anchor.iso())
                taken.put(i.to_text(), true)
            } else if w == "tonight" {
                chosen.put("d", anchor.iso())
                tonight.put("on", true)
                taken.put(i.to_text(), true)
            } else if w == "tomorrow" {
                chosen.put("d", (anchor + 1d).iso())
                taken.put(i.to_text(), true)
            } else if w == "on" and quick_day(said.at(i + 1) ?? "", now) != .none {
                if let d = quick_day(said.at(i + 1) ?? "", now) { chosen.put("d", d.iso()) }
                taken.put(i.to_text(), true)
                taken.put((i + 1).to_text(), true)
            } else if quick_day(w, now) != .none {
                if let d = quick_day(w, now) {
                    chosen.put("d", if next == "next" { d + Duration(7 * 86400) } else { d }.iso())
                }
                taken.put(i.to_text(), true)
                if next == "next" { taken.put((i - 1).to_text(), true) }
            }
        }
    }
    // The clock: "at 9", "9am", "15:30", "noon" or "midnight"; without `at` only a spelling that
    // is unmistakably a clock, so "5 things to do" keeps its five.
    let clock = Map<Int>()
    for i in at {
        if clock.get("s") == .none {
            let w = said.at(i) ?? ""
            if w == "at" and quick_clock(said.at(i + 1) ?? "", true) != .none {
                if let seconds = quick_clock(said.at(i + 1) ?? "", true) { clock.put("s", seconds) }
                taken.put(i.to_text(), true)
                taken.put((i + 1).to_text(), true)
            } else if quick_clock(w, false) != .none {
                if let seconds = quick_clock(w, false) { clock.put("s", seconds) }
                taken.put(i.to_text(), true)
            }
        }
    }
    // The length: "for 90 min", "for 2h", "for half an hour" or "for an hour".
    let length = Map<Int>()
    for i in at {
        if length.get("s") == .none and (said.at(i) ?? "") == "for" {
            if let minutes = quick_length(said, i) {
                length.put("s", minutes * 60)
                let second = said.at(i + 1) ?? ""
                let used = if second == "half" {
                    4
                } else if second == "an" or second == "a" {
                    3
                } else if unit_of(second) == "" { 3 } else { 2 }
                for n in [0, 1, 2, 3] {
                    if n < used { taken.put((i + n).to_text(), true) }
                }
            }
        }
    }
    let day = if let iso = chosen.get("d") { Date.parse(iso, near: now) ?? anchor } else { anchor }
    let seconds = if let s = clock.get("s") {
        s
    } else if tonight.get("on") == .none {
        next_quarter(now).time_of_day().seconds()
    } else { 20 * 3600 }
    let start = (Time.parse("{day.iso()}T00:00:00", near: now) ?? now) + Duration(seconds)
    let end = start + Duration(length.get("s") ?? 3600)
    let kept = at.filter(i -> taken.get(i.to_text()) == .none).map(i -> words.at(i) ?? "")
    [kept.join(" ").trim(), start.wall("floating") ?? "", end.wall("floating") ?? ""]
}

/// The day `word` names, read from the day it is typed near: a weekday is the next one, or a day
/// as written (`2026-11-03`). Nothing for any other word.
fn quick_day(word: Text, now: Time) -> Option<Date> {
    if word.len() != 10 or word.slice(4, 5) != "-" or word.slice(7, 8) != "-" {
        if let want = quick_weekday(word) {
            let ahead = (want - weekday_of(now.day()) + 7) % 7
            return .some(now.day() + Duration(ahead * 86400))
        }
        return .none
    }
    Date.parse(word, near: now)
}

/// The weekday a word names, Monday zero: `mon`, `monday` and `mo` are all Monday.
fn quick_weekday(word: Text) -> Option<Int> {
    if word == "mon" or word == "monday" or word == "mo" {
        .some(0)
    } else if word == "tue" or word == "tuesday" or word == "tues" {
        .some(1)
    } else if word == "wed" or word == "wednesday" {
        .some(2)
    } else if word == "thu" or word == "thursday" or word == "thur" or word == "thurs" {
        .some(3)
    } else if word == "fri" or word == "friday" {
        .some(4)
    } else if word == "sat" or word == "saturday" {
        .some(5)
    } else if word == "sun" or word == "sunday" { .some(6) } else { .none }
}

/// A day's weekday, Monday zero.
fn weekday_of(day: Date) -> Int {
    let w = day.weekday()
    if w == .monday {
        0
    } else if w == .tuesday {
        1
    } else if w == .wednesday {
        2
    } else if w == .thursday {
        3
    } else if w == .friday { 4 } else if w == .saturday { 5 } else { 6 }
}

/// The time of day a word names, in seconds from midnight: `9am`, `9:30pm`, `15:30`, `noon`,
/// `midnight`, or an hour alone after `at`. Nothing when the word is no clock, and nothing for an
/// impossible one ("25:99"), which stays in the title.
fn quick_clock(word: Text, bare: Bool) -> Option<Int> {
    if word == "noon" or word == "midday" {
        return .some(12 * 3600)
    }
    if word == "midnight" {
        return .some(0)
    }
    let afternoon: Option<Bool> = if word.ends_with("pm") {
        .some(true)
    } else if word.ends_with("am") { .some(false) } else { .none }
    let rest = if afternoon != .none { word.slice(0, word.len() - 2) } else { word }
    let colon = rest.find(":")
    if let at = colon {
        let minutes_text = rest.slice(at + 1, rest.len())
        if minutes_text.len() != 2 {
            return .none
        }
        if let h = rest.slice(0, at).to_int(), m = minutes_text.to_int() {
            if h > 23 or m > 59 {
                return .none
            }
            return .some(twelve_hour(h, m, afternoon) * 3600 + m * 60)
        }
        return .none
    }
    if afternoon == .none and not bare {
        return .none
    }
    if let h = rest.to_int() {
        let hours = twelve_hour(h, 0, afternoon)
        if hours > 23 {
            return .none
        }
        return .some(hours * 3600)
    }
    .none
}

/// `hours` as a 24-hour clock: the afternoon after noon, and an hour alone after `at` taken in the
/// afternoon when it is one to six.
fn twelve_hour(hours: Int, minutes: Int, afternoon: Option<Bool>) -> Int {
    if afternoon == .some(true) and hours < 12 {
        hours + 12
    } else if afternoon == .some(false) and hours == 12 {
        0
    } else if afternoon == .none and minutes == 0 and hours >= 1 and hours <= 6 {
        hours + 12
    } else { hours }
}

/// The minutes a length starting at `for` (word `index`) names: "90 min", "2 hours", "2h",
/// "half an hour" or "an hour". Nothing for any other words.
fn quick_length(said: List<Text>, index: Int) -> Option<Int> {
    let second = said.at(index + 1) ?? ""
    if second == "half" {
        let article = said.at(index + 2) ?? ""
        let unit = said.at(index + 3) ?? ""
        if (article == "an" or article == "a") and hour_word(unit) {
            return .some(30)
        }
        return .none
    }
    if second == "an" or second == "a" {
        if hour_word(said.at(index + 2) ?? "") {
            return .some(60)
        }
        return .none
    }
    let unit = unit_of(second)
    if unit == "" {
        return minutes_of(amount_of(second), said.at(index + 2) ?? "")
    }
    minutes_of(amount_of(second), unit)
}

/// Whether a word is an hour: `h`, `hr`, `hrs`, `hour` or `hours`.
fn hour_word(word: Text) -> Bool {
    word == "h" or word == "hr" or word == "hrs" or word == "hour" or word == "hours"
}

/// How many minutes `amount` of `unit` is, rounded and kept to what a calendar holds; nothing for
/// any other unit or a length under a minute.
fn minutes_of(amount: Float, unit: Text) -> Option<Int> {
    let minutes = if unit == "min"
        or unit == "mins"
        or unit == "minute"
        or unit == "minutes"
        or unit == "m" {
        amount
    } else if hour_word(unit) {
        amount * 60.0
    } else {
        return .none
    }
    if minutes < 1.0 {
        return .none
    }
    .some(minutes.round().clamp(15, 1440))
}

/// Where the number at the start of `word` ends: the first letter or sign that is no digit and
/// no point.
fn number_end(word: Text) -> Int {
    let end = Map<Int>()
    let letters = word.split("")
    for i in letters.map((c, i) -> i) {
        let here = letters.at(i) ?? ""
        if end.get("e") == .none and not (digit_char(here) or here == ".") { end.put("e", i) }
    }
    end.get("e") ?? word.len()
}

/// Whether a one-letter text is a digit.
fn digit_char(c: Text) -> Bool {
    c != "" and "0123456789".contains(c)
}

/// The number a word starts with (`90` of `90min`), zero where it starts with none.
fn amount_of(word: Text) -> Float {
    word.slice(0, number_end(word)).to_float() ?? 0.0
}

/// The unit a word ends with after its number (`min` of `90min`); empty where it holds none.
fn unit_of(word: Text) -> Text {
    word.slice(number_end(word), word.len())
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
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    e.color = color_name(color)
}

/// Takes an event's own colour off: it is drawn in its calendar's.
action clear_event_color(e: Event) (icon: .x, label: "Its calendar's colour") {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    e.color = ""
}

/// Sets the colour a calendar is drawn in.
action set_calendar_color(k: Calendar, color: Color) (icon: .palette, label: "Colour") {
    k.color = color_name(color)
}

/// Sets the categories an event is filed under in its person's words.
action set_categories(e: Event, categories: List<Text>) (label: "Categories", icon: .tag) {
    require files_for(e, invoker) "Its person invites others to that entry, so its person files it: filing rewrites it on the server, which would write to them"
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
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require zone.trim() != "" "A zone is floating, UTC, or a name such as Europe/Berlin"
    require Time.from_wall(zone.trim(), "1970-01-01T00:00:00") != .none "That is not a time zone this build knows"
    e.timezone = zone.trim()
}

/// Files an event under one more category.
action add_category(e: Event, category: Text) (icon: .tag, label: "Add a category") {
    require files_for(e, invoker) "Its person invites others to that entry, so its person files it: filing rewrites it on the server, which would write to them"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require category.trim() != "" "A category is a few words"
    e.categories.add(category.trim())
}

/// Takes a category off an event.
action remove_category(e: Event, category: Text) (icon: .x, label: "Remove a category") {
    require files_for(e, invoker) "Its person invites others to that entry, so its person files it: filing rewrites it on the server, which would write to them"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    e.categories.remove(category)
}

/// Makes someone the organiser of an event.
action organize(e: Event, who: Text) (
    label: "Organiser",
    icon: .user,
    shows: [e.title, (who, .addresses), (guests(e), .addresses)],
) {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    e.organizer = who
}

/// Reminds the people coming when the event starts.
action remind_at_start(e: Event) (label: "At the start", icon: .bell, group: "Remind") {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    e.remind = starts(e)
}

/// Reminds them a given number of minutes before it starts.
action remind_before(e: Event, minutes: Int) (label: "Before", icon: .bell, group: "Remind") {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require minutes >= 0 "A reminder is before the start"
    e.remind = starts(e) - Duration(minutes * 60)
}

/// Tells nobody, once more.
action remind_off(e: Event) (label: "No reminder", icon: .bell_off, group: "Remind") {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
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
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
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
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require not days.is_empty() "Choose the days it repeats on"
    repeat_rule(e, .weekdays, weekdays: days)
}

/// Adds a weekday to a weekly repeat, or takes it off: the days chosen are the rule's days,
/// and the repeat keeps the day it ends on, if it ends on one.
action toggle_weekday(e: Event, day: Text) (icon: .repeat, label: "Repeat on this day") {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require not e.recurrence_weekdays.contains(day) or e.recurrence_weekdays.len() > 1 "An event repeats on at least one day"
    let kept = e.recurrence_weekdays.filter(d -> d != day)
    let adding = kept.len() == e.recurrence_weekdays.len()
    let days = if adding { kept + [day] } else { kept }
    repeat_rule(e, .weekdays, weekdays: days, until: e.recurrence_until, count: e.recurrence_count)
}

/// Stops an event repeating from the day it stands on.
action repeat_off(e: Event) (icon: .x, label: "Does not repeat") {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
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
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require e.recurrence != "" "It does not repeat"
    e.exdates.add(day.iso())
}

/// Gives a skipped date of a repeating event back.
action restore_date(e: Event, day: Date) (icon: .calendar, label: "Restore this date") {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
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
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
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
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
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
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    if let o = apart(e, at) { calendar.events.remove(o) }
}

/// Moves a repeating event from one date on: the series ends the day before, and a new one
/// begins where it was moved to, with every date after it.
action move_following(e: Event, at: Time, start: Time, end: Time) uses time (
    icon: .arrow_right,
    label: "This and following events",
) {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
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

/// Asks one account's server for its calendars now: what "Sync again" over its notice does.
action sync_account(a: Account) (icon: .refresh, label: "Sync again") {
    outside("caldav.check", a.key)
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
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    c.events.remove(e)
}

/// Throws away an event nothing was written in yet: what a person's New event leaves when they
/// decide against it.
destructive action discard_draft(c: Calendars, e: Event) (
    icon: .trash,
    label: "Discard this event",
) {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require pristine(e) "The event has something in it"
    c.events.remove(e)
}

/// Whether nothing was put in an event since it was made: its title, its place, its notes and
/// its body are empty, and nobody was invited to it.
fn pristine(e: Event) -> Bool {
    e.title.trim() == ""
        and e.location.trim() == ""
        and e.notes.trim() == ""
        and e.body.markdown.trim() == ""
        and e.attendees.is_empty()
        and e.categories.is_empty()
        and e.filed_under == .none
        and e.organizer == ""
        and e.recurrence == ""
        and e.key == ""
}

/// Moves an event to a day, keeping the time of day it starts and how long it lasts, and its
/// reminder at the same distance from its start.
action move_to_day(e: Event, day: Date) uses time (settles: e.day) {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
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
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    move_to_day(e, at.day())
}

/// Sets the time of day an event starts, its end keeping how long it lasts. A day the moment
/// names is the day it moves to.
action change_start(e: Event, at: Time) uses time {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
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
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require at > starts(e) "An event ends after it starts"
    e.end = at.wall(zone_of(e)) ?? e.end
    e.end_day = end_day_of(e.all_day, Date.parse(e.end, near: now) ?? today)
}

/// Ends an event on the day a moment names, keeping the time of day it ends at: a day after the
/// one it starts on makes it run through every day between, and a whole-day event ends at the
/// midnight after its last day, which is the day it shows on.
action move_end_day(e: Event, at: Time) uses time {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    end_on_day(e, at.day())
}

/// Ends an event on a day, keeping the time of day it ends at, as `move_end_day` does with the
/// day a moment names.
action end_on_day(e: Event, end_day: Date) uses time (settles: e.end_day) {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require end_day >= (Date.parse(e.start, near: now) ?? today) "An event does not end before it starts"
    let ends_on = if e.all_day { end_day + 1d } else { end_day }
    e.end = wall_moved(e.end, zone_of(e), ends_on)
    e.end_day = end_day_of(e.all_day, ends_on)
}

/// Repeats an event every day, week, month or year from the day it stands on, on the days
/// chosen of a week, and no longer on the days it named before.
action repeat(e: Event, how: Repeat) (icon: .repeat, label: "Repeat", settles: e.repeat) {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    repeat_rule(e, how)
}

/// How often it repeats, in the unit it repeats in: every two weeks is 2.
action repeat_every(e: Event, every: Int) (icon: .repeat, label: "How often") {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require every >= 1 "It repeats at least once"
    repeat_rule(e, e.repeat, every: every)
}

/// The last day it repeats by, or none for a series that does not end.
action repeat_until(e: Event, until: Option<Date>) (
    icon: .calendar,
    label: "Repeat until",
    settles: e.recurrence_until,
) {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    repeat_rule(e, e.repeat, until: until)
}

/// The last day it repeats by, as the day a moment names.
action repeat_until_at(e: Event, at: Time) uses time (icon: .calendar, label: "Repeat until") {
    repeat_until(e, .some(at.day()))
}

/// How many times it repeats in all, or none for a series that does not end.
action repeat_count(e: Event, count: Option<Int>) (icon: .hash, label: "Times") {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    repeat_rule(e, e.repeat, count: count)
}

/// Files an event under a calendar the person picks.
action file_in(e: Event, k: Calendar) (label: "Calendar", icon: .calendar) {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require e.calendar != .some(k) "It is in that calendar already"
    e.calendar = k
}

/// Invites someone to an event, by the name and address the person gives.
action invite(e: Event, name: Text, email: Text) -> Attendee (
    icon: .user_plus,
    label: "Invite",
    shows: [e.title, (e.organizer, .addresses), (guests(e), .addresses), name, (email, .addresses)],
) {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require email.trim().contains("@") "That is not an email address"
    require not e.attendees.any(g -> g.email == email.trim()) "They are coming already"
    e.attendees.insert(Attendee(name: name.trim(), email: email.trim()))
}

/// Records someone's answer to an invitation: `accepted`, `declined` or `tentative`.
action answer(e: Event, g: Attendee, status: Text) (icon: .check, label: "Answer") {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    require status == "accepted" or status == "declined" or status == "tentative" "An answer is accepted, declined or maybe"
    g.status = status
}

/// Answers the person's own invitation to an account's event, telling the organiser through the
/// account's server: what settles the reply the invitation asked for.
action answer_and_tell(e: Event, g: Attendee, said: Text) (
    icon: .check,
    label: "Answer the invitation",
) {
    require own_guest(e, g) "Only your own invitation is answered at the server"
    answer(e, g, said)
    if let k = e.calendar {
        if let a = calendar.accounts.find(a -> a.key == k.account) {
            outside("caldav.answer", (a.key, e.key, g.email, said))
        }
    }
}

/// Takes someone off an event's guests: nothing is sent to them.
action uninvite(e: Event, g: Attendee) (icon: .x, label: "Remove guest") {
    require changes_for(e, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require writable(e) "This calendar is connected for reading only, so nothing in it is changed"
    e.attendees.remove(g)
}

/// Starts a message inviting a guest, the event's calendar invitation carried beside it: what Mail
/// shows the person, to read and send. Nothing is sent by this.
action invite_by_mail(e: Event, g: Attendee) (icon: .mail, label: "Invite by mail") {
    // Mail prepares the invitation, and a workspace with no mailbox to send it from says so
    // here rather than leaving the press with nothing of its own to show.
    require not mail.mail.accounts.is_empty() "Add a mail account in Settings first"
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
    for who in workspace.members.people {
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

/// The colour an event is drawn in: the one it names, or its calendar's when it names none.
fn event_colour(e: Event) -> Text {
    if e.color != "" { e.color } else if let k = e.calendar { k.color } else { "" }
}

/// Whether an event may be changed: its calendar's server keeps some for reading only.
fn writable(e: Event) -> Bool {
    if let k = e.calendar { k.writable } else { true }
}

/// Whether the guest is the person the event's account signs in as: only their own invitation
/// is answered at the server, since anyone else's answer is the organiser's record of theirs.
fn own_guest(e: Event, g: Attendee) -> Bool {
    if let k = e.calendar {
        if k.account != "" {
            if let a = calendar.accounts.find(a -> a.key == k.account) {
                return a.address != "" and a.address.lower() == g.email.lower()
            }
        }
    }
    false
}

/// Whether `by` may change or take out an entry. A change reaches the entry's server, so an agent
/// asks its person first; and what the person entered themselves, or an invitation that reached
/// the calendar another way than through the agent, is theirs to change: an agent changes only
/// an entry it put in itself, as the entry's history says.
fn changes_for(e: Event, by: Actor) -> Bool {
    by.kind() != .agent or created(e)?.who == by
}

/// Whether `by` may file an entry under categories: filing rewrites the entry whole on its
/// server, and a server that schedules by itself writes to the people an entry its person
/// organises invites. So an agent files no entry its person invites others to.
fn files_for(e: Event, by: Actor) -> Bool {
    by.kind() != .agent or not invites_others(e)
}

/// Whether the entry's person invites others to it: it names people, and its organiser is this
/// workspace or the address its account signs in as.
fn invites_others(e: Event) -> Bool {
    if e.attendees.is_empty() {
        return false
    }
    let own = account_address(e)
    e.organizer == "" or own != "" and e.organizer.lower() == own.lower()
}

/// The address the account of an entry's calendar signs in as; empty for an entry no account
/// holds.
fn account_address(e: Event) -> Text {
    if let k = e.calendar {
        if let a = calendar.accounts.find(a -> a.key == k.account) {
            return a.address
        }
    }
    ""
}

/// The value of the invitation line `name` in an invitation's text: `UID`, `METHOD`, the address
/// `ORGANIZER` names. Empty where the text holds none.
fn invitation_line(text: Text, name: Text) -> Text {
    let lines = text.split("\n").map(x -> x.trim())
    let value = field(lines, name)
    if name == "ORGANIZER" { ics_mailto(value) } else { value }
}

/// The entry an invitation's text is a version of: the one under its `UID`, if the calendar holds
/// one.
fn invitation_held(c: Calendars, text: Text) -> Option<Event> {
    let uid = invitation_line(text, "UID")
    if uid == "" { .none } else { c.events.find(e -> e.key == uid) }
}

/// Whether `by` may take in the invitation `text`: a new one, or a version of an entry it may
/// change ([`changes_for`]).
fn invitation_changes_for(c: Calendars, text: Text, by: Actor) -> Bool {
    if let e = invitation_held(c, text) { changes_for(e, by) } else { true }
}

/// Whether the invitation `text` comes from the sender of the entry it is a version of, where an
/// agent hands it over: a person's own word changes what is theirs whoever it names.
fn invitation_from_sender(c: Calendars, text: Text, by: Actor) -> Bool {
    if by.kind() != .agent {
        return true
    }
    if let e = invitation_held(c, text) {
        invitation_line(text, "ORGANIZER").lower() == e.organizer.lower()
    } else { true }
}

/// Who an invitation's text invites: each address it names as a guest, in its order. What its
/// person reads before an agent takes it in.
fn invites(text: Text) -> List<Text> {
    text
        .split("\n")
        .map(x -> x.trim())
        .filter(x -> x.starts_with("ATTENDEE"))
        .map(x -> ics_mailto(x))
        .filter(x -> x != "")
}

/// Who organises the invitation `text`: the address its `ORGANIZER` names, empty where it names
/// none.
fn organised_by(text: Text) -> Text {
    invitation_line(text, "ORGANIZER")
}

/// How many addresses an invitation's text names: its organiser's and each guest's. Every one of
/// them is shown to the person before an agent takes it in, so a text naming more than one
/// question shows is not put to them at all.
fn addresses_named(text: Text) -> Int {
    let organiser = if organised_by(text) == "" { 0 } else { 1 }
    organiser + invites(text).len()
}

/// The addresses an event invites, in its order.
fn guests(e: Event) -> List<Text> {
    e.attendees.map(g -> g.email)
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
    let base = if e.repeat == .daily {
        "FREQ=DAILY"
    } else if e.repeat == .weekly or e.repeat == .weekdays {
        "FREQ=WEEKLY"
    } else if e.repeat == .monthly {
        "FREQ=MONTHLY"
    } else if e.repeat == .yearly { "FREQ=YEARLY" } else { "" }
    // The days a weekly repeat comes on are its rule's `BYDAY`, so the server keeps them.
    let days = weekday_rule(e)
    if base == "FREQ=WEEKLY" and days != "" { "{base};BYDAY={days}" } else { base }
}

/// The days a weekly repeat comes on, as its rule writes them (`MO,WE`): empty when it keeps
/// the day it stands on.
fn weekday_rule(e: Event) -> Text {
    if e.repeat != .weekly and e.repeat != .weekdays {
        return ""
    }
    ["MO", "TU", "WE", "TH", "FR", "SA", "SU"]
        .filter(day -> e.recurrence_weekdays.contains(day))
        .join(",")
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
            if e.recurrence_every > 1 { "INTERVAL={e.recurrence_every}" } else { "" },
            if let u = e.recurrence_until { "UNTIL={stamp_day(u)}" } else { "" },
            if let n = e.recurrence_count { "COUNT={n}" } else { "" },
            if e.recurrence_weekdays.is_empty() {
                ""
            } else {
                "BYDAY={e.recurrence_weekdays.sort_by(d -> d).join(",")}"
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

/// A time the workspace's calendars hold taken: when it starts and ends, and whether it takes the
/// whole day. What a reader of free and busy times learns of an entry, and nothing else of it.
type Busy (noun: "busy time") {
    start: Time
    end: Time
    all_day: Bool = false
}

/// When the workspace's calendars are taken between two days, both included: every occurrence of
/// each entry that is not called off, as the calendar shows it, in the order they start. What a
/// member, or their agent, reads to find a time that suits; somebody who is no member of the
/// workspace reads nothing.
action busy(c: Calendars, from: Date, to: Date) -> List<Busy> (
    icon: .calendar,
    label: "Busy times",
) {
    let taken: List<Busy> = []
    let held = c.events.filter(
        e -> not e.cancelled and (e.recurrence != "" or e.day <= to and e.end_day >= from),
    )
    for e in held {
        for at in moments(e, from, to) {
            taken = taken + [Busy(start: at, end: at + (ends(e) - starts(e)), all_day: e.all_day)]
        }
    }
    taken.sort_by(b -> b.start)
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

/// The days from `from` to `to`, both included: the bands the agenda stands its rows under.
fn days_between(from: Date, to: Date) -> List<Date> {
    if from > to { [] } else { [from] + days_between(from + 1d, to) }
}

/// Whether a task is due on a day from `from` to `to`, both included.
fn task_in(t: Task, from: Date, to: Date) -> Bool {
    if let day = t.due { day >= from and day <= to } else { false }
}

/// Whether a task is found by what the window's search holds.
fn task_named(t: Task, q: Text) -> Bool {
    q.is_empty() or t.title.lower().contains(q.lower())
}

/// A task's priority in the person's words.
fn priority_name(p: Priority) -> Text {
    if p == .high {
        std.say("High")
    } else if p == .medium { std.say("Medium") } else if p == .low { std.say("Low") } else { "" }
}

/// A day of the week a repeat names, in the person's words: `MO` is Monday.
fn weekday_name(day: Text) -> Text {
    if day == "MO" {
        std.say("Monday")
    } else if day == "TU" {
        std.say("Tuesday")
    } else if day == "WE" {
        std.say("Wednesday")
    } else if day == "TH" {
        std.say("Thursday")
    } else if day == "FR" {
        std.say("Friday")
    } else if day == "SA" { std.say("Saturday") } else { std.say("Sunday") }
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

/// The calendar (every event and task it holds, or only one event) as an iCalendar file, for
/// a person to save or an agent to hand on. Nothing is written by this. The system's one
/// Export… lists it, and saves the file where the person says.
action export_calendar(c: Calendars, e: Option<Event> = .none) -> File (
    icon: .download,
    label: "Export calendar",
    export: "iCalendar",
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

/// Takes an invitation into the calendar: the event it asks for is kept by the account's server
/// where one can be written to it, so the organiser hears the answer, and by this workspace
/// otherwise. What the server took comes back with the account's next sync.
action take_invitation(c: Calendars, text: Text) -> Text uses time (
    icon: .calendar,
    label: "Take an invitation in",
    shows: [
    addresses_named(text),
    (organised_by(text), .addresses),
    (invites(text), .addresses),
    (text, .long),
],
) {
    require text.find("BEGIN:VEVENT") != .none "That is not a calendar invitation"
    // An agent brings up to date and takes out only an invitation it put in itself, and only
    // for the sender it came from: the text it hands over is its own writing, and naming the
    // entry's organiser proves nothing.
    require invitation_changes_for(c, text, invoker) "That entry is its person's: an agent changes only what it put in itself"
    require invitation_from_sender(c, text, invoker) "Only its sender changes or calls off an invitation, and this one names another"
    let uid = invitation_line(text, "UID")
    let held = invitation_held(c, text)
    if let k = c.calendars.find(k -> k.account != "" and k.writable) {
        // What is put in for the first time is the invoker's entry from now on, as its history
        // says: it stands here under its UID until its server's copy of it comes in over it.
        if held == .none and uid != "" and invitation_line(text, "METHOD") != "CANCEL" {
            let start = wall_text(invitation_line(text, "DTSTART"))
            c.events.insert(
                Event(
                    key: uid,
                    title: invitation_line(text, "SUMMARY"),
                    start: start,
                    end: start,
                    organizer: invitation_line(text, "ORGANIZER"),
                    day: Date.parse(start, near: now) ?? today,
                    end_day: Date.parse(start, near: now) ?? today,
                ),
            )
        }
        outside("caldav.import", (k.account, k.key, text))
        std.say("Added to {k.name}")
    } else {
        import_ics(c, File.of_text("invitation.ics", text))
    }
}

/// Imports the events and tasks of an iCalendar file, and answers how many it took: what a
/// person's own file offers, and what another calendar exported, such as the calendars another
/// computer's mail program kept. An event the calendar holds already, by its `UID`, is kept
/// once, so importing the same file again adds nothing. A line naming a zone this build cannot
/// read is read in the person's zone.
action import_ics(c: Calendars, file: File) -> Text uses time (
    icon: .upload,
    label: "Import calendar",
    opens: ["text/calendar", .ics, .ical],
) {
    let read = match file.text() {
        .ok(text) => unfolded(text),
        .err(why) => {
            return std.say("Could not read {file.name}: {why}")
        }
    }
    // A calendar's parts, each a block between its BEGIN and END: what one event or task says,
    // read by its own lines. An event's own blocks (an alarm) end without each being one.
    let events = read
        .split("END:VEVENT")
        .filter(x -> x.contains("BEGIN:VEVENT") and not ics_held(c, x))
    for segment in events {
        let chunk = segment.split("BEGIN:VEVENT").last() ?? ""
        let lines = chunk.split("\n")
        let title = ics_text(field(lines, "SUMMARY"))
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
                    key: field(lines, "UID"),
                    title: title,
                    start: start_wall,
                    end: end_wall,
                    all_day: all_day,
                    timezone: zone,
                    location: ics_text(field(lines, "LOCATION")),
                    body: RichText(ics_text(field(lines, "DESCRIPTION"))),
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
/// names none. Thunderbird writes the zone's name behind a prefix of its own
/// (`/mozilla.org/20050126_1/America/New_York`), which is left out.
fn ics_zone(lines: List<Text>, name: Text) -> Text {
    let line = lines.find(x -> x.starts_with("{name};") and x.contains("TZID=")) ?? ""
    if let at = line.find("TZID=") {
        let rest = line.slice(at + 5, line.len())
        let zone = if let colon = rest.find(":") {
            rest.slice(0, colon).trim()
        } else { rest.trim() }
        if zone.starts_with("/mozilla.org/") {
            let tail = zone.slice(13, zone.len())
            if let slash = tail.find("/") { tail.slice(slash + 1, tail.len()) } else { zone }
        } else { zone }
    } else { "" }
}

/// Whether the calendar holds the event a file's block describes already: one with its `UID`.
fn ics_held(c: Calendars, block: Text) -> Bool {
    let uid = field(block.split("\n"), "UID")
    uid != "" and c.events.find(e -> e.key == uid) != .none
}

/// A calendar file's text with each long line whole again: a line it folded goes on in the
/// next one, which starts with a space or a tab.
fn unfolded(text: Text) -> Text {
    text.replace("\r\n ", "").replace("\r\n\t", "").replace("\n ", "").replace("\n\t", "")
}

/// A text value as a calendar file escapes it (`\,`, `\;`, `\n`), as it reads.
fn ics_text(value: Text) -> Text {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
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
                status: ics_parameter(line, "PARTSTAT=").lower(),
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
    // The quick-add line being typed and whether its sheet is open.
    state quick: Text = ""
    state quick_open: Bool = false
    // Whether the keyboard reference is open.
    state help: Bool = false
    // A window asked to show an event shows the week it stands on and opens it.
    on show(e: Event): set(place, "week").then(open(e))
    // A window asked to show a project lists its tasks.
    on show(p: Project): set(place, "tasks").then(set(task_project, .some(p)))
    // A window asked to show one calendar shows its events alone, as choosing it in the sidebar does.
    on show(k: Calendar): set(chosen, .some(k))
    // The places the launcher, the palette, an agent or a script opens the window at.
    on show week: set(place, "week")
    on show month: set(place, "month")
    on show day: set(place, "day")
    on show agenda: set(place, "agenda")
    on show tasks: set(place, "tasks")
    on show calendars "All calendars": set(chosen, .none)
    on show inbox "Task inbox": set(place, "tasks")
        .then(set(smart, "inbox"))
        .then(set(task_project, .none))
    on show today "Today's tasks": set(place, "tasks")
        .then(set(smart, "today"))
        .then(set(task_project, .none))
    on show upcoming "Upcoming tasks": set(place, "tasks")
        .then(set(smart, "upcoming"))
        .then(set(task_project, .none))
    on show done "Done tasks": set(place, "tasks")
        .then(set(smart, "done"))
        .then(set(task_project, .none))
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
        // What each account's server says of itself stands beside the sidebar's name, so a
        // calendar whose server will not answer is seen from wherever the person is.
        sidebar_actions: {
            for a in c.accounts {
                std.service_mark(account_name(a), a.online, a.trouble)
            }
        }
        actions: {
            ui.Button("New event", icon: .calendar_plus, key: "N") {
                on press: new_event(c)
            }
            ui.Button(
                "",
                icon: .circle_help,
                icon_only: true,
                accessible_name: std.say("Keyboard shortcuts"),
                keys: ["?"],
            ) {
                on press: set(help, true)
            }
            ui.Button("Quick add", icon: .plus, key: "Shift+N") {
                on press: set(quick, "").then(set(quick_open, true))
            }
            ui.Button(
                "",
                icon: .refresh,
                icon_only: true,
                accessible_name: std.say("Sync calendars"),
            ) {
                on press: sync(c)
            }
            ui.SearchButton()
        }
        sidebar: {
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
            stack(gap: .related, fill: true) {
                account_notices(c)
                if place == "agenda" {
                    agenda(c, chosen, searched, hidden)
                } else if place == "tasks" {
                    tasks(c, smart, task_project, searched)
                } else {
                    timeline(c, chosen, searched, hidden, place, picked)
                }
            }
        }
    }
    if quick_open {
        quick_add_sheet(c, quick, quick_open)
    }
    if help {
        shortcuts_sheet(help)
    }
}

/// The keyboard reference: what a press does in the calendar window.
view shortcuts_sheet(open: State<Bool>) {
    ui.ModalDialog(
        title: "Keyboard shortcuts",
        icon: .circle_help,
        open: open,
        secondary: "Close",
    ) {
        on act_secondary: set(open, false)
        on dismiss: set(open, false)
        stack(gap: .related, padding: .related) {
            ui.KeyHints {
                ui.KeyHint(["N"], "New event")
                ui.KeyHint(["Shift+N"], "Quick add")
                ui.KeyHint(["Left, Right"], "Previous or next day")
                ui.KeyHint(["Up, Down"], "Previous or next week")
                ui.KeyHint(["Delete"], "Delete the event on its page")
                ui.KeyHint(["?", "Esc"], "Show or close this reference")
            }
        }
    }
}

/// The quick-add sheet: one line read as an event, what it will write said under it, and then the
/// event made. Reading what is typed says its day and hours before anything is written.
view quick_add_sheet(c: Calendars, line: State<Text>, open: State<Bool>) {
    ui.ModalDialog(title: "Quick add", open: open, primary: "Add event", secondary: "Cancel") {
        body: {
            stack(gap: .related) {
                ui.Text("What and when", role: .caption, secondary: true)
                ui.TextInput(bind: line, placeholder: "Lunch with Sam friday 1pm", autofocus: true)
                ui.Text(
                    quick_said(line, c.anchor_at, quick_anchor(c, c.anchor_at)),
                    role: .caption,
                    secondary: true,
                )
            }
        }
        on act_primary: quick_add(c, line).then(set(line, "")).then(set(open, false))
        on act_secondary: set(open, false)
        on dismiss: set(open, false)
    }
}

/// What each account's server says of itself, over the window: a server that cannot be reached
/// says so with "Sync again", calendars it would not take say why, and one being asked now says
/// that. An account that stands well and is not being asked takes no room.
view account_notices(c: Calendars) {
    for a in c.accounts {
        let who = account_name(a)
        if a.trouble != "" {
            stack(padding: .group, gap: .tight) {
                std.service_banner(
                    who,
                    a.online,
                    a.trouble,
                    retry: sync_account(a),
                    fetched: fetched_clock(a),
                    retry_label: std.say("Sync again"),
                )
            }
        } else if a.checking {
            stack(padding: .group, gap: .tight) {
                ui.Spinner(std.say("Syncing {who}…"))
            }
        }
    }
    // A calendar kept by an account the workspace no longer configures said nothing of its
    // server: its events stand as they were last read.
    for k in c.calendars {
        if k.account != "" and not c.accounts.any(a -> a.key == k.account) {
            stack(padding: .group, gap: .tight) {
                ui.Banner(
                    std.say("{k.name} is no longer kept"),
                    message: std.say(
                        "Its account is not configured any more. Add it back in Settings, or its events stay as they are.",
                    ),
                    kind: .neutral,
                    icon: .calendar,
                )
            }
        }
    }
}

/// Whether the address book holds anyone with an address to invite.
fn book_has_email(book: contacts.Book) -> Bool {
    book.contacts.any(who -> not who.emails.is_empty())
}

/// What an account is called: its name, or the address it signs in with when it has none.
fn account_name(a: Account) -> Text {
    if a.name != "" { a.name } else { a.address }
}

/// When an account's calendars were last taken in, as the clock the person reads: empty while
/// they never were.
fn fetched_clock(a: Account) -> Text {
    let never = Time("1970-01-01T00:00:00Z")
    if a.fetched <= never {
        return ""
    }
    a.fetched.clock()
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
    picked: State<Option<Date>>,
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
                    keys: ["ArrowLeft"],
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
                    keys: ["ArrowRight"],
                ) {
                    on press: next_day(c)
                }
            } else if place == "month" {
                ui.Button(
                    "",
                    icon: .chevron_left,
                    icon_only: true,
                    accessible_name: std.say("Previous month"),
                    keys: ["ArrowLeft"],
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
                    keys: ["ArrowRight"],
                ) {
                    on press: next_month(c)
                }
            } else {
                ui.Button(
                    "",
                    icon: .chevron_left,
                    icon_only: true,
                    accessible_name: std.say("Previous week"),
                    keys: ["ArrowLeft"],
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
                    keys: ["ArrowRight"],
                ) {
                    on press: next_week(c)
                }
            }
            ui.MenuButton("Jump", icon: .calendar, icon_only: true) {
                ui.MenuItem(std.say("Previous week"), key: "ArrowUp") {
                    on choose: previous_week(c)
                }
                ui.MenuItem(std.say("Next week"), key: "ArrowDown") {
                    on choose: next_week(c)
                }
            }
            ui.DatePicker(
                bind: picked,
                label: std.say("Pick a day"),
                marked: c.events.filter(e -> not e.cancelled).map(e -> e.day.iso()),
            ) {
                on change(at): go_to_day(c, at)
            }
        }
        // A line typed here becomes an event where the window stands: "Lunch with Sam Friday
        // 1pm" names the day and the hour, and the words nothing else takes are its title.
        std.add_entry(std.say("New event, such as Lunch with Sam Friday at 1pm"), new_event(c))
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
    let colour = event_colour(e)
    ui.TimelineItem(
        e,
        title: e.title,
        start: starts(e),
        end: if e.all_day { ends(e) - 1d } else { ends(e) },
        all_day: e.all_day,
        location: e.location,
        tone: if colour == "indigo" {
            .first
        } else if colour == "emerald" {
            .second
        } else if colour == "amber" {
            .third
        } else if colour == "red" {
            .fourth
        } else if colour == "blue" {
            .fifth
        } else if colour == "violet" {
            .sixth
        } else if colour == "slate" { .seventh } else { .neutral },
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
    let colour = event_colour(e)
    ui.TimelineItem(
        e,
        title: e.title,
        start: at,
        end: at + (ends(e) - starts(e)),
        location: e.location,
        tone: if colour == "indigo" {
            .first
        } else if colour == "emerald" {
            .second
        } else if colour == "amber" {
            .third
        } else if colour == "red" {
            .fourth
        } else if colour == "blue" {
            .fifth
        } else if colour == "violet" {
            .sixth
        } else if colour == "slate" { .seventh } else { .neutral },
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

/// What is coming, by day: the events of each day and the tasks due on it, with the controls
/// that walk the agenda a week at a time and the entry that adds an event at the foot.
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
    // The tasks due in the same days, each on the day it is due: what is coming is the day's
    // work, not only what stands on a calendar.
    let due = c.tasks.filter(
        t -> t.parent == .none and task_in(t, from, to) and task_named(t, searched),
    )
    ui.Docked {
        foot: {
            stack {
                std.add_entry(std.say("New event"), new_event(c))
            }
        }
        stack(gap: .group) {
            // Walking the agenda a week at a time, as the week and month are walked.
            row(gap: .tight) {
                ui.Button(
                    "",
                    icon: .chevron_left,
                    icon_only: true,
                    accessible_name: std.say("Previous week"),
                    keys: ["ArrowLeft"],
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
                    keys: ["ArrowRight"],
                ) {
                    on press: next_week(c)
                }
                ui.Text("{from} – {to}", role: .caption, secondary: true, fill: 1.0)
            }
            if shown.is_empty() and due.is_empty() {
                ui.EmptyState(std.say("Nothing scheduled in the next three weeks"), icon: .calendar)
            } else {
                // One band per day, its events at their own moments and its tasks beside them,
                // each opening where it stands.
                for day in days_between(from, to) {
                    let on_day = shown.filter(r -> r.day == day).sort_by(.at)
                    let due_on = due.filter(t -> t.due == .some(day))
                    if not on_day.is_empty() or not due_on.is_empty() {
                        section("{day}") {
                            for r in on_day {
                                ui.ListRow(
                                    title: r.event.title,
                                    secondary: r.event.location,
                                    item: r.event,
                                    icon: .calendar,
                                    meta: if r.event.all_day {
                                        std.say("All day")
                                    } else { r.at.clock() },
                                    motion: .brisk,
                                ) {
                                    on activate: open(r.event)
                                }
                            }
                            for t in due_on {
                                agenda_task(t)
                            }
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

/// A task as the agenda draws it: its box, its title, its project and how much it matters.
view agenda_task(t: Task) {
    row(
        t.title,
        checkbox: t.done,
        check: check_task(t),
        secondary: if let p = t.project { p.name } else { "" },
        meta: if t.priority != .none { priority_name(t.priority) } else { "" },
    )
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
        // An event still waiting for its first word says so, and the person can let it go
        // instead of leaving an untitled entry behind.
        if pristine(e) {
            ui.Banner(
                std.say("This event is still empty"),
                message: std.say("Fill it in, or discard it and it leaves no trace."),
                kind: .neutral,
                icon: .calendar,
                action: std.say("Discard"),
            ) {
                on act: discard_draft(calendar, e)
            }
        }
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
                ui.DateField(bind: e.end_day, label: std.say("Last day")) {
                    on change(at): move_end_day(e, at)
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
                    ui.MenuItem(std.say("15 minutes before"), icon: .bell) {
                        on choose: remind_before(e, 15)
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
                    // A weekly repeat says which days of the week it comes on: every day of the
                    // chosen ones, in the order a calendar writes them.
                    if e.repeat == .weekly or e.repeat == .weekdays {
                        row(gap: .tight, wrap: true) {
                            for day in ["MO", "TU", "WE", "TH", "FR", "SA", "SU"] {
                                ui.Chip(
                                    weekday_name(day),
                                    selected: e.recurrence_weekdays.contains(day),
                                ) {
                                    on press: toggle_weekday(e, day)
                                }
                            }
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
                                        on choose: if own_guest(e, g) {
                                            answer_and_tell(e, g, "accepted")
                                        } else { answer(e, g, "accepted") }
                                    }
                                    ui.MenuItem(
                                        std.say("Declined"),
                                        icon: .x,
                                        checked: g.status == "declined",
                                    ) {
                                        on choose: if own_guest(e, g) {
                                            answer_and_tell(e, g, "declined")
                                        } else { answer(e, g, "declined") }
                                    }
                                    ui.MenuItem(
                                        std.say("Maybe"),
                                        icon: .circle_help,
                                        checked: g.status == "tentative",
                                    ) {
                                        on choose: if own_guest(e, g) {
                                            answer_and_tell(e, g, "tentative")
                                        } else { answer(e, g, "tentative") }
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
                // Anyone the workspace's address book knows, invited by the name and address the
                // book keeps; a guest the book does not know is typed by their address.
                if book_has_email(contacts.book) {
                    ui.MenuButton(std.say("From Contacts"), icon: .user_plus) {
                        for who in contacts.book.contacts {
                            for reach in who.emails {
                                if reach.value != ""
                                    and not e.attendees.any(g -> g.email == reach.value) {
                                    ui.MenuItem(who.name, icon: .user, description: reach.value) {
                                        on choose: invite(e, who.name, reach.value)
                                    }
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
