//! When a conversation's day changes, and the clock each line carries.
//!
//! One rule, used by both apps: a conversation's lines are ordered by instant;
//! a **day divider** appears before the first line of each local day, and never
//! otherwise; and every line can name its own time. A divider carries the day
//! in the person's language — "Today", "Yesterday", the weekday inside the last
//! week, then the date — so a line from three days ago never reads as "14:32"
//! with no day anywhere near it.
//!
//! The local zone is read once ([`ConversationClock::local`]) and threaded
//! through, so a paint pass cannot straddle two offsets and a test can pin a
//! zone instead of hoping the machine is in one.

use chrono::Datelike as _;
use vello::kurbo::Rect;

use crate::chrome::nav_row_height;
use crate::format;
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;

/// Seconds in a day: the divisor that turns an instant into a day index.
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
    /// The local day `ts` falls on, given the zone's offset from UTC in
    /// seconds east (the value `chrono` reports for "now").
    #[must_use]
    pub fn of(ts: i64, offset: i64) -> Self {
        Self(ts.saturating_add(offset).div_euclid(DAY_SECS))
    }

    /// The day index from the epoch.
    #[must_use]
    pub const fn index(self) -> i64 {
        self.0
    }

    /// The day `days` before this one.
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

/// The local zone and "now", read once per pass.
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

    /// A pinned clock: `offset` seconds east of UTC, as if it were `now`.
    #[must_use]
    pub fn at(offset: i64, now: i64) -> Self {
        Self { offset, now }
    }

    /// The zone's offset from UTC in seconds east.
    #[must_use]
    pub const fn offset(self) -> i64 {
        self.offset
    }

    /// The instant this clock calls now.
    #[must_use]
    pub const fn now(self) -> i64 {
        self.now
    }

    /// The day `ts` falls on.
    #[must_use]
    pub fn day(self, ts: i64) -> ConversationDay {
        ConversationDay::of(ts, self.offset)
    }

    /// Today, as a day index.
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

    /// What a day divider says for `day`: "Today", "Yesterday", the weekday
    /// within the last week, else the date (with the year when it is not this
    /// year's).
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

/// The calendar date of a local day index.
fn date_of(day: ConversationDay) -> Option<chrono::NaiveDate> {
    let secs = day.index().checked_mul(DAY_SECS)?;
    chrono::DateTime::from_timestamp(secs, 0).map(|at| at.date_naive())
}

/// The weekday of a local day index.
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

    /// Forget what came before: the next line starts a day.
    pub fn reset(&mut self) {
        self.previous = None;
    }
}

/// The indices of `days` whose day differs from the one before (the first is
/// always included).
#[must_use]
pub fn day_break_indices(days: impl IntoIterator<Item = ConversationDay>) -> Vec<usize> {
    let mut breaks = DayBreaks::new();
    days.into_iter()
        .enumerate()
        .filter_map(|(index, day)| breaks.begins(day).then_some(index))
        .collect()
}

/// The day divider: a caption centred in a band, ruled out to both sides.
///
/// The band is a list row tall, so a divider sits in the same rhythm as the
/// lines it separates, and the rules stop short of the text at an `s3` gap so
/// the label never touches one.
pub struct DayDividerFace<'a> {
    label: &'a str,
}

impl<'a> DayDividerFace<'a> {
    #[must_use]
    pub fn new(label: &'a str) -> Self {
        Self { label }
    }

    /// The height a divider takes: one list row, at every text scale.
    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        nav_row_height(theme)
    }

    /// Paint the divider across `band`, its rules running from `leading` to
    /// `trailing` (the message column's edges, not the window's).
    pub fn paint(&self, paint: &mut PaintCtx<'_>, band: Rect, leading: f64, trailing: f64) {
        let theme = paint.theme;
        let style = theme.text(TextRole::Caption);
        let gap = f64::from(theme.tokens.spacing.s3);
        let ground = theme.tokens.colors.surfaces.content;
        let width = f64::from(paint.text.measure_style(self.label, None, &style).0);
        let centre = band.center().x;
        let left_edge = centre - width / 2.0 - gap;
        let right_edge = centre + width / 2.0 + gap;
        let rule_y = band.center().y;
        if left_edge > leading {
            crate::surface::separator(
                paint.scene,
                paint.transform,
                leading,
                left_edge,
                rule_y,
                0.0,
                ground,
            );
        }
        if trailing > right_edge {
            crate::surface::separator(
                paint.scene,
                paint.transform,
                right_edge,
                trailing,
                rule_y,
                0.0,
                ground,
            );
        }
        if self.label.is_empty() {
            return;
        }
        // A label wider than the band (a long date at 1.5× in a narrow window)
        // is elided rather than drawn over the column's edges.
        let room = crate::num::f32_of_f64((trailing - leading).max(0.0));
        let shown = paint.text.elide_to_width_style(self.label, room, &style);
        let shown_w = f64::from(paint.text.measure_style(&shown, None, &style).0);
        paint.text_role_in_band(
            &shown,
            TextRole::Caption,
            theme.text_dim,
            centre - shown_w / 2.0,
            band,
            None,
        );
    }
}

/// The height of one line of `role` in `theme` — what a conversation row
/// reserves for its time or its delivery line.
#[must_use]
pub fn line_height(text: &mut TextRenderer, theme: &Theme, role: TextRole) -> f64 {
    f64::from(text.measure_style("Ag", None, &theme.text(role)).1)
}
