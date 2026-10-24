//! Pure data model for the calendar/todo app.
//!
//! This is the in-memory source of truth. It is mirrored into the
//! Automerge document by [`crate::resource`] (events / tasks / projects as
//! CRDT objects), and each event/task's rich-text **notes** live in the
//! CRDT as rich text edited in the SDK rich-text editor.
//!
//! No rendering, no IO — just types, filters, and recurrence expansion.

use std::collections::BTreeMap;

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime};
use tilo_document::RichText;
use ui::tilo_resource::{self, Identified, Keyed, Model, Outbox, Refusal, Set};
use uuid::Uuid;
use vello::peniko::Color;

use ui::theme::Theme;
use ui::tokens::colors::PresenceHue;

pub type Id = Uuid;

/// The five top-level screens. `Agenda` is the flat "upcoming" list of
/// events; `Tasks` lists the sidebar's selected task list in full.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Month,
    Week,
    Day,
    Agenda,
    Tasks,
}

impl View {
    pub fn label(self) -> &'static str {
        match self {
            View::Month => "Month",
            View::Week => "Week",
            View::Day => "Day",
            View::Agenda => "Agenda",
            View::Tasks => "Tasks",
        }
    }
    /// The visible label; `label` remains the stable action identifier.
    pub(crate) fn display_label(self) -> String {
        if self == Self::Tasks {
            crate::resource::t("calendar-tasks-title")
        } else {
            self.label().to_owned()
        }
    }
    pub const ALL: [View; 5] = [
        View::Month,
        View::Week,
        View::Day,
        View::Agenda,
        View::Tasks,
    ];
}

/// Which "smart list" or project the sidebar has chosen. Drives the
/// Tasks screen and the sidebar list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListSel {
    Inbox,
    Today,
    Upcoming,
    Completed,
    Project(Id),
}

/// Task priority — Planify-style P1..P4 (P1 == most urgent).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Model)]
pub enum Priority {
    #[default]
    None,
    Low,
    Medium,
    High,
}

impl Priority {
    pub fn label(self) -> &'static str {
        match self {
            Priority::None => "No priority",
            Priority::Low => "Low priority",
            Priority::Medium => "Medium priority",
            Priority::High => "High priority",
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
#[derive(
    Clone,
    Copy,
    PartialEq,
    Eq,
    Debug,
    Default,
    Model,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Palette {
    #[default]
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

    /// The colour an item of this calendar colour is painted in: a hue of
    /// the theme's presence palette (calendar colours are user content), or
    /// the quiet ink tier for the colourless slate.
    pub fn color(self, theme: &Theme) -> Color {
        let c = &theme.tokens.colors;
        let hue = match self {
            Palette::Indigo => PresenceHue::Blue,
            Palette::Emerald => PresenceHue::Green,
            Palette::Amber => PresenceHue::Amber,
            Palette::Red => PresenceHue::Red,
            Palette::Blue => PresenceHue::Sky,
            Palette::Violet => PresenceHue::Violet,
            Palette::Slate => return c.text_tertiary,
        };
        c.presence.hue(hue)
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

    /// The colour called `name` (any case).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|palette| palette.name().eq_ignore_ascii_case(name))
    }

    /// The colour for something new beside things already in `taken`: the
    /// first colour none of them has, or, with every colour taken, the one
    /// fewest have.
    pub fn next_after(taken: impl IntoIterator<Item = Palette>) -> Self {
        let taken: Vec<Palette> = taken.into_iter().collect();
        Self::ALL
            .into_iter()
            .min_by_key(|palette| taken.iter().filter(|used| *used == palette).count())
            .unwrap_or_default()
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
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Model)]
pub enum Recurrence {
    #[default]
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
#[derive(Clone, PartialEq, Eq, Debug, Default, Model)]
#[model(check = Attendee::valid)]
pub struct Attendee {
    /// Its id
    #[model(readonly)]
    pub id: Id,
    /// Name
    pub name: String,
    /// Email address
    #[model(label)]
    pub email: String,
    /// Whether they come
    pub status: InviteStatus,
}

impl Attendee {
    fn valid(&self) -> Result<(), Refusal> {
        if crate::actions::email_address(&self.email).is_none() {
            return Err(Refusal::new("an attendee needs a complete email address"));
        }
        Ok(())
    }
}

impl Identified for Attendee {
    fn id(&self) -> tilo_resource::Id {
        key(self.id)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Model)]
pub enum InviteStatus {
    #[default]
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
#[derive(Clone, PartialEq, Eq, Debug, Default, Model)]
#[model(check = OccurrenceOverride::valid)]
pub struct OccurrenceOverride {
    /// The date it was on
    #[model(readonly)]
    pub original_date: NaiveDate,
    /// Time component when the RFC identity was DATE-TIME rather than DATE.
    #[model(readonly)]
    pub original_time: Option<NaiveTime>,
    /// Cancelled
    pub cancelled: bool,
    /// Start
    pub start: Option<NaiveDateTime>,
    /// End
    pub end: Option<NaiveDateTime>,
    /// Whether the end selects the later instant of a repeated wall time.
    pub end_later_fold: bool,
    /// Title
    pub title: Option<String>,
    /// Place
    pub location: Option<String>,
}

impl OccurrenceOverride {
    /// The key this change is stored under: the occurrence it changes.
    pub fn key_of(date: NaiveDate) -> tilo_resource::Id {
        tilo_resource::Id::from_raw(date.format("%Y-%m-%d").to_string())
    }

    fn valid(&self) -> Result<(), Refusal> {
        if let (Some(start), Some(end)) = (self.start, self.end)
            && end < start
            && !self.end_later_fold
        {
            return Err(Refusal::new("an occurrence ends after it starts"));
        }
        Ok(())
    }
}

impl Identified for OccurrenceOverride {
    fn id(&self) -> tilo_resource::Id {
        Self::key_of(self.original_date)
    }
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
        let rule = rule.to_lowercase();
        if rule.contains("freq=daily") {
            Recurrence::Daily
        } else if rule.contains("byday=mo,tu,we,th,fr") {
            Recurrence::Weekdays
        } else if rule.contains("freq=weekly") {
            Recurrence::Weekly
        } else if rule.contains("freq=monthly") {
            Recurrence::Monthly
        } else if rule.contains("freq=yearly") {
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

/// A task project: a named, coloured list tasks and events file under.
#[derive(Clone, Debug, Default, PartialEq, Model)]
#[model(check = Project::valid)]
pub struct Project {
    /// Its id
    #[model(readonly)]
    pub id: Id,
    /// Name
    #[model(label)]
    pub name: String,
    /// Colour
    pub color: Palette,
    /// Where it lists among the projects
    pub order: u32,
}

impl Project {
    fn valid(&self) -> Result<(), Refusal> {
        if self.name.trim().is_empty() {
            return Err(Refusal::new("a project needs a name"));
        }
        Ok(())
    }
}

/// Projects list in the order they were arranged in.
impl Identified for Project {
    fn id(&self) -> tilo_resource::Id {
        key(self.id)
    }
    fn order(&self, other: &Self) -> std::cmp::Ordering {
        self.order.cmp(&other.order)
    }
}

/// The key an event, task, project or attendee is stored under: its id.
pub fn key(id: Id) -> tilo_resource::Id {
    tilo_resource::Id::from_raw(id.to_string())
}

/// An event: a time, maybe repeating, with who comes and where.
#[derive(Clone, Debug, PartialEq, Model)]
#[model(check = Event::valid)]
pub struct Event {
    /// Its id
    #[model(readonly)]
    pub id: Id,
    /// Title
    #[model(label)]
    pub title: String,
    /// Start
    pub start: NaiveDateTime,
    /// End
    pub end: NaiveDateTime,
    /// Select the later instant when the end falls in a DST fold. Missing in
    /// older documents means false, preserving the earlier-instant policy.
    pub end_later_fold: bool,
    /// All day
    pub all_day: bool,
    /// Place
    pub location: String,
    /// Colour
    pub color: Palette,
    /// How it repeats
    pub recurrence: Recurrence,
    /// Positive recurrence interval. For example, weekly + 2 is every other
    /// week. Old documents default to 1.
    pub recurrence_interval: u32,
    /// Inclusive final original occurrence date.
    pub recurrence_until: Option<NaiveDate>,
    /// Maximum number of original occurrences, including the seed.
    pub recurrence_count: Option<u32>,
    /// Absolute original occurrence dates omitted from the series.
    pub recurrence_exdates: Set<NaiveDate>,
    /// Per-occurrence absolute cancellation/reschedule/content edits.
    pub occurrence_overrides: Keyed<OccurrenceOverride>,
    /// IANA time-zone identifier, or `floating` for legacy/local wall time.
    pub timezone: String,
    /// Stable attendee id designating the organizer, when known.
    pub organizer: Option<Id>,
    /// Who is invited. Taking someone off the list asks nothing.
    #[model(remove = "quiet")]
    pub attendees: Keyed<Attendee>,
    /// Minutes before start for a reminder, if any.
    pub reminder_min: Option<i64>,
    /// The project it is filed under
    pub project: Option<Id>,
    /// Which remote calendar this event was synced from, if any. Local events
    /// (created in-app or imported) carry `None` and are always shown; a synced
    /// event carries its source [`crate::account::RemoteCalendar`] id so the
    /// sidebar's per-calendar visibility toggle can hide/show it on the spot,
    /// without a re-sync. Persisted in the CRDT under the `cal` key.
    pub calendar_id: Option<String>,
}

/// An event at midnight of no day, an instant long, on the local calendar:
/// not repeating, floating time, no reminder, nobody invited.
impl Default for Event {
    fn default() -> Self {
        Self {
            id: Id::nil(),
            title: String::new(),
            start: NaiveDateTime::default(),
            end: NaiveDateTime::default(),
            end_later_fold: false,
            all_day: false,
            location: String::new(),
            color: Palette::default(),
            recurrence: Recurrence::None,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Set::default(),
            occurrence_overrides: Keyed::default(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Keyed::default(),
            reminder_min: None,
            project: None,
            calendar_id: None,
        }
    }
}

/// Events list by when they start.
impl Identified for Event {
    fn id(&self) -> tilo_resource::Id {
        key(self.id)
    }
    fn order(&self, other: &Self) -> std::cmp::Ordering {
        self.start.cmp(&other.start)
    }
}

/// How many minutes before an event its reminder may ring: the alerts the
/// editor offers, from the start itself to a day before.
pub const REMINDERS: [i64; 7] = [0, 5, 10, 15, 30, 60, 1440];

impl Event {
    fn valid(&self) -> Result<(), Refusal> {
        if !crate::timezone::valid_span(&self.timezone, self.start, self.end, self.end_later_fold) {
            return Err(Refusal::new("an event ends after it starts"));
        }
        if self
            .reminder_min
            .is_some_and(|minutes| !REMINDERS.contains(&minutes))
        {
            return Err(Refusal::new(
                "a reminder rings at the start, 5, 10, 15 or 30 minutes, an hour or a day before",
            ));
        }
        if self.recurrence_interval == 0 {
            return Err(Refusal::new("an event repeats every 1 or more"));
        }
        if crate::actions::normalize_timezone(&self.timezone).as_deref()
            != Some(self.timezone.as_str())
        {
            return Err(Refusal::new(
                "an event's time zone is an IANA name or `floating`",
            ));
        }
        for edit in &self.occurrence_overrides {
            let start = edit
                .start
                .unwrap_or_else(|| edit.original_date.and_time(self.start.time()));
            if let Some(end) = edit.end
                && !crate::timezone::valid_span(&self.timezone, start, end, edit.end_later_fold)
            {
                return Err(Refusal::new("an occurrence ends after it starts"));
            }
        }
        if let Some(organizer) = self.organizer
            && !self
                .attendees
                .iter()
                .any(|attendee| attendee.id == organizer)
        {
            return Err(Refusal::new("the organizer is one of the attendees"));
        }
        Ok(())
    }
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

    /// Absolute end, including a duration-derived later-fold endpoint.
    pub fn end_instant(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        crate::timezone::endpoint_utc(&self.timezone, self.end, self.end_later_fold)
    }

    /// The elapsed interval; floating events have only a wall interval.
    pub fn elapsed_duration(&self) -> Duration {
        crate::timezone::wall_to_utc(&self.timezone, self.start)
            .zip(self.end_instant())
            .map_or(self.end - self.start, |((start, _), end)| end - start)
    }

    /// End of a generated occurrence. A later-fold seed carries its exact
    /// elapsed interval forward without changing the series' wall-time anchor.
    pub fn occurrence_end(&self, start: NaiveDateTime) -> NaiveDateTime {
        if self.end_later_fold
            && let Some((instant, _)) = crate::timezone::wall_to_utc(&self.timezone, start)
            && let Ok(zone) = self.timezone.parse::<chrono_tz::Tz>()
            && let Some(end) = instant.checked_add_signed(self.elapsed_duration())
        {
            return end.with_timezone(&zone).naive_local();
        }
        start + (self.end - self.start)
    }

    /// Duration in fractional hours (timed events only).
    pub fn hours(&self) -> f64 {
        self.elapsed_duration().num_minutes().max(0) as f64 / 60.0
    }
}

/// A task: something to do, maybe by a date, maybe beneath another.
#[derive(Clone, Debug, Default, PartialEq, Model)]
#[model(check = Task::valid)]
pub struct Task {
    /// Its id
    #[model(readonly)]
    pub id: Id,
    /// Title
    #[model(label)]
    pub title: String,
    /// Done
    pub done: bool,
    /// When it is due
    pub due: Option<NaiveDate>,
    /// Priority
    pub priority: Priority,
    /// The project it is filed under
    pub project: Option<Id>,
    /// The task it is a step of
    #[model(readonly)]
    pub parent: Option<Id>,
}

impl Task {
    fn valid(&self) -> Result<(), Refusal> {
        if self.parent == Some(self.id) {
            return Err(Refusal::new("a task is not a step of itself"));
        }
        Ok(())
    }
}

impl Identified for Task {
    fn id(&self) -> tilo_resource::Id {
        key(self.id)
    }
}

impl Task {
    /// The order tasks list in: open before done, then by priority, then by
    /// due date, then by title, so the same tasks always list the same way
    /// whatever order the document holds them in.
    pub fn list_order(a: &Task, b: &Task) -> std::cmp::Ordering {
        a.done
            .cmp(&b.done)
            .then(a.priority.rank().cmp(&b.priority.rank()))
            .then(a.due.cmp(&b.due))
            .then_with(|| a.title.cmp(&b.title))
            .then(a.id.cmp(&b.id))
    }
}

/// What a `CalDAV` server last confirmed holding of one event or task: where
/// it keeps it, its version there and what it holds. An event whose content
/// differs from what its server holds is queued for upload; a copy whose
/// event is gone, or on another calendar, is queued for deletion (see
/// `crate::outbox`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Model)]
pub struct RemoteCopy {
    /// The remote calendar it is in
    pub calendar: String,
    /// Where the server keeps it; empty for an item a sample server sent
    pub href: String,
    /// The version the server last confirmed (its `ETag`)
    pub etag: Option<String>,
    /// A fingerprint of the event the server holds; empty for a task, which
    /// is never uploaded
    pub holds: String,
    /// Whether it is a task
    pub task: bool,
}

/// The calendar: its events, tasks and projects, their notes, and what the
/// `CalDAV` servers of its remote calendars last confirmed.
#[derive(Clone, Debug, Default, PartialEq, Model)]
pub struct Calendar {
    /// The task projects
    pub projects: Keyed<Project>,
    /// The events
    pub events: Keyed<Event>,
    /// The tasks
    pub tasks: Keyed<Task>,
    /// Each event's and task's notes, under its id
    pub notes: BTreeMap<String, RichText>,
    /// What each remote item's server last confirmed, under the item's id
    #[model(readonly)]
    pub remote: BTreeMap<String, RemoteCopy>,
    /// Where each remote calendar's last pull ended (its sync token)
    #[model(readonly)]
    pub sync_tokens: BTreeMap<String, String>,
    /// Events whose version here their server does not hold yet
    pub uploads: Outbox<Upload>,
    /// Copies servers hold of events gone from their calendar here
    pub deletes: Outbox<Removal>,
}

/// An event to upload to the server of its remote calendar: whatever
/// version it has when the device holding the account sends it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Model)]
pub struct Upload {
    /// The event
    pub event: Id,
    /// The remote calendar it is uploaded to
    pub calendar: String,
}

/// A copy a server holds of an event no longer on that calendar here, to
/// delete there.
#[derive(Clone, Debug, Default, PartialEq, Eq, Model)]
pub struct Removal {
    /// The event
    pub item: Id,
    /// The remote calendar holding the copy
    pub calendar: String,
    /// Where the server keeps it
    pub href: String,
    /// The version the server last confirmed (its `ETag`)
    pub etag: Option<String>,
}

impl Calendar {
    /// The text of the notes of the event or task `id`, a paragraph per
    /// line; empty when it has none.
    pub fn notes_text(&self, id: Id) -> String {
        self.notes
            .get(&id.to_string())
            .map(RichText::text)
            .unwrap_or_default()
    }

    pub fn project(&self, id: Id) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn event(&self, id: Id) -> Option<&Event> {
        self.events.iter().find(|e| e.id == id)
    }
    /// The event `id`, to change. Found through [`tilo_resource::Keyed::find_mut`],
    /// so a copy of the calendar remembers which event changed and saving it
    /// writes that event, not every one.
    pub fn event_mut(&mut self, id: Id) -> Option<&mut Event> {
        self.events.find_mut(&key(id))
    }

    pub fn task(&self, id: Id) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }
    /// The task `id`, to change, found as [`Self::event_mut`] finds an event.
    pub fn task_mut(&mut self, id: Id) -> Option<&mut Task> {
        self.tasks.find_mut(&key(id))
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
        out.sort_by(|a, b| Task::list_order(a, b));
        out
    }

    /// The name a smart list or project is shown under.
    pub fn list_title(&self, sel: &ListSel) -> String {
        match sel {
            ListSel::Inbox => "Inbox".into(),
            ListSel::Today => "Today".into(),
            ListSel::Upcoming => "Upcoming".into(),
            ListSel::Completed => "Completed".into(),
            ListSel::Project(id) => self
                .project(*id)
                .map_or_else(|| "Project".into(), |project| project.name.clone()),
        }
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
pub fn seed_demo(today: NaiveDate) -> Calendar {
    let pid_work = Uuid::new_v4();
    let pid_personal = Uuid::new_v4();
    let pid_health = Uuid::new_v4();

    Calendar {
        projects: seed_demo_projects(pid_work, pid_personal, pid_health).into(),
        events: seed_demo_events(today, pid_work, pid_personal, pid_health).into(),
        tasks: seed_demo_tasks(today, pid_personal, pid_health, pid_work).into(),
        ..Calendar::default()
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
            location: "Huddle room / Meet".into(),
            recurrence: Recurrence::Weekdays,
            reminder_min: Some(10),
            project: Some(pid_work),
            ..Event::default()
        },
        Event {
            id: Uuid::new_v4(),
            title: "Design review".into(),
            start: seed_demo_at(today, 11, 0),
            end: seed_demo_at(today, 12, 0),
            location: "Figma + Meet".into(),
            color: Palette::Violet,
            recurrence: Recurrence::Weekly,
            recurrence_interval: 2,
            recurrence_until: Some(d(90)),
            recurrence_exdates: [d(28)].into_iter().collect(),
            occurrence_overrides: vec![OccurrenceOverride {
                original_date: d(42),
                original_time: None,
                cancelled: false,
                start: Some(seed_demo_at(d(43), 14, 0)),
                end: Some(seed_demo_at(d(43), 15, 0)),
                end_later_fold: false,
                title: Some("Design review (moved)".into()),
                location: None,
            }]
            .into(),
            timezone: "Europe/Berlin".into(),
            organizer: Some(design_organizer),
            attendees: vec![Attendee {
                id: design_organizer,
                name: "Mina Patel".into(),
                email: "mina@example.com".into(),
                status: InviteStatus::Accepted,
            }]
            .into(),
            reminder_min: Some(10),
            project: Some(pid_work),
            ..Event::default()
        },
        Event {
            id: Uuid::new_v4(),
            title: "Lunch with Sam".into(),
            start: seed_demo_at(today, 13, 0),
            end: seed_demo_at(today, 14, 0),
            location: "Café Mori".into(),
            color: Palette::Emerald,
            project: Some(pid_personal),
            ..Event::default()
        },
        Event {
            id: Uuid::new_v4(),
            title: "Strength training".into(),
            start: seed_demo_at(today, 18, 30),
            end: seed_demo_at(today, 19, 30),
            location: "Northside".into(),
            color: Palette::Red,
            recurrence: Recurrence::Weekly,
            reminder_min: Some(30),
            project: Some(pid_health),
            ..Event::default()
        },
        Event {
            id: Uuid::new_v4(),
            title: "Quarterly planning".into(),
            start: seed_demo_at(d(2), 10, 0),
            end: seed_demo_at(d(2), 11, 30),
            location: "War room".into(),
            reminder_min: Some(10),
            project: Some(pid_work),
            ..Event::default()
        },
    ]
}

/// The rest of the week's events: dentist through payday rent.
fn seed_demo_events_later(today: NaiveDate, pid_work: Id, pid_personal: Id) -> Vec<Event> {
    let d = |delta: i64| today + Duration::days(delta);
    vec![
        // Overlaps the quarterly planning: the two share the column.
        Event {
            id: Uuid::new_v4(),
            title: "Interview: Lena".into(),
            start: seed_demo_at(d(2), 11, 0),
            end: seed_demo_at(d(2), 12, 0),
            location: "Room 4".into(),
            color: Palette::Amber,
            timezone: "Europe/Berlin".into(),
            project: Some(pid_work),
            ..Event::default()
        },
        // Authored in New York: a morning there, an afternoon in Berlin.
        Event {
            id: Uuid::new_v4(),
            title: "Call with the New York team".into(),
            start: seed_demo_at(d(1), 9, 0),
            end: seed_demo_at(d(1), 9, 45),
            location: "Meet".into(),
            color: Palette::Blue,
            timezone: "America/New_York".into(),
            reminder_min: Some(10),
            project: Some(pid_work),
            ..Event::default()
        },
        Event {
            id: Uuid::new_v4(),
            title: "Dentist".into(),
            start: seed_demo_at(d(3), 8, 0),
            end: seed_demo_at(d(3), 8, 45),
            location: "Bright Smiles".into(),
            color: Palette::Blue,
            reminder_min: Some(60),
            project: Some(pid_personal),
            ..Event::default()
        },
        Event {
            id: Uuid::new_v4(),
            title: "Company offsite".into(),
            start: seed_demo_at(d(4), 0, 0),
            end: seed_demo_at(d(4), 23, 59),
            all_day: true,
            color: Palette::Amber,
            project: Some(pid_work),
            ..Event::default()
        },
        Event {
            id: Uuid::new_v4(),
            title: "Weekend hike".into(),
            start: seed_demo_at(d(5), 7, 30),
            end: seed_demo_at(d(5), 12, 0),
            location: "Ridge trail".into(),
            color: Palette::Emerald,
            project: Some(pid_personal),
            ..Event::default()
        },
        Event {
            id: Uuid::new_v4(),
            title: "1:1 with manager".into(),
            start: seed_demo_at(d(-1), 15, 0),
            end: seed_demo_at(d(-1), 15, 30),
            location: "Meet".into(),
            reminder_min: Some(10),
            project: Some(pid_work),
            ..Event::default()
        },
        Event {
            id: Uuid::new_v4(),
            title: "Pay rent".into(),
            start: seed_demo_at(d(7), 9, 0),
            end: seed_demo_at(d(7), 9, 15),
            all_day: true,
            color: Palette::Amber,
            recurrence: Recurrence::Monthly,
            reminder_min: Some(60),
            project: Some(pid_personal),
            ..Event::default()
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
        {
            let mut e = sample_event(date(2026, 6, 15)); // Monday
            e.recurrence = Recurrence::Weekly;
            let occ = e.occurrences_in(date(2026, 6, 1), date(2026, 6, 30));
            assert_eq!(
                occ,
                vec![date(2026, 6, 15), date(2026, 6, 22), date(2026, 6, 29)]
            );
        }
        {
            let mut e = sample_event(date(2026, 6, 15));
            e.recurrence = Recurrence::Weekdays;
            let occ = e.occurrences_in(date(2026, 6, 15), date(2026, 6, 21));
            // Mon..Fri only.
            assert_eq!(occ.len(), 5);
            assert!(occ.iter().all(|d| d.weekday().num_days_from_monday() < 5));
        }
        {
            let mut event = sample_event(date(2026, 6, 1));
            event.recurrence = Recurrence::Daily;
            event.recurrence_interval = 2;
            event.recurrence_count = Some(4);
            event.recurrence_exdates.insert(date(2026, 6, 3));
            event.occurrence_overrides.push(OccurrenceOverride {
                original_date: date(2026, 6, 5),
                original_time: None,
                cancelled: false,
                start: Some(date(2026, 6, 6).and_hms_opt(9, 0, 0).unwrap()),
                end: Some(date(2026, 6, 6).and_hms_opt(10, 0, 0).unwrap()),
                end_later_fold: false,
                title: Some("Moved instance".into()),
                location: None,
            });
            assert_eq!(
                event.occurrences_in(date(2026, 6, 1), date(2026, 6, 10)),
                vec![date(2026, 6, 1), date(2026, 6, 6), date(2026, 6, 7)]
            );
        }
        {
            let e = sample_event(date(2026, 6, 15));
            assert!(e.occurs_on(date(2026, 6, 15)));
            assert!(!e.occurs_on(date(2026, 6, 16)));
        }
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
            ..Event::default()
        }
    }
}
