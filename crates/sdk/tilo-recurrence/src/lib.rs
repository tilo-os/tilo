//! RFC 5545 recurrence rules, shared by every iCalendar consumer.
//!
//! A calendar event, a repeating task and a snooze control all ask the same
//! questions of a stored rule: which dates does it produce, which is the next
//! one, and what instant does an authored wall time mean in the series' own
//! zone. This crate answers them from the text RFC 5545 already stores, so
//! Calendar, the language runtime and any later app share one expansion
//! instead of each growing its own.
//!
//! Two types share the work:
//!
//! * [`Rule`] is one parsed rule — `FREQ`, `INTERVAL`, `BYDAY`,
//!   `BYMONTHDAY`, `UNTIL`, `COUNT` and `EXDATE` — and expands it from a seed
//!   date (RFC 5545's `DTSTART`).
//! * [`Repeat`] is the small preset set a picker cycles through ("Every
//!   week", …) and the canonical rule text for each.
//!
//! Occurrences keep their authored wall time: [`zone`] resolves a wall time to
//! an instant with one cross-device policy — a fold takes the earlier instant,
//! a gap advances to the first valid minute — so a 09:00 series stays at
//! 09:00 across a daylight-saving change and still lands on the right minute
//! on every device.
//!
//! Stored spellings are format: [`Rule::to_rrule`] and [`Repeat::to_rrule`]
//! write the same text the documents already hold, so adopting this crate
//! never rewrites a saved rule.

pub mod zone;

use std::fmt::Write as _;

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, Utc, Weekday};

pub use zone::{Resolution, canonical_zone, normalize_wall, wall_to_utc};

/// A rule this crate accepts produces a date within any 400-year span (the
/// Gregorian leap cycle), so a search that walks further has run past the end
/// of the series. It only bounds unbounded searches; window expansion is
/// bounded by the window the caller asked for.
const SEARCH_DAYS: i64 = 146_100;

#[cfg(test)]
thread_local! {
    /// The days [`Rule::occurrences`] has stepped through on this thread, so
    /// its cost can be pinned to the window it was asked for.
    static DAYS_WALKED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// One `FREQ` value this crate expands. RFC 5545's sub-daily frequencies are
/// refused rather than approximated: a calendar occurrence is a date.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Freq {
    /// The RFC 5545 spelling, without the `FREQ=` name.
    #[must_use]
    pub fn as_rrule(self) -> &'static str {
        match self {
            Self::Daily => "DAILY",
            Self::Weekly => "WEEKLY",
            Self::Monthly => "MONTHLY",
            Self::Yearly => "YEARLY",
        }
    }

    /// Parse one `FREQ` value, case-insensitively. `None` for a frequency this
    /// crate cannot expand.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "DAILY" => Some(Self::Daily),
            "WEEKLY" => Some(Self::Weekly),
            "MONTHLY" => Some(Self::Monthly),
            "YEARLY" => Some(Self::Yearly),
            _ => None,
        }
    }
}

/// One parsed recurrence rule.
///
/// The seed date is not part of the rule text — RFC 5545 keeps it in
/// `DTSTART` — so every expansion takes it as an argument. That is also what
/// lets one parsed rule serve a series whose seed later moves.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rule {
    freq: Freq,
    interval: u32,
    by_day: Vec<Weekday>,
    by_month_day: Vec<i8>,
    until: Option<NaiveDate>,
    count: Option<u32>,
    exdates: Vec<NaiveDate>,
}

impl Rule {
    /// A rule that fires every `freq` from the seed, with no limits.
    #[must_use]
    pub fn new(freq: Freq) -> Self {
        Self {
            freq,
            interval: 1,
            by_day: Vec::new(),
            by_month_day: Vec::new(),
            until: None,
            count: None,
            exdates: Vec::new(),
        }
    }

    /// Parse the text [`Rule::to_text`] writes: the `RRULE` body, optionally
    /// with the `RRULE:` prefix, plus an `EXDATE=` list when the set excludes
    /// dates.
    ///
    /// Returns `None` when a part is present that this crate cannot expand
    /// exactly (`SECONDLY`, `BYSETPOS`, an ordinal `BYDAY` such as `-1FR`, …).
    /// Refusing is deliberate: silently dropping a limit the author wrote
    /// would show occurrences the rule does not have.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let body = strip_rrule_prefix(text.trim());
        if body.is_empty() {
            return None;
        }
        let mut freq = None;
        let mut interval = 1;
        let mut by_day = Vec::new();
        let mut by_month_day = Vec::new();
        let mut until = None;
        let mut count = None;
        let mut exdates = Vec::new();
        for part in body.split(';').filter(|part| !part.trim().is_empty()) {
            let (name, value) = part.split_once('=')?;
            match name.trim().to_ascii_uppercase().as_str() {
                "FREQ" => freq = Some(Freq::parse(value)?),
                "INTERVAL" => interval = parse_positive(value)?,
                "BYDAY" => {
                    by_day = value
                        .split(',')
                        .map(|code| parse_weekday(code.trim()))
                        .collect::<Option<Vec<_>>>()?;
                }
                "BYMONTHDAY" => {
                    by_month_day = value
                        .split(',')
                        .map(|day| parse_month_day(day.trim()))
                        .collect::<Option<Vec<_>>>()?;
                }
                "UNTIL" => until = Some(parse_cal_date(value)?),
                "COUNT" => count = Some(parse_positive(value)?),
                "EXDATE" => {
                    exdates = value
                        .split(',')
                        .map(|date| parse_cal_date(date.trim()))
                        .collect::<Option<Vec<_>>>()?;
                }
                _ => return None,
            }
        }
        Some(
            Self::new(freq?)
                .with_interval(interval)
                .with_by_day(by_day)
                .with_month_days(by_month_day)
                .with_until(until)
                .with_count(count)
                .with_exdates(exdates),
        )
    }

    /// The `RRULE` body (no `RRULE:` prefix): `FREQ`, `INTERVAL`, `BYDAY`,
    /// `BYMONTHDAY`, `UNTIL`, `COUNT`, in that order.
    ///
    /// `EXDATE` is a content line of its own in RFC 5545, so this writes the
    /// rule alone; [`Rule::to_text`] writes the excluded dates with it.
    #[must_use]
    pub fn to_rrule(&self) -> String {
        let mut out = format!("FREQ={}", self.freq.as_rrule());
        if self.interval > 1 {
            let _ = write!(out, ";INTERVAL={}", self.interval);
        }
        if !self.by_day.is_empty() {
            let codes: Vec<&str> = self.by_day.iter().map(|day| weekday_code(*day)).collect();
            let _ = write!(out, ";BYDAY={}", codes.join(","));
        }
        if !self.by_month_day.is_empty() {
            let days: Vec<String> = self.by_month_day.iter().map(i8::to_string).collect();
            let _ = write!(out, ";BYMONTHDAY={}", days.join(","));
        }
        if let Some(until) = self.until {
            let _ = write!(out, ";UNTIL={}", until.format("%Y%m%d"));
        }
        if let Some(count) = self.count {
            let _ = write!(out, ";COUNT={count}");
        }
        out
    }

    /// One string carrying the whole rule set, including exclusions: the
    /// [`Rule::to_rrule`] body followed by `;EXDATE=YYYYMMDD,…`.
    ///
    /// RFC 5545 stores excluded dates in their own `EXDATE` content line.
    /// Consumers that keep a rule as a single value (a language-level repeat
    /// type, a settings field) would otherwise have two strings to keep in
    /// step, so this compact form round-trips through [`Rule::parse`].
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = self.to_rrule();
        if !self.exdates.is_empty() {
            let dates: Vec<String> = self
                .exdates
                .iter()
                .map(|date| date.format("%Y%m%d").to_string())
                .collect();
            let _ = write!(out, ";EXDATE={}", dates.join(","));
        }
        out
    }

    #[must_use]
    pub fn freq(&self) -> Freq {
        self.freq
    }

    /// Positive step between firings; values below 1 are treated as 1.
    #[must_use]
    pub fn interval(&self) -> u32 {
        self.interval.max(1)
    }

    /// Weekdays a `WEEKLY` rule fires on, in week order. Empty means the
    /// seed's own weekday.
    #[must_use]
    pub fn by_day(&self) -> &[Weekday] {
        &self.by_day
    }

    /// Days of the month a `MONTHLY` or `YEARLY` rule fires on. Empty means
    /// the seed's own day; negative values count back from the month's end.
    #[must_use]
    pub fn by_month_day(&self) -> &[i8] {
        &self.by_month_day
    }

    /// Inclusive last date of the series.
    #[must_use]
    pub fn until(&self) -> Option<NaiveDate> {
        self.until
    }

    /// How many firings the series has, counting the seed.
    #[must_use]
    pub fn count(&self) -> Option<u32> {
        self.count
    }

    /// Dates dropped from the series. They still count towards
    /// [`Rule::count`], which is how RFC 5545 ranges a series before
    /// exclusions are applied.
    #[must_use]
    pub fn exdates(&self) -> &[NaiveDate] {
        &self.exdates
    }

    #[must_use]
    pub fn with_interval(mut self, interval: u32) -> Self {
        self.interval = interval.max(1);
        self
    }

    #[must_use]
    pub fn with_by_day(mut self, days: impl IntoIterator<Item = Weekday>) -> Self {
        self.by_day = canonical_days(days);
        self
    }

    #[must_use]
    pub fn with_month_days(mut self, days: impl IntoIterator<Item = i8>) -> Self {
        let mut days: Vec<i8> = days
            .into_iter()
            .filter(|day| (1..=31).contains(day) || (-31..=-1).contains(day))
            .collect();
        days.sort_unstable();
        days.dedup();
        self.by_month_day = days;
        self
    }

    #[must_use]
    pub fn with_until(mut self, until: Option<NaiveDate>) -> Self {
        self.until = until;
        self
    }

    #[must_use]
    pub fn with_count(mut self, count: Option<u32>) -> Self {
        self.count = count.filter(|count| *count > 0);
        self
    }

    #[must_use]
    pub fn with_exdates(mut self, exdates: impl IntoIterator<Item = NaiveDate>) -> Self {
        let mut exdates: Vec<NaiveDate> = exdates.into_iter().collect();
        exdates.sort_unstable();
        exdates.dedup();
        self.exdates = exdates;
        self
    }

    /// Whether `date` is one of the original dates this rule produces from
    /// `seed`, ignoring `UNTIL`, `COUNT` and exclusions. `date` must not be
    /// before `seed`.
    #[must_use]
    pub fn matches(&self, seed: NaiveDate, date: NaiveDate) -> bool {
        let interval = i64::from(self.interval());
        match self.freq {
            Freq::Daily => {
                date.signed_duration_since(seed)
                    .num_days()
                    .rem_euclid(interval)
                    == 0
                    && self.day_selected(seed, date)
            }
            Freq::Weekly => {
                self.week_selected(seed, date)
                    && if self.by_day.is_empty() {
                        date.weekday() == seed.weekday()
                    } else {
                        self.by_day.contains(&date.weekday())
                    }
                    && self.day_selected(seed, date)
            }
            Freq::Monthly => {
                months_between(seed, date).rem_euclid(interval) == 0
                    && self.day_selected(seed, date)
            }
            Freq::Yearly => {
                i64::from(date.year())
                    .saturating_sub(i64::from(seed.year()))
                    .rem_euclid(interval)
                    == 0
                    && date.month() == seed.month()
                    && self.day_selected(seed, date)
            }
        }
    }

    /// The rule's dates within `[from, to]`, ascending, without duplicates.
    ///
    /// `UNTIL` and `COUNT` bound the series, exclusions remove dates from it.
    /// A rule that runs out before `from` yields nothing.
    #[must_use]
    pub fn occurrences(&self, seed: NaiveDate, from: NaiveDate, to: NaiveDate) -> Vec<NaiveDate> {
        let last = self.until.map_or(to, |until| until.min(to));
        if last < from {
            return Vec::new();
        }
        // The walk covers the window, not the series: a counted rule needs the
        // ordinal of the window's first date, which `count_before` works out
        // without stepping through the years before it.
        let mut date = from.max(seed);
        let mut ordinal = if self.count.is_some() {
            u32::try_from(self.count_before(seed, date)).unwrap_or(u32::MAX)
        } else {
            0
        };
        let mut out = Vec::new();
        while date <= last {
            #[cfg(test)]
            DAYS_WALKED.with(|walked| walked.set(walked.get().saturating_add(1)));
            if self.matches(seed, date) {
                ordinal = ordinal.saturating_add(1);
                if self.count.is_some_and(|count| ordinal > count) {
                    break;
                }
                if date >= from && !self.exdates.contains(&date) {
                    out.push(date);
                }
            }
            let Some(next) = date.checked_add_signed(Duration::days(1)) else {
                break;
            };
            date = next;
        }
        out
    }

    /// The first date of the series on or after `date`, or `None` once the
    /// series has ended. Excluded dates are stepped over.
    #[must_use]
    pub fn next_on_or_after(&self, seed: NaiveDate, date: NaiveDate) -> Option<NaiveDate> {
        let mut current = date.max(seed);
        let mut ordinal = if self.count.is_some() {
            u32::try_from(self.count_before(seed, current)).unwrap_or(u32::MAX)
        } else {
            0
        };
        for _ in 0..SEARCH_DAYS {
            if self.until.is_some_and(|until| current > until) {
                return None;
            }
            if self.matches(seed, current) {
                ordinal = ordinal.saturating_add(1);
                if self.count.is_some_and(|count| ordinal > count) {
                    return None;
                }
                if current >= date && !self.exdates.contains(&current) {
                    return Some(current);
                }
            }
            current = current.checked_add_signed(Duration::days(1))?;
        }
        None
    }

    /// The first date of the series after `after`.
    #[must_use]
    pub fn next_after(&self, seed: NaiveDate, after: NaiveDate) -> Option<NaiveDate> {
        self.next_on_or_after(seed, after.checked_add_signed(Duration::days(1))?)
    }

    /// The rule's occurrences for a timed series that starts at `seed`: each
    /// occurrence's authored wall time plus, for a named zone, the instant it
    /// denotes.
    ///
    /// The wall time is the same clock time on every date, which is what keeps
    /// a 09:00 series at 09:00 after a daylight-saving change. `floating` and
    /// unknown zones have no instant.
    #[must_use]
    pub fn timed_occurrences(
        &self,
        seed: NaiveDateTime,
        zone: &str,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Vec<TimedOccurrence> {
        let time = seed.time();
        self.occurrences(seed.date(), from, to)
            .into_iter()
            .map(|date| {
                let wall = date.and_time(time);
                TimedOccurrence {
                    wall,
                    instant: zone::wall_to_utc(zone, wall).map(|(instant, _)| instant),
                }
            })
            .collect()
    }

    /// How many original dates the rule produces before `date`, counting the
    /// seed: exclusions and edits aside, as `COUNT` counts them.
    ///
    /// Arithmetic for the presets a picker writes, so a counted series stays
    /// as cheap to show as an unbounded one however old it is; a rule with
    /// `BYDAY` or `BYMONTHDAY` filters of its own steps through the dates.
    #[must_use]
    pub fn count_before(&self, seed: NaiveDate, date: NaiveDate) -> u64 {
        if date <= seed {
            return 0;
        }
        let interval = i64::from(self.interval());
        // The dates before `date` are the seed plus every `interval`th step
        // below it, so their count is the distance rounded up.
        let steps_before = |steps: i64| u64::try_from(ceil_div(steps, interval)).unwrap_or(0);
        if !self.by_day.is_empty() || !self.by_month_day.is_empty() {
            return self.count_before_by_walking(seed, date);
        }
        match self.freq {
            Freq::Daily => steps_before(date.signed_duration_since(seed).num_days()),
            Freq::Weekly => u64::try_from(ceil_div(
                date.signed_duration_since(seed).num_days(),
                interval.saturating_mul(7),
            ))
            .unwrap_or(0),
            Freq::Monthly => months_before(seed, date, interval),
            Freq::Yearly => years_before(seed, date, interval),
        }
    }

    /// The dates of a filtered rule, stepped day by day. Only rules the
    /// presets never write reach this: their filters do not divide into the
    /// frequency's steps.
    fn count_before_by_walking(&self, seed: NaiveDate, date: NaiveDate) -> u64 {
        let mut count = 0_u64;
        let mut day = seed;
        while day < date {
            if self.matches(seed, day) {
                count = count.saturating_add(1);
            }
            let Some(next) = day.checked_add_signed(Duration::days(1)) else {
                break;
            };
            day = next;
        }
        count
    }

    /// Whether the rule's day filters accept `date`: an explicit list wins,
    /// and without one only a monthly or yearly rule is pinned to the seed's
    /// day of the month — a daily or weekly one fires on every selected day.
    fn day_selected(&self, seed: NaiveDate, date: NaiveDate) -> bool {
        if !self.by_day.is_empty() && !self.by_day.contains(&date.weekday()) {
            return false;
        }
        if self.by_month_day.is_empty() {
            return !matches!(self.freq, Freq::Monthly | Freq::Yearly) || date.day() == seed.day();
        }
        let day_of_month = i64::from(date.day());
        let span = i64::from(days_in_month(date));
        self.by_month_day.iter().any(|entry| {
            let wanted = i64::from(*entry);
            if wanted > 0 {
                day_of_month == wanted
            } else {
                day_of_month == span.saturating_add(wanted).saturating_add(1)
            }
        })
    }

    /// Whether `date` falls in a counted week of a `WEEKLY` rule. Weeks start
    /// on Monday (RFC 5545's default `WKST`), and the seed's own week is the
    /// first: a rule the seed itself starts mid-week keeps counting from that
    /// week, so every later week is whole.
    fn week_selected(&self, seed: NaiveDate, date: NaiveDate) -> bool {
        let lead = i64::from(seed.weekday().num_days_from_monday());
        let Some(week_start) = seed.checked_sub_signed(Duration::days(lead)) else {
            return false;
        };
        date.signed_duration_since(week_start)
            .num_days()
            .div_euclid(7)
            .rem_euclid(i64::from(self.interval()))
            == 0
    }
}

/// [`Rule::count_before`] for yearly rules, which skip the years that have no
/// 29 February.
fn years_before(seed: NaiveDate, date: NaiveDate, interval: i64) -> u64 {
    let years = i64::from(date.year()).saturating_sub(i64::from(seed.year()));
    if (seed.month(), seed.day()) != (2, 29) {
        let reached = years.saturating_add(i64::from(
            (date.month(), date.day()) > (seed.month(), seed.day()),
        ));
        return u64::try_from(ceil_div(reached, interval)).unwrap_or(0);
    }
    let mut count = 0_u64;
    let mut offset = 0_i64;
    while offset <= years {
        let year = seed.year().checked_add(i32::try_from(offset).unwrap_or(0));
        if let Some(candidate) = year.and_then(|year| NaiveDate::from_ymd_opt(year, 2, 29)) {
            if candidate >= date {
                break;
            }
            count = count.saturating_add(1);
        }
        offset = offset.saturating_add(interval);
    }
    count
}

/// [`Rule::count_before`] for monthly rules, which skip the months that have
/// no such day.
fn months_before(seed: NaiveDate, date: NaiveDate, interval: i64) -> u64 {
    let months = months_between(seed, date);
    if seed.day() <= 28 {
        let reached = months.saturating_add(i64::from(date.day() > seed.day()));
        return u64::try_from(ceil_div(reached, interval)).unwrap_or(0);
    }
    let mut count = 0_u64;
    let mut offset = 0_i64;
    while offset <= months {
        let total = i64::from(seed.month0()).saturating_add(offset);
        let year = seed
            .year()
            .checked_add(i32::try_from(total.div_euclid(12)).unwrap_or(0));
        let month = u32::try_from(total.rem_euclid(12))
            .unwrap_or(0)
            .saturating_add(1);
        if let Some(candidate) =
            year.and_then(|year| NaiveDate::from_ymd_opt(year, month, seed.day()))
        {
            if candidate >= date {
                break;
            }
            count = count.saturating_add(1);
        }
        offset = offset.saturating_add(interval);
    }
    count
}

/// One occurrence of a timed series: the authored wall time and, in a named
/// zone, the absolute instant it denotes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TimedOccurrence {
    pub wall: NaiveDateTime,
    pub instant: Option<DateTime<Utc>>,
}

/// The preset set a repeat picker cycles through, and the canonical rule text
/// for each. Reading it back from arbitrary rule text is lossy on purpose:
/// [`Repeat::from_rrule`] names the preset that best describes a stored rule,
/// while [`Rule::parse`] keeps every part of it.
///
/// A repeat is a resource value, so its variants' stored names are format:
/// adding one is a document change, not a rename.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, tilo_resource::Model)]
pub enum Repeat {
    /// A single occurrence.
    #[default]
    None,
    Daily,
    Weekdays,
    Weekly,
    Monthly,
    Yearly,
}

impl Repeat {
    /// The full phrase a menu or row shows.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "Does not repeat",
            Self::Daily => "Every day",
            Self::Weekdays => "Every weekday",
            Self::Weekly => "Every week",
            Self::Monthly => "Every month",
            Self::Yearly => "Every year",
        }
    }

    /// The compact fact shown beside a row's date.
    #[must_use]
    pub fn short(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Daily => "Daily",
            Self::Weekdays => "Weekdays",
            Self::Weekly => "Weekly",
            Self::Monthly => "Monthly",
            Self::Yearly => "Yearly",
        }
    }

    /// RFC 5545 rule text (no `RRULE:` prefix), or empty for a single
    /// occurrence.
    #[must_use]
    pub fn to_rrule(self) -> String {
        match self {
            Self::None => String::new(),
            Self::Daily => "FREQ=DAILY".into(),
            Self::Weekdays => "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".into(),
            Self::Weekly => "FREQ=WEEKLY".into(),
            Self::Monthly => "FREQ=MONTHLY".into(),
            Self::Yearly => "FREQ=YEARLY".into(),
        }
    }

    /// The preset that best describes stored rule text, or [`Repeat::None`]
    /// when it repeats on no schedule a preset names.
    #[must_use]
    pub fn from_rrule(text: &str) -> Self {
        let up = text.to_uppercase();
        if up.contains("FREQ=DAILY") {
            Self::Daily
        } else if up.contains("BYDAY=MO,TU,WE,TH,FR") {
            Self::Weekdays
        } else if up.contains("FREQ=WEEKLY") {
            Self::Weekly
        } else if up.contains("FREQ=MONTHLY") {
            Self::Monthly
        } else if up.contains("FREQ=YEARLY") {
            Self::Yearly
        } else {
            Self::None
        }
    }

    /// The next preset in the picker's cycle.
    #[must_use]
    pub fn cycle(self) -> Self {
        match self {
            Self::None => Self::Daily,
            Self::Daily => Self::Weekdays,
            Self::Weekdays => Self::Weekly,
            Self::Weekly => Self::Monthly,
            Self::Monthly => Self::Yearly,
            Self::Yearly => Self::None,
        }
    }

    /// The expansion rule for this preset, with the series' limits. `None`
    /// for a single occurrence.
    #[must_use]
    pub fn rule(
        self,
        interval: u32,
        until: Option<NaiveDate>,
        count: Option<u32>,
        exdates: &[NaiveDate],
    ) -> Option<Rule> {
        let freq = match self {
            Self::None => return None,
            Self::Daily => Freq::Daily,
            Self::Weekdays | Self::Weekly => Freq::Weekly,
            Self::Monthly => Freq::Monthly,
            Self::Yearly => Freq::Yearly,
        };
        let rule = Rule::new(freq)
            .with_interval(interval)
            .with_until(until)
            .with_count(count)
            .with_exdates(exdates.to_vec());
        Some(if self == Self::Weekdays {
            rule.with_by_day([
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ])
        } else {
            rule
        })
    }
}

fn strip_rrule_prefix(text: &str) -> &str {
    text.split_once(':')
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("RRULE"))
        .map_or(text, |(_, body)| body.trim())
}

fn parse_positive(value: &str) -> Option<u32> {
    let parsed: u32 = value.trim().parse().ok()?;
    (parsed > 0).then_some(parsed)
}

fn parse_cal_date(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    let compact = value
        .get(..8)
        .filter(|head| head.bytes().all(|byte| byte.is_ascii_digit()));
    match compact {
        Some(head) => NaiveDate::parse_from_str(head, "%Y%m%d").ok(),
        None => NaiveDate::parse_from_str(value, "%Y-%m-%d").ok(),
    }
}

fn parse_month_day(value: &str) -> Option<i8> {
    let day: i8 = value.parse().ok()?;
    ((1..=31).contains(&day) || (-31..=-1).contains(&day)).then_some(day)
}

fn parse_weekday(code: &str) -> Option<Weekday> {
    match code.to_ascii_uppercase().as_str() {
        "MO" => Some(Weekday::Mon),
        "TU" => Some(Weekday::Tue),
        "WE" => Some(Weekday::Wed),
        "TH" => Some(Weekday::Thu),
        "FR" => Some(Weekday::Fri),
        "SA" => Some(Weekday::Sat),
        "SU" => Some(Weekday::Sun),
        _ => None,
    }
}

fn weekday_code(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

fn canonical_days(days: impl IntoIterator<Item = Weekday>) -> Vec<Weekday> {
    let mut days: Vec<Weekday> = days.into_iter().collect();
    days.sort_by_key(Weekday::num_days_from_monday);
    days.dedup();
    days
}

/// `steps / step` rounded up, for a positive `step`.
fn ceil_div(steps: i64, step: i64) -> i64 {
    steps
        .saturating_add(step.saturating_sub(1))
        .div_euclid(step)
}

fn months_between(seed: NaiveDate, date: NaiveDate) -> i64 {
    i64::from(date.year())
        .saturating_sub(i64::from(seed.year()))
        .saturating_mul(12)
        .saturating_add(i64::from(date.month()).saturating_sub(i64::from(seed.month())))
}

/// Days in `date`'s month.
fn days_in_month(date: NaiveDate) -> u32 {
    const DAYS: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let base = DAYS
        .get(usize::try_from(date.month0()).unwrap_or(0))
        .copied()
        .unwrap_or(31);
    if date.month() == 2 && is_leap_year(date.year()) {
        29
    } else {
        base
    }
}

fn is_leap_year(year: i32) -> bool {
    let year = i64::from(year);
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> NaiveDateTime {
        date(year, month, day).and_hms_opt(hour, minute, 0).unwrap()
    }

    fn parse(text: &str) -> Rule {
        Rule::parse(text).unwrap()
    }

    #[test]
    fn rule_text_round_trips_every_part_it_accepts() {
        // The presets write the rule text documents already store.
        assert_eq!(Repeat::None.to_rrule(), "");
        assert_eq!(Repeat::Daily.to_rrule(), "FREQ=DAILY");
        assert_eq!(
            Repeat::Weekdays.to_rrule(),
            "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"
        );
        assert_eq!(Repeat::Weekly.to_rrule(), "FREQ=WEEKLY");
        assert_eq!(Repeat::Monthly.to_rrule(), "FREQ=MONTHLY");
        assert_eq!(Repeat::Yearly.to_rrule(), "FREQ=YEARLY");
        for repeat in [
            Repeat::Daily,
            Repeat::Weekdays,
            Repeat::Weekly,
            Repeat::Monthly,
            Repeat::Yearly,
        ] {
            assert_eq!(Repeat::from_rrule(&repeat.to_rrule()), repeat);
        }

        // Every part survives: names, limits and exclusions in one string.
        let text = "FREQ=MONTHLY;INTERVAL=2;BYDAY=MO,FR;BYMONTHDAY=1,-1;\
                    UNTIL=20261231;COUNT=7;EXDATE=20260201,20260301";
        let rule = parse(text);
        assert_eq!(rule.freq(), Freq::Monthly);
        assert_eq!(rule.interval(), 2);
        assert_eq!(rule.by_day(), [Weekday::Mon, Weekday::Fri]);
        assert_eq!(rule.by_month_day(), [-1, 1]);
        assert_eq!(rule.until(), Some(date(2026, 12, 31)));
        assert_eq!(rule.count(), Some(7));
        assert_eq!(rule.exdates(), [date(2026, 2, 1), date(2026, 3, 1)]);
        let written = rule.to_text();
        assert_eq!(
            written,
            "FREQ=MONTHLY;INTERVAL=2;BYDAY=MO,FR;BYMONTHDAY=-1,1;\
             UNTIL=20261231;COUNT=7;EXDATE=20260201,20260301"
        );
        assert_eq!(parse(&written), rule);
        assert_eq!(parse(&format!("RRULE:{written}")), rule);
        // The rule body on its own carries no exclusions.
        assert_eq!(parse(&rule.to_rrule()).exdates(), [] as [NaiveDate; 0]);

        // A part this crate cannot expand exactly is refused, not dropped.
        assert!(Rule::parse("FREQ=HOURLY").is_none());
        assert!(Rule::parse("FREQ=WEEKLY;BYDAY=-1FR").is_none());
        assert!(Rule::parse("FREQ=WEEKLY;BYSETPOS=1").is_none());
        assert!(Rule::parse("INTERVAL=2").is_none());
        assert!(Rule::parse("FREQ=DAILY;COUNT=0").is_none());
        assert!(Rule::parse("").is_none());
    }

    #[test]
    fn next_after_steps_each_frequency() {
        // Daily, and every third day.
        let daily = Repeat::Daily.rule(1, None, None, &[]).unwrap();
        assert_eq!(
            daily.next_after(date(2026, 6, 15), date(2026, 6, 15)),
            Some(date(2026, 6, 16))
        );
        let third = Repeat::Daily.rule(3, None, None, &[]).unwrap();
        assert_eq!(
            third.next_after(date(2026, 6, 15), date(2026, 6, 15)),
            Some(date(2026, 6, 18))
        );
        assert_eq!(
            third.next_after(date(2026, 6, 15), date(2026, 6, 20)),
            Some(date(2026, 6, 21))
        );

        // Weekly keeps the seed's weekday; weekdays skip the weekend.
        let weekly = Repeat::Weekly.rule(1, None, None, &[]).unwrap();
        assert_eq!(
            weekly.next_after(date(2026, 6, 15), date(2026, 6, 15)),
            Some(date(2026, 6, 22))
        );
        let weekdays = Repeat::Weekdays.rule(1, None, None, &[]).unwrap();
        assert_eq!(
            weekdays.next_after(date(2026, 6, 15), date(2026, 6, 19)),
            Some(date(2026, 6, 22))
        );

        // Monthly on the 31st skips the months that have no 31st.
        let monthly = Repeat::Monthly.rule(1, None, None, &[]).unwrap();
        assert_eq!(
            monthly.occurrences(date(2026, 1, 31), date(2026, 1, 1), date(2026, 7, 31)),
            vec![
                date(2026, 1, 31),
                date(2026, 3, 31),
                date(2026, 5, 31),
                date(2026, 7, 31),
            ]
        );
        assert_eq!(
            monthly.next_after(date(2026, 1, 31), date(2026, 1, 31)),
            Some(date(2026, 3, 31))
        );
        // A counted monthly series keeps the same day-from-the-end.
        let last_day = parse("FREQ=MONTHLY;BYMONTHDAY=-1");
        assert_eq!(
            last_day.occurrences(date(2026, 1, 31), date(2026, 2, 1), date(2026, 3, 31)),
            vec![date(2026, 2, 28), date(2026, 3, 31)]
        );

        // Yearly keeps month and day; 29 February only exists in leap years.
        let yearly = Repeat::Yearly.rule(1, None, None, &[]).unwrap();
        assert_eq!(
            yearly.next_after(date(2024, 2, 29), date(2024, 2, 29)),
            Some(date(2028, 2, 29))
        );
        assert_eq!(
            yearly.next_after(date(2024, 2, 29), date(2027, 1, 1)),
            Some(date(2028, 2, 29))
        );
        assert_eq!(
            yearly.occurrences(date(2024, 6, 1), date(2024, 1, 1), date(2026, 12, 31)),
            vec![date(2024, 6, 1), date(2025, 6, 1), date(2026, 6, 1)]
        );
    }

    #[test]
    fn weekday_weeks_start_on_monday_and_respect_interval() {
        let weekdays = Repeat::Weekdays.rule(1, None, None, &[]).unwrap();
        assert_eq!(
            weekdays.occurrences(date(2026, 6, 15), date(2026, 6, 15), date(2026, 6, 21)),
            vec![
                date(2026, 6, 15),
                date(2026, 6, 16),
                date(2026, 6, 17),
                date(2026, 6, 18),
                date(2026, 6, 19),
            ]
        );

        // Every other week, seeded mid-week: the rest of the seed's week still
        // belongs to the first week, and every later week is whole.
        let alternate = Repeat::Weekdays.rule(2, None, None, &[]).unwrap();
        let seed = date(2026, 6, 17); // Wednesday
        assert_eq!(
            alternate.occurrences(seed, seed, date(2026, 7, 1)),
            vec![
                date(2026, 6, 17),
                date(2026, 6, 18),
                date(2026, 6, 19),
                date(2026, 6, 29),
                date(2026, 6, 30),
                date(2026, 7, 1),
            ]
        );

        // A weekend seed starts the weekdays on the following Monday.
        let weekend_seed = date(2026, 6, 20); // Saturday
        assert_eq!(
            weekdays.next_after(weekend_seed, weekend_seed),
            Some(date(2026, 6, 22))
        );
    }

    #[test]
    fn count_until_and_exclusions_bound_the_series() {
        let counted = Repeat::Daily.rule(2, None, Some(4), &[]).unwrap();
        assert_eq!(
            counted.occurrences(date(2026, 6, 1), date(2026, 6, 1), date(2026, 6, 30)),
            vec![
                date(2026, 6, 1),
                date(2026, 6, 3),
                date(2026, 6, 5),
                date(2026, 6, 7),
            ]
        );
        assert_eq!(counted.next_after(date(2026, 6, 1), date(2026, 6, 7)), None);

        // Excluded dates still spend their place in the count.
        let excluded = Repeat::Daily
            .rule(2, None, Some(4), &[date(2026, 6, 3)])
            .unwrap();
        assert_eq!(
            excluded.occurrences(date(2026, 6, 1), date(2026, 6, 1), date(2026, 6, 30)),
            vec![date(2026, 6, 1), date(2026, 6, 5), date(2026, 6, 7)]
        );
        assert_eq!(
            excluded.next_after(date(2026, 6, 1), date(2026, 6, 5)),
            Some(date(2026, 6, 7))
        );

        let until = Repeat::Daily
            .rule(1, Some(date(2026, 6, 5)), None, &[])
            .unwrap();
        assert_eq!(
            until.occurrences(date(2026, 6, 1), date(2026, 6, 1), date(2026, 6, 30)),
            vec![
                date(2026, 6, 1),
                date(2026, 6, 2),
                date(2026, 6, 3),
                date(2026, 6, 4),
                date(2026, 6, 5),
            ]
        );
        assert_eq!(until.next_after(date(2026, 6, 1), date(2026, 6, 5)), None);
    }

    #[test]
    fn count_before_matches_the_dates_the_rule_walks() {
        // A counted series must know how many dates came before the window
        // without walking them; this checks the arithmetic against the walk.
        let cases = [
            ("FREQ=DAILY", date(2026, 6, 1)),
            ("FREQ=DAILY;INTERVAL=3", date(2026, 6, 1)),
            ("FREQ=WEEKLY", date(2026, 6, 17)),
            ("FREQ=WEEKLY;INTERVAL=2", date(2026, 6, 17)),
            ("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR", date(2026, 6, 17)),
            ("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE,FR", date(2026, 6, 17)),
            ("FREQ=WEEKLY;INTERVAL=3;BYDAY=SA,SU", date(2026, 6, 20)),
            ("FREQ=MONTHLY", date(2026, 1, 15)),
            ("FREQ=MONTHLY", date(2026, 1, 31)),
            ("FREQ=MONTHLY;INTERVAL=2", date(2026, 1, 31)),
            ("FREQ=MONTHLY;BYMONTHDAY=-1", date(2026, 1, 31)),
            ("FREQ=MONTHLY;BYDAY=MO", date(2026, 1, 5)),
            ("FREQ=YEARLY", date(2026, 6, 1)),
            ("FREQ=YEARLY", date(2024, 2, 29)),
            ("FREQ=YEARLY;INTERVAL=2", date(2024, 2, 29)),
            ("FREQ=DAILY;BYDAY=MO", date(2026, 6, 1)),
        ];
        for (text, seed) in cases {
            let rule = parse(text);
            for offset in [0, 1, 6, 7, 40, 400, 1_500] {
                let probe = seed + Duration::days(offset);
                let walked = rule
                    .occurrences(seed, seed, probe - Duration::days(1))
                    .len();
                assert_eq!(
                    rule.count_before(seed, probe),
                    u64::try_from(walked).unwrap(),
                    "{text} from {seed} before {probe}"
                );
            }
        }
    }

    /// A month view asks each shown day for its occurrences; a counted series
    /// begun decades ago must cost the window it shows, not its age.
    #[test]
    fn a_counted_series_costs_its_window_not_its_age() {
        let weekly = Repeat::Weekly.rule(1, None, Some(10_000), &[]).unwrap();
        let grid = |offset: i64| date(2026, 9, 28) + Duration::days(offset);
        let shown_and_walked = |seed: NaiveDate| {
            DAYS_WALKED.with(|walked| walked.set(0));
            let shown: Vec<NaiveDate> = (0..42)
                .map(grid)
                .filter(|day| weekly.occurrences(seed, *day, *day).contains(day))
                .collect();
            (shown, DAYS_WALKED.with(std::cell::Cell::get))
        };
        // Mondays both: 1 June 2026 and 3 June 1996.
        let (young, young_cost) = shown_and_walked(date(2026, 6, 1));
        let (old, old_cost) = shown_and_walked(date(1996, 6, 3));
        assert_eq!(young.len(), 6);
        assert_eq!(old, young);
        assert_eq!(old_cost, young_cost);
        assert_eq!(young_cost, 42, "one day stepped per day asked");
    }

    #[test]
    fn occurrence_instants_keep_the_authored_wall_time_across_dst() {
        // Berlin springs forward on 2026-03-29: 09:00 stays 09:00 local, so
        // its instant moves from 08:00Z to 07:00Z.
        let weekly = Repeat::Weekly.rule(1, None, None, &[]).unwrap();
        let occurrences = weekly.timed_occurrences(
            at(2026, 3, 22, 9, 0),
            "Europe/Berlin",
            date(2026, 3, 22),
            date(2026, 4, 5),
        );
        assert_eq!(
            occurrences.iter().map(|o| o.wall).collect::<Vec<_>>(),
            vec![
                at(2026, 3, 22, 9, 0),
                at(2026, 3, 29, 9, 0),
                at(2026, 4, 5, 9, 0),
            ]
        );
        assert_eq!(
            occurrences
                .iter()
                .filter_map(|o| o.instant)
                .map(|instant| instant.naive_utc())
                .collect::<Vec<_>>(),
            vec![
                at(2026, 3, 22, 8, 0),
                at(2026, 3, 29, 7, 0),
                at(2026, 4, 5, 7, 0),
            ]
        );

        // Berlin falls back on 2026-10-25: the repeated 02:30 takes the
        // earlier instant, and the wall time never moves.
        let daily = Repeat::Daily.rule(1, None, None, &[]).unwrap();
        let occurrences = daily.timed_occurrences(
            at(2026, 10, 24, 2, 30),
            "Europe/Berlin",
            date(2026, 10, 24),
            date(2026, 10, 26),
        );
        assert!(
            occurrences
                .iter()
                .all(|occurrence| occurrence.wall.time() == at(2026, 1, 1, 2, 30).time())
        );
        assert_eq!(
            occurrences
                .iter()
                .filter_map(|o| o.instant)
                .map(|instant| instant.naive_utc())
                .collect::<Vec<_>>(),
            vec![
                at(2026, 10, 24, 0, 30),
                at(2026, 10, 25, 0, 30),
                at(2026, 10, 26, 1, 30),
            ]
        );

        // A floating series has wall times but no instant.
        let floating = daily.timed_occurrences(
            at(2026, 6, 1, 9, 0),
            "floating",
            date(2026, 6, 1),
            date(2026, 6, 3),
        );
        assert_eq!(floating.len(), 3);
        assert!(
            floating
                .iter()
                .all(|occurrence| occurrence.instant.is_none())
        );
    }
}
