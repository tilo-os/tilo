//! The event, task and calendar shapes a calendar server's entry is read into
//! and written from.

// Calendar arithmetic is bounded by the values a calendar holds: a time of
// day, a duration within one, a count of occurrences. Panic-producing lints
// stay denied.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "bounded date, time and duration arithmetic on calendar values"
)]

use std::collections::BTreeMap;

use jiff::civil::{Date, DateTime, Time, Weekday};
use tilo_document::RichText;
use tilo_recurrence::Rule;
use tilo_resource::{self, Identified, Keyed, Model, Outbox, Refusal, Set};
use uuid::Uuid;

pub use tilo_recurrence::Repeat as Recurrence;

pub type Id = Uuid;

/// The address `value` is, when it is a complete one: `None` for a name with
/// no host or a host with no dot.
#[must_use]
pub fn email_address(value: &str) -> Option<&str> {
    let (local, domain) = value.split_once('@')?;
    (!local.is_empty() && domain.contains('.') && !domain.contains(char::is_whitespace))
        .then_some(value)
}

/// The IANA name `value` names, or `floating`: `None` for a zone this build
/// does not know.
#[must_use]
pub fn normalize_timezone(value: &str) -> Option<String> {
    (value.len() <= 128)
        .then(|| crate::timezone::canonical_zone(value))
        .flatten()
}

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
    pub fn display_label(self) -> String {
        match self {
            Self::Month => tilo_resource::word!("calendar-view-month").read(),
            Self::Week => tilo_resource::word!("calendar-view-week").read(),
            Self::Day => tilo_resource::word!("calendar-view-day").read(),
            Self::Agenda => tilo_resource::word!("calendar-view-agenda").read(),
            Self::Tasks => tilo_resource::word!("calendar-tasks-title").read(),
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListSel {
    Inbox,
    Today,
    Upcoming,
    Completed,
    Project(Id),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Model)]
pub enum Priority {
    #[default]
    None,
    Low,
    Medium,
    High,
}

impl Priority {
    pub fn display_label(self) -> String {
        match self {
            Priority::None => tilo_resource::word!("calendar-tasks-priority-none").read(),
            Priority::Low => tilo_resource::word!("calendar-tasks-priority-low").read(),
            Priority::Medium => tilo_resource::word!("calendar-tasks-priority-medium").read(),
            Priority::High => tilo_resource::word!("calendar-tasks-priority-high").read(),
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

/// A small colour palette for calendars/projects/events.
/// The enum encodes *which* semantic place an item picked; the window resolves
/// it through its theme, so the OS controls the look.
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

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|palette| palette.name().eq_ignore_ascii_case(name))
    }

    pub fn next_after(taken: impl IntoIterator<Item = Palette>) -> Self {
        let taken: Vec<Palette> = taken.into_iter().collect();
        Self::ALL
            .into_iter()
            .min_by_key(|palette| taken.iter().filter(|used| *used == palette).count())
            .unwrap_or_default()
    }

    pub fn from_index(i: usize) -> Self {
        Self::ALL
            .get(i % Self::ALL.len())
            .copied()
            .unwrap_or(Self::ALL[0])
    }
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }
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
        if email_address(&self.email).is_none() {
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

#[derive(Clone, PartialEq, Eq, Debug, Default, Model)]
#[model(check = OccurrenceOverride::valid)]
pub struct OccurrenceOverride {
    /// The date it was on
    #[model(readonly)]
    pub original_date: Date,
    /// Time component when the RFC identity was DATE-TIME rather than DATE.
    #[model(readonly)]
    pub original_time: Option<Time>,
    /// Cancelled
    pub cancelled: bool,
    /// Start
    pub start: Option<DateTime>,
    /// End
    pub end: Option<DateTime>,
    /// Whether the end selects the later instant of a repeated wall time.
    pub end_later_fold: bool,
    /// Title
    pub title: Option<String>,
    /// Place
    pub location: Option<String>,
}

impl OccurrenceOverride {
    pub fn key_of(date: Date) -> tilo_resource::Id {
        tilo_resource::Id::from_raw(tilo_formats::date_text(date))
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

impl Identified for Project {
    fn id(&self) -> tilo_resource::Id {
        key(self.id)
    }
    fn order(&self, other: &Self) -> std::cmp::Ordering {
        self.order.cmp(&other.order)
    }
}

pub fn key(id: Id) -> tilo_resource::Id {
    tilo_resource::Id::from_raw(id.to_string())
}

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
    pub start: DateTime,
    /// End
    pub end: DateTime,
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
    pub recurrence_until: Option<Date>,
    /// Maximum number of original occurrences, including the seed.
    pub recurrence_count: Option<u32>,
    /// Absolute original occurrence dates omitted from the series.
    pub recurrence_exdates: Set<Date>,
    /// The days of the week it repeats on, as RFC 5545 writes them (`MO`..`SU`), when its rule
    /// names them: empty leaves the preset's days.
    pub recurrence_weekdays: Set<String>,
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
    /// event carries its source calendar's account id so the
    /// sidebar's per-calendar visibility toggle can hide/show it on the spot,
    /// without a re-sync. Persisted in the CRDT under the `cal` key.
    pub calendar_id: Option<String>,
    /// The categories it is filed under: its person's own words, kept as
    /// its server's `CATEGORIES`.
    pub categories: Set<String>,
}

impl Default for Event {
    fn default() -> Self {
        Self {
            id: Id::nil(),
            title: String::new(),
            start: DateTime::default(),
            end: DateTime::default(),
            end_later_fold: false,
            all_day: false,
            location: String::new(),
            color: Palette::default(),
            recurrence: Recurrence::None,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Set::default(),
            recurrence_weekdays: Set::default(),
            occurrence_overrides: Keyed::default(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Keyed::default(),
            reminder_min: None,
            project: None,
            calendar_id: None,
            categories: Set::default(),
        }
    }
}

/// The weekday an RFC 5545 code names (`MO`..`SU`), in any case.
pub fn weekday_of(code: &str) -> Option<Weekday> {
    Some(match code.trim().to_ascii_uppercase().as_str() {
        "MO" => Weekday::Monday,
        "TU" => Weekday::Tuesday,
        "WE" => Weekday::Wednesday,
        "TH" => Weekday::Thursday,
        "FR" => Weekday::Friday,
        "SA" => Weekday::Saturday,
        "SU" => Weekday::Sunday,
        _ => return None,
    })
}

/// An RFC 5545 code for `day`, as its rule writes it.
#[must_use]
pub fn weekday_code(day: Weekday) -> &'static str {
    match day {
        Weekday::Monday => "MO",
        Weekday::Tuesday => "TU",
        Weekday::Wednesday => "WE",
        Weekday::Thursday => "TH",
        Weekday::Friday => "FR",
        Weekday::Saturday => "SA",
        Weekday::Sunday => "SU",
    }
}

/// The days a rule comes on, as it writes them (`MO,TU`): its own, in the week's order and each
/// once; none when it names none.
#[must_use]
pub fn rule_weekdays(days: &Set<String>) -> Vec<&'static str> {
    ["MO", "TU", "WE", "TH", "FR", "SA", "SU"]
        .into_iter()
        .filter(|code| days.iter().any(|day| day.trim().eq_ignore_ascii_case(code)))
        .collect()
}

impl Identified for Event {
    fn id(&self) -> tilo_resource::Id {
        key(self.id)
    }
    fn order(&self, other: &Self) -> std::cmp::Ordering {
        self.start.cmp(&other.start)
    }
}

pub const REMINDERS: [i64; 7] = [0, 5, 10, 15, 30, 60, 1440];

/// The most categories one event is filed under.
pub const MAX_CATEGORIES: usize = 32;
/// The longest a category may be, in characters.
pub const MAX_CATEGORY_CHARS: usize = 64;

/// A category as an event keeps it: its person's own words, trimmed. `None`
/// for one no event holds: empty, longer than [`MAX_CATEGORY_CHARS`], or
/// with a character nobody can read.
#[must_use]
pub fn category(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()
        && text.chars().count() <= MAX_CATEGORY_CHARS
        && !text.chars().any(char::is_control))
    .then(|| text.to_owned())
}

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
        if self.categories.len() > MAX_CATEGORIES
            || self
                .categories
                .iter()
                .any(|held| category(held).as_deref() != Some(held.as_str()))
        {
            return Err(Refusal::new(
                "an event has at most 32 categories, each a few words of at most 64 characters",
            ));
        }
        if normalize_timezone(&self.timezone).as_deref() != Some(self.timezone.as_str()) {
            return Err(Refusal::new(
                "an event's time zone is an IANA name or `floating`",
            ));
        }
        for edit in &self.occurrence_overrides {
            let start = edit
                .start
                .unwrap_or_else(|| edit.original_date.to_datetime(self.start.time()));
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

impl Event {
    pub fn all_day_last_date(&self) -> Date {
        if !self.all_day {
            return self.end.date();
        }
        if self.end.time() == Time::MIN && self.end.date() > self.start.date() {
            self.end.date() - jiff::Span::new().days(1)
        } else {
            self.end.date().max(self.start.date())
        }
    }

    pub fn all_day_end_exclusive(&self) -> Date {
        self.all_day_last_date() + jiff::Span::new().days(1)
    }

    /// Number of visible dates occupied by an all-day event.  Kept separate
    /// from a timestamp duration because RFC all-day events are date ranges,
    /// and a daylight-saving transition must never shorten one in the views.
    pub fn all_day_span_days(&self) -> i64 {
        i64::from(
            self.all_day_end_exclusive()
                .since(self.start.date())
                .map_or(0, |span| span.get_days()),
        )
        .max(1)
    }

    pub fn visible_under(&self, hidden: &std::collections::HashSet<String>) -> bool {
        match &self.calendar_id {
            Some(id) => !hidden.contains(id),
            None => true,
        }
    }

    pub(crate) fn rule(&self) -> Option<Rule> {
        let exdates: Vec<Date> = self.recurrence_exdates.iter().copied().collect();
        let rule = self.recurrence.rule(
            self.recurrence_interval,
            self.recurrence_until,
            self.recurrence_count,
            &exdates,
        )?;
        // The days the rule names are the rule's own, whatever preset picked them.
        let days: Vec<Weekday> = self
            .recurrence_weekdays
            .iter()
            .filter_map(|code| weekday_of(code))
            .collect();
        Some(match (days.is_empty(), self.recurrence) {
            (true, _) => rule,
            (false, Recurrence::Weekly | Recurrence::Weekdays) => rule.with_by_day(days),
            (false, _) => rule,
        })
    }

    pub fn occurrences_in(&self, from: Date, to: Date) -> Vec<Date> {
        let seed = self.start.date();
        let moved_originals = || {
            self.occurrence_overrides
                .iter()
                .filter(|edit| {
                    edit.start
                        .is_some_and(|start| start.date() >= from && start.date() <= to)
                })
                .map(|edit| edit.original_date)
        };
        let last_original = moved_originals().max().unwrap_or(to).max(to);
        let window_start = moved_originals().min().unwrap_or(from).min(from).max(seed);
        let originals = match self.rule() {
            Some(rule) => rule.occurrences(seed, window_start, last_original),
            None if window_start <= seed
                && seed <= last_original
                && !self.recurrence_exdates.contains(&seed)
                && self.recurrence_until.is_none_or(|until| seed <= until)
                && self.recurrence_count != Some(0) =>
            {
                vec![seed]
            }
            None => Vec::new(),
        };
        let mut out = Vec::new();
        for day in originals {
            let edit = self
                .occurrence_overrides
                .iter()
                .find(|edit| edit.original_date == day);
            let actual = edit.and_then(|edit| edit.start).map_or(day, DateTime::date);
            if !edit.is_some_and(|edit| edit.cancelled) && actual >= from && actual <= to {
                out.push(actual);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn occurrences_before(&self, day: Date) -> u64 {
        self.rule().map_or_else(
            || u64::from(self.start.date() < day),
            |rule| rule.count_before(self.start.date(), day),
        )
    }

    pub fn occurs_on(&self, date: Date) -> bool {
        if self.all_day {
            // Query far enough back to include an event that started before
            // `date` but still spans it.  This also makes recurring all-day
            // conferences appear on every occupied day, not only their first.
            let from = date - jiff::Span::new().days(self.all_day_span_days().saturating_sub(1));
            return self.occurrences_in(from, date).into_iter().any(|start| {
                date >= start && date < start + jiff::Span::new().days(self.all_day_span_days())
            });
        }
        !self.occurrences_in(date, date).is_empty()
    }

    pub fn end_instant(&self) -> Option<jiff::Timestamp> {
        crate::timezone::endpoint_utc(&self.timezone, self.end, self.end_later_fold)
    }

    /// The elapsed interval; floating events have only a wall interval.
    pub fn elapsed_duration(&self) -> jiff::SignedDuration {
        crate::timezone::wall_to_utc(&self.timezone, self.start)
            .zip(self.end_instant())
            .map_or_else(
                || self.end.duration_since(self.start),
                |((start, _), end)| end.duration_since(start),
            )
    }

    pub fn occurrence_end(&self, start: DateTime) -> DateTime {
        if self.end_later_fold
            && let Some((instant, _)) = crate::timezone::wall_to_utc(&self.timezone, start)
            && let Some(zone) = crate::timezone::named(&self.timezone)
            && let Ok(end) = instant.checked_add(self.elapsed_duration())
        {
            return end.to_zoned(zone).datetime();
        }
        start
            .checked_add(self.end.duration_since(self.start))
            .unwrap_or(start)
    }

    /// Duration in fractional hours (timed events only).
    pub fn hours(&self) -> f64 {
        self.elapsed_duration().as_mins().max(0) as f64 / 60.0
    }
}

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
    pub due: Option<Date>,
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
    pub fn list_order(a: &Task, b: &Task) -> std::cmp::Ordering {
        a.done
            .cmp(&b.done)
            .then(a.priority.rank().cmp(&b.priority.rank()))
            .then(a.due.cmp(&b.due))
            .then_with(|| a.title.cmp(&b.title))
            .then(a.id.cmp(&b.id))
    }
}

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

#[derive(Clone, Debug, Default, PartialEq, Eq, Model)]
pub struct Upload {
    /// The event
    pub event: Id,
    /// The remote calendar it is uploaded to
    pub calendar: String,
}

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
    pub fn event_mut(&mut self, id: Id) -> Option<&mut Event> {
        self.events.find_mut(&key(id))
    }

    pub fn task(&self, id: Id) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }
    pub fn task_mut(&mut self, id: Id) -> Option<&mut Task> {
        self.tasks.find_mut(&key(id))
    }

    pub fn tasks_for(&self, sel: &ListSel, today: Date) -> Vec<&Task> {
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

    pub fn list_title(&self, sel: &ListSel) -> String {
        match sel {
            ListSel::Inbox => tilo_resource::word!("calendar-tasks-inbox").read(),
            ListSel::Today => tilo_resource::word!("calendar-tasks-today").read(),
            ListSel::Upcoming => tilo_resource::word!("calendar-tasks-upcoming").read(),
            ListSel::Completed => tilo_resource::word!("calendar-tasks-completed").read(),
            ListSel::Project(id) => self.project(*id).map_or_else(
                || tilo_resource::word!("calendar-project").read(),
                |project| project.name.clone(),
            ),
        }
    }

    pub fn subtasks_of(&self, parent: Id) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|t| t.parent == Some(parent))
            .collect()
    }

    pub fn open_count(&self, sel: &ListSel, today: Date) -> usize {
        self.tasks_for(sel, today)
            .iter()
            .filter(|t| !t.done)
            .count()
    }

    pub fn events_on(&self, date: Date) -> Vec<&Event> {
        let mut out: Vec<&Event> = self.events.iter().filter(|e| e.occurs_on(date)).collect();
        out.sort_by(|a, b| {
            b.all_day
                .cmp(&a.all_day)
                .then(a.start.time().cmp(&b.start.time()))
        });
        out
    }

    pub fn agenda(&self, from: Date, to: Date) -> Vec<(Date, &Event)> {
        let mut out: Vec<(Date, &Event)> = Vec::new();
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

pub fn seed_demo(today: Date) -> Calendar {
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

/// A literal (h, m) is always in range, so the fallback only matters if that stops being true; a wrong
/// demo time beats panicking the whole seed.
fn seed_demo_at(date: Date, h: i8, m: i8) -> DateTime {
    date.to_datetime(Time::new(h, m, 0, 0).unwrap_or(Time::MIN))
}

fn seed_demo_events(today: Date, pid_work: Id, pid_personal: Id, pid_health: Id) -> Vec<Event> {
    let mut events = seed_demo_events_early(today, pid_work, pid_personal, pid_health);
    events.extend(seed_demo_events_later(today, pid_work, pid_personal));
    events.sort_by_key(|e| e.start);
    events
}

fn seed_demo_events_early(
    today: Date,
    pid_work: Id,
    pid_personal: Id,
    pid_health: Id,
) -> Vec<Event> {
    let d = |delta: i64| today + jiff::Span::new().days(delta);
    let design_organizer = Uuid::new_v4();
    vec![
        Event {
            id: Uuid::new_v4(),
            title: "Team standup".into(),
            start: seed_demo_at(today, 9, 30),
            end: seed_demo_at(today, 9, 45),
            location: "Huddle room (Meet)".into(),
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

fn seed_demo_events_later(today: Date, pid_work: Id, pid_personal: Id) -> Vec<Event> {
    let d = |delta: i64| today + jiff::Span::new().days(delta);
    vec![
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

fn seed_demo_tasks(today: Date, pid_personal: Id, pid_health: Id, pid_work: Id) -> Vec<Task> {
    let flights = Uuid::new_v4();
    let mut tasks = seed_demo_top_level_tasks(today, pid_work, pid_personal, pid_health, flights);
    tasks.extend(seed_demo_flight_subtasks(today, pid_personal, flights));
    tasks
}

fn seed_demo_top_level_tasks(
    today: Date,
    pid_work: Id,
    pid_personal: Id,
    pid_health: Id,
    flights: Id,
) -> Vec<Task> {
    let d = |delta: i64| today + jiff::Span::new().days(delta);
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

fn seed_demo_flight_subtasks(today: Date, pid_personal: Id, flights: Id) -> Vec<Task> {
    let d = |delta: i64| today + jiff::Span::new().days(delta);
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

    fn date(y: i16, m: i8, d: i8) -> Date {
        Date::new(y, m, d).unwrap()
    }

    tilo_test_env::scenario! {
        /// Recurrence expansion against its rule.
        fn recurrence_expands_as_its_rule_says_within_its_window;

        phase recurrence_expands_rules_limits_zones_and_round_trips_rrule;
        {
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
                assert!(occ.iter().all(|d| d.weekday().to_monday_zero_offset() < 5));
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
                    start: Some(date(2026, 6, 6).at(9, 0, 0, 0)),
                    end: Some(date(2026, 6, 6).at(10, 0, 0, 0)),
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

        phase a_counted_series_shows_its_window_not_its_age;
        {
            let grid: Vec<Date> = (0..42)
                .map(|day| date(2026, 9, 28) + jiff::Span::new().days(day))
                .collect();
            let shown = |event: &Event| {
                grid.iter()
                    .copied()
                    .filter(|day| event.occurs_on(*day))
                    .collect::<Vec<_>>()
            };
            let weekly = |seed: Date, count| {
                let mut event = sample_event(seed);
                event.recurrence = Recurrence::Weekly;
                event.recurrence_count = Some(count);
                event
            };
            let young = shown(&weekly(date(2026, 6, 1), 10_000));
            let old = shown(&weekly(date(1996, 6, 3), 10_000));
            assert_eq!(young.len(), 6);
            assert_eq!(old, young);
            let ended = shown(&weekly(date(1996, 6, 3), 1584));
            assert_eq!(ended, vec![date(2026, 9, 28), date(2026, 10, 5)]);
        }
    }

    #[test]
    fn day_membership_covers_all_day_spans_and_overdue_open_tasks() {
        {
            let mut e = sample_event(date(2026, 6, 15));
            e.all_day = true;
            e.start = date(2026, 6, 15).to_datetime(Time::MIN);
            e.end = date(2026, 6, 18).to_datetime(Time::MIN);

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
            e.start = date(2026, 6, 15).to_datetime(Time::MIN);
            e.end = date(2026, 6, 15).at(23, 59, 0, 0);

            assert_eq!(e.all_day_last_date(), date(2026, 6, 15));
            assert_eq!(e.all_day_end_exclusive(), date(2026, 6, 16));
            assert!(e.occurs_on(date(2026, 6, 15)));
            assert!(!e.occurs_on(date(2026, 6, 16)));
        }
        {
            // A repeating all-day event takes every day of each occurrence and none between.
            let mut e = sample_event(date(2026, 6, 15));
            e.all_day = true;
            e.start = date(2026, 6, 15).to_datetime(Time::MIN);
            e.end = date(2026, 6, 18).to_datetime(Time::MIN);
            e.recurrence = Recurrence::Weekly;
            let taken: Vec<i8> = (12..=30)
                .filter(|day| e.occurs_on(date(2026, 6, *day)))
                .collect();
            assert_eq!(taken, vec![15, 16, 17, 22, 23, 24, 29, 30]);
        }
        {
            let model = seed_demo(date(2026, 6, 15));
            let today = model.tasks_for(&ListSel::Today, date(2026, 6, 15));
            assert!(
                today
                    .iter()
                    .all(|t| !t.done && t.due.unwrap() <= date(2026, 6, 15))
            );
        }
    }

    fn sample_event(start_date: Date) -> Event {
        Event {
            id: Uuid::new_v4(),
            title: "x".into(),
            start: start_date.to_datetime(Time::new(9, 0, 0, 0).unwrap()),
            end: start_date.to_datetime(Time::new(10, 0, 0, 0).unwrap()),
            ..Event::default()
        }
    }

    #[test]
    fn occurrences_before_counts_what_the_rule_matches() {
        let seeds = [
            date(2024, 1, 31),
            date(2024, 2, 29),
            date(2026, 6, 15),
            date(2026, 6, 20),
            date(2026, 3, 29),
        ];
        let rules = [
            Recurrence::None,
            Recurrence::Daily,
            Recurrence::Weekdays,
            Recurrence::Weekly,
            Recurrence::Monthly,
            Recurrence::Yearly,
        ];
        for seed in seeds {
            for recurrence in rules {
                for interval in 1..=3 {
                    let mut event = sample_event(seed);
                    event.recurrence = recurrence;
                    event.recurrence_interval = interval;
                    // Nothing of a series lies before its first day.
                    for offset in [-366, -7, -1] {
                        let day = seed + jiff::Span::new().days(offset);
                        let row = format!("{recurrence:?} every {interval} from {seed}, {day}");
                        assert_eq!(event.occurrences_before(day), 0, "{row}");
                        assert!(!event.occurs_on(day), "{row}");
                    }
                    let mut matched = 0;
                    for offset in 0..1500 {
                        let day = seed + jiff::Span::new().days(offset);
                        assert_eq!(
                            event.occurrences_before(day),
                            matched,
                            "{recurrence:?} every {interval} from {seed}, before {day}"
                        );
                        if event.occurs_on(day) {
                            matched += 1;
                        }
                    }
                }
            }
        }
    }
}
