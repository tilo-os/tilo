//! Pure data model for the calendar/todo app.
//!
//! This is the in-memory source of truth. It is mirrored into the
//! Automerge document by [`crate::doc`] (events / tasks / projects as
//! CRDT objects), and each event/task's rich-text **notes** live in the
//! CRDT as a real block list bound to a `BlockEditor`.
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
        Self::ALL[i % Self::ALL.len()]
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
        let mut day = from.max(seed);
        match self.recurrence {
            Recurrence::None => {
                if seed >= from && seed <= to {
                    out.push(seed);
                }
            }
            Recurrence::Daily => {
                while day <= to {
                    out.push(day);
                    day += Duration::days(1);
                }
            }
            Recurrence::Weekdays => {
                while day <= to {
                    let wd = day.weekday().num_days_from_monday();
                    if wd < 5 {
                        out.push(day);
                    }
                    day += Duration::days(1);
                }
            }
            Recurrence::Weekly => {
                // Align to the seed's weekday.
                while day.weekday() != seed.weekday() && day <= to {
                    day += Duration::days(1);
                }
                while day <= to {
                    out.push(day);
                    day += Duration::days(7);
                }
            }
            Recurrence::Monthly => {
                let dom = seed.day();
                while day <= to {
                    if day.day() == dom {
                        out.push(day);
                    }
                    day += Duration::days(1);
                }
            }
            Recurrence::Yearly => {
                while day <= to {
                    if day.day() == seed.day() && day.month() == seed.month() {
                        out.push(day);
                    }
                    day += Duration::days(1);
                }
            }
        }
        out
    }

    pub fn occurs_on(&self, date: NaiveDate) -> bool {
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

/// Build a populated demo model anchored on `today` so the seeded
/// screenshot always lands inside the visible month with a realistic
/// spread of work, regardless of the real date.
pub fn seed_demo(today: NaiveDate) -> Model {
    let pid_work = Uuid::new_v4();
    let pid_personal = Uuid::new_v4();
    let pid_health = Uuid::new_v4();

    let projects = vec![
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
    ];

    let at = |date: NaiveDate, h: u32, m: u32| -> NaiveDateTime {
        date.and_time(NaiveTime::from_hms_opt(h, m, 0).unwrap())
    };
    let d = |delta: i64| today + Duration::days(delta);

    let mut events = vec![
        Event {
            id: Uuid::new_v4(),
            title: "Team standup".into(),
            start: at(today, 9, 30),
            end: at(today, 9, 45),
            all_day: false,
            location: "Huddle room / Meet".into(),
            color: Palette::Indigo,
            recurrence: Recurrence::Weekdays,
            reminder_min: Some(10),
            calendar_id: None,
            project: Some(pid_work),
        },
        Event {
            id: Uuid::new_v4(),
            title: "Design review".into(),
            start: at(today, 11, 0),
            end: at(today, 12, 0),
            all_day: false,
            location: "Figma + Meet".into(),
            color: Palette::Violet,
            recurrence: Recurrence::None,
            reminder_min: Some(15),
            calendar_id: None,
            project: Some(pid_work),
        },
        Event {
            id: Uuid::new_v4(),
            title: "Lunch with Sam".into(),
            start: at(today, 13, 0),
            end: at(today, 14, 0),
            all_day: false,
            location: "Café Mori".into(),
            color: Palette::Emerald,
            recurrence: Recurrence::None,
            reminder_min: None,
            calendar_id: None,
            project: Some(pid_personal),
        },
        Event {
            id: Uuid::new_v4(),
            title: "Gym — strength".into(),
            start: at(today, 18, 30),
            end: at(today, 19, 30),
            all_day: false,
            location: "Northside".into(),
            color: Palette::Red,
            recurrence: Recurrence::Weekly,
            reminder_min: Some(30),
            calendar_id: None,
            project: Some(pid_health),
        },
        Event {
            id: Uuid::new_v4(),
            title: "Quarterly planning".into(),
            start: at(d(2), 10, 0),
            end: at(d(2), 11, 30),
            all_day: false,
            location: "War room".into(),
            color: Palette::Indigo,
            recurrence: Recurrence::None,
            reminder_min: Some(15),
            calendar_id: None,
            project: Some(pid_work),
        },
        Event {
            id: Uuid::new_v4(),
            title: "Dentist".into(),
            start: at(d(3), 8, 0),
            end: at(d(3), 8, 45),
            all_day: false,
            location: "Bright Smiles".into(),
            color: Palette::Blue,
            recurrence: Recurrence::None,
            reminder_min: Some(60),
            calendar_id: None,
            project: Some(pid_health),
        },
        Event {
            id: Uuid::new_v4(),
            title: "Company offsite".into(),
            start: at(d(4), 0, 0),
            end: at(d(4), 23, 59),
            all_day: true,
            location: String::new(),
            color: Palette::Amber,
            recurrence: Recurrence::None,
            reminder_min: None,
            calendar_id: None,
            project: Some(pid_work),
        },
        Event {
            id: Uuid::new_v4(),
            title: "Weekend hike".into(),
            start: at(d(5), 7, 30),
            end: at(d(5), 12, 0),
            all_day: false,
            location: "Ridge trail".into(),
            color: Palette::Emerald,
            recurrence: Recurrence::None,
            reminder_min: None,
            calendar_id: None,
            project: Some(pid_personal),
        },
        Event {
            id: Uuid::new_v4(),
            title: "1:1 with manager".into(),
            start: at(d(-1), 15, 0),
            end: at(d(-1), 15, 30),
            all_day: false,
            location: "Meet".into(),
            color: Palette::Indigo,
            recurrence: Recurrence::None,
            reminder_min: Some(10),
            calendar_id: None,
            project: Some(pid_work),
        },
        Event {
            id: Uuid::new_v4(),
            title: "Pay rent".into(),
            start: at(d(7), 9, 0),
            end: at(d(7), 9, 15),
            all_day: true,
            location: String::new(),
            color: Palette::Amber,
            recurrence: Recurrence::Monthly,
            reminder_min: Some(720),
            calendar_id: None,
            project: Some(pid_personal),
        },
    ];
    // Stable display order: by start time within the model vector.
    events.sort_by_key(|e| e.start);

    // Captured so the offsite-flights task can own a few subtasks — this
    // exercises the parent/child rendering + toggle path in the detail
    // panel with real seeded data.
    let flights = Uuid::new_v4();

    let mut tasks = vec![
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
    ];

    // Subtasks of "Book flights for offsite" — completing one strikes it
    // through; the parent shows them in its detail panel.
    tasks.push(Task {
        id: Uuid::new_v4(),
        title: "Compare airline prices".into(),
        done: true,
        due: Some(d(1)),
        priority: Priority::None,
        project: Some(pid_personal),
        parent: Some(flights),
    });
    tasks.push(Task {
        id: Uuid::new_v4(),
        title: "Hold seats on the corporate card".into(),
        done: false,
        due: Some(d(2)),
        priority: Priority::Low,
        project: Some(pid_personal),
        parent: Some(flights),
    });
    tasks.push(Task {
        id: Uuid::new_v4(),
        title: "Add itinerary to the calendar".into(),
        done: false,
        due: Some(d(3)),
        priority: Priority::None,
        project: Some(pid_personal),
        parent: Some(flights),
    });

    Model {
        projects,
        events,
        tasks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn weekly_recurrence_lands_on_same_weekday() {
        let mut e = sample_event(date(2026, 6, 15)); // Monday
        e.recurrence = Recurrence::Weekly;
        let occ = e.occurrences_in(date(2026, 6, 1), date(2026, 6, 30));
        assert_eq!(
            occ,
            vec![date(2026, 6, 15), date(2026, 6, 22), date(2026, 6, 29)]
        );
    }

    #[test]
    fn weekdays_recurrence_skips_weekends() {
        let mut e = sample_event(date(2026, 6, 15));
        e.recurrence = Recurrence::Weekdays;
        let occ = e.occurrences_in(date(2026, 6, 15), date(2026, 6, 21));
        // Mon..Fri only.
        assert_eq!(occ.len(), 5);
        assert!(occ.iter().all(|d| d.weekday().num_days_from_monday() < 5));
    }

    #[test]
    fn none_recurrence_is_single_day() {
        let e = sample_event(date(2026, 6, 15));
        assert!(e.occurs_on(date(2026, 6, 15)));
        assert!(!e.occurs_on(date(2026, 6, 16)));
    }

    #[test]
    fn today_filter_includes_overdue_open_tasks() {
        let model = seed_demo(date(2026, 6, 15));
        let today = model.tasks_for(&ListSel::Today, date(2026, 6, 15));
        // Every "Today" task is open and due on/before today.
        assert!(
            today
                .iter()
                .all(|t| !t.done && t.due.unwrap() <= date(2026, 6, 15))
        );
    }

    #[test]
    fn demo_seed_has_no_developer_todo_titles() {
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

    #[test]
    fn demo_seed_keeps_a_realistic_spread() {
        // Sanity that the neutral replacements are actually present, so a future
        // edit can't silently gut the sample into an empty/placeholder set.
        let model = seed_demo(date(2026, 6, 15));
        let has = |t: &str| {
            model.events.iter().any(|e| e.title == t) || model.tasks.iter().any(|k| k.title == t)
        };
        assert!(has("Company offsite"), "neutral all-day event present");
        assert!(has("Reply to Alex's email"), "neutral task present");
        assert!(has("Lunch with Sam"), "everyday event present");
    }

    #[test]
    fn rrule_round_trips() {
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
            reminder_min: None,
            calendar_id: None,
            project: None,
        }
    }
}
