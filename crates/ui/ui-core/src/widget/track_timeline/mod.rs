//! A track timeline: a time ruler over rows of tracks, the spans that stand on them, the markers
//! a person set and the playhead, as a video editor, a sound editor or a slide show's timing lays
//! them out.
//!
//! The geometry here is the one place a time becomes an x and a row a y, a tick interval is
//! chosen for a zoom, and a dragged edge finds what it snaps to; the widget draws and hit-tests
//! through it, so what is drawn is what a press lands on.
//!
//! Nothing here holds the app's data: the view hands the tracks, spans and markers in, and a drag
//! reports the change it names as a typed event. The view decides what the change is, and the
//! timeline shows whatever the view hands back, settling each span into its new place.

#![allow(
    clippy::module_name_repetitions,
    reason = "the timeline's names read beside their module and are the API tilolang calls"
)]

mod component;
mod widget;

#[cfg(test)]
mod tests;

pub use component::{
    TimelineMarkerProps, TimelineSpanProps, TimelineTrackProps, TrackTimelineProps,
};
pub use widget::TrackTimeline;

use vello::kurbo::Rect;

/// How close to a span's edge a press grabs that edge rather than the body, in logical pixels.
pub const EDGE_GRAB: f64 = 6.0;

/// How close to an edge, a marker or the playhead a dragged time comes, in logical pixels, before
/// it lines up with it.
pub const SNAP_REACH: f64 = 8.0;

/// The fewest logical pixels between two labelled ticks, so their times never touch.
const LABEL_ROOM: f64 = 72.0;

/// The intervals a ruler labels, shortest first, each with how many parts its unlabelled ticks
/// cut it into: tenths of a second up to two hours.
const STEPS: [(f64, f64); 17] = [
    (0.1, 2.0),
    (0.25, 5.0),
    (0.5, 5.0),
    (1.0, 5.0),
    (2.0, 4.0),
    (5.0, 5.0),
    (10.0, 5.0),
    (15.0, 3.0),
    (30.0, 6.0),
    (60.0, 6.0),
    (120.0, 4.0),
    (300.0, 5.0),
    (600.0, 5.0),
    (900.0, 3.0),
    (1800.0, 6.0),
    (3600.0, 4.0),
    (7200.0, 4.0),
];

/// The interval the ruler labels at `px_per_second`, and the smaller one its unlabelled ticks
/// stand at between them.
#[must_use]
pub fn tick_steps(px_per_second: f64) -> (f64, f64) {
    let (major, parts) = STEPS
        .iter()
        .copied()
        .find(|(step, _)| step * px_per_second >= LABEL_ROOM)
        .unwrap_or(STEPS[STEPS.len() - 1]);
    (major, major / parts)
}

/// A moment on the timeline as its ruler and a reader write it: `1:05` at whole seconds,
/// `1:05.5` when the ruler counts in parts of a second.
#[must_use]
pub fn timecode(seconds: f64, fine: bool) -> String {
    let seconds = seconds.max(0.0);
    let whole = seconds.floor();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a timeline's moment is a few hours of seconds at most, never negative"
    )]
    let base = tilo_ui_base::format::timecode(whole as u64);
    if fine {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a tenth is 0 to 9"
        )]
        let tenth = (((seconds - whole) * 10.0).floor() as u64).min(9);
        format!("{base}.{tenth}")
    } else {
        base
    }
}

/// `t` on the grid of `step` seconds: the frame it is nearest, when the step is a frame. A step of
/// nothing leaves it where it is.
#[must_use]
pub fn on_grid(t: f64, step: f64) -> f64 {
    if step > 0.0 {
        ((t / step).round() * step).max(0.0)
    } else {
        t.max(0.0)
    }
}

/// Which of `targets` lies within `reach` seconds of `t`, the nearest first: what a dragged edge
/// lines up with.
#[must_use]
pub fn snap_target(t: f64, targets: &[f64], reach: f64) -> Option<f64> {
    targets
        .iter()
        .copied()
        .filter(|target| (target - t).abs() <= reach)
        .min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
}

/// Where the timeline's parts fall: the header column, the ruler band, the rows, and the lane the
/// spans stand in, with the time the lane's leading edge shows and how wide a second is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geom {
    pub rect: Rect,
    pub header_w: f64,
    pub ruler_h: f64,
    pub track_h: f64,
    pub tracks: usize,
    /// The time at the lane's leading edge.
    pub scroll: f64,
    /// How many logical pixels one second takes.
    pub px_per_second: f64,
}

impl Geom {
    /// The geometry of a timeline in `rect` showing `span` seconds from `scroll`.
    #[must_use]
    pub fn new(
        rect: Rect,
        header_w: f64,
        ruler_h: f64,
        track_h: f64,
        tracks: usize,
        scroll: f64,
        span: f64,
    ) -> Self {
        let lane_w = (rect.width() - header_w).max(1.0);
        Self {
            rect,
            header_w,
            ruler_h,
            track_h,
            tracks,
            scroll: scroll.max(0.0),
            px_per_second: lane_w / span.max(0.01),
        }
    }

    /// The lane the spans and the ruler's ticks stand in: right of the header column.
    #[must_use]
    pub fn lane(&self) -> Rect {
        Rect::new(
            self.rect.x0 + self.header_w,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1,
        )
    }

    /// The ruler's band, over the lane.
    #[must_use]
    pub fn ruler(&self) -> Rect {
        let lane = self.lane();
        Rect::new(lane.x0, lane.y0, lane.x1, lane.y0 + self.ruler_h)
    }

    /// How many seconds the lane shows.
    #[must_use]
    pub fn span(&self) -> f64 {
        self.lane().width() / self.px_per_second
    }

    #[must_use]
    pub fn x_of(&self, t: f64) -> f64 {
        self.lane().x0 + (t - self.scroll) * self.px_per_second
    }

    #[must_use]
    pub fn time_at(&self, x: f64) -> f64 {
        (self.scroll + (x - self.lane().x0) / self.px_per_second).max(0.0)
    }

    /// `px` logical pixels as seconds at this zoom.
    #[must_use]
    pub fn seconds(&self, px: f64) -> f64 {
        px / self.px_per_second
    }

    /// The whole row of track `row`: its header and its lane.
    #[must_use]
    pub fn row(&self, row: usize) -> Rect {
        #[allow(
            clippy::cast_precision_loss,
            reason = "a timeline has a handful of tracks"
        )]
        let y0 = self.rect.y0 + self.ruler_h + self.track_h * row as f64;
        Rect::new(self.rect.x0, y0, self.rect.x1, y0 + self.track_h)
    }

    /// The header of track `row`: its name and the toggles its view offers.
    #[must_use]
    pub fn header(&self, row: usize) -> Rect {
        let row = self.row(row);
        Rect::new(row.x0, row.y0, row.x0 + self.header_w, row.y1)
    }

    /// The box a span from `start` lasting `length` takes on row `row`, inset from the row's lines.
    #[must_use]
    pub fn span_rect(&self, row: usize, start: f64, length: f64, inset: f64) -> Rect {
        let band = self.row(row);
        let x0 = self.x_of(start);
        let x1 = self.x_of(start + length).max(x0 + 2.0);
        Rect::new(x0, band.y0 + inset, x1, band.y1 - inset)
    }

    /// The row a `y` lands on, clamped to the rows there are; none above the rows.
    #[must_use]
    pub fn row_at(&self, y: f64) -> Option<usize> {
        if self.tracks == 0 || y < self.rect.y0 + self.ruler_h {
            return None;
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the row is non-negative and small"
        )]
        let row = ((y - self.rect.y0 - self.ruler_h) / self.track_h)
            .floor()
            .max(0.0) as usize;
        Some(row.min(self.tracks - 1))
    }

    /// How tall the whole timeline is: the ruler and every row.
    #[must_use]
    pub fn height(ruler_h: f64, track_h: f64, tracks: usize) -> f64 {
        #[allow(
            clippy::cast_precision_loss,
            reason = "a timeline has a handful of tracks"
        )]
        let rows = tracks.max(1) as f64;
        ruler_h + track_h * rows
    }
}
