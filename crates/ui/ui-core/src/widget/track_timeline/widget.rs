//! [`TrackTimeline`], the retained widget: draws the ruler, the tracks, their spans, the markers
//! and the playhead, and turns a press, a drag, a wheel or a key into the typed event that names
//! the change. It never changes what it shows itself: a span the person let go of is drawn where
//! they let go until the view hands back where it stands now, and then settles there.

use std::collections::HashMap;

use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Affine, BezPath, Line, Point, Rect, RoundedRect, Vec2};
use vello::peniko::{Color, Fill, ImageBrush};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::component::{Event, Value};
use crate::icons::Icon;
use crate::motion::{Direction, MotionValue};
use crate::paint::{PaintCtx, with_alpha_scaled};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::{MotionSource, TextRole};
use crate::widget::scrollbar::{PressOutcome, ScrollMetrics, Scrollbar};
use crate::widget::{Ink, TaffyTreeExt, Widget};

use super::{EDGE_GRAB, Geom, SNAP_REACH, on_grid, snap_target, tick_steps, timecode};

const DRAG_THRESHOLD: f64 = crate::dnd::DRAG_START_THRESHOLD_PX;

/// The shortest a trim leaves a span when the view gives no grid: a tenth of a second.
const LEAST_LENGTH: f64 = 0.1;

/// How far the zoom goes: half a second across the lane at the closest, four hours at the widest.
const ZOOM_RANGE: (f64, f64) = (0.5, 4.0 * 3600.0);

/// How long a dropped span waits where it was let go for the view to say where it stands.
const ANSWER_WAIT_MS: u64 = 400;

/// One row of the timeline, as the view describes it.
pub(crate) struct Track {
    pub key: String,
    pub label: String,
    pub muted: bool,
    pub solo: bool,
    pub locked: bool,
    pub toggled_mute: Event,
    pub toggled_solo: Event,
    pub toggled_lock: Event,
    pub spans: Vec<Span>,
}

/// One span on a track, as the view describes it.
pub(crate) struct Span {
    pub key: String,
    pub label: String,
    pub start: f64,
    pub length: f64,
    pub tone: Ink,
    pub selected: bool,
    pub frames: Vec<String>,
    pub peaks: Vec<f64>,
    pub keys: Vec<f64>,
    pub lead_in: f64,
    pub pressed: Event,
    pub moved: Event,
    pub trimmed: Event,
}

/// One marker on the ruler, as the view describes it.
pub(crate) struct Marker {
    pub key: String,
    pub label: String,
    pub at: f64,
    pub tone: Ink,
    pub pressed: Event,
    pub moved: Event,
}

/// The toggles a track's header can offer, in the order they stand from its trailing edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Toggle {
    Lock,
    Solo,
    Mute,
}

impl Toggle {
    const ALL: [Self; 3] = [Self::Lock, Self::Solo, Self::Mute];

    fn event(self, track: &Track) -> &Event {
        match self {
            Self::Mute => &track.toggled_mute,
            Self::Solo => &track.toggled_solo,
            Self::Lock => &track.toggled_lock,
        }
    }

    fn on(self, track: &Track) -> bool {
        match self {
            Self::Mute => track.muted,
            Self::Solo => track.solo,
            Self::Lock => track.locked,
        }
    }

    fn icon(self, on: bool) -> Icon {
        match (self, on) {
            (Self::Mute, true) => Icon::VolumeX,
            (Self::Mute, false) => Icon::Volume2,
            (Self::Solo, _) => Icon::Headphones,
            (Self::Lock, true) => Icon::Lock,
            (Self::Lock, false) => Icon::LockOpen,
        }
    }

    fn words(self) -> &'static str {
        match self {
            Self::Mute => "timeline-mute",
            Self::Solo => "timeline-solo",
            Self::Lock => "timeline-lock",
        }
    }
}

/// What part of a span a press took hold of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grab {
    Body,
    Head,
    Tail,
}

/// What a press took hold of.
#[derive(Clone, Debug, PartialEq)]
enum Hold {
    /// The ruler or the playhead: the playhead follows the hand.
    Playhead,
    /// A span, by its track and its place on it, and how far into it the hand is in seconds.
    Span {
        track: usize,
        index: usize,
        grab: Grab,
        offset: f64,
    },
    Marker {
        index: usize,
        offset: f64,
    },
    Toggle {
        track: usize,
        toggle: Toggle,
    },
    /// A track's empty stretch.
    Empty {
        track: usize,
    },
    /// The bar under the tracks that scrolls the lane along the time.
    Scrollbar,
}

#[derive(Clone, Debug)]
struct Drag {
    hold: Hold,
    origin: Point,
    current: Point,
    active: bool,
}

/// Where what is held lands if the hand lets go now, and what it lined up with.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Landing {
    Playhead {
        at: f64,
    },
    Span {
        track: usize,
        index: usize,
        to_track: usize,
        start: f64,
        length: f64,
    },
    Marker {
        index: usize,
        at: f64,
    },
}

/// How one span is drawn: where, from which moment, how chosen it looks, and its state.
#[derive(Clone, Copy, Debug)]
struct Look {
    rect: Rect,
    start: f64,
    lift: f64,
    focused: bool,
    faded: bool,
    locked: bool,
}

/// What has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Playhead,
    Span { track: usize, index: usize },
}

/// A ruler over tracks with spans, markers and a playhead; see the module.
pub struct TrackTimeline {
    pub(crate) label: Option<String>,
    pub(crate) span: f64,
    pub(crate) scroll: f64,
    pub(crate) playhead: f64,
    pub(crate) length: f64,
    /// The moments a measurement found a problem at, marked where they happened.
    pub(crate) clips: Vec<f64>,
    pub(crate) step: f64,
    pub(crate) tracks: Vec<Track>,
    pub(crate) markers: Vec<Marker>,
    pub(crate) scrub: Event,
    pub(crate) seek: Event,
    pub(crate) zoomed: Event,
    pub(crate) scrolled: Event,
    pub(crate) press: Event,
    /// Where each span is drawn, by its key: its start then its length, settling toward where its
    /// props put it.
    shown: HashMap<String, MotionValue<Vec2>>,
    /// How chosen each span looks, by its key, from 0 to 1.
    lift: HashMap<String, MotionValue<f64>>,
    theme: Option<Theme>,
    rect: Rect,
    dirty: bool,
    pointer: Point,
    modifiers: ModifiersState,
    drag: Option<Drag>,
    focus: Option<Focus>,
    /// The bar under the tracks that scrolls the lane along the time: the shared scroll model,
    /// in seconds rather than pixels.
    bar: Scrollbar,
    /// The spans let go of, by their keys, held where they were dropped until the view says
    /// where they stand, or until each one's moment passes with no answer: then they glide back.
    pending: Vec<(String, crate::time::Instant)>,
}

impl TrackTimeline {
    pub(crate) fn new(tracks: Vec<Track>, markers: Vec<Marker>) -> Self {
        let mut timeline = Self {
            label: None,
            span: 20.0,
            scroll: 0.0,
            playhead: 0.0,
            length: 0.0,
            clips: Vec::new(),
            step: 0.0,
            tracks: Vec::new(),
            markers: Vec::new(),
            scrub: Event::None,
            seek: Event::None,
            zoomed: Event::None,
            scrolled: Event::None,
            press: Event::None,
            shown: HashMap::new(),
            lift: HashMap::new(),
            theme: None,
            rect: Rect::ZERO,
            dirty: true,
            pointer: Point::ZERO,
            modifiers: ModifiersState::empty(),
            drag: None,
            focus: None,
            bar: Scrollbar::horizontal(),
            pending: Vec::new(),
        };
        timeline.retarget(tracks, markers);
        timeline
    }

    /// Take what the view hands in now: a span whose place changed glides from where it was drawn
    /// to its new one, a span newly chosen rises, and one no longer there is forgotten.
    pub(crate) fn retarget(&mut self, tracks: Vec<Track>, markers: Vec<Marker>) {
        let now = crate::motion::frame_now();
        let theme = self.theme.clone();
        let timing = |spec: crate::motion::MotionSpec| {
            theme
                .as_ref()
                .map(|theme| spec.timing(Direction::Enter, theme.motion(), MotionSource::Pointer))
        };
        let mut seen = std::collections::HashSet::new();
        for span in tracks.iter().flat_map(|track| &track.spans) {
            seen.insert(span.key.clone());
            let place = Vec2::new(span.start, span.length);
            match self.shown.get_mut(&span.key) {
                Some(shown) if shown.target() != place => {
                    match timing(crate::motion::ListChange::MOVE) {
                        Some(timing) => shown.set(place, timing, now),
                        None => shown.snap(place),
                    }
                }
                Some(_) => {}
                None => {
                    self.shown.insert(span.key.clone(), MotionValue::new(place));
                }
            }
            let chosen = if span.selected { 1.0 } else { 0.0 };
            match self.lift.get_mut(&span.key) {
                Some(lift) if (lift.target() - chosen).abs() > f64::EPSILON => {
                    match timing(crate::motion::ControlChange::SHOW) {
                        Some(timing) => lift.set(chosen, timing, now),
                        None => lift.snap(chosen),
                    }
                }
                Some(_) => {}
                None => {
                    self.lift.insert(span.key.clone(), MotionValue::new(chosen));
                }
            }
        }
        self.shown.retain(|key, _| seen.contains(key));
        // The view answered: a span it kept where it was dropped stays, one it refused glides
        // back above.
        self.pending.clear();
        self.lift.retain(|key, _| seen.contains(key));
        // A drag holds a span by its place: one the view took away or moved under the hand ends it.
        if let Some(Drag {
            hold: Hold::Span { track, index, .. },
            ..
        }) = &self.drag
        {
            let held = self
                .tracks
                .get(*track)
                .and_then(|t| t.spans.get(*index))
                .map(|s| s.key.clone());
            let still = held.is_some_and(|key| {
                tracks
                    .get(*track)
                    .and_then(|t| t.spans.get(*index))
                    .is_some_and(|s| s.key == key)
            });
            if !still {
                self.drag = None;
            }
        }
        self.tracks = tracks;
        self.markers = markers;
        if let Some(Focus::Span { track, index }) = self.focus
            && self
                .tracks
                .get(track)
                .and_then(|t| t.spans.get(index))
                .is_none()
        {
            self.focus = Some(Focus::Playhead);
        }
        self.dirty = true;
    }

    fn geom(&self) -> Geom {
        let (header_w, ruler_h, track_h) = self.metrics();
        Geom::new(
            self.rect,
            header_w,
            ruler_h,
            track_h,
            self.tracks.len(),
            self.scroll,
            self.span,
        )
    }

    /// The header column's width, the ruler's height and a track's height, from the theme.
    fn metrics(&self) -> (f64, f64, f64) {
        let theme = self.theme.clone().unwrap_or_else(Theme::dark);
        let sp = &theme.tokens.spacing;
        let caption = f64::from(theme.text(TextRole::Caption).size);
        let ruler_h = caption * 1.4 + f64::from(sp.s2) * 2.0 + 6.0;
        let track_h = (caption * 1.4 * 3.0 + f64::from(sp.s2) * 2.0).max(44.0);
        let header_w = 132.0_f64.max(f64::from(sp.s16) * 2.0);
        (header_w, ruler_h, track_h)
    }

    fn theme(&self) -> Theme {
        self.theme.clone().unwrap_or_else(Theme::dark)
    }

    /// Where span `index` of track `track` is drawn now: its start and its length.
    fn drawn_place(&self, span: &Span) -> (f64, f64) {
        let now = crate::motion::frame_now();
        self.shown
            .get(&span.key)
            .map_or((span.start, span.length), |shown| {
                let place = shown.get(now);
                (place.x, place.y)
            })
    }

    fn least_length(&self) -> f64 {
        if self.step > 0.0 {
            self.step
        } else {
            LEAST_LENGTH
        }
    }

    /// The one step a key moves something by: the grid's, or a tenth of a second without one.
    fn key_step(&self, big: bool) -> f64 {
        let one = if self.step > 0.0 { self.step } else { 0.1 };
        if big { one * 10.0 } else { one }
    }

    /// What a dragged moment lines up with: the start, the playhead, the markers and every
    /// span's edges, leaving out the span held.
    fn targets(&self, except: Option<(usize, usize)>, playhead: bool) -> Vec<f64> {
        let mut targets = vec![0.0];
        if playhead {
            targets.push(self.playhead);
        }
        targets.extend(self.markers.iter().map(|m| m.at));
        for (t, track) in self.tracks.iter().enumerate() {
            for (i, span) in track.spans.iter().enumerate() {
                if except == Some((t, i)) {
                    continue;
                }
                targets.push(span.start);
                targets.push(span.start + span.length);
            }
        }
        targets
    }

    /// `t` lined up with the nearest of `targets` within reach, else on the grid.
    fn snapped(&self, t: f64, targets: &[f64], geom: &Geom) -> f64 {
        snap_target(t, targets, geom.seconds(SNAP_REACH)).unwrap_or_else(|| on_grid(t, self.step))
    }

    fn landing(&self, drag: &Drag) -> Option<Landing> {
        let geom = self.geom();
        let at = geom.time_at(drag.current.x);
        match drag.hold {
            Hold::Playhead => {
                let targets = self.targets(None, false);
                Some(Landing::Playhead {
                    at: self.snapped(at, &targets, &geom),
                })
            }
            Hold::Marker { index, offset } => {
                let mut targets = self.targets(None, true);
                if let Some(own) = self.markers.get(index).map(|m| m.at) {
                    targets.retain(|t| (t - own).abs() > f64::EPSILON);
                }
                Some(Landing::Marker {
                    index,
                    at: self.snapped(at - offset, &targets, &geom),
                })
            }
            Hold::Span {
                track,
                index,
                grab,
                offset,
            } => {
                let span = self.tracks.get(track)?.spans.get(index)?;
                let targets = self.targets(Some((track, index)), true);
                let reach = geom.seconds(SNAP_REACH);
                let end = span.start + span.length;
                let (to_track, start, length) = match grab {
                    Grab::Body => {
                        let raw = (at - offset).max(0.0);
                        let by_start = snap_target(raw, &targets, reach).map(|s| (s - raw, s));
                        let by_end = snap_target(raw + span.length, &targets, reach)
                            .map(|e| (e - raw - span.length, e - span.length));
                        let start = match (by_start, by_end) {
                            (Some(a), Some(b)) => {
                                if a.0.abs() <= b.0.abs() {
                                    a.1
                                } else {
                                    b.1
                                }
                            }
                            (Some(a), None) => a.1,
                            (None, Some(b)) => b.1,
                            (None, None) => on_grid(raw, self.step),
                        };
                        let to_track = geom
                            .row_at(drag.current.y)
                            .filter(|row| self.tracks.get(*row).is_some_and(|t| !t.locked))
                            .unwrap_or(track);
                        (to_track, start.max(0.0), span.length)
                    }
                    Grab::Head => {
                        let start = self
                            .snapped(at - offset, &targets, &geom)
                            .min(end - self.least_length())
                            .max(0.0);
                        (track, start, end - start)
                    }
                    Grab::Tail => {
                        let end = self
                            .snapped(at - offset, &targets, &geom)
                            .max(span.start + self.least_length());
                        (track, span.start, end - span.start)
                    }
                };
                Some(Landing::Span {
                    track,
                    index,
                    to_track,
                    start,
                    length,
                })
            }
            Hold::Toggle { .. } | Hold::Empty { .. } | Hold::Scrollbar => None,
        }
    }

    /// Whether the lane shows less than what is on the tracks runs.
    fn scrollable(&self) -> bool {
        self.length > self.span + 0.5
    }

    /// How tall the band under the tracks is: the scroll bar's gutter, when there is one.
    fn scrollbar_h(&self) -> f64 {
        if self.scrollable() {
            f64::from(self.theme().tokens.metrics.scrollbar_gutter)
        } else {
            0.0
        }
    }

    /// The band under the tracks the bar lies in, and the track within it: the thumb's lane
    /// along the time. `None` when the lane shows everything.
    fn scrollbar(&self) -> Option<(Rect, Rect)> {
        if !self.scrollable() {
            return None;
        }
        let geom = self.geom();
        let gutter = self.scrollbar_h();
        let rows_bottom =
            geom.rect.y0 + geom.ruler_h + geom.track_h * crate::num::f64_of_usize(geom.tracks);
        let y0 = rows_bottom.min(self.rect.y1 - gutter).max(geom.rect.y0);
        if y0 + gutter > self.rect.y1 + 0.5 {
            return None;
        }
        let area = Rect::new(geom.lane().x0, y0, geom.rect.x1, y0 + gutter);
        Some((area, self.bar.track_in(area, &self.theme())))
    }

    /// The bar's scroll geometry: the whole cut along the lane, what the lane shows, and where
    /// it stands.
    fn scroll_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(self.length.max(self.span), self.span, self.scroll)
    }

    /// The moment the playhead is drawn at: where the hand holds it while it is dragged.
    fn shown_playhead(&self) -> f64 {
        match self
            .drag
            .as_ref()
            .filter(|d| d.active)
            .and_then(|d| self.landing(d))
        {
            Some(Landing::Playhead { at }) => at,
            _ => self.playhead,
        }
    }

    /// The rectangle a header toggle stands in, when the view offers it.
    fn toggle_rect(&self, geom: &Geom, row: usize, toggle: Toggle) -> Option<Rect> {
        let track = self.tracks.get(row)?;
        if toggle.event(track).is_none() {
            return None;
        }
        let header = geom.header(row);
        let side = 22.0;
        let gap = 2.0;
        let mut x1 = header.x1 - 6.0;
        for each in Toggle::ALL {
            if each.event(track).is_none() {
                continue;
            }
            if each == toggle {
                let y0 = header.y1 - side - 6.0;
                return Some(Rect::new(x1 - side, y0, x1, y0 + side));
            }
            x1 -= side + gap;
        }
        None
    }

    fn marker_flag(&self, geom: &Geom, marker: &Marker) -> Rect {
        let x = geom.x_of(marker.at);
        let ruler = geom.ruler();
        Rect::new(x - 5.0, ruler.y0 + 2.0, x + 5.0, ruler.y0 + 14.0)
    }

    /// What a press at `point` takes hold of.
    fn hold_at(&self, point: Point) -> Option<Hold> {
        let geom = self.geom();
        if !self.rect.contains(point) {
            return None;
        }
        let lane = geom.lane();
        let ruler = geom.ruler();
        if point.x < lane.x0 {
            let row = geom.row_at(point.y)?;
            return Toggle::ALL.into_iter().find_map(|toggle| {
                self.toggle_rect(&geom, row, toggle)
                    .filter(|rect| rect.contains(point))
                    .map(|_| Hold::Toggle { track: row, toggle })
            });
        }
        if point.y < ruler.y1 {
            for (index, marker) in self.markers.iter().enumerate().rev() {
                let flag = self.marker_flag(&geom, marker).inflate(3.0, 3.0);
                if flag.contains(point) {
                    return Some(Hold::Marker {
                        index,
                        offset: geom.time_at(point.x) - marker.at,
                    });
                }
            }
            return Some(Hold::Playhead);
        }
        if (point.x - geom.x_of(self.playhead)).abs() <= 3.0 {
            return Some(Hold::Playhead);
        }
        let row = geom.row_at(point.y)?;
        let track = self.tracks.get(row)?;
        let at = geom.time_at(point.x);
        let inset = self.span_inset();
        for (index, span) in track.spans.iter().enumerate().rev() {
            let rect = geom.span_rect(row, span.start, span.length, inset);
            if !rect.contains(point) {
                continue;
            }
            let edge = EDGE_GRAB.min(rect.width() / 3.0);
            let grab = if point.x - rect.x0 <= edge {
                Grab::Head
            } else if rect.x1 - point.x <= edge {
                Grab::Tail
            } else {
                Grab::Body
            };
            let offset = match grab {
                Grab::Body | Grab::Head => at - span.start,
                Grab::Tail => at - (span.start + span.length),
            };
            return Some(Hold::Span {
                track: row,
                index,
                grab,
                offset,
            });
        }
        Some(Hold::Empty { track: row })
    }

    fn span_inset(&self) -> f64 {
        f64::from(self.theme().tokens.spacing.s1).max(2.0)
    }

    fn begin(&mut self, point: Point) -> bool {
        if let Some((area, track)) = self.scrollbar() {
            let metrics = self.scroll_metrics();
            match self.bar.press(track, metrics, point) {
                PressOutcome::Thumb => {
                    self.drag = Some(Drag {
                        hold: Hold::Scrollbar,
                        origin: point,
                        current: point,
                        active: true,
                    });
                    self.dirty = true;
                    return true;
                }
                PressOutcome::Page(offset) => {
                    self.scroll_to(offset);
                    return true;
                }
                // The band around the thumb is the bar's, not the last track's.
                PressOutcome::Miss if area.contains(point) => return true,
                PressOutcome::Miss => {}
            }
        }
        let Some(hold) = self.hold_at(point) else {
            return false;
        };
        // The playhead follows the hand from the press on: a press on the ruler is a seek.
        let active = hold == Hold::Playhead;
        self.drag = Some(Drag {
            hold,
            origin: point,
            current: point,
            active,
        });
        if active {
            self.report_scrub();
        }
        self.dirty = true;
        true
    }

    fn update_drag(&mut self, point: Point) {
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        drag.current = point;
        if drag.hold == Hold::Scrollbar {
            self.drag_bar(point);
            return;
        }
        let movable = match drag.hold {
            Hold::Span { track, .. } => self.tracks.get(track).is_some_and(|t| !t.locked),
            Hold::Marker { .. } | Hold::Playhead => true,
            Hold::Toggle { .. } | Hold::Empty { .. } | Hold::Scrollbar => false,
        };
        if !drag.active && movable && (point - drag.origin).hypot() >= DRAG_THRESHOLD {
            drag.active = true;
        }
        if self
            .drag
            .as_ref()
            .is_some_and(|d| d.active && d.hold == Hold::Playhead)
        {
            self.report_scrub();
        }
        self.dirty = true;
    }

    fn report_scrub(&self) {
        if let Some(Landing::Playhead { at }) = self.drag.as_ref().and_then(|d| self.landing(d)) {
            self.scrub.fire(Some(Value::Float(at)));
        }
    }

    fn finish(&mut self, point: Point) {
        let Some(mut drag) = self.drag.take() else {
            return;
        };
        drag.current = point;
        self.dirty = true;
        if drag.hold == Hold::Scrollbar {
            self.bar.release();
            return;
        }
        if drag.active {
            match self.landing(&drag) {
                Some(Landing::Playhead { at }) => {
                    self.playhead = at;
                    self.seek.fire(Some(Value::Float(at)));
                }
                Some(Landing::Marker { index, at }) => {
                    if let Some(marker) = self.markers.get(index) {
                        marker.moved.fire(Some(Value::Float(at)));
                    }
                }
                Some(Landing::Span {
                    track,
                    index,
                    to_track,
                    start,
                    length,
                }) => self.land_span(track, index, to_track, start, length),
                None => {}
            }
            return;
        }
        match drag.hold {
            Hold::Span { track, index, .. } => {
                self.focus = Some(Focus::Span { track, index });
                self.press_span(track, index);
            }
            Hold::Marker { index, .. } => {
                if let Some(marker) = self.markers.get(index) {
                    marker.pressed.fire(None);
                }
            }
            Hold::Toggle { track, toggle } => {
                if let Some(t) = self.tracks.get(track)
                    && self
                        .toggle_rect(&self.geom(), track, toggle)
                        .is_some_and(|rect| rect.contains(point))
                {
                    toggle.event(t).fire(Some(Value::Bool(!toggle.on(t))));
                }
            }
            Hold::Empty { track } => {
                // Shift on an empty stretch adds nothing to what is chosen: it keeps the
                // selection as it stands rather than clearing it.
                if self.modifiers.shift_key() {
                    return;
                }
                if let Some(t) = self.tracks.get(track) {
                    let at = on_grid(self.geom().time_at(point.x), self.step);
                    self.press.fire(Some(Value::List(vec![
                        Value::Float(at),
                        Value::Text(t.key.clone()),
                    ])));
                }
            }
            Hold::Playhead | Hold::Scrollbar => {}
        }
    }

    /// Report where a span was let go, and draw it there until the view says where it stands:
    /// it glides back if the view refuses the change, and stays if the view takes it.
    fn land_span(&mut self, track: usize, index: usize, to_track: usize, start: f64, length: f64) {
        let Some(span) = self.tracks.get(track).and_then(|t| t.spans.get(index)) else {
            return;
        };
        let moved_track = to_track != track;
        let changed =
            moved_track || (start - span.start).abs() > 1e-9 || (length - span.length).abs() > 1e-9;
        if !changed {
            return;
        }
        let trim = (length - span.length).abs() > 1e-9;
        let answered = if trim { &span.trimmed } else { &span.moved };
        let key = span.key.clone();
        let unanswered = answered.is_none();
        if let Some(shown) = self.shown.get_mut(&key) {
            shown.snap(Vec2::new(start, length));
        }
        // Nothing will answer a change no view handles: it glides back at once.
        let wait = if unanswered {
            crate::time::Duration::ZERO
        } else {
            crate::time::Duration::from_millis(ANSWER_WAIT_MS)
        };
        self.pending.push((key, crate::time::now() + wait));
        if unanswered {
            self.settle_pending(crate::time::now());
        }
        let Some(span) = self.tracks.get(track).and_then(|t| t.spans.get(index)) else {
            return;
        };
        if trim {
            span.trimmed.fire(Some(Value::List(vec![
                Value::Float(start),
                Value::Float(length),
            ])));
        } else {
            let key = self
                .tracks
                .get(to_track)
                .map_or_else(String::new, |t| t.key.clone());
            span.moved.fire(Some(Value::List(vec![
                Value::Float(start),
                Value::Text(key),
            ])));
        }
    }

    /// A dropped span the view has not answered for by `now` glides back to where it stands,
    /// and so does every span dropped with it.
    fn settle_pending(&mut self, now: crate::time::Instant) -> bool {
        let (due, waiting): (
            Vec<(String, crate::time::Instant)>,
            Vec<(String, crate::time::Instant)>,
        ) = self.pending.drain(..).partition(|(_, by)| *by <= now);
        self.pending = waiting;
        if due.is_empty() {
            return false;
        }
        let theme = self.theme();
        let timing = crate::motion::Snap::SPEC.timing(
            Direction::Enter,
            theme.motion(),
            MotionSource::Pointer,
        );
        let frame = crate::motion::frame_now();
        for (key, _) in due {
            let Some(span) = self
                .tracks
                .iter()
                .flat_map(|t| &t.spans)
                .find(|s| s.key == key)
            else {
                continue;
            };
            let place = Vec2::new(span.start, span.length);
            if let Some(shown) = self.shown.get_mut(&key) {
                shown.set(place, timing, frame);
            }
        }
        self.dirty = true;
        true
    }

    /// Report a press on span `index` of track `track`: it carries its place on the track, which a
    /// view reads as a `host.Press` with the modifier keys the window says are held.
    fn press_span(&self, track: usize, index: usize) {
        let Some(span) = self.tracks.get(track).and_then(|t| t.spans.get(index)) else {
            return;
        };
        #[allow(
            clippy::cast_possible_wrap,
            reason = "a track holds far fewer spans than i64::MAX"
        )]
        span.pressed.fire(Some(Value::Int(index as i64)));
    }

    /// Zoom by `notches` of the wheel about the moment at `x`, keeping that moment under it.
    fn zoom_about(&mut self, notches: f64, x: f64) {
        self.zoom_by(1.25_f64.powf(-notches), x);
    }

    /// Zoom by `factor`, as a pinch scales what the lane shows, about the moment at `x`.
    fn zoom_by(&mut self, factor: f64, x: f64) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let geom = self.geom();
        let pinned = geom.time_at(x);
        let span = (self.span * factor).clamp(ZOOM_RANGE.0, ZOOM_RANGE.1);
        if (span - self.span).abs() < f64::EPSILON {
            return;
        }
        let lane = geom.lane();
        let px_per_second = lane.width().max(1.0) / span;
        let scroll = (pinned - (x - lane.x0) / px_per_second).max(0.0);
        self.span = span;
        self.zoomed.fire(Some(Value::Float(span)));
        self.scroll_to(scroll);
        self.dirty = true;
    }

    fn scroll_to(&mut self, scroll: f64) {
        let scroll = scroll.max(0.0);
        if (scroll - self.scroll).abs() < f64::EPSILON {
            return;
        }
        self.scroll = scroll;
        self.scrolled.fire(Some(Value::Float(scroll)));
        self.dirty = true;
    }

    /// Set the playhead down at `at` by a key: reported as a seek, and kept in view.
    fn seek_to(&mut self, at: f64) {
        let at = at.max(0.0);
        self.playhead = at;
        self.seek.fire(Some(Value::Float(at)));
        self.keep_in_view(at, at);
        self.dirty = true;
    }

    fn keep_in_view(&mut self, from: f64, to: f64) {
        let span = self.geom().span();
        if from < self.scroll {
            self.scroll_to(from);
        } else if to > self.scroll + span {
            self.scroll_to(to - span);
        }
    }

    /// The spans of `track` in the order they stand.
    fn order(&self, track: usize) -> Vec<usize> {
        let Some(t) = self.tracks.get(track) else {
            return Vec::new();
        };
        let mut order: Vec<usize> = (0..t.spans.len()).collect();
        order.sort_by(|a, b| t.spans[*a].start.total_cmp(&t.spans[*b].start));
        order
    }

    /// Move the keyboard along a track (`along`) or across tracks.
    fn move_focus(&mut self, along: i32, across: i32) {
        let focus = self.focus.unwrap_or(Focus::Playhead);
        let next = match focus {
            Focus::Playhead if across > 0 => self.first_on_or_after(0, self.playhead),
            Focus::Playhead => None,
            Focus::Span { track, index } => {
                if across != 0 {
                    let at = self.tracks[track].spans[index].start;
                    let to = i64::try_from(track).unwrap_or(0) + i64::from(across);
                    if to < 0 {
                        Some(Focus::Playhead)
                    } else {
                        usize::try_from(to)
                            .ok()
                            .and_then(|to| self.nearest_on(to, at))
                    }
                } else {
                    let order = self.order(track);
                    let place = order.iter().position(|i| *i == index).unwrap_or(0);
                    let to = i64::try_from(place).unwrap_or(0) + i64::from(along);
                    usize::try_from(to)
                        .ok()
                        .and_then(|to| order.get(to))
                        .map(|i| Focus::Span { track, index: *i })
                }
            }
        };
        if let Some(next) = next {
            self.focus = Some(next);
            if let Focus::Span { track, index } = next {
                let span = &self.tracks[track].spans[index];
                let (from, to) = (span.start, span.start + span.length.min(self.geom().span()));
                self.keep_in_view(from, to);
            }
            self.dirty = true;
        }
    }

    /// The first span on or after `track` that a reader comes to: the one nearest `at` on the
    /// first track that has any.
    fn first_on_or_after(&self, track: usize, at: f64) -> Option<Focus> {
        (track..self.tracks.len()).find_map(|t| self.nearest_on(t, at))
    }

    fn nearest_on(&self, track: usize, at: f64) -> Option<Focus> {
        let t = self.tracks.get(track)?;
        let index = (0..t.spans.len()).min_by(|a, b| {
            let da = (t.spans[*a].start - at).abs();
            let db = (t.spans[*b].start - at).abs();
            da.total_cmp(&db)
        })?;
        Some(Focus::Span { track, index })
    }

    /// Move or trim the focused span by a key, as a drag would.
    fn nudge(&mut self, by: f64, trim: bool) {
        let Some(Focus::Span { track, index }) = self.focus else {
            return;
        };
        let Some(t) = self.tracks.get(track) else {
            return;
        };
        if t.locked {
            return;
        }
        let Some(span) = t.spans.get(index) else {
            return;
        };
        let (start, length) = if trim {
            (span.start, (span.length + by).max(self.least_length()))
        } else {
            ((span.start + by).max(0.0), span.length)
        };
        self.land_span(track, index, track, start, length);
    }

    fn handle_key(&mut self, key: &Key, modifiers: ModifiersState) -> bool {
        let big = modifiers.shift_key();
        match key {
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight) => {
                let sign = if matches!(key, Key::Named(NamedKey::ArrowLeft)) {
                    -1.0
                } else {
                    1.0
                };
                match self.focus {
                    Some(Focus::Playhead) | None => {
                        self.seek_to(self.playhead + sign * self.key_step(big));
                    }
                    Some(Focus::Span { .. }) if modifiers.alt_key() => {
                        self.nudge(sign * self.key_step(false), big);
                    }
                    Some(Focus::Span { .. }) => {
                        self.move_focus(if sign < 0.0 { -1 } else { 1 }, 0);
                    }
                }
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_focus(0, -1);
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.move_focus(0, 1);
                true
            }
            Key::Named(NamedKey::Home) => {
                self.seek_to(0.0);
                true
            }
            Key::Named(NamedKey::End) => {
                self.seek_to(self.length);
                true
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                if let Some(Focus::Span { track, index }) = self.focus {
                    self.press_span(track, index);
                    return true;
                }
                false
            }
            Key::Named(NamedKey::Escape) => {
                let held = self.drag.take().is_some();
                self.dirty = true;
                held
            }
            Key::Character(c) => match c.as_str() {
                "," => {
                    self.seek_to(self.playhead - self.key_step(big));
                    true
                }
                "." => {
                    self.seek_to(self.playhead + self.key_step(big));
                    true
                }
                "+" | "=" => {
                    let x = self.geom().x_of(self.playhead);
                    self.zoom_about(1.0, x);
                    true
                }
                "-" => {
                    let x = self.geom().x_of(self.playhead);
                    self.zoom_about(-1.0, x);
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn tone(theme: &Theme, ink: Ink) -> Color {
        ink.colour(theme)
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let geom = self.geom();
        let lane = geom.lane();
        let ruler = geom.ruler();
        let now = crate::motion::frame_now();
        let radius = f64::from(theme.tokens.radii.item).min(6.0);
        let hairline = f64::from(theme.tokens.border.width).max(1.0);
        let ink = colors.text_primary;
        let dim = colors.text_dim;
        let line = with_alpha_scaled(ink, 0.12);
        let pad = f64::from(theme.tokens.spacing.s2);

        // The ground: the header column, the ruler's band and the tracks' lanes.
        ctx.fill_rect(self.rect, colors.surfaces.content);
        ctx.fill_rect(
            Rect::new(self.rect.x0, self.rect.y0, lane.x0, self.rect.y1),
            colors.surfaces.sidebar,
        );
        ctx.fill_rect(ruler, with_alpha_scaled(ink, 0.04));

        // The rows: alternate grounds, each row's line, the header's name and toggles.
        for (row, track) in self.tracks.iter().enumerate() {
            let band = geom.row(row);
            if row % 2 == 1 {
                ctx.fill_rect(
                    Rect::new(lane.x0, band.y0, lane.x1, band.y1),
                    with_alpha_scaled(ink, 0.025),
                );
            }
            crate::paint::stroke_line(
                ctx.scene,
                ctx.transform,
                Line::new((self.rect.x0, band.y1), (self.rect.x1, band.y1)),
                line,
                hairline,
            );
            let header = geom.header(row);
            let name = if track.label.is_empty() {
                crate::i18n::widget_format("timeline-track-untitled", &[])
            } else {
                track.label.clone()
            };
            let text_band = Rect::new(
                header.x0,
                header.y0 + pad,
                header.x1,
                header.y0 + pad + 18.0,
            );
            ctx.scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                ctx.transform,
                &header,
            );
            ctx.text_role_in_band(
                &name,
                TextRole::Label,
                if track.muted { dim } else { ink },
                header.x0 + pad * 1.5,
                text_band,
                None,
            );
            ctx.scene.pop_layer();
            for toggle in Toggle::ALL {
                let Some(rect) = self.toggle_rect(&geom, row, toggle) else {
                    continue;
                };
                let on = toggle.on(track);
                if on {
                    ctx.fill_round(rect, radius, with_alpha_scaled(colors.intent.accent, 0.18));
                }
                crate::icons::draw_icon(
                    ctx.scene,
                    ctx.transform,
                    rect.inset(-4.0),
                    toggle.icon(on),
                    if on { colors.intent.accent } else { dim },
                    crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
        }
        crate::paint::stroke_line(
            ctx.scene,
            ctx.transform,
            Line::new((lane.x0, self.rect.y0), (lane.x0, self.rect.y1)),
            line,
            hairline,
        );

        // Everything in the lane is clipped to it: spans scrolled half out stop at its edge.
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &lane,
        );

        // The ruler: a labelled tick each interval, small ones between.
        let (major, minor) = tick_steps(geom.px_per_second);
        let fine = major < 1.0;
        let first = (geom.scroll / minor).floor() * minor;
        let last = geom.scroll + geom.span();
        let mut t = first;
        let mut guard = 0;
        while t <= last + minor && guard < 4096 {
            guard += 1;
            let x = geom.x_of(t);
            let labelled = ((t / major).round() * major - t).abs() < minor * 0.01;
            let top = if labelled {
                ruler.y0 + ruler.height() * 0.45
            } else {
                ruler.y1 - 5.0
            };
            crate::paint::stroke_line(
                ctx.scene,
                ctx.transform,
                Line::new((x, top), (x, ruler.y1)),
                if labelled {
                    with_alpha_scaled(ink, 0.35)
                } else {
                    line
                },
                hairline,
            );
            if labelled && t >= 0.0 {
                let band = Rect::new(x, ruler.y0, x + 80.0, ruler.y0 + ruler.height() * 0.6);
                ctx.text_role_in_band(
                    &timecode(t, fine),
                    TextRole::Caption,
                    dim,
                    x + 3.0,
                    band,
                    None,
                );
            }
            t += minor;
        }
        crate::paint::stroke_line(
            ctx.scene,
            ctx.transform,
            Line::new((lane.x0, ruler.y1), (lane.x1, ruler.y1)),
            line,
            hairline,
        );

        // The spans, each where it is drawn now; the one held is drawn where the hand puts it.
        let landing = self
            .drag
            .as_ref()
            .filter(|d| d.active)
            .and_then(|d| self.landing(d));
        let inset = self.span_inset();
        let (first_t, last_t) = (geom.scroll, geom.scroll + geom.span());
        for (row, track) in self.tracks.iter().enumerate() {
            for (index, span) in track.spans.iter().enumerate() {
                let held = matches!(landing, Some(Landing::Span { track: t, index: i, .. }) if t == row && i == index);
                let (start, length) = self.drawn_place(span);
                if start > last_t || start + length < first_t {
                    continue;
                }
                let rect = geom.span_rect(row, start, length, inset);
                let lift = self.lift.get(&span.key).map_or(0.0, |l| l.get(now));
                let focused = self.focus == Some(Focus::Span { track: row, index });
                let faded = track.muted || held;
                let look = Look {
                    rect,
                    start,
                    lift,
                    focused,
                    faded,
                    locked: track.locked,
                };
                self.paint_span(ctx, span, look, &geom);
            }
        }
        if let Some(Landing::Span {
            track,
            index,
            to_track,
            start,
            length,
        }) = landing
            && let Some(span) = self.tracks.get(track).and_then(|t| t.spans.get(index))
        {
            let rect = geom.span_rect(to_track, start, length, inset);
            let look = Look {
                rect,
                start,
                lift: 1.0,
                focused: false,
                faded: false,
                locked: false,
            };
            self.paint_span(ctx, span, look, &geom);
            for edge in [start, start + length] {
                if snap_target(edge, &self.targets(Some((track, index)), true), 1e-6).is_some() {
                    let x = geom.x_of(edge);
                    crate::paint::stroke_line(
                        ctx.scene,
                        ctx.transform,
                        Line::new((x, ruler.y1), (x, self.rect.y1)),
                        colors.intent.accent,
                        hairline,
                    );
                }
            }
        }

        // The markers: a flag on the ruler, a faint line down the tracks.
        for (index, marker) in self.markers.iter().enumerate() {
            let at = match landing {
                Some(Landing::Marker { index: i, at }) if i == index => at,
                _ => marker.at,
            };
            let x = geom.x_of(at);
            let tone = Self::tone(theme, marker.tone);
            crate::paint::stroke_line(
                ctx.scene,
                ctx.transform,
                Line::new(
                    (x, ruler.y0),
                    (
                        x,
                        (ruler.y1 + geom.track_h * crate::num::f64_of_usize(geom.tracks))
                            .min(self.rect.y1),
                    ),
                ),
                with_alpha_scaled(tone, 0.55),
                hairline,
            );
            let mut flag = BezPath::new();
            flag.move_to((x, ruler.y0 + 2.0));
            flag.line_to((x + 9.0, ruler.y0 + 6.0));
            flag.line_to((x, ruler.y0 + 10.0));
            flag.close_path();
            ctx.scene
                .fill(Fill::NonZero, ctx.transform, tone, None, &flag);
            if !marker.label.is_empty() {
                let band = Rect::new(x, ruler.y0, x + 120.0, ruler.y0 + 12.0);
                ctx.text_role_in_band(&marker.label, TextRole::Caption, tone, x + 11.0, band, None);
            }
        }

        let tracks_bottom = ruler.y1 + geom.track_h * crate::num::f64_of_usize(geom.tracks);
        // Where a measurement found a problem: a danger flag on the ruler and a line down the
        // tracks, so the flag stands at the moment it is about.
        for at in &self.clips {
            let x = geom.x_of(*at);
            let danger = crate::widget::Tone::Danger.colour(theme);
            crate::paint::stroke_line(
                ctx.scene,
                ctx.transform,
                Line::new((x, ruler.y0), (x, tracks_bottom.min(self.rect.y1))),
                with_alpha_scaled(danger, 0.45),
                hairline,
            );
            let mut flag = BezPath::new();
            flag.move_to((x, ruler.y1 - 10.0));
            flag.line_to((x + 8.0, ruler.y1 - 6.0));
            flag.line_to((x, ruler.y1 - 2.0));
            flag.close_path();
            ctx.scene
                .fill(Fill::NonZero, ctx.transform, danger, None, &flag);
        }

        // The playhead: a line down every row with a handle on the ruler.
        let head = self.shown_playhead();
        let x = geom.x_of(head);
        let red = crate::widget::Tone::Danger.colour(theme);
        crate::paint::stroke_line(
            ctx.scene,
            ctx.transform,
            Line::new((x, ruler.y0), (x, tracks_bottom.min(self.rect.y1))),
            red,
            hairline * 2.0,
        );
        let mut handle = BezPath::new();
        handle.move_to((x - 6.0, ruler.y1 - 12.0));
        handle.line_to((x + 6.0, ruler.y1 - 12.0));
        handle.line_to((x + 6.0, ruler.y1 - 6.0));
        handle.line_to((x, ruler.y1));
        handle.line_to((x - 6.0, ruler.y1 - 6.0));
        handle.close_path();
        ctx.scene
            .fill(Fill::NonZero, ctx.transform, red, None, &handle);
        if self.focus == Some(Focus::Playhead) {
            crate::paint::stroke_line(
                ctx.scene,
                ctx.transform,
                Line::new((x - 8.0, ruler.y1 - 14.0), (x + 8.0, ruler.y1 - 14.0)),
                colors.border_focus,
                hairline * 2.0,
            );
        }
        ctx.scene.pop_layer();

        // The bar under the tracks: how much of the cut the lane shows and where it stands.
        if let Some((_, track)) = self.scrollbar() {
            self.bar.draw(
                ctx.scene,
                ctx.transform,
                track,
                self.scroll_metrics(),
                theme,
                false,
            );
        }
    }

    /// Scroll the lane to where the hand holds the bar.
    fn drag_bar(&mut self, point: Point) {
        let Some((_, track)) = self.scrollbar() else {
            return;
        };
        if let Some(offset) = self.bar.drag_to(track, self.scroll_metrics(), point) {
            self.scroll_to(offset);
        }
    }

    /// Light the bar's thumb while the pointer rests on it, and let it go when it leaves.
    fn hover_bar(&mut self) {
        let metrics = self.scroll_metrics();
        if let Some((_, track)) = self.scrollbar()
            && self.bar.update_hover(track, metrics, self.pointer)
        {
            self.dirty = true;
        }
    }

    fn paint_span(&self, ctx: &mut PaintCtx<'_>, span: &Span, look: Look, geom: &Geom) {
        let Look {
            rect,
            start,
            lift,
            focused,
            faded,
            locked,
        } = look;
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let tone = Self::tone(theme, span.tone);
        let radius = f64::from(theme.tokens.radii.item).min(6.0);
        let hairline = f64::from(theme.tokens.border.width).max(1.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "an alpha factor from 0 to 1"
        )]
        let alpha = |a: f64| a as f32;
        let fade = if faded {
            0.45
        } else if locked {
            0.7
        } else {
            1.0
        };
        let body = RoundedRect::from_rect(rect, radius);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            with_alpha_scaled(tone, alpha((0.30 + 0.25 * lift) * fade)),
            None,
            &body,
        );
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &body,
        );
        let pad = f64::from(theme.tokens.spacing.s1).max(3.0);
        let label_h = 16.0;
        let content = Rect::new(rect.x0, rect.y0 + label_h + 2.0, rect.x1, rect.y1);
        // The frames laid edge to edge under the name.
        if !span.frames.is_empty() && content.height() > 4.0 {
            #[allow(clippy::cast_precision_loss, reason = "a strip has a few frames")]
            let count = span.frames.len() as f64;
            let w = rect.width() / count;
            for (i, src) in span.frames.iter().enumerate() {
                #[allow(clippy::cast_precision_loss, reason = "a strip has a few frames")]
                let x0 = rect.x0 + w * i as f64;
                let dest = Rect::new(x0, content.y0, x0 + w, content.y1);
                self.paint_picture(ctx, src, dest, fade);
            }
        }
        // Its sound: one bar each peak, mirrored about the middle.
        if !span.peaks.is_empty() {
            let mid = (content.y0 + content.y1) / 2.0;
            let half = content.height() / 2.0 * 0.9;
            #[allow(
                clippy::cast_precision_loss,
                reason = "a waveform has a few hundred peaks"
            )]
            let count = span.peaks.len() as f64;
            let mut wave = BezPath::new();
            for (i, peak) in span.peaks.iter().enumerate() {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "a waveform has a few hundred peaks"
                )]
                let x = rect.x0 + rect.width() * (i as f64 + 0.5) / count;
                let h = (peak.clamp(0.0, 1.0) * half).max(0.5);
                wave.move_to((x, mid - h));
                wave.line_to((x, mid + h));
            }
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new((rect.width() / count * 0.6).clamp(hairline, 3.0)),
                ctx.transform,
                with_alpha_scaled(colors.text_primary, alpha(0.7 * fade)),
                None,
                &wave,
            );
        }
        // The transition into it: a ramp over its leading stretch.
        if span.lead_in > 0.0 {
            let x1 = geom.x_of(start + span.lead_in).min(rect.x1);
            let mut ramp = BezPath::new();
            ramp.move_to((rect.x0, rect.y1));
            ramp.line_to((x1, rect.y0));
            ramp.line_to((rect.x0, rect.y0));
            ramp.close_path();
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                with_alpha_scaled(
                    crate::widget::Tone::Warning.colour(theme),
                    alpha(0.35 * fade),
                ),
                None,
                &ramp,
            );
        }
        // Its keyframes: a diamond each along its foot.
        for key in &span.keys {
            let x = geom.x_of(start + key);
            let y = rect.y1 - 6.0;
            let mut diamond = BezPath::new();
            diamond.move_to((x, y - 4.0));
            diamond.line_to((x + 4.0, y));
            diamond.line_to((x, y + 4.0));
            diamond.line_to((x - 4.0, y));
            diamond.close_path();
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                crate::widget::Tone::Warning.colour(theme),
                None,
                &diamond,
            );
        }
        // Its name over the top, on a band of its colour so it reads over the frames.
        let band = Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + label_h);
        ctx.fill_rect(band, with_alpha_scaled(tone, alpha(0.55 * fade)));
        if !span.label.is_empty() && rect.width() > pad * 3.0 {
            ctx.text_role_in_band(
                &span.label,
                TextRole::Caption,
                with_alpha_scaled(colors.text_primary, alpha(fade)),
                rect.x0 + pad,
                band,
                None,
            );
        }
        ctx.scene.pop_layer();
        // Its outline: the accent when chosen, rising as it is chosen; the focus ring over it.
        let outline = if lift > 0.01 {
            colors.intent.accent
        } else {
            with_alpha_scaled(tone, alpha(0.9 * fade))
        };
        ctx.scene.stroke(
            &vello::kurbo::Stroke::new(hairline * (1.0 + 1.5 * lift)),
            ctx.transform,
            outline,
            None,
            &body,
        );
        if focused {
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new(hairline * 2.0),
                ctx.transform,
                colors.border_focus,
                None,
                &RoundedRect::from_rect(rect.inflate(2.0, 2.0), radius + 2.0),
            );
        }
    }

    fn paint_picture(&self, ctx: &mut PaintCtx<'_>, src: &str, dest: Rect, fade: f64) {
        let _ = self;
        if dest.width() <= 0.0 || dest.height() <= 0.0 {
            return;
        }
        let picture = crate::widget::PictureSource {
            src: src.to_owned(),
        };
        let Some(image) = crate::pictures::Pictures::window().get(&picture) else {
            return;
        };
        let (iw, ih) = (f64::from(image.width), f64::from(image.height));
        if iw <= 0.0 || ih <= 0.0 {
            return;
        }
        let scale = (dest.width() / iw).max(dest.height() / ih);
        let (dw, dh) = (iw * scale, ih * scale);
        let brush = Affine::translate((
            dest.x0 + (dest.width() - dw) * 0.5,
            dest.y0 + (dest.height() - dh) * 0.5,
        )) * Affine::scale(scale);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "an alpha factor from 0 to 1"
        )]
        let alpha = fade as f32;
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            alpha,
            ctx.transform,
            &dest,
        );
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            &ImageBrush::new(image),
            Some(brush),
            &dest,
        );
        ctx.scene.pop_layer();
    }

    fn track_key(track: &Track) -> u64 {
        crate::a11y::hash_key(&("track-timeline.track", &track.key))
    }

    fn span_key(span: &Span) -> u64 {
        crate::a11y::hash_key(&("track-timeline.span", &span.key))
    }

    fn marker_key(marker: &Marker) -> u64 {
        crate::a11y::hash_key(&("track-timeline.marker", &marker.key))
    }

    fn toggle_key(toggle: Toggle) -> u64 {
        crate::a11y::hash_key(&("track-timeline.toggle", toggle.words()))
    }

    fn playhead_key() -> u64 {
        crate::a11y::hash_key(&"track-timeline.playhead")
    }

    /// When a span is, as a person reads it: `0:03.0 to 0:07.5`.
    fn span_time(span: &Span) -> String {
        crate::format::range(
            &timecode(span.start, true),
            &timecode(span.start + span.length, true),
        )
    }

    fn span_access(&self, geom: &Geom, row: usize, index: usize) -> Option<AccessNode> {
        let span = self.tracks.get(row)?.spans.get(index)?;
        let rect = geom.span_rect(row, span.start, span.length, self.span_inset());
        let name = if span.label.is_empty() {
            crate::i18n::widget_format("timeline-span-untitled", &[])
        } else {
            span.label.clone()
        };
        Some(
            AccessNode::new(Role::Button)
                .with_name(name)
                .with_value(Self::span_time(span))
                .with_item(span.key.clone())
                .with_key(Self::span_key(span))
                .with_bounds(rect.intersect(geom.lane()))
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.focus == Some(Focus::Span { track: row, index }))
                        .selected(span.selected),
                ),
        )
    }

    fn track_access(&self, geom: &Geom, row: usize) -> Option<AccessNode> {
        let track = self.tracks.get(row)?;
        let (first, last) = (geom.scroll, geom.scroll + geom.span());
        let mut children = Vec::new();
        for toggle in Toggle::ALL.into_iter().rev() {
            let Some(rect) = self.toggle_rect(geom, row, toggle) else {
                continue;
            };
            children.push(
                AccessNode::new(Role::ToggleButton)
                    .with_name(crate::i18n::widget_format(toggle.words(), &[]))
                    .with_key(Self::toggle_key(toggle))
                    .with_bounds(rect)
                    .with_state(AccessState::new().checked(toggle.on(track)).focusable(true)),
            );
        }
        for index in self.order(row) {
            let span = &track.spans[index];
            if span.start > last || span.start + span.length < first {
                continue;
            }
            if let Some(node) = self.span_access(geom, row, index) {
                children.push(node);
            }
        }
        let mut said = Vec::new();
        if track.muted {
            said.push(crate::i18n::widget_format("timeline-muted", &[]));
        }
        if track.solo {
            said.push(crate::i18n::widget_format("timeline-soloed", &[]));
        }
        if track.locked {
            said.push(crate::i18n::widget_format("timeline-locked", &[]));
        }
        let name = if track.label.is_empty() {
            crate::i18n::widget_format("timeline-track-untitled", &[])
        } else {
            track.label.clone()
        };
        Some(
            AccessNode::new(Role::Group)
                .with_name(name)
                .with_optional_description((!said.is_empty()).then(|| crate::format::list(&said)))
                .with_key(Self::track_key(track))
                .with_bounds(geom.row(row))
                .with_children(children),
        )
    }

    /// Run `action` on the element `key` names.
    fn act_on(&mut self, action: &crate::a11y::Action, key: u64, parent: Option<u64>) -> bool {
        use crate::a11y::Action;
        if key == Self::playhead_key() {
            self.focus = Some(Focus::Playhead);
            self.dirty = true;
            match action {
                Action::Increment => self.seek_to(self.playhead + self.key_step(false)),
                Action::Decrement => self.seek_to(self.playhead - self.key_step(false)),
                Action::SetValue(text) => match text.trim().parse::<f64>() {
                    Ok(at) => self.seek_to(at),
                    Err(_) => return false,
                },
                Action::Focus | Action::Activate => {}
                _ => return false,
            }
            return true;
        }
        if let Some(index) = self.markers.iter().position(|m| Self::marker_key(m) == key) {
            if matches!(action, Action::Activate) {
                self.markers[index].pressed.fire(None);
            }
            return matches!(action, Action::Activate | Action::Focus);
        }
        for row in 0..self.tracks.len() {
            let track = &self.tracks[row];
            if parent.is_some_and(|p| p != Self::track_key(track)) {
                continue;
            }
            if let Some(index) = track.spans.iter().position(|s| Self::span_key(s) == key) {
                self.focus = Some(Focus::Span { track: row, index });
                self.dirty = true;
                match action {
                    Action::Activate => self.press_span(row, index),
                    Action::Increment => self.nudge(self.key_step(false), false),
                    Action::Decrement => self.nudge(-self.key_step(false), false),
                    Action::Focus => {}
                    _ => return false,
                }
                return true;
            }
            if let Some(toggle) = Toggle::ALL
                .into_iter()
                .find(|t| Self::toggle_key(*t) == key && !t.event(track).is_none())
            {
                if matches!(action, Action::Activate | Action::ToggleCheck) {
                    toggle
                        .event(track)
                        .fire(Some(Value::Bool(!toggle.on(track))));
                    return true;
                }
                return false;
            }
        }
        false
    }

    #[cfg(test)]
    pub(crate) fn layout_for(&mut self, area: Rect, theme: &Theme) {
        self.theme = Some(theme.clone());
        self.rect = area;
        self.drag = None;
    }

    #[cfg(test)]
    pub(crate) fn press_at_for_test(&mut self, point: Point, modifiers: ModifiersState) -> bool {
        self.pointer = point;
        self.modifiers = modifiers;
        self.begin(point)
    }

    #[cfg(test)]
    pub(crate) fn move_to_for_test(&mut self, point: Point) {
        self.pointer = point;
        self.update_drag(point);
    }

    #[cfg(test)]
    pub(crate) fn release_at_for_test(&mut self, point: Point) {
        self.finish(point);
    }

    #[cfg(test)]
    pub(crate) fn key_for_test(&mut self, key: &Key, modifiers: ModifiersState) -> bool {
        if self.focus.is_none() {
            self.set_focus(true);
        }
        self.handle_key(key, modifiers)
    }

    #[cfg(test)]
    pub(crate) fn geom_for_test(&self) -> Geom {
        self.geom()
    }

    #[cfg(test)]
    pub(crate) fn drawn_for_test(&self, key: &str) -> Option<(f64, f64)> {
        self.tracks
            .iter()
            .flat_map(|t| &t.spans)
            .find(|s| s.key == key)
            .map(|s| self.drawn_place(s))
    }

    #[cfg(test)]
    pub(crate) fn wheel_for_test(&mut self, notches: f64, x: f64) {
        self.zoom_about(notches, x);
    }

    #[cfg(test)]
    pub(crate) fn pinch_for_test(&mut self, factor: f64, x: f64) {
        self.zoom_by(factor, x);
    }

    #[cfg(test)]
    pub(crate) fn scrollbar_for_test(&self) -> Option<(Rect, Rect)> {
        self.scrollbar()
    }

    #[cfg(test)]
    pub(crate) fn bar_metrics_for_test(&self) -> ScrollMetrics {
        self.scroll_metrics()
    }
}

impl Widget for TrackTimeline {
    crate::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = Some(theme.clone());
        let style = Style {
            size: Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                height: auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => 0.0,
        });
        let (_, ruler_h, track_h) = self.metrics();
        let rows = Geom::height(ruler_h, track_h, self.tracks.len()) + self.scrollbar_h();
        taffy::Size {
            width,
            height: crate::num::f32_of_f64(rows),
        }
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if env.should_cull(self.rect) || self.rect.width() <= 0.0 {
            return;
        }
        self.theme = Some(theme.clone());
        let mut paint = PaintCtx::new(scene, transform, text_renderer, theme);
        self.paint(&mut paint);
        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.modifiers = ctx.modifiers.state();
        match event {
            WindowEvent::PinchGesture { delta, .. } if self.rect.contains(self.pointer) => {
                if *delta != 0.0 {
                    // Fingers apart magnify: the lane shows less of the cut, about the pointer.
                    self.zoom_by(1.0 / (1.0 + *delta), self.pointer.x);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } if self.rect.contains(self.pointer) => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        (f64::from(*x) * 48.0, f64::from(*y) * 48.0)
                    }
                    MouseScrollDelta::PixelDelta(position) => (position.x, position.y),
                };
                if self.modifiers.control_key() || self.modifiers.super_key() {
                    self.zoom_about(dy / 48.0, self.pointer.x);
                    *ctx.handled = true;
                } else if dx.abs() > dy.abs() || self.modifiers.shift_key() {
                    let by = if dx.abs() > dy.abs() { dx } else { dy };
                    let seconds = self.geom().seconds(by);
                    self.scroll_to(self.scroll - seconds);
                    *ctx.handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = ctx.logical(position);
                self.hover_bar();
                if self.drag.is_some() {
                    self.update_drag(self.pointer);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.begin(self.pointer) {
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.drag.is_some() {
                    self.finish(self.pointer);
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && self.focus.is_some() =>
            {
                *ctx.handled |= self.handle_key(&event.logical_key, self.modifiers);
            }
            _ => {}
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.drag.as_ref().is_some_and(|drag| drag.active)
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        if focused && self.focus.is_none() {
            self.focus = Some(
                self.tracks
                    .iter()
                    .enumerate()
                    .find_map(|(row, track)| {
                        self.order(row)
                            .into_iter()
                            .find(|i| track.spans[*i].selected)
                            .map(|index| Focus::Span { track: row, index })
                    })
                    .unwrap_or(Focus::Playhead),
            );
        } else if !focused {
            self.focus = None;
        }
        self.dirty = true;
    }

    fn is_focused(&self) -> bool {
        self.focus.is_some()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.needs_animation_frame()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn needs_animation_frame(&self) -> bool {
        let now = crate::motion::frame_now();
        self.shown.values().any(|v| v.is_moving(now))
            || self.lift.values().any(|v| v.is_moving(now))
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        self.pending.iter().map(|(_, by)| *by).min()
    }

    fn serve_deadline(&mut self, now: crate::time::Instant) -> bool {
        self.settle_pending(now)
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        use winit::window::CursorIcon;
        if let Some(drag) = self.drag.as_ref().filter(|d| d.active) {
            return Some(match drag.hold {
                Hold::Span {
                    grab: Grab::Head | Grab::Tail,
                    ..
                } => CursorIcon::EwResize,
                Hold::Playhead => CursorIcon::ColResize,
                _ => CursorIcon::Grabbing,
            });
        }
        if let Some((area, track)) = self.scrollbar()
            && area.contains(point)
        {
            let over_thumb = self
                .bar
                .thumb_rect(track, self.scroll_metrics())
                .is_some_and(|thumb| thumb.contains(point));
            return Some(if over_thumb {
                CursorIcon::Grab
            } else {
                CursorIcon::Default
            });
        }
        Some(match self.hold_at(point)? {
            Hold::Playhead => CursorIcon::ColResize,
            Hold::Span { track, grab, .. } => {
                if self.tracks.get(track).is_some_and(|t| t.locked) {
                    CursorIcon::NotAllowed
                } else if grab == Grab::Body {
                    CursorIcon::Grab
                } else {
                    CursorIcon::EwResize
                }
            }
            Hold::Marker { .. } => CursorIcon::Grab,
            Hold::Toggle { .. } => CursorIcon::Pointer,
            Hold::Empty { .. } | Hold::Scrollbar => CursorIcon::Default,
        })
    }

    fn name(&self) -> &'static str {
        "TrackTimeline"
    }

    /// The timeline as a reader hears it: the playhead as a slider over the time, each track a
    /// group named by its name with its toggles and the spans in view in the order they stand,
    /// and each marker. Only the spans the lane shows are nodes, as only they are painted.
    fn accessibility(&self) -> Option<AccessNode> {
        let geom = self.geom();
        let head = self.shown_playhead();
        let mut children = vec![
            AccessNode::new(Role::Slider)
                .with_name(crate::i18n::widget_format("timeline-playhead", &[]))
                .with_value(timecode(head, true))
                .with_key(Self::playhead_key())
                .with_bounds(geom.ruler())
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.focus == Some(Focus::Playhead)),
                ),
        ];
        for marker in &self.markers {
            let name = if marker.label.is_empty() {
                crate::i18n::widget_format("timeline-marker", &[])
            } else {
                marker.label.clone()
            };
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_value(timecode(marker.at, true))
                    .with_key(Self::marker_key(marker))
                    .with_bounds(self.marker_flag(&geom, marker)),
            );
        }
        for row in 0..self.tracks.len() {
            if let Some(node) = self.track_access(&geom, row) {
                children.push(node);
            }
        }
        Some(
            AccessNode::new(Role::Group)
                .with_optional_name(self.label.clone())
                .with_value(timecode(head, true))
                .with_bounds(self.rect)
                .with_state(AccessState::new().focused(self.focus.is_some()))
                .with_children(children)
                .with_keyed_action_routes("track-timeline"),
        )
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        let Some(&key) = key_path.last() else {
            return false;
        };
        let parent = key_path
            .len()
            .checked_sub(2)
            .and_then(|i| key_path.get(i))
            .copied();
        self.act_on(action, key, parent)
    }
}
