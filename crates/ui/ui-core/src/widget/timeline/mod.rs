//! A time grid: the week and day as hours with events at their start and end, and the month as a
//! grid of days with events in it.
//!
//! [`TimelineFace`] and [`MonthFace`] paint the grid and lay its events out; a painted app calls
//! them with the same geometry the retained [`Timeline`] widget draws with. `splits`,
//! `snap_minute` and `block_rect` are the one place overlapping events are given side-by-side
//! columns, a drag is snapped to a slot, and an event's time becomes a rectangle, so every view
//! of a day agrees.
//!
//! Nothing here holds the app's data: a caller hands in [`TimelineItem`]s and gets back the
//! rectangles a press lands on, and the item or day each one means.

#![allow(
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    reason = "the time grid's names read beside their module and are the API tilolang calls"
)]

mod component;
mod face;
mod widget;

#[cfg(test)]
mod tests;

pub use component::{TimelineItemProps, TimelineProps};
pub use face::{MonthFace, MonthReport, TimelineFace, TimelineReport};
pub use widget::Timeline;

use jiff::civil::Date;
use vello::kurbo::Rect;
use vello::peniko::Color;

use tilo_ui_base::{text_render::TextRenderer, text_render::TextStyle};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use crate::widget::TimelineDensity;

/// A quarter hour: the default slot a drag snaps to.
pub const SNAP_MIN: i64 = 15;

pub const MINUTES_PER_DAY: i64 = 24 * 60;

/// What a timeline shows: one day, a week, or a month.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimelineMode {
    Day,
    #[default]
    Week,
    Month,
}

impl TimelineMode {
    /// How many days the hour grid shows (the month is a grid of its own).
    #[must_use]
    pub fn days(self) -> usize {
        match self {
            Self::Day => 1,
            Self::Week | Self::Month => 7,
        }
    }

    /// The mode named by the catalogue's text (`day`, `week`, `month`).
    #[must_use]
    pub fn of(name: &str) -> Self {
        match name {
            "day" => Self::Day,
            "month" => Self::Month,
            _ => Self::Week,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
        }
    }
}

tilo_ui_base::choice!(TimelineMode, "TimelineMode", {
    Day => "day",
    Week => "week",
    Month => "month",
});

/// The colour an item is tinted with, chosen from the theme's categorical series so a calendar's
/// colours follow the workspace's material like every other chart and mark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimelineTone {
    #[default]
    First,
    Second,
    Third,
    Fourth,
    Fifth,
    Sixth,
    Seventh,
    Eighth,
    Neutral,
}

impl TimelineTone {
    #[must_use]
    pub fn color(self, theme: &Theme) -> Color {
        if self == Self::Neutral {
            return theme.tokens.colors.text_tertiary;
        }
        let index = match self {
            Self::First | Self::Neutral => 0,
            Self::Second => 1,
            Self::Third => 2,
            Self::Fourth => 3,
            Self::Fifth => 4,
            Self::Sixth => 5,
            Self::Seventh => 6,
            Self::Eighth => 7,
        };
        tilo_ui_base::chart_palette::ChartPalette::from_theme(theme).categorical(index)
    }
}

tilo_ui_base::choice!(TimelineTone, "TimelineTone", {
    First => "first",
    Second => "second",
    Third => "third",
    Fourth => "fourth",
    Fifth => "fifth",
    Sixth => "sixth",
    Seventh => "seventh",
    Eighth => "eighth",
    Neutral => "neutral",
});

/// Something on the grid, as the grid draws it.
///
/// A day's timed event carries the day it stands on and its start and end as minutes from
/// midnight; an all-day event, and a month event, carry the span of days it covers instead.
#[derive(Clone, Debug, PartialEq)]
pub struct TimelineItem {
    /// The item's stable key.
    pub id: String,
    /// The resource it stands for, when it stands for one: what a press, a move or a resize
    /// carries back to the view.
    pub resource: Option<String>,
    /// What it is called.
    pub title: String,
    /// Where it happens, shown under a timed event's title when the block is tall enough.
    pub location: String,
    /// The colour it picks from the theme's series.
    pub tone: TimelineTone,
    /// The calendar colour it is tinted with, resolved from `tone` on the theme it is drawn on.
    pub color: Color,
    /// The day it starts on (the only day, for a timed event).
    pub day: Date,
    /// The last day it covers: the same as `day` for a timed event.
    pub end_day: Date,
    /// Which column it takes among the events it overlaps, from 1; 0 lays it out by itself.
    pub stack: i64,
    /// Where in the day it starts, in minutes from midnight.
    pub start_min: i64,
    /// Where in the day it ends, in minutes from midnight; past 24 hours for an event that runs over.
    pub end_min: i64,
    /// The wall-clock minute its end reads as, where that differs from `end_min` (an event across a
    /// clock change): what the label says.
    pub clock_end_min: i64,
    /// Whether it takes the whole day (or days) rather than the hours it names.
    pub all_day: bool,
}

impl TimelineItem {
    /// How many days it covers, at least one.
    #[must_use]
    pub fn nights(&self) -> i64 {
        i64::from(
            self.end_day
                .since(self.day)
                .map_or(0, |span| span.get_days()),
        )
        .max(0)
    }

    /// Whether it stands for more than one day.
    #[must_use]
    pub fn spans_days(&self) -> bool {
        self.end_day > self.day
    }
}

/// What a press on the hour grid means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimelineTarget {
    /// A day's header: show that day.
    OpenDay(Date),
    /// A drawn event: select the item with this key.
    Select(String),
}

/// What a press on the month grid means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonthTarget {
    /// The cell's body: move the chosen day here.
    PickDate(Date),
    /// The cell's head or its "more" row: show that day.
    OpenDay(Date),
    /// A drawn event: select the item with this key.
    OpenEvent(String),
}

/// A drag in progress, as the grid previews it before it lands.
#[derive(Clone, Debug)]
pub struct Ghost {
    /// The day column the preview stands in.
    pub column: usize,
    pub start_min: i64,
    pub end_min: i64,
    pub title: String,
    pub fill: Color,
    pub edge: Color,
}

/// The text styles the grid draws with, measured once for a frame.
#[derive(Clone, Debug)]
pub struct TimelineText {
    pub weekday: TextStyle,
    pub day: TextStyle,
    pub strip: TextStyle,
    pub title: TextStyle,
    pub subtitle: TextStyle,
    pub hour: TextStyle,
}

impl TimelineText {
    #[must_use]
    pub fn of(theme: &Theme) -> Self {
        Self {
            weekday: theme.text(TextRole::Caption),
            day: theme.text(TextRole::Title).numeric(),
            strip: theme.text(TextRole::Caption),
            title: theme.text(TextRole::Caption).strong(),
            subtitle: theme.text(TextRole::Caption),
            hour: theme.text(TextRole::Caption).numeric(),
        }
    }
}

/// The measurements a grid needs, read from the theme and its text.
#[derive(Clone, Debug)]
pub struct TimelineTokens {
    pub text: TimelineText,
    pub weekday_h: f64,
    pub day_h: f64,
    pub strip_h: f64,
    pub title_h: f64,
    pub hour_w: f64,
    pub s1: f64,
    pub s2: f64,
    pub s3: f64,
    pub narrow_below: f64,
    pub min_hour_regular: f64,
    pub min_hour_narrow: f64,
    /// How tall an hour is drawn against its legible minimum: the person's zoom.
    pub zoom: f64,
}

impl TimelineTokens {
    #[must_use]
    pub fn of(theme: &Theme, text: &mut TextRenderer) -> Self {
        let styles = TimelineText::of(theme);
        let line = |text: &mut TextRenderer, style: &TextStyle| {
            f64::from(text.measure_style("Ag", None, style).1)
        };
        let weekday_h = line(text, &styles.weekday);
        let day_h = line(text, &styles.day);
        let strip_h = line(text, &styles.strip);
        let title_h = line(text, &styles.title);
        let hour_w = [0, 10, 12, 22]
            .into_iter()
            .map(|hour| f64::from(text.measure_style(&hour_label(hour), None, &styles.hour).0))
            .fold(0.0, f64::max);
        let sp = &theme.tokens.spacing;
        Self {
            text: styles,
            weekday_h,
            day_h,
            strip_h,
            title_h,
            hour_w,
            s1: f64::from(sp.s1),
            s2: f64::from(sp.s2),
            s3: f64::from(sp.s3),
            narrow_below: TimelineDensity::narrow_below(theme),
            min_hour_regular: TimelineDensity::Regular.min_hour_height(theme, title_h),
            min_hour_narrow: TimelineDensity::Narrow.min_hour_height(theme, title_h),
            zoom: 1.0,
        }
    }

    #[must_use]
    pub fn default_dark() -> Self {
        let theme = Theme::dark();
        tilo_ui_base::text_render::with_layout_renderer(&theme, |text| Self::of(&theme, text))
    }
}

impl Default for TimelineTokens {
    fn default() -> Self {
        Self::default_dark()
    }
}

/// The geometry of one grid: where its header, all-day lane and hour rows fall.
#[derive(Clone, Copy, Debug)]
pub struct TimelineGeom {
    pub gutter: f64,
    pub head_h: f64,
    pub allday_h: f64,
    pub allday_rows: usize,
    pub body_top: f64,
    pub body_bottom: f64,
    pub hour_h: f64,
    pub content_h: f64,
    pub grid_right: f64,
    pub density: TimelineDensity,
    pub min_block_h: f64,
    pub block_inset: f64,
}

impl TimelineGeom {
    #[must_use]
    pub fn body(self, x0: f64, x1: f64) -> Rect {
        Rect::new(x0, self.body_top, x1, self.body_bottom)
    }
}

/// The height a day or week grid wants at `width`, so a scroll shows every hour without scaling
/// them: the header, the all-day lane, and 24 legible hours.
#[must_use]
pub fn natural_height(
    tokens: &TimelineTokens,
    width: f64,
    days_len: usize,
    allday_rows: usize,
) -> f64 {
    let (_, _, _, density) = chrome(tokens, width, days_len);
    let min_hour = match density {
        TimelineDensity::Regular => tokens.min_hour_regular,
        TimelineDensity::Narrow => tokens.min_hour_narrow,
    } * tokens.zoom.max(0.1);
    let head = head_h(tokens).ceil();
    let allday = allday_h(tokens, allday_rows, density).ceil();
    head + allday + 24.0 * min_hour + tokens.s2
}

fn chrome(
    tokens: &TimelineTokens,
    width: f64,
    days_len: usize,
) -> (f64, f64, f64, TimelineDensity) {
    let gutter = (tokens.hour_w + 2.0 * tokens.s3).ceil();
    let column = (width - gutter - tokens.s3) / days_len.max(1) as f64;
    let density = if column < tokens.narrow_below {
        TimelineDensity::Narrow
    } else {
        TimelineDensity::Regular
    };
    (gutter, column, tokens.s3, density)
}

fn head_h(tokens: &TimelineTokens) -> f64 {
    2.0 * tokens.s2 + tokens.weekday_h + tokens.day_h
}

fn allday_h(tokens: &TimelineTokens, rows: usize, density: TimelineDensity) -> f64 {
    match rows {
        0 => tokens.s1,
        1 => tokens.strip_h * density.chip_lines() as f64 + 3.0 * tokens.s1,
        rows => {
            let rows = rows as f64;
            rows * (tokens.strip_h + tokens.s1) + 2.0 * tokens.s1
        }
    }
}

/// The geometry of a day or week grid drawn in `area`.
#[must_use]
pub fn geometry(
    area: Rect,
    tokens: &TimelineTokens,
    days_len: usize,
    allday_rows: usize,
) -> TimelineGeom {
    let (gutter, _, _, density) = chrome(tokens, area.width(), days_len);
    let min_hour_h = match density {
        TimelineDensity::Regular => tokens.min_hour_regular,
        TimelineDensity::Narrow => tokens.min_hour_narrow,
    } * tokens.zoom.max(0.1);
    let head_h = head_h(tokens).ceil();
    let allday_h = allday_h(tokens, allday_rows, density).ceil();
    let min_block_h = (tokens.title_h + tokens.s1).ceil();
    let body_top = area.y0 + head_h + allday_h;
    let body_bottom = (area.y1 - tokens.s2).max(body_top + min_hour_h);
    let viewport_h = (body_bottom - body_top).max(min_hour_h);
    let hour_h = (viewport_h / 24.0).max(min_hour_h);
    let content_h = hour_h * 24.0;
    let grid_right = if content_h > viewport_h + 0.5 {
        area.x1 - tokens.s3
    } else {
        area.x1
    };
    TimelineGeom {
        gutter,
        head_h,
        allday_h,
        allday_rows,
        body_top,
        body_bottom,
        hour_h,
        content_h,
        grid_right,
        density,
        min_block_h,
        block_inset: tokens.s1 / 2.0,
    }
}

#[must_use]
pub fn column_width(area: Rect, days_len: usize, geom: TimelineGeom) -> f64 {
    ((geom.grid_right - (area.x0 + geom.gutter)) / days_len.max(1) as f64).max(1.0)
}

#[must_use]
pub fn column_at(area: Rect, days_len: usize, geom: TimelineGeom, x: f64) -> usize {
    let Some(last) = days_len.checked_sub(1) else {
        return 0;
    };
    let grid_x0 = area.x0 + geom.gutter;
    let col_w = column_width(area, days_len, geom);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "column_width floors at 1.0, so the quotient is bounded by the grid width"
    )]
    let col = ((x - grid_x0) / col_w).floor() as i64;
    usize::try_from(col.clamp(0, i64::try_from(last).unwrap_or(i64::MAX))).unwrap_or(0)
}

#[must_use]
pub fn minute_at(geom: TimelineGeom, offset_y: f64, y: f64) -> i64 {
    let content_y = (y - geom.body_top + offset_y).clamp(0.0, geom.content_h);
    (((content_y / geom.hour_h) * 60.0).round() as i64).clamp(0, MINUTES_PER_DAY)
}

/// Where `start_min..end_min` of column `col` is drawn, with the grid scrolled by `offset_y`.
#[must_use]
pub fn block_rect(
    area: Rect,
    days_len: usize,
    geom: TimelineGeom,
    offset_y: f64,
    col: usize,
    start_min: i64,
    end_min: i64,
) -> Rect {
    let col_w = column_width(area, days_len, geom);
    let x0 = area.x0 + geom.gutter + col_w * col as f64;
    let top = geom.body_top + (start_min as f64 / 60.0) * geom.hour_h - offset_y;
    let bottom = geom.body_top + (end_min as f64 / 60.0) * geom.hour_h - offset_y;
    Rect::new(
        x0 + geom.block_inset,
        top,
        x0 + col_w - geom.block_inset,
        bottom.max(top + geom.min_block_h),
    )
}

/// How one event shares a day's width with the events it overlaps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Split {
    pub index: usize,
    pub of: usize,
}

/// The clock `minute` minutes from midnight reads as (`09:00`), in the person's clock: what an
/// event's chip shows and a reader is told.
pub(crate) fn minute_text(minute: i64) -> String {
    let minute = u32::try_from(minute.clamp(0, MINUTES_PER_DAY)).unwrap_or(0);
    tilo_ui_base::clock::hour_minute(minute / 60, minute % 60)
}

/// The same translation from a block to its share of the column, as the app's hit-testing used.
#[must_use]
pub fn in_split(block: Rect, split: Split) -> Rect {
    if split.of <= 1 {
        return block;
    }
    let width = block.width() / split.of as f64;
    let x0 = block.x0 + width * split.index as f64;
    let gap = if split.index + 1 < split.of { 1.0 } else { 0.0 };
    Rect::new(x0, block.y0, x0 + width - gap, block.y1)
}

/// Each interval's share of the width, events that overlap laid side by side in columns.
#[must_use]
pub fn splits(intervals: &[(i64, i64)]) -> Vec<Split> {
    let mut order: Vec<(usize, i64, i64)> = intervals
        .iter()
        .enumerate()
        .map(|(i, &(start, end))| (i, start, end.max(start + 1)))
        .collect();
    order.sort_by_key(|&(_, start, end)| (start, -end));
    let mut placed: Vec<(usize, Split)> = Vec::with_capacity(order.len());
    let mut split_ends: Vec<i64> = Vec::new();
    let mut cluster_start = 0;
    let mut cluster_end = i64::MIN;
    let close = |placed: &mut [(usize, Split)], from: usize, of: usize| {
        for (_, split) in placed.iter_mut().skip(from) {
            split.of = of.max(1);
        }
    };
    for (i, start, end) in order {
        if start >= cluster_end && placed.len() > cluster_start {
            close(&mut placed, cluster_start, split_ends.len());
            cluster_start = placed.len();
            split_ends.clear();
        }
        let index = if let Some((free, slot)) = split_ends
            .iter_mut()
            .enumerate()
            .find(|(_, split_end)| **split_end <= start)
        {
            *slot = end;
            free
        } else {
            split_ends.push(end);
            split_ends.len() - 1
        };
        cluster_end = if placed.len() == cluster_start {
            end
        } else {
            cluster_end.max(end)
        };
        placed.push((i, Split { index, of: 1 }));
    }
    close(&mut placed, cluster_start, split_ends.len());
    placed.sort_by_key(|&(i, _)| i);
    placed.into_iter().map(|(_, split)| split).collect()
}

/// Each interval's share of the width, with a stated column (`stack`, from 1) kept where the
/// view set one.
#[must_use]
pub fn splits_for(intervals: &[(i64, i64)], stacks: &[i64]) -> Vec<Split> {
    let mut out = splits(intervals);
    for (split, &stack) in out.iter_mut().zip(stacks) {
        if stack > 0 {
            #[allow(clippy::cast_sign_loss, reason = "stack is positive here")]
            let index = (stack - 1) as usize;
            split.of = split.of.max(index + 1);
            split.index = index.min(split.of.saturating_sub(1));
        }
    }
    out
}

/// A minute snapped to the nearest `step`-minute slot.
#[must_use]
pub fn snap_minute(minute: i64, step: i64) -> i64 {
    let step = step.max(1);
    ((minute + step / 2) / step * step).clamp(0, MINUTES_PER_DAY)
}

/// `hour` as a person reads a whole hour: "09:00" or "9 AM".
#[must_use]
pub fn hour_label(hour: u32) -> String {
    i8::try_from(hour)
        .ok()
        .and_then(|hour| jiff::civil::Time::new(hour, 0, 0, 0).ok())
        .map(tilo_ui_base::format::time)
        .unwrap_or_default()
}

/// The 42 days a month grid shows, starting on the first day of the week the person's calendar uses.
#[must_use]
pub fn month_grid(anchor: Date) -> Vec<Date> {
    let first = anchor.first_of_month();
    let first_weekday = tilo_ui_base::format::first_weekday();
    let into_week = i64::from(first.weekday().since(first_weekday));
    let start = first - jiff::Span::new().days(into_week);
    (0..42).map(|i| start + jiff::Span::new().days(i)).collect()
}

/// How many rows the all-day lane needs for these items: one for each week's worth of days that
/// hold something all day.
#[must_use]
pub fn all_day_rows(items: &[TimelineItem]) -> usize {
    items
        .iter()
        .filter(|item| item.all_day)
        .map(|item| item.day)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        .div_ceil(7)
        .max(usize::from(items.iter().any(|item| item.all_day)))
}

/// The `day` an item starts on and the minute of its first day, read from the instant it carries.
#[must_use]
pub fn local_day_minute(at: tilo_ui_base::component::Moment) -> (Date, i64) {
    let stamp = jiff::Timestamp::from_second(at.0).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
    let zoned = tilo_ui_base::clock::at(stamp);
    (
        zoned.date(),
        i64::from(zoned.hour()) * 60 + i64::from(zoned.minute()),
    )
}
