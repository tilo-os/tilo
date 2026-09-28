//! Pure data model for the calendar/todo app.
//!
//! This is the in-memory source of truth. It is mirrored into the
//! Automerge document by [`crate::doc`] (events / tasks / projects as
//! CRDT objects), and each event/task's rich-text **notes** live in the
//! CRDT as rich text edited in the SDK rich-text editor.
//!
//! No rendering, no IO — just types, filters, and recurrence expansion.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime};
use uuid::Uuid;
use vello::peniko::Color;

use ui::theme::Theme;

pub type Id = Uuid;

/// The four top-level screens. `Agenda` is the flat "upcoming" list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Month,
    Week,
    Day,
    Agenda,
}

impl View {
    pub fn label(self) -> &'static str {
        match self {
            View::Month => "Month",
            View::Week => "Week",
            View::Day => "Day",
            View::Agenda => "Agenda",
        }
    }
    pub const ALL: [View; 4] = [View::Month, View::Week, View::Day, View::Agenda];
}

/// Which "smart list" or project the sidebar has selected. Drives the
/// task list in the agenda/day rails.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ListSel {
    Inbox,
    Today,
    Upcoming,
    Completed,
    Project(Id),
}

/// Task priority — Planify-style P1..P4 (P1 == most urgent).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Priority {
    None,
    Low,
    Medium,
    High,
}

impl Priority {
    pub fn label(self) -> &'static str {
        match self {
            Priority::None => "No priority",
            Priority::Low => "P3 · Low",
            Priority::Medium => "P2 · Medium",
            Priority::High => "P1 · High",
        }
    }
    pub fn flag(self) -> &'static str {
        match self {
            Priority::None => "",
            Priority::Low => "P3",
            Priority::Medium => "P2",
            Priority::High => "P1",
        }
    }
    /// Flag colour, read from the theme's semantic ramp.
    pub fn color(self, theme: &Theme) -> Color {
        let c = &theme.tokens.colors;
        match self {
            Priority::None => c.text_dim,
            Priority::Low => c.semantic.info,
            Priority::Medium => c.semantic.warning,
            Priority::High => c.semantic.danger,
        }
    }
    pub fn rank(self) -> u8 {
        match self {
            Priority::High => 0,
            Priority::Medium => 1,
            Priority::Low => 2,
            Priority::None => 3,
        }
    }
    pub fn from_rank(r: u8) -> Self {
        match r {
            0 => Priority::High,
            1 => Priority::Medium,
            2 => Priority::Low,
            _ => Priority::None,
        }
    }
}

/// A small, theme-derived colour palette for calendars/projects/events.
/// Every value resolves through [`Theme`] so the OS controls the look —
/// the enum only encodes *which* semantic slot an item picked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Palette {
    Indigo,
    Emerald,
    Amber,
    Red,
    Blue,
    Violet,
    Slate,
}

impl Palette {
    pub const ALL: [Palette; 7] = [
        Palette::Indigo,
        Palette::Emerald,
        Palette::Amber,
        Palette::Red,
        Palette::Blue,
        Palette::Violet,
        Palette::Slate,
    ];

    pub fn color(self, theme: &Theme) -> Color {
        let c = &theme.tokens.colors;
        match self {
            Palette::Indigo => c.accent[5],
            Palette::Emerald => c.semantic.success,
            Palette::Amber => c.semantic.warning,
            Palette::Red => c.semantic.danger,
            Palette::Blue => c.semantic.info,
            Palette::Violet => c.accent[7],
            Palette::Slate => c.neutral[5],
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Palette::Indigo => "Indigo",
            Palette::Emerald => "Emerald",
            Palette::Amber => "Amber",
            Palette::Red => "Red",
            Palette::Blue => "Blue",
            Palette::Violet => "Violet",
            Palette::Slate => "Slate",
        }
    }

    pub fn from_index(i: usize) -> Self {
        // `i % Self::ALL.len()` is always in bounds since `ALL` is a fixed,
        // non-empty array; `Self::ALL[0]` is a compile-time-safe fallback.
        Self::ALL
            .get(i % Self::ALL.len())
            .copied()
            .unwrap_or(Self::ALL[0])
    }
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }
}

/// Recurrence rule. A small closed set plus an RRULE bridge for `.ics`
/// interop ([`Recurrence::to_rrule`] / [`Recurrence::from_rrule`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Recurrence {
    None,
    Daily,
    Weekdays,
    Weekly,
    Monthly,
    Yearly,
}

/// One invitation participant. The UUID is the durable resource identity;
/// email is editable contact data and therefore must never be used as a CRDT
/// map key.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Attendee {
    pub id: Id,
    pub name: String,
    pub email: String,
    pub status: InviteStatus,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InviteStatus {
    NeedsAction,
    Accepted,
    Tentative,
    Declined,
}

impl InviteStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsAction => "Awaiting response",
            Self::Accepted => "Accepted",
            Self::Tentative => "Maybe",
            Self::Declined => "Declined",
        }
    }

    pub fn as_ical(self) -> &'static str {
        match self {
            Self::NeedsAction => "NEEDS-ACTION",
            Self::Accepted => "ACCEPTED",
            Self::Tentative => "TENTATIVE",
            Self::Declined => "DECLINED",
        }
    }

    pub fn from_ical(value: &str) -> Self {
        match value.trim().to_ascii_uppercase().as_str() {
            "ACCEPTED" => Self::Accepted,
            "TENTATIVE" => Self::Tentative,
            "DECLINED" => Self::Declined,
            _ => Self::NeedsAction,
        }
    }
}

/// An absolute edit to one generated recurrence instance. `original_date` is
/// the stable occurrence identity even when this override moves the instance.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OccurrenceOverride {
    pub original_date: NaiveDate,
    /// Time component when the RFC identity was DATE-TIME rather than DATE.
    pub original_time: Option<NaiveTime>,
    pub cancelled: bool,
    pub start: Option<NaiveDateTime>,
    pub end: Option<NaiveDateTime>,
    pub title: Option<String>,
    pub location: Option<String>,
}

impl Recurrence {
    pub fn label(self) -> &'static str {
        match self {
            Recurrence::None => "Does not repeat",
            Recurrence::Daily => "Every day",
            Recurrence::Weekdays => "Every weekday",
            Recurrence::Weekly => "Every week",
            Recurrence::Monthly => "Every month",
            Recurrence::Yearly => "Every year",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Recurrence::None => "",
            Recurrence::Daily => "Daily",
            Recurrence::Weekdays => "Weekdays",
            Recurrence::Weekly => "Weekly",
            Recurrence::Monthly => "Monthly",
            Recurrence::Yearly => "Yearly",
        }
    }

    /// RFC 5545 RRULE body (without the `RRULE:` prefix), or empty for
    /// non-recurring events.
    pub fn to_rrule(self) -> String {
        match self {
            Recurrence::None => String::new(),
            Recurrence::Daily => "FREQ=DAILY".into(),
            Recurrence::Weekdays => "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".into(),
            Recurrence::Weekly => "FREQ=WEEKLY".into(),
            Recurrence::Monthly => "FREQ=MONTHLY".into(),
            Recurrence::Yearly => "FREQ=YEARLY".into(),
        }
    }

    pub fn from_rrule(rule: &str) -> Self {
        let up = rule.to_uppercase();
        if up.contains("FREQ=DAILY") {
            Recurrence::Daily
        } else if up.contains("BYDAY=MO,TU,WE,TH,FR") {
            Recurrence::Weekdays
        } else if up.contains("FREQ=WEEKLY") {
            Recurrence::Weekly
        } else if up.contains("FREQ=MONTHLY") {
            Recurrence::Monthly
        } else if up.contains("FREQ=YEARLY") {
            Recurrence::Yearly
        } else {
            Recurrence::None
        }
    }

    #[must_use]
    pub fn cycle(self) -> Self {
        match self {
            Recurrence::None => Recurrence::Daily,
            Recurrence::Daily => Recurrence::Weekdays,
            Recurrence::Weekdays => Recurrence::Weekly,
            Recurrence::Weekly => Recurrence::Monthly,
            Recurrence::Monthly => Recurrence::Yearly,
            Recurrence::Yearly => Recurrence::None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Project {
    pub id: Id,
    pub name: String,
    pub color: Palette,
    pub order: u32,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub id: Id,
    pub title: String,
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub all_day: bool,
    pub location: String,
    pub color: Palette,
    pub recurrence: Recurrence,
    /// Positive recurrence interval. For example, weekly + 2 is every other
    /// week. Old documents default to 1.
    pub recurrence_interval: u32,
    /// Inclusive final original occurrence date.
    pub recurrence_until: Option<NaiveDate>,
    /// Maximum number of original occurrences, including the seed.
    pub recurrence_count: Option<u32>,
    /// Absolute original occurrence dates omitted from the series.
    pub recurrence_exdates: Vec<NaiveDate>,
    /// Per-occurrence absolute cancellation/reschedule/content edits.
    pub occurrence_overrides: Vec<OccurrenceOverride>,
    /// IANA time-zone identifier, or `floating` for legacy/local wall time.
    pub timezone: String,
    /// Stable attendee id designating the organizer, when known.
    pub organizer: Option<Id>,
    pub attendees: Vec<Attendee>,
    /// Minutes before start for a reminder, if any.
    pub reminder_min: Option<i64>,
    pub project: Option<Id>,
    /// Which remote calendar this event was synced from, if any. Local events
    /// (created in-app or imported) carry `None` and are always shown; a synced
    /// event carries its source [`crate::account::RemoteCalendar`] id so the
    /// sidebar's per-calendar visibility toggle can hide/show it on the spot,
    /// without a re-sync. Persisted in the CRDT under the `cal` key.
    pub calendar_id: Option<String>,
}

/// Fallback label shown for an event whose title is still empty — a freshly
/// created event whose title field the user hasn't filled in yet. Kept as a
/// const so views and the form agree on the same wording.
pub const UNTITLED_EVENT: &str = "New event";

impl Event {
    /// The title to render in views: the user's title, or [`UNTITLED_EVENT`]
    /// when it is still empty. The stored `title` stays empty so the edit
    /// form shows its placeholder rather than literal text the user must
    /// delete.
    pub fn display_title(&self) -> &str {
        if self.title.trim().is_empty() {
            UNTITLED_EVENT
        } else {
            self.title.as_str()
        }
    }

    /// The last date an all-day event occupies in Calendar's editor and
    /// views.  RFC 5545 represents all-day `DTEND` as an *exclusive* date,
    /// while people naturally choose an inclusive final day.  Older local
    /// documents used `23:59` on that final day, so accept both shapes at the
    /// model boundary instead of making existing events appear a day longer.
    pub fn all_day_last_date(&self) -> NaiveDate {
        if !self.all_day {
            return self.end.date();
        }
        if self.end.time() == NaiveTime::MIN && self.end.date() > self.start.date() {
            self.end.date() - Duration::days(1)
        } else {
            self.end.date().max(self.start.date())
        }
    }

    /// The RFC 5545 exclusive end date for this all-day event.  This keeps
    /// CalDAV/ICS interchange honest while allowing pre-existing local data
    /// that stored `23:59` on the final day to round-trip correctly.
    pub fn all_day_end_exclusive(&self) -> NaiveDate {
        self.all_day_last_date() + Duration::days(1)
    }

    /// Number of visible dates occupied by an all-day event.  Kept separate
    /// from a timestamp duration because RFC all-day events are date ranges,
    /// and a daylight-saving transition must never shorten one in the views.
    pub fn all_day_span_days(&self) -> i64 {
        (self.all_day_end_exclusive() - self.start.date())
            .num_days()
            .max(1)
    }

    /// Resolve an occurrence start to an absolute UTC instant. Named-zone
    /// recurrences retain their local clock time across DST; folds choose the
    /// earlier instant and gaps advance to the first valid minute. Floating
    /// events intentionally have no absolute interpretation.
    pub fn occurrence_start_utc(&self, date: NaiveDate) -> Option<chrono::DateTime<chrono::Utc>> {
        (self.timezone != "floating")
            .then(|| {
                crate::timezone::wall_to_utc(&self.timezone, date.and_time(self.start.time()))
                    .map(|(instant, _)| instant)
            })
            .flatten()
    }

    /// Whether this should be shown given the set of currently-hidden
    /// remote calendar ids. Local events (no `calendar_id`) are always visible;
    /// a synced event is hidden exactly when its source calendar is unchecked.
    pub fn visible_under(&self, hidden: &std::collections::HashSet<String>) -> bool {
        match &self.calendar_id {
            Some(id) => !hidden.contains(id),
            None => true,
        }
    }

    /// Dates on which this event occurs within `[from, to]` (inclusive),
    /// honouring its recurrence rule. Anchored on `start.date()`.
    pub fn occurrences_in(&self, from: NaiveDate, to: NaiveDate) -> Vec<NaiveDate> {
        let seed = self.start.date();
        let mut out = Vec::new();
        let interval = self.recurrence_interval.max(1);
        let last_original = self
            .occurrence_overrides
            .iter()
            .map(|edit| edit.original_date)
            .max()
            .unwrap_or(to)
            .max(to);
        let mut generated = 0_u32;
        // Unbounded series jump directly to the requested window; the modular
        // predicates below stay anchored on `seed`. A finite COUNT needs its
        // ordinal, so it starts at the seed but can never inspect more than the
        // explicitly bounded series plus interval gaps.
        let moved_original = self
            .occurrence_overrides
            .iter()
            .filter(|edit| {
                edit.start
                    .is_some_and(|start| start.date() >= from && start.date() <= to)
            })
            .map(|edit| edit.original_date)
            .min();
        let window_start = moved_original.unwrap_or(from).min(from).max(seed);
        let mut day = if self.recurrence_count.is_some() {
            seed
        } else {
            window_start
        };
        while day <= last_original {
            let days = (day - seed).num_days();
            let months = (day.year() - seed.year()) * 12
                + (i32::try_from(day.month()).unwrap_or(0)
                    - i32::try_from(seed.month()).unwrap_or(0));
            let base_match = match self.recurrence {
                Recurrence::None => day == seed,
                Recurrence::Daily => days % i64::from(interval) == 0,
                Recurrence::Weekdays => {
                    day.weekday().num_days_from_monday() < 5
                        && (days / 7) % i64::from(interval) == 0
                }
                Recurrence::Weekly => {
                    day.weekday() == seed.weekday() && (days / 7) % i64::from(interval) == 0
                }
                Recurrence::Monthly => {
                    day.day() == seed.day()
                        && months >= 0
                        && u32::try_from(months).is_ok_and(|value| value % interval == 0)
                }
                Recurrence::Yearly => {
                    day.day() == seed.day()
                        && day.month() == seed.month()
                        && u32::try_from(day.year() - seed.year())
                            .is_ok_and(|value| value % interval == 0)
                }
            };
            if base_match {
                generated = generated.saturating_add(1);
                let within_end = self.recurrence_until.is_none_or(|until| day <= until)
                    && self.recurrence_count.is_none_or(|count| generated <= count);
                if !within_end {
                    break;
                }
                let edit = self
                    .occurrence_overrides
                    .iter()
                    .find(|edit| edit.original_date == day);
                let excluded = self.recurrence_exdates.contains(&day);
                let actual = edit
                    .and_then(|edit| edit.start)
                    .map_or(day, |start| start.date());
                if !excluded
                    && !edit.is_some_and(|edit| edit.cancelled)
                    && actual >= from
                    && actual <= to
                {
                    out.push(actual);
                }
            }
            if self.recurrence == Recurrence::None {
                break;
            }
            day += Duration::days(1);
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn occurs_on(&self, date: NaiveDate) -> bool {
        if self.all_day {
            // Query far enough back to include an event that started before
            // `date` but still spans it.  This also makes recurring all-day
            // conferences appear on every occupied day, not only their first.
            let from = date - Duration::days(self.all_day_span_days().saturating_sub(1));
            return self.occurrences_in(from, date).into_iter().any(|start| {
                date >= start && date < start + Duration::days(self.all_day_span_days())
            });
        }
        !self.occurrences_in(date, date).is_empty()
    }

    /// Duration in fractional hours (timed events only).
    pub fn hours(&self) -> f64 {
        (self.end - self.start).num_minutes().max(0) as f64 / 60.0
    }
}

#[derive(Clone, Debug)]
pub struct Task {
    pub id: Id,
    pub title: String,
    pub done: bool,
    pub due: Option<NaiveDate>,
    pub priority: Priority,
    pub project: Option<Id>,
    pub parent: Option<Id>,
}

/// Aggregate model. Order within the vectors is the display order.
#[derive(Clone, Debug, Default)]
pub struct Model {
    pub projects: Vec<Project>,
    pub events: Vec<Event>,
    pub tasks: Vec<Task>,
}

impl Model {
    pub fn project(&self, id: Id) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn event(&self, id: Id) -> Option<&Event> {
        self.events.iter().find(|e| e.id == id)
    }
    pub fn event_mut(&mut self, id: Id) -> Option<&mut Event> {
        self.events.iter_mut().find(|e| e.id == id)
    }

    pub fn task(&self, id: Id) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }
    pub fn task_mut(&mut self, id: Id) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    /// Top-level tasks (no parent) matching a smart list / project,
    /// sorted by completion, then priority, then due date.
    pub fn tasks_for(&self, sel: &ListSel, today: NaiveDate) -> Vec<&Task> {
        let mut out: Vec<&Task> = self
            .tasks
            .iter()
            .filter(|t| t.parent.is_none())
            .filter(|t| match sel {
                ListSel::Inbox => t.project.is_none() && !t.done,
                ListSel::Today => t.due.is_some_and(|d| d <= today) && !t.done,
                ListSel::Upcoming => t.due.is_some_and(|d| d > today) && !t.done,
                ListSel::Completed => t.done,
                ListSel::Project(p) => t.project == Some(*p),
            })
            .collect();
        out.sort_by(|a, b| {
            a.done
                .cmp(&b.done)
                .then(a.priority.rank().cmp(&b.priority.rank()))
                .then(a.due.cmp(&b.due))
        });
        out
    }

    pub fn subtasks_of(&self, parent: Id) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|t| t.parent == Some(parent))
            .collect()
    }

    /// Count of open (not done) tasks matching a smart list — used for
    /// the sidebar badges.
    pub fn open_count(&self, sel: &ListSel, today: NaiveDate) -> usize {
        self.tasks_for(sel, today)
            .iter()
            .filter(|t| !t.done)
            .count()
    }

    /// Events occurring on `date`, sorted all-day first then by start.
    pub fn events_on(&self, date: NaiveDate) -> Vec<&Event> {
        let mut out: Vec<&Event> = self.events.iter().filter(|e| e.occurs_on(date)).collect();
        out.sort_by(|a, b| {
            b.all_day
                .cmp(&a.all_day)
                .then(a.start.time().cmp(&b.start.time()))
        });
        out
    }

    /// Flat upcoming agenda: every (date, event) pair in `[from, to]`,
    /// chronologically. Used by the Agenda view.
    pub fn agenda(&self, from: NaiveDate, to: NaiveDate) -> Vec<(NaiveDate, &Event)> {
        let mut out: Vec<(NaiveDate, &Event)> = Vec::new();
        for e in &self.events {
            for d in e.occurrences_in(from, to) {
                out.push((d, e));
            }
        }
        out.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(b.1.all_day.cmp(&a.1.all_day))
                .then(a.1.start.time().cmp(&b.1.start.time()))
        });
        out
    }

    pub fn next_event_order_color(&self) -> Palette {
        Palette::from_index(self.events.len())
    }
}

/// Build a populated demo model anchored on `today` so the seeded screenshot
/// always lands inside the visible month with a spread of work.
pub fn seed_demo(today: NaiveDate) -> Model {
    let pid_work = Uuid::new_v4();
    let pid_personal = Uuid::new_v4();
    let pid_health = Uuid::new_v4();

    Model {
        projects: seed_demo_projects(pid_work, pid_personal, pid_health),
        events: seed_demo_events(today, pid_work, pid_personal, pid_health),
        tasks: seed_demo_tasks(today, pid_personal, pid_health, pid_work),
    }
}

/// The three demo projects `seed_demo`'s events and tasks file under.
fn seed_demo_projects(pid_work: Id, pid_personal: Id, pid_health: Id) -> Vec<Project> {
    vec![
        Project {
            id: pid_work,
            name: "Work".into(),
            color: Palette::Indigo,
            order: 0,
        },
        Project {
            id: pid_personal,
            name: "Personal".into(),
            color: Palette::Emerald,
            order: 1,
        },
        Project {
            id: pid_health,
            name: "Health".into(),
            color: Palette::Red,
            order: 2,
        },
    ]
}

/// A literal (h, m) is always in range, so the fallback to midnight only
/// matters if that ever stops being true — and even then a wrong demo time
/// beats panicking the whole seed.
fn seed_demo_at(date: NaiveDate, h: u32, m: u32) -> NaiveDateTime {
    date.and_time(NaiveTime::from_hms_opt(h, m, 0).unwrap_or(NaiveTime::MIN))
}

/// The demo's full event list, in stable start-time display order. Split
/// into two halves purely to keep each builder short.
fn seed_demo_events(
    today: NaiveDate,
    pid_work: Id,
    pid_personal: Id,
    pid_health: Id,
) -> Vec<Event> {
    let mut events = seed_demo_events_early(today, pid_work, pid_personal, pid_health);
    events.extend(seed_demo_events_later(today, pid_work, pid_personal));
    // Stable display order: by start time within the model vector.
    events.sort_by_key(|e| e.start);
    events
}

/// Today's events: standup through quarterly planning.
#[allow(
    clippy::too_many_lines,
    reason = "declarative demo fixture is intentionally grouped by visible calendar period"
)]
fn seed_demo_events_early(
    today: NaiveDate,
    pid_work: Id,
    pid_personal: Id,
    pid_health: Id,
) -> Vec<Event> {
    let d = |delta: i64| today + Duration::days(delta);
    let design_organizer = Uuid::new_v4();
    vec![
        Event {
            id: Uuid::new_v4(),
            title: "Team standup".into(),
            start: seed_demo_at(today, 9, 30),
            end: seed_demo_at(today, 9, 45),
            all_day: false,
            location: "Huddle room / Meet".into(),
            color: Palette::Indigo,
            recurrence: Recurrence::Weekdays,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
            reminder_min: Some(10),
            calendar_id: None,
            project: Some(pid_work),
        },
        Event {
            id: Uuid::new_v4(),
            title: "Design review".into(),
            start: seed_demo_at(today, 11, 0),
            end: seed_demo_at(today, 12, 0),
            all_day: false,
            location: "Figma + Meet".into(),
            color: Palette::Violet,
            recurrence: Recurrence::Weekly,
            recurrence_interval: 2,
            recurrence_until: Some(d(90)),
            recurrence_count: None,
            recurrence_exdates: vec![d(28)],
            occurrence_overrides: vec![OccurrenceOverride {
                original_date: d(42),
                original_time: None,
                cancelled: false,
                start: Some(seed_demo_at(d(43), 14, 0)),
                end: Some(seed_demo_at(d(43), 15, 0)),
                title: Some("Design review · rescheduled".into()),
                location: None,
            }],
            timezone: "Europe/Berlin".into(),
            organizer: Some(design_organizer),
            attendees: vec![Attendee {
                id: design_organizer,
                name: "Mina Patel".into(),
                email: "mina@example.com".into(),
                status: InviteStatus::Accepted,
            }],
            reminder_min: Some(15),
            calendar_id: None,
            project: Some(pid_work),
        },
        Event {
            id: Uuid::new_v4(),
            title: "Lunch with Sam".into(),
            start: seed_demo_at(today, 13, 0),
            end: seed_demo_at(today, 14, 0),
            all_day: false,
            location: "Café Mori".into(),
            color: Palette::Emerald,
            recurrence: Recurrence::None,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
            reminder_min: None,
            calendar_id: None,
            project: Some(pid_personal),
        },
        Event {
            id: Uuid::new_v4(),
            title: "Gym — strength".into(),
            start: seed_demo_at(today, 18, 30),
            end: seed_demo_at(today, 19, 30),
            all_day: false,
            location: "Northside".into(),
            color: Palette::Red,
            recurrence: Recurrence::Weekly,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
            reminder_min: Some(30),
            calendar_id: None,
            project: Some(pid_health),
        },
        Event {
            id: Uuid::new_v4(),
            title: "Quarterly planning".into(),
            start: seed_demo_at(d(2), 10, 0),
            end: seed_demo_at(d(2), 11, 30),
            all_day: false,
            location: "War room".into(),
            color: Palette::Indigo,
            recurrence: Recurrence::None,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
            reminder_min: Some(15),
            calendar_id: None,
            project: Some(pid_work),
        },
    ]
}

/// The rest of the week's events: dentist through payday rent.
#[allow(
    clippy::too_many_lines,
    reason = "declarative demo fixture is intentionally grouped by visible calendar period"
)]
fn seed_demo_events_later(today: NaiveDate, pid_work: Id, pid_personal: Id) -> Vec<Event> {
    let d = |delta: i64| today + Duration::days(delta);
    vec![
        Event {
            id: Uuid::new_v4(),
            title: "Dentist".into(),
            start: seed_demo_at(d(3), 8, 0),
            end: seed_demo_at(d(3), 8, 45),
            all_day: false,
            location: "Bright Smiles".into(),
            color: Palette::Blue,
            recurrence: Recurrence::None,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
            reminder_min: Some(60),
            calendar_id: None,
            project: Some(pid_personal),
        },
        Event {
            id: Uuid::new_v4(),
            title: "Company offsite".into(),
            start: seed_demo_at(d(4), 0, 0),
            end: seed_demo_at(d(4), 23, 59),
            all_day: true,
            location: String::new(),
            color: Palette::Amber,
            recurrence: Recurrence::None,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
            reminder_min: None,
            calendar_id: None,
            project: Some(pid_work),
        },
        Event {
            id: Uuid::new_v4(),
            title: "Weekend hike".into(),
            start: seed_demo_at(d(5), 7, 30),
            end: seed_demo_at(d(5), 12, 0),
            all_day: false,
            location: "Ridge trail".into(),
            color: Palette::Emerald,
            recurrence: Recurrence::None,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
            reminder_min: None,
            calendar_id: None,
            project: Some(pid_personal),
        },
        Event {
            id: Uuid::new_v4(),
            title: "1:1 with manager".into(),
            start: seed_demo_at(d(-1), 15, 0),
            end: seed_demo_at(d(-1), 15, 30),
            all_day: false,
            location: "Meet".into(),
            color: Palette::Indigo,
            recurrence: Recurrence::None,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
            reminder_min: Some(10),
            calendar_id: None,
            project: Some(pid_work),
        },
        Event {
            id: Uuid::new_v4(),
            title: "Pay rent".into(),
            start: seed_demo_at(d(7), 9, 0),
            end: seed_demo_at(d(7), 9, 15),
            all_day: true,
            location: String::new(),
            color: Palette::Amber,
            recurrence: Recurrence::Monthly,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
            reminder_min: Some(720),
            calendar_id: None,
            project: Some(pid_personal),
        },
    ]
}

/// The demo's full task list: the top-level tasks plus the offsite-flights
/// subtasks appended after them.
fn seed_demo_tasks(today: NaiveDate, pid_personal: Id, pid_health: Id, pid_work: Id) -> Vec<Task> {
    // Captured so the offsite-flights task can own a few subtasks — this
    // exercises the parent/child rendering + toggle path in the detail panel
    // with real seeded data.
    let flights = Uuid::new_v4();
    let mut tasks = seed_demo_top_level_tasks(today, pid_work, pid_personal, pid_health, flights);
    tasks.extend(seed_demo_flight_subtasks(today, pid_personal, flights));
    tasks
}

/// The seven top-level demo tasks (one of which, "Book flights for
/// offsite", owns the subtasks `seed_demo_flight_subtasks` returns).
fn seed_demo_top_level_tasks(
    today: NaiveDate,
    pid_work: Id,
    pid_personal: Id,
    pid_health: Id,
    flights: Id,
) -> Vec<Task> {
    let d = |delta: i64| today + Duration::days(delta);
    vec![
        Task {
            id: Uuid::new_v4(),
            title: "Prepare quarterly report".into(),
            done: false,
            due: Some(today),
            priority: Priority::High,
            project: Some(pid_work),
            parent: None,
        },
        Task {
            id: Uuid::new_v4(),
            title: "Send meeting notes".into(),
            done: false,
            due: Some(today),
            priority: Priority::Medium,
            project: Some(pid_work),
            parent: None,
        },
        Task {
            id: Uuid::new_v4(),
            title: "Reply to Alex's email".into(),
            done: true,
            due: Some(d(-1)),
            priority: Priority::Low,
            project: Some(pid_work),
            parent: None,
        },
        Task {
            id: flights,
            title: "Book flights for offsite".into(),
            done: false,
            due: Some(d(3)),
            priority: Priority::Medium,
            project: Some(pid_personal),
            parent: None,
        },
        Task {
            id: Uuid::new_v4(),
            title: "Groceries".into(),
            done: false,
            due: Some(d(1)),
            priority: Priority::None,
            project: None,
            parent: None,
        },
        Task {
            id: Uuid::new_v4(),
            title: "Renew passport".into(),
            done: false,
            due: Some(d(14)),
            priority: Priority::Low,
            project: Some(pid_personal),
            parent: None,
        },
        Task {
            id: Uuid::new_v4(),
            title: "Drink water".into(),
            done: false,
            due: Some(today),
            priority: Priority::None,
            project: Some(pid_health),
            parent: None,
        },
    ]
}

/// Subtasks of "Book flights for offsite" — completing one strikes it
/// through; the parent shows them in its detail panel.
fn seed_demo_flight_subtasks(today: NaiveDate, pid_personal: Id, flights: Id) -> Vec<Task> {
    let d = |delta: i64| today + Duration::days(delta);
    vec![
        Task {
            id: Uuid::new_v4(),
            title: "Compare airline prices".into(),
            done: true,
            due: Some(d(1)),
            priority: Priority::None,
            project: Some(pid_personal),
            parent: Some(flights),
        },
        Task {
            id: Uuid::new_v4(),
            title: "Hold seats on the corporate card".into(),
            done: false,
            due: Some(d(2)),
            priority: Priority::Low,
            project: Some(pid_personal),
            parent: Some(flights),
        },
        Task {
            id: Uuid::new_v4(),
            title: "Add itinerary to the calendar".into(),
            done: false,
            due: Some(d(3)),
            priority: Priority::None,
            project: Some(pid_personal),
            parent: Some(flights),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn recurrence_expands_rules_limits_zones_and_round_trips_rrule() {
        // Case `weekly_recurrence_lands_on_same_weekday`.
        {
            let mut e = sample_event(date(2026, 6, 15)); // Monday
            e.recurrence = Recurrence::Weekly;
            let occ = e.occurrences_in(date(2026, 6, 1), date(2026, 6, 30));
            assert_eq!(
                occ,
                vec![date(2026, 6, 15), date(2026, 6, 22), date(2026, 6, 29)]
            );
        }
        // Case `weekdays_recurrence_skips_weekends`.
        {
            let mut e = sample_event(date(2026, 6, 15));
            e.recurrence = Recurrence::Weekdays;
            let occ = e.occurrences_in(date(2026, 6, 15), date(2026, 6, 21));
            // Mon..Fri only.
            assert_eq!(occ.len(), 5);
            assert!(occ.iter().all(|d| d.weekday().num_days_from_monday() < 5));
        }
        // Case `recurrence_limits_exceptions_and_moved_occurrences_are_absolute`.
        {
            let mut event = sample_event(date(2026, 6, 1));
            event.recurrence = Recurrence::Daily;
            event.recurrence_interval = 2;
            event.recurrence_count = Some(4);
            event.recurrence_exdates.push(date(2026, 6, 3));
            event.occurrence_overrides.push(OccurrenceOverride {
                original_date: date(2026, 6, 5),
                original_time: None,
                cancelled: false,
                start: Some(date(2026, 6, 6).and_hms_opt(9, 0, 0).unwrap()),
                end: Some(date(2026, 6, 6).and_hms_opt(10, 0, 0).unwrap()),
                title: Some("Moved instance".into()),
                location: None,
            });
            assert_eq!(
                event.occurrences_in(date(2026, 6, 1), date(2026, 6, 10)),
                vec![date(2026, 6, 1), date(2026, 6, 6), date(2026, 6, 7)]
            );
        }
        // Case `named_zone_recurrence_keeps_wall_clock_time_across_dst`.
        {
            let mut event = sample_event(date(2026, 3, 22));
            event.start = date(2026, 3, 22).and_hms_opt(9, 0, 0).unwrap();
            event.end = date(2026, 3, 22).and_hms_opt(10, 0, 0).unwrap();
            event.timezone = "Europe/Berlin".into();
            let before = event.occurrence_start_utc(date(2026, 3, 22)).unwrap();
            let after = event.occurrence_start_utc(date(2026, 3, 29)).unwrap();
            assert_eq!(
                before.naive_utc().time(),
                NaiveTime::from_hms_opt(8, 0, 0).unwrap()
            );
            assert_eq!(
                after.naive_utc().time(),
                NaiveTime::from_hms_opt(7, 0, 0).unwrap()
            );
        }
        // Case `none_recurrence_is_single_day`.
        {
            let e = sample_event(date(2026, 6, 15));
            assert!(e.occurs_on(date(2026, 6, 15)));
            assert!(!e.occurs_on(date(2026, 6, 16)));
        }
        // Case `rrule_round_trips`.
        {
            for r in [
                Recurrence::Daily,
                Recurrence::Weekdays,
                Recurrence::Weekly,
                Recurrence::Monthly,
                Recurrence::Yearly,
            ] {
                assert_eq!(Recurrence::from_rrule(&r.to_rrule()), r);
            }
        }
    }

    #[test]
    fn day_membership_covers_all_day_spans_and_overdue_open_tasks() {
        // Case `all_day_events_use_an_exclusive_end_but_occupy_each_visible_day`.
        {
            let mut e = sample_event(date(2026, 6, 15));
            e.all_day = true;
            e.start = date(2026, 6, 15).and_time(NaiveTime::MIN);
            e.end = date(2026, 6, 18).and_time(NaiveTime::MIN);

            assert_eq!(e.all_day_last_date(), date(2026, 6, 17));
            assert_eq!(e.all_day_end_exclusive(), date(2026, 6, 18));
            assert_eq!(e.all_day_span_days(), 3);
            assert!(e.occurs_on(date(2026, 6, 15)));
            assert!(e.occurs_on(date(2026, 6, 16)));
            assert!(e.occurs_on(date(2026, 6, 17)));
            assert!(!e.occurs_on(date(2026, 6, 18)));
        }
        // Case `legacy_all_day_final_minute_stays_one_visible_day`.
        {
            let mut e = sample_event(date(2026, 6, 15));
            e.all_day = true;
            e.start = date(2026, 6, 15).and_time(NaiveTime::MIN);
            e.end = date(2026, 6, 15).and_hms_opt(23, 59, 0).unwrap();

            assert_eq!(e.all_day_last_date(), date(2026, 6, 15));
            assert_eq!(e.all_day_end_exclusive(), date(2026, 6, 16));
            assert!(e.occurs_on(date(2026, 6, 15)));
            assert!(!e.occurs_on(date(2026, 6, 16)));
        }
        // Case `today_filter_includes_overdue_open_tasks`.
        {
            let model = seed_demo(date(2026, 6, 15));
            let today = model.tasks_for(&ListSel::Today, date(2026, 6, 15));
            // Every "Today" task is open and due on/before today.
            assert!(
                today
                    .iter()
                    .all(|t| !t.done && t.due.unwrap() <= date(2026, 6, 15))
            );
        }
    }

    #[test]
    fn demo_seed_keeps_a_realistic_spread_without_developer_todo_titles() {
        // Case `demo_seed_has_no_developer_todo_titles`.
        {
            // The demo/sample seed must read as a realistic person's calendar, not
            // leak the app's own backlog. Guard every event/task title (and the
            // curated project names) against known developer-TODO fragments.
            let model = seed_demo(date(2026, 6, 15));
            let markers = [
                "ICS",
                "round-trip",
                "PR #",
                "hit test",
                "month-grid",
                "Release v",
                "Sprint",
                "TODO",
                "refactor",
                "unit test",
                "bugfix",
            ];
            let mut titles: Vec<&str> = Vec::new();
            titles.extend(model.events.iter().map(|e| e.title.as_str()));
            titles.extend(model.tasks.iter().map(|t| t.title.as_str()));
            titles.extend(model.projects.iter().map(|p| p.name.as_str()));
            for title in titles {
                let lower = title.to_ascii_lowercase();
                for m in markers {
                    assert!(
                        !lower.contains(&m.to_ascii_lowercase()),
                        "demo seed leaks developer-TODO text: {title:?} matched {m:?}",
                    );
                }
            }
        }
        // Case `demo_seed_keeps_a_realistic_spread`.
        {
            // Sanity that the neutral replacements are actually present, so a future
            // edit can't silently gut the sample into an empty/placeholder set.
            let model = seed_demo(date(2026, 6, 15));
            let has = |t: &str| {
                model.events.iter().any(|e| e.title == t)
                    || model.tasks.iter().any(|k| k.title == t)
            };
            assert!(has("Company offsite"), "neutral all-day event present");
            assert!(has("Reply to Alex's email"), "neutral task present");
            assert!(has("Lunch with Sam"), "everyday event present");
        }
    }

    fn sample_event(start_date: NaiveDate) -> Event {
        Event {
            id: Uuid::new_v4(),
            title: "x".into(),
            start: start_date.and_time(NaiveTime::from_hms_opt(9, 0, 0).unwrap()),
            end: start_date.and_time(NaiveTime::from_hms_opt(10, 0, 0).unwrap()),
            all_day: false,
            location: String::new(),
            color: Palette::Indigo,
            recurrence: Recurrence::None,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
            reminder_min: None,
            calendar_id: None,
            project: None,
        }
    }
}
