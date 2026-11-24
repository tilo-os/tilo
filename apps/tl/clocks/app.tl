app so.tilo.clocks "Clocks" (
    icon: .clock,
    category: .utility,
    tagline: "World clocks, alarms, timers and a stopwatch",
    keywords: ["clock", "clocks", "alarm", "alarms", "timer", "timers", "stopwatch", "world clock", "time", "countdown", "wecker", "uhr", "stoppuhr", "weltzeit", "zeit", "countdown"],
    previously: so.tilo.lang.clocks,
)

use so.tilo.device as device

/// A day of the week an alarm repeats on, Monday first.
enum Weekday {
    /// Monday
    mon
    /// Tuesday
    tue
    /// Wednesday
    wed
    /// Thursday
    thu
    /// Friday
    fri
    /// Saturday
    sat
    /// Sunday
    sun
}

/// Every weekday an alarm may repeat on, Monday first.
fn weekdays() -> List<Weekday> {
    [.mon, .tue, .wed, .thu, .fri, .sat, .sun]
}

/// A city whose time a person compares with their own.
type WorldClock (untitled: "City") {
    /// What the person calls the clock: the city's name.
    city: Text
    /// The IANA zone it keeps time by, such as `Asia/Tokyo`.
    zone: Text
}

/// An alarm: a time of day, the days it repeats on, and where it rings.
type Alarm (untitled: "Alarm") {
    label: Text
    /// The hour it rings at, in the person's zone.
    hour: Int = 7
    /// The minute it rings at.
    minute: Int = 30
    /// The weekdays it repeats on; none rings once, at the next such time.
    days: Set<Weekday>
    /// Whether it rings at all.
    enabled: Bool = true
    /// The instant it next rings, which the app keeps as the trigger reads it; none while it is
    /// off.
    at: Option<Time> = .none
    /// The device it rings on, by its id; empty rings on every device of the person's.
    rings_on: Text = ""
    /// Whether the ring at `at` is a snooze: a repeating alarm snoozed rings again and carries on
    /// with its days.
    snoozed: Bool = false
}

/// A timer: a countdown that runs to zero and rings there.
type Timer (untitled: "Timer") {
    label: Text
    /// How long it counts down, in seconds.
    seconds: Int = 300
    /// The instant it finishes; none while it is paused.
    ends_at: Option<Time> = .none
    /// What it has left while it is paused, in seconds.
    left: Option<Int> = .none
}

/// A lap the stopwatch kept: the time it stood at.
type Lap {
    /// The time it stood at, in seconds.
    at: Int
}

/// The stopwatch: what it has counted, the run it is in, and the laps it kept.
type Stopwatch {
    /// What it has counted before the run it is in, in seconds.
    elapsed: Int merge counter
    /// The instant its run started; none while it is paused.
    since: Option<Time> = .none
    /// The times it stood at when a lap was kept, in order.
    laps: Collection<Lap>
}

/// The workspace's clocks.
type Clocks {
    /// The cities the person compares time with.
    world_clocks: Collection<WorldClock>
    /// The alarms set, on whichever device each rings.
    alarms: Collection<Alarm>
    /// The timers running, paused or done.
    timers: Collection<Timer>
    /// The stopwatch: at zero until it is started.
    stopwatch: Stopwatch
    /// Whether a timer is running or paused, or the stopwatch is running or has counted: the
    /// clocks stand in the shell's bar, quick settings and on the lock screen of every device
    /// while one is, and the person floats them as a card on the devices they choose.
    active: Bool = false

    live when active
}

root clocks: Clocks

default view clocks_page for Clocks

/// `n` as two digits, for the minute of a wall-clock time.
fn pad2(n: Int) -> Text {
    if n < 10 { "0{n}" } else { "{n}" }
}

/// The weekday `day` falls on.
fn day_of(day: Date) -> Weekday {
    let weekday = day.weekday()
    if weekday == .monday {
        .mon
    } else if weekday == .tuesday {
        .tue
    } else if weekday == .wednesday {
        .wed
    } else if weekday == .thursday {
        .thu
    } else if weekday == .friday { .fri } else if weekday == .saturday { .sat } else { .sun }
}

/// A weekday's name as the window shows it (`Mon`).
fn day_name(d: Weekday) -> Text {
    if d == .mon {
        "Mon"
    } else if d == .tue {
        "Tue"
    } else if d == .wed {
        "Wed"
    } else if d == .thu {
        "Thu"
    } else if d == .fri { "Fri" } else if d == .sat { "Sat" } else { "Sun" }
}

/// The weekdays `a` repeats on, as the list `next_ring` reads.
fn days_of(a: Alarm) -> List<Weekday> {
    weekdays().filter(d -> a.days.contains(d))
}

/// The next instant the alarm at `hour`:`minute` rings, by the days it repeats on, after `from`:
/// today at that time while it is still to come, else the next day the alarm names. No days
/// rings once, at the next such time.
fn next_ring(hour: Int, minute: Int, days: List<Weekday>, from: Time) -> Option<Time> {
    let today = from.day()
    for ahead in [0, 1, 2, 3, 4, 5, 6, 7] {
        let day = today + Duration(ahead * 86400)
        if days.is_empty() or days.contains(day_of(day)) {
            if let at = Time.parse("{day.iso()} {hour}:{pad2(minute)}", near: from) {
                if at > from {
                    return .some(at)
                }
            }
        }
    }
    .none
}

/// The days `days` holds after `d` is added or taken away: a chip's press.
fn toggled_day(days: List<Weekday>, d: Weekday) -> List<Weekday> {
    if days.contains(d) { days.filter(x -> x != d) } else { days + [d] }
}

/// Whether a timer or the stopwatch is running or paused with time counted: whether anything of
/// the clocks stands in the shell.
fn anything_running(c: Clocks) -> Bool {
    c.timers.any(t -> t.ends_at != .none or t.left != .none)
        or c.stopwatch.since != .none
        or c.stopwatch.elapsed > 0
}

/// Adds a city's clock to the workspace.
action add_world_clock(c: Clocks, city: Text, zone: Text) -> WorldClock (icon: .globe) {
    if let seen = c.world_clocks.find(w -> w.zone == zone) {
        return seen
    }
    c.world_clocks.insert(WorldClock(city: city, zone: zone))
}

/// Removes a city's clock.
destructive action remove_world_clock(c: Clocks, w: WorldClock) (icon: .trash) {
    c.world_clocks.remove(w)
}

/// Sets an alarm: at the time of day `when` names, on the days it repeats on, ringing on
/// `rings_on`, every device of the person's when it names none. It rings first at the next such
/// time.
action add_alarm(
    c: Clocks,
    label: Text,
    hour: Int,
    minute: Int,
    days: List<Weekday> = [],
    rings_on: Text = "",
) -> Alarm uses time (icon: .alarm_clock, label: "New alarm") {
    require hour >= 0 and hour <= 23 "An alarm rings at an hour from 0 to 23"
    require minute >= 0 and minute <= 59 "An alarm rings at a minute from 0 to 59"
    let made = c.alarms.insert(
        Alarm(
            label: label,
            hour: hour,
            minute: minute,
            at: next_ring(hour, minute, days, now),
            rings_on: rings_on,
        ),
    )
    for d in days {
        made.days.add(d)
    }
    made
}

/// Changes an alarm's words, time and days. Its on/off state stays as it was, and one that is on
/// rings next at the time it now names.
action edit_alarm(
    a: Alarm,
    label: Text,
    hour: Int,
    minute: Int,
    days: List<Weekday> = [],
) uses time (icon: .pencil, label: "Edit alarm") {
    require hour >= 0 and hour <= 23 "An alarm rings at an hour from 0 to 23"
    require minute >= 0 and minute <= 59 "An alarm rings at a minute from 0 to 59"
    a.label = label
    a.hour = hour
    a.minute = minute
    let kept = days_of(a)
    for d in kept {
        a.days.remove(d)
    }
    for d in days {
        a.days.add(d)
    }
    if a.enabled {
        a.at = next_ring(hour, minute, days, now)
    }
}

/// Turns an alarm on or off: off, it does not ring; on, it rings next at its time.
action set_alarm_enabled(a: Alarm, enabled: Bool = not a.enabled) uses time (icon: .bell) {
    a.enabled = enabled
    if enabled {
        a.at = next_ring(a.hour, a.minute, days_of(a), now)
    } else {
        a.at = .none
        a.snoozed = false
    }
}

/// Names the device an alarm rings on: one device's id, or none for every device of the person's.
action set_alarm_device(a: Alarm, rings_on: Text) (icon: .monitor) {
    a.rings_on = rings_on
}

/// Repeats an alarm on `d`, or stops repeating on it: a day it names rings, a day it does not
/// skips. Its next ring moves to the next day it names.
action toggle_day(a: Alarm, d: Weekday) uses time (icon: .calendar) {
    if a.days.contains(d) { a.days.remove(d) } else { a.days.add(d) }
    if a.enabled {
        a.at = next_ring(a.hour, a.minute, days_of(a), now)
    }
}

/// Removes an alarm.
destructive action remove_alarm(c: Clocks, a: Alarm) (icon: .trash) {
    c.alarms.remove(a)
}

/// Snoozes the alarm ringing now: it rings again in five minutes.
action snooze_alarm(a: Alarm) uses time (icon: .clock, label: "Snooze 5 minutes") {
    a.at = .some(now + 5m)
    a.snoozed = true
}

/// Rings an alarm when its moment comes, on the device it names, window or no window. A repeating
/// alarm rings again the next day it names; one that rings once turns itself off.
action ring_alarm(a: Alarm) uses notify, time (icon: .alarm_clock, label: "Ring the alarm now") {
    if not a.enabled {
        return
    }
    let who: Option<Actor> = if let made = created(a) { .some(made.who) } else { .none }
    if let told = who {
        notify(
            to: told,
            title: a.label,
            body: "It is {a.hour}:{pad2(a.minute)}",
            opens: a,
            device: a.rings_on,
            on: ["snooze_alarm"],
        )
    }
    if a.days.is_empty() {
        a.enabled = false
        a.at = .none
    } else {
        a.at = next_ring(a.hour, a.minute, days_of(a), now)
    }
    a.snoozed = false
}

/// Rings an alarm when its moment comes, once for the whole workspace, window or not. The device
/// it names arms its own wakeup for the moment, so it rings there offline and asleep too.
trigger alarm_rings(a: Alarm) at a.at on a.rings_on: ring_alarm(a)

/// Starts a timer of `seconds`.
action add_timer(c: Clocks, label: Text, seconds: Int) -> Timer uses time (
    icon: .timer,
    label: "Start a timer",
) {
    require seconds > 0 "A timer needs a duration greater than zero"
    let made = c.timers.insert(
        Timer(label: label, seconds: seconds, ends_at: .some(now + Duration(seconds))),
    )
    c.active = anything_running(c)
    made
}

/// Pauses a running timer where it stands.
action pause_timer(t: Timer) uses time (icon: .pause) {
    if let ends = t.ends_at {
        t.left = .some((ends - now).seconds())
        t.ends_at = .none
    }
    let c = root Clocks
    c.active = anything_running(c)
}

/// Resumes a paused timer, or starts a finished one over.
action resume_timer(t: Timer) uses time (icon: .play) {
    if t.ends_at != .none {
        return
    }
    let left = Duration(t.left ?? t.seconds)
    t.left = .none
    t.ends_at = .some(now + left)
    let c = root Clocks
    c.active = anything_running(c)
}

/// Adds a minute to a timer, running or paused.
action add_minute(t: Timer) uses time (icon: .plus) {
    if let ends = t.ends_at {
        t.ends_at = .some(ends + 1m)
    } else if let left = t.left {
        t.left = .some(left + 60)
    } else {
        t.ends_at = .some(now + Duration(t.seconds) + 1m)
    }
    let c = root Clocks
    c.active = anything_running(c)
}

/// Removes a timer.
destructive action remove_timer(c: Clocks, t: Timer) (icon: .trash) {
    c.timers.remove(t)
    c.active = anything_running(c)
}

/// Rings a timer when it finishes, on every device of the person's, window or no window.
action ring_timer(t: Timer) uses notify (icon: .bell) {
    let who: Option<Actor> = if let made = created(t) { .some(made.who) } else { .none }
    if let told = who {
        notify(to: told, title: t.label, body: "The timer finished", opens: t)
    }
}

/// Rings a timer when it finishes, once for the whole workspace.
trigger timer_rings(t: Timer) at t.ends_at: ring_timer(t)

/// Starts the stopwatch.
action start_stopwatch(c: Clocks) uses time (icon: .play, label: "Start the stopwatch") {
    if c.stopwatch.since == .none {
        c.stopwatch.since = .some(now)
    }
    c.active = anything_running(c)
}

/// Pauses the stopwatch where it stands.
action pause_stopwatch(c: Clocks) uses time (icon: .pause, label: "Pause the stopwatch") {
    if let since = c.stopwatch.since {
        c.stopwatch.elapsed += (now - since).seconds()
        c.stopwatch.since = .none
    }
    c.active = anything_running(c)
}

/// Keeps a lap: the time the stopwatch stands at.
action lap(c: Clocks) uses time (icon: .flag, label: "Lap") {
    if let since = c.stopwatch.since {
        c.stopwatch.laps.insert(Lap(at: c.stopwatch.elapsed + (now - since).seconds()))
    }
}

/// Resets the stopwatch: it is at zero and stopped, with no laps.
action reset_stopwatch(c: Clocks) (icon: .rotate_ccw, label: "Reset the stopwatch") {
    c.stopwatch.elapsed -= c.stopwatch.elapsed
    c.stopwatch.since = .none
    let kept = c.stopwatch.laps.filter(lap -> true)
    for lap in kept {
        c.stopwatch.laps.remove(lap)
    }
    c.active = anything_running(c)
}

/// What a timer says it has left, as the window reads it while it is paused.
fn left_text(left: Option<Int>) -> Text {
    if let left = left {
        let secs = left
        "{secs / 60}:{pad2(secs % 60)}"
    } else { "" }
}

/// The Clocks window: world clocks, alarms, timers and the stopwatch, each a place of its own.
view clocks_page(c: Clocks) {
    state place: Text = "timers"
    ui.NavShell(content_kind: .page, sidebar_title: "Clocks", title: place_title(place)) {
        sidebar: {
            ui.NavList(selected: place) {
                ui.NavItem("world", "World", icon: .globe) {
                    on select: set(place, "world")
                }
                ui.NavItem(
                    "alarms",
                    "Alarms",
                    icon: .alarm_clock,
                    count: c.alarms.count(a -> a.enabled),
                ) {
                    on select: set(place, "alarms")
                }
                ui.NavItem("timers", "Timers", icon: .hourglass, count: c.timers.len()) {
                    on select: set(place, "timers")
                }
                ui.NavItem("stopwatch", "Stopwatch", icon: .timer) {
                    on select: set(place, "stopwatch")
                }
            }
        }
        content: {
            if place == "world" {
                world_place(c)
            } else if place == "alarms" {
                alarms_place(c)
            } else if place == "stopwatch" {
                stopwatch_place(c)
            } else {
                timers_place(c)
            }
        }
    }
}

/// The title the window's header reads for `place`.
fn place_title(place: Text) -> Text {
    if place == "world" {
        "World clocks"
    } else if place == "alarms" {
        "Alarms"
    } else if place == "stopwatch" { "Stopwatch" } else { "Timers" }
}

/// What a city is called, from the zone it keeps time by: the last part of `Europe/Vienna`,
/// with its underscores opened (`New_York` → `New York`).
fn zone_label(zone: Text) -> Text {
    let name = zone.split("/").last() ?? zone
    name.replace("_", " ")
}

/// The zones `query` names, by their city's name or the zone itself: what the city search
/// offers as the person types.
fn zone_matches(query: Text) -> List<Text> {
    let asked = query.trim().lower()
    if asked.is_empty() {
        []
    } else {
        zones()
            .filter(
                z -> zone_label(z).lower().contains(asked) or z.lower().contains(asked),
            )
            .take(8)
    }
}

/// The day the alarm `a` rings, said against today: the day it next rings, or that it is off.
fn next_ring_text(a: Alarm) -> Text {
    if not a.enabled {
        return "Off"
    }
    if let at = a.at { "Next {at.day()}, {at.clock()}" } else { "No next ring" }
}

/// What the alarm repeats on, said in a line: `Every day`, `Mon, Wed, Fri` or that it rings once.
fn repeat_text(a: Alarm) -> Text {
    if a.days.is_empty() {
        return "Once"
    }
    if a.days.len() == 7 {
        return "Every day"
    }
    days_of(a).map(d -> day_name(d)).join(", ")
}

/// The world clocks: the cities compared, and a search that adds one.
view world_place(c: Clocks) {
    state query: Text = ""
    let matches = zone_matches(query)
    page {
        stack(gap: .section) {
            ui.Card(title: "Add a city") {
                stack(gap: .related) {
                    input(bind: query, placeholder: "Search a city or a zone")
                    for zone in matches {
                        row(gap: .related, align: .center) {
                            text(zone_label(zone), fill: 1.0)
                            text(zone, secondary: true)
                            button(label: "Add", on: add_world_clock(c, zone_label(zone), zone))
                        }
                    }
                }
            }
            ui.Card(title: "Here") {
                ui.Clock(facts: true, role: .title)
            }
            if c.world_clocks.is_empty() {
                ui.EmptyState("No cities yet. Add one to compare times at a glance.", icon: .globe)
            }
            for w in c.world_clocks {
                ui.Card(title: w.city) {
                    stack(gap: .related) {
                        ui.Clock(zone: w.zone, facts: true, role: .title)
                        row {
                            spacer()
                            button(remove_world_clock(c, w))
                        }
                    }
                }
            }
        }
    }
}

/// The alarms: those set, the one being edited, and the form that sets another.
view alarms_place(c: Clocks) {
    // The new alarm form's words, kept while the person fills it.
    state draft_label: Text = ""
    state draft_hour: Float = 7.0
    state draft_minute: Float = 30.0
    state draft_days: List<Weekday> = []
    // The alarm being edited, if one is.
    state editing: Option<Alarm> = .none
    state edit_label: Text = ""
    state edit_hour: Float = 7.0
    state edit_minute: Float = 30.0
    page {
        stack(gap: .section) {
            if c.alarms.is_empty() {
                ui.EmptyState("No alarms yet. Set one for a future time.", icon: .alarm_clock)
            }
            for a in c.alarms {
                ui.Card(title: a.label) {
                    stack(gap: .related) {
                        text("{a.hour}:{pad2(a.minute)}", role: .large_title)
                        text(
                            "{repeat_text(a)}, {if a.enabled { "on" } else { "off" }}",
                            secondary: true,
                        )
                        text(next_ring_text(a), secondary: true)
                        ui.Text("Rings on", role: .caption, secondary: true)
                        for d in device.devices.devices {
                            ui.ListRow(
                                d.label,
                                secondary: if d.local { "This device" } else { "" },
                                selected: a.rings_on == d.id,
                                icon: .monitor,
                            ) {
                                on activate: set_alarm_device(a, d.id)
                            }
                        }
                        ui.ListRow(
                            "Every device",
                            secondary: "All of yours",
                            selected: a.rings_on == "",
                            icon: .laptop,
                        ) {
                            on activate: set_alarm_device(a, "")
                        }
                        row(gap: .tight) {
                            for d in weekdays() {
                                ui.Button(
                                    day_name(d),
                                    role: .plain,
                                    toggled: a.days.contains(d),
                                    accessible_name: "Repeat on {day_name(d)}",
                                ) {
                                    on press: toggle_day(a, d)
                                }
                            }
                        }
                        row(gap: .related) {
                            button(
                                label: "Edit",
                                on: set(edit_label, a.label)
                                    .then(
                                        set(edit_hour, a.hour.to_float()),
                                    )
                                    .then(
                                        set(edit_minute, a.minute.to_float()),
                                    )
                                    .then(
                                        set(editing, .some(a)),
                                    ),
                            )
                            button(set_alarm_enabled(a))
                            button(remove_alarm(c, a))
                        }
                    }
                }
            }
            if let a = editing {
                ui.Card(title: "Edit {a.label}") {
                    stack(gap: .related) {
                        input(bind: edit_label, placeholder: "What is this alarm for?")
                        row(gap: .related) {
                            ui.NumberInput(
                                bind: edit_hour,
                                min: 0.0,
                                max: 23.0,
                                precision: 0,
                                label: "Hour",
                                suffix: "h",
                            )
                            ui.NumberInput(
                                bind: edit_minute,
                                min: 0.0,
                                max: 59.0,
                                precision: 0,
                                label: "Minute",
                                suffix: "min",
                            )
                        }
                        row(gap: .related) {
                            ui.Button("Save changes", role: .primary) {
                                on press: edit_alarm(
                                        a,
                                        if edit_label.is_empty() { a.label } else { edit_label },
                                        edit_hour.to_int(),
                                        edit_minute.to_int(),
                                    )
                                    .then(set(editing, .none))
                            }
                            ui.Button("Cancel") {
                                on press: set(editing, .none)
                            }
                        }
                    }
                }
            }
            ui.Card(title: "New alarm") {
                stack(gap: .related) {
                    input(bind: draft_label, placeholder: "What is this alarm for?")
                    row(gap: .related) {
                        ui.NumberInput(
                            bind: draft_hour,
                            min: 0.0,
                            max: 23.0,
                            precision: 0,
                            label: "Hour",
                            suffix: "h",
                        )
                        ui.NumberInput(
                            bind: draft_minute,
                            min: 0.0,
                            max: 59.0,
                            precision: 0,
                            label: "Minute",
                            suffix: "min",
                        )
                    }
                    ui.Text("Repeat days", role: .caption, secondary: true)
                    row(gap: .tight) {
                        for d in weekdays() {
                            ui.Button(
                                day_name(d),
                                role: .plain,
                                toggled: draft_days.contains(d),
                                accessible_name: "Repeat on {day_name(d)}",
                            ) {
                                on press: set(draft_days, toggled_day(draft_days, d))
                            }
                        }
                    }
                    ui.Button("Add alarm", role: .primary) {
                        on press: add_alarm(
                                c,
                                if draft_label.is_empty() { "Alarm" } else { draft_label },
                                draft_hour.to_int(),
                                draft_minute.to_int(),
                                draft_days,
                            )
                            .then(set(draft_label, ""))
                            .then(set(draft_days, []))
                    }
                }
            }
        }
    }
}

/// The timers: those running, paused or done, and the form that starts another.
view timers_place(c: Clocks) {
    state timer_label: Text = ""
    state timer_hours: Float = 0.0
    state timer_minutes: Float = 10.0
    state timer_seconds: Float = 0.0
    let chosen = timer_hours.to_int() * 3600 + timer_minutes.to_int() * 60 + timer_seconds.to_int()
    page {
        stack(gap: .section) {
            if c.timers.is_empty() {
                ui.EmptyState("No timers yet", icon: .hourglass)
            }
            for t in c.timers {
                ui.Card(title: t.label) {
                    stack(gap: .related) {
                        if let ends = t.ends_at {
                            ui.Countdown(at: ends, total: t.seconds.to_float(), label: t.label)
                        } else {
                            text(left_text(t.left), role: .display)
                            text("Paused", secondary: true)
                        }
                        row(gap: .related) {
                            ui.Button("+1 min") {
                                on press: add_minute(t)
                            }
                            if t.ends_at == .none {
                                ui.Button("Resume", role: .primary) {
                                    on press: resume_timer(t)
                                }
                            } else {
                                ui.Button("Pause", role: .primary) {
                                    on press: pause_timer(t)
                                }
                            }
                            ui.Button("Remove", role: .destructive) {
                                on press: remove_timer(c, t)
                            }
                        }
                    }
                }
            }
            ui.Card(title: "Quick starts") {
                stack(gap: .related) {
                    row(gap: .related) {
                        ui.Button("Tea (5 min)") {
                            on press: add_timer(c, "Tea", 300)
                        }
                        ui.Button("Break (10 min)") {
                            on press: add_timer(c, "Break", 600)
                        }
                    }
                    row(gap: .related) {
                        ui.Button("Focus (25 min)") {
                            on press: add_timer(c, "Focus", 1500)
                        }
                        ui.Button("Laundry (45 min)") {
                            on press: add_timer(c, "Laundry", 2700)
                        }
                    }
                }
            }
            ui.Card(title: "Custom timer") {
                stack(gap: .related) {
                    input(bind: timer_label, placeholder: "What is this timer for?")
                    row(gap: .related) {
                        ui.NumberInput(
                            bind: timer_hours,
                            min: 0.0,
                            max: 99.0,
                            precision: 0,
                            label: "Hours",
                            suffix: "h",
                        )
                        ui.NumberInput(
                            bind: timer_minutes,
                            min: 0.0,
                            max: 59.0,
                            precision: 0,
                            label: "Minutes",
                            suffix: "min",
                        )
                        ui.NumberInput(
                            bind: timer_seconds,
                            min: 0.0,
                            max: 59.0,
                            precision: 0,
                            label: "Seconds",
                            suffix: "s",
                        )
                    }
                    ui.Text(
                        "Use whole numbers: hours 0–99 and minutes and seconds 0–59.",
                        role: .caption,
                        secondary: true,
                    )
                    ui.Button("Start timer", role: .primary) {
                        on press: add_timer(
                                c,
                                if timer_label.is_empty() { "Timer" } else { timer_label },
                                chosen,
                            )
                            .then(set(timer_label, ""))
                    }
                }
            }
        }
    }
}

/// The stopwatch: its time, its controls and its laps.
view stopwatch_place(c: Clocks) {
    page {
        stack(gap: .section) {
            ui.Card {
                stack(gap: .related) {
                    if let since = c.stopwatch.since {
                        ui.Countdown(at: since, label: "Stopwatch")
                    } else {
                        text(left_text(.some(c.stopwatch.elapsed)), role: .display)
                    }
                    row(gap: .related) {
                        ui.Button(
                            if c.stopwatch.since == .none { "Start" } else { "Pause" },
                            role: .primary,
                        ) {
                            on press: if c.stopwatch.since == .none {
                                start_stopwatch(c)
                            } else { pause_stopwatch(c) }
                        }
                        ui.Button("Lap") {
                            on press: lap(c)
                        }
                        ui.Button("Reset") {
                            on press: reset_stopwatch(c)
                        }
                    }
                    for lap in c.stopwatch.laps {
                        text(left_text(.some(lap.at)), secondary: true)
                    }
                }
            }
        }
    }
}

/// What the shell shows while a timer or the stopwatch runs: each one with what it has left and
/// its controls, on the bar, in quick settings and on the lock screen of every device.
view live(c: Clocks) {
    stack(gap: .related) {
        for t in c.timers.filter(t -> t.ends_at != .none or t.left != .none) {
            row(gap: .related, align: .center) {
                ui.Icon(.hourglass)
                stack(fill) {
                    ui.Text(t.label, role: .label, lines: 1, truncate)
                    if let ends = t.ends_at {
                        ui.Countdown(at: ends)
                    } else {
                        ui.Text(left_text(t.left), role: .caption, secondary: true, numeric: true)
                    }
                }
                if t.ends_at == .none {
                    ui.Button("Resume", icon: .play, icon_only: true, role: .plain) {
                        on press: resume_timer(t)
                    }
                } else {
                    ui.Button("Pause", icon: .pause, icon_only: true, role: .plain) {
                        on press: pause_timer(t)
                    }
                }
                ui.Button("Add a minute", icon: .plus, icon_only: true, role: .plain) {
                    on press: add_minute(t)
                }
                ui.Button("Remove", icon: .x, icon_only: true, role: .plain) {
                    on press: remove_timer(c, t)
                }
            }
        }
        if c.stopwatch.since != .none or c.stopwatch.elapsed > 0 {
            row(gap: .related, align: .center) {
                ui.Icon(.timer)
                stack(fill) {
                    ui.Text("Stopwatch", role: .label)
                    if let since = c.stopwatch.since {
                        ui.Countdown(at: since)
                    } else {
                        ui.Text(
                            left_text(.some(c.stopwatch.elapsed)),
                            role: .caption,
                            secondary: true,
                            numeric: true,
                        )
                    }
                }
                ui.Button(
                    if c.stopwatch.since == .none { "Start" } else { "Pause" },
                    icon: if c.stopwatch.since == .none { .play } else { .pause },
                    icon_only: true,
                    role: .plain,
                ) {
                    on press: if c.stopwatch.since == .none {
                        start_stopwatch(c)
                    } else { pause_stopwatch(c) }
                }
                ui.Button("Lap", icon: .flag, icon_only: true, role: .plain) {
                    on press: lap(c)
                }
                ui.Button("Reset", icon: .rotate_ccw, icon_only: true, role: .plain) {
                    on press: reset_stopwatch(c)
                }
            }
        }
    }
}
