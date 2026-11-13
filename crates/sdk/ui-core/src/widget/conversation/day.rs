//! When a conversation's day changes, and the clock each line carries.

use chrono::Datelike as _;

use crate::format;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;

const DAY_SECS: i64 = 86_400;

/// How many days back a divider names by its weekday rather than by a date.
/// A week is as far as "Tuesday" is unambiguous to read; past it, a date.
const WEEKDAY_SPAN: i64 = 7;

/// The local calendar day an instant falls on.
///
/// A day is an index from the epoch of local days, so two instants on the same
/// local day are equal and a divider is drawn exactly when this changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConversationDay(i64);

impl ConversationDay {
    #[must_use]
    pub fn of(ts: i64, offset: i64) -> Self {
        Self(ts.saturating_add(offset).div_euclid(DAY_SECS))
    }

    #[must_use]
    pub const fn index(self) -> i64 {
        self.0
    }

    #[must_use]
    pub fn days_back(self, days: i64) -> Self {
        Self(self.0.saturating_sub(days))
    }

    /// How many days `self` is after `earlier` (0 when the same day).
    #[must_use]
    pub fn days_after(self, earlier: Self) -> i64 {
        self.0.saturating_sub(earlier.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConversationClock {
    offset: i64,
    now: i64,
}

impl ConversationClock {
    /// This device's zone, at this moment. The only place the local zone is
    /// read; everything else takes the clock.
    #[must_use]
    pub fn local() -> Self {
        let now = crate::clock::local_now();
        Self {
            offset: i64::from(now.offset().local_minus_utc()),
            now: now.timestamp(),
        }
    }

    #[must_use]
    pub fn at(offset: i64, now: i64) -> Self {
        Self { offset, now }
    }

    #[must_use]
    pub const fn offset(self) -> i64 {
        self.offset
    }

    #[must_use]
    pub const fn now(self) -> i64 {
        self.now
    }

    #[must_use]
    pub fn day(self, ts: i64) -> ConversationDay {
        ConversationDay::of(ts, self.offset)
    }

    #[must_use]
    pub fn today(self) -> ConversationDay {
        self.day(self.now)
    }

    /// The time of day `ts` names, as the person reads clocks in this
    /// workspace (24- or 12-hour).
    #[must_use]
    pub fn clock(self, ts: i64) -> String {
        let in_day = ts.saturating_add(self.offset).rem_euclid(DAY_SECS);
        crate::clock::hour_minute(
            crate::num::u32_of_i64(in_day.div_euclid(3_600)),
            crate::num::u32_of_i64(in_day.rem_euclid(3_600).div_euclid(60)),
        )
    }

    #[must_use]
    pub fn day_label(self, day: ConversationDay) -> String {
        let age = self.today().days_after(day);
        if let Ok(within_a_week) = u32::try_from(age)
            && within_a_week < u32::try_from(WEEKDAY_SPAN).unwrap_or(7)
        {
            if let Some(near) = format::near_day(-age) {
                return near;
            }
            if let Some(weekday) = weekday_of(day) {
                return format::weekday_name(weekday);
            }
        }
        let Some(date) = date_of(day) else {
            return day.index().to_string();
        };
        let today = date_of(self.today());
        if today.is_some_and(|today| today.year() == date.year()) {
            format::month_day(date)
        } else {
            format::month_day_year(date)
        }
    }

    /// `ts` as a date and a time: what an accessible name or a tooltip says
    /// when the divider above is not in view ("Tue, Jun 30, 14:05").
    #[must_use]
    pub fn stamp(self, ts: i64) -> String {
        let day = self.day(ts);
        let Some(date) = date_of(day) else {
            return self.clock(ts);
        };
        format!("{} {}", format::day_heading_short(date), self.clock(ts))
    }
}

fn date_of(day: ConversationDay) -> Option<chrono::NaiveDate> {
    let secs = day.index().checked_mul(DAY_SECS)?;
    chrono::DateTime::from_timestamp(secs, 0).map(|at| at.date_naive())
}

fn weekday_of(day: ConversationDay) -> Option<chrono::Weekday> {
    date_of(day).map(|date| date.weekday())
}

/// Walks a conversation's lines and says which of them start a new day.
///
/// This is the whole of the day-divider rule: the first line always starts
/// one, and so does any line whose day differs from the line before it — so a
/// divider appears wherever the person has scrolled to, not only at the top of
/// what happens to be loaded.
#[derive(Clone, Copy, Debug, Default)]
pub struct DayBreaks {
    previous: Option<ConversationDay>,
}

impl DayBreaks {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a line on `day` starts a new day, in order.
    pub fn begins(&mut self, day: ConversationDay) -> bool {
        let begins = self.previous != Some(day);
        self.previous = Some(day);
        begins
    }

    pub fn reset(&mut self) {
        self.previous = None;
    }
}

#[must_use]
pub fn day_break_indices(days: impl IntoIterator<Item = ConversationDay>) -> Vec<usize> {
    let mut breaks = DayBreaks::new();
    days.into_iter()
        .enumerate()
        .filter_map(|(index, day)| breaks.begins(day).then_some(index))
        .collect()
}

#[must_use]
pub fn line_height(text: &mut TextRenderer, theme: &Theme, role: TextRole) -> f64 {
    f64::from(text.measure_style("Ag", None, &theme.text(role)).1)
}
