//! The room canvas of the speaker layout step.
//!
//! A top-down room with the listener at the centre and one marker per
//! speaker. Pressing or tapping a speaker selects it (the pane shares that
//! selection with its speaker picker, so only that speaker's settings show
//! under it); dragging one moves it 1:1 under the pointer or finger. While
//! it moves only the drawing changes: nothing is laid out and nothing is
//! stored. Within a small threshold it snaps magnetically onto the centre
//! line ahead of the listener, onto another speaker's line or row, and onto
//! the mirror of another speaker, so a pair stands symmetric; an alignment
//! guide shows each line it lines up with. On release it settles, on the
//! standard settle spring, into the nearest place inside the room that is
//! neither on the listener nor on another speaker, and that place is
//! stored once. At rest the canvas asks for no frames.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use ui::motion::{Direction, MotionValue, Snap, Timing};
use ui::theme::Theme;
use ui::time::Instant;
use ui::tokens::{EffectiveMotion, MotionSource};
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, Stroke, Vec2};
use winit::event::{ElementState, MouseButton, TouchPhase, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::i18n::t;
use crate::speaker_setup::ROOM_EXTENT_MM;

/// The least the canvas shows either side of the speakers' middle: a set
/// standing close together still reads as a room, not as a magnified dot.
const MIN_HALF_VIEW_MM: f64 = 1_500.0;
/// Room left around the speakers (and the listener) the view fits.
// ratchet-allow(pixel_const): a distance in the room, in millimetres, not pixels
const VIEW_MARGIN_MM: f64 = 400.0;

/// The part of the room the canvas shows, in millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct View {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl View {
    /// The speakers at `points` and the listener, with a margin, and at
    /// least [`MIN_HALF_VIEW_MM`] either side of their middle.
    pub(crate) fn fitting(points: impl IntoIterator<Item = (f64, f64)>) -> Self {
        let mut view = Self {
            x0: 0.0,
            y0: 0.0,
            x1: 0.0,
            y1: 0.0,
        };
        for (x, y) in points {
            view = view.including((x, y));
        }
        let grow = |low: f64, high: f64| {
            let middle = f64::midpoint(low, high);
            let half = ((high - low) / 2.0 + VIEW_MARGIN_MM).max(MIN_HALF_VIEW_MM);
            (middle - half, middle + half)
        };
        let (x0, x1) = grow(view.x0, view.x1);
        let (y0, y1) = grow(view.y0, view.y1);
        Self { x0, y0, x1, y1 }
    }

    fn including(self, (x, y): (f64, f64)) -> Self {
        Self {
            x0: self.x0.min(x),
            y0: self.y0.min(y),
            x1: self.x1.max(x),
            y1: self.y1.max(y),
        }
    }

    /// Grown, never shrunk, to hold `point` with the margin around it.
    fn holding(self, (x, y): (f64, f64)) -> Self {
        Self {
            x0: self.x0.min(x - VIEW_MARGIN_MM),
            y0: self.y0.min(y - VIEW_MARGIN_MM),
            x1: self.x1.max(x + VIEW_MARGIN_MM),
            y1: self.y1.max(y + VIEW_MARGIN_MM),
        }
    }

    fn toward(self, to: Self, progress: f64) -> Self {
        let mix = |a: f64, b: f64| a + (b - a) * progress;
        Self {
            x0: mix(self.x0, to.x0),
            y0: mix(self.y0, to.y0),
            x1: mix(self.x1, to.x1),
            y1: mix(self.y1, to.y1),
        }
    }
}

/// The view springing from one framing of the room to another: the set
/// refitting after a drop, or growing while a speaker is dragged out.
#[derive(Clone, Debug)]
struct ViewMotion {
    from: View,
    to: View,
    /// From 0 (`from`) to 1 (`to`).
    progress: MotionValue<f64>,
}

/// A line a held speaker lines up with, in room millimetres (x to the
/// listener's right, y ahead of them): vertical at `x == at`, or
/// horizontal at `y == at`, drawn across the whole canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RoomGuide {
    pub vertical: bool,
    pub at: i32,
}

/// Where a held speaker stands once pulled onto the lines near it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapped {
    pub at: (i32, i32),
    pub guides: Vec<RoomGuide>,
}

/// Pull `at` onto the lines within `threshold` millimetres of it, each axis
/// on its own: the centre line ahead of and behind the listener and the
/// listener's own row; each other speaker's line and row; and the mirror
/// of each other speaker across the centre line.
pub(crate) fn snap(at: (i32, i32), others: &[(i32, i32)], threshold: i32) -> Snapped {
    // Each candidate line: across (x) and row (y).
    let mut across: Vec<i32> = vec![0];
    let mut rows: Vec<i32> = vec![0];
    for &(x, y) in others {
        across.push(x);
        across.push(x.saturating_neg());
        rows.push(y);
    }
    let nearest = |lines: &[i32], value: i32| {
        lines
            .iter()
            .copied()
            .filter(|line| line.abs_diff(value) <= threshold.unsigned_abs())
            .min_by_key(|line| line.abs_diff(value))
    };
    let x = nearest(&across, at.0);
    let y = nearest(&rows, at.1);
    let guides = x
        .map(|at| RoomGuide { vertical: true, at })
        .into_iter()
        .chain(y.map(|at| RoomGuide {
            vertical: false,
            at,
        }))
        .collect();
    Snapped {
        at: (x.unwrap_or(at.0), y.unwrap_or(at.1)),
        guides,
    }
}

/// Where a let-go speaker rests: inside the room, and at least `gap`
/// millimetres from the listener and from every other speaker, moved out
/// along the line from whatever it landed on.
pub(crate) fn rest(at: (i32, i32), others: &[(i32, i32)], gap: i32) -> (i32, i32) {
    let clamp = |(x, y): (f64, f64)| -> (i32, i32) {
        let extent = f64::from(ROOM_EXTENT_MM);
        (
            round_to_i32(x.clamp(-extent, extent)),
            round_to_i32(y.clamp(-extent, extent)),
        )
    };
    let gap = f64::from(gap.max(1));
    let mut point = clamp((f64::from(at.0), f64::from(at.1)));
    let blockers: Vec<(i32, i32)> = std::iter::once((0, 0))
        .chain(others.iter().copied())
        .collect();
    // A push can land on a neighbour: a few passes settle every crowd a
    // room holds.
    for _ in 0..blockers.len().saturating_mul(2).max(1) {
        let mut moved = false;
        for &(bx, by) in &blockers {
            let dx = f64::from(point.0) - f64::from(bx);
            let dy = f64::from(point.1) - f64::from(by);
            let distance = dx.hypot(dy);
            if distance + 0.5 < gap {
                // Exactly on it: step straight ahead, away from the listener.
                let (ux, uy) = if distance < 1.0 {
                    (0.0, 1.0)
                } else {
                    (dx / distance, dy / distance)
                };
                point = clamp((f64::from(bx) + ux * gap, f64::from(by) + uy * gap));
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    point
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "clamped to the i32 range before the cast"
)]
fn round_to_i32(value: f64) -> i32 {
    value
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// The canvas's gesture, kept by the pane across recompositions so a drag
/// or a settle survives the page being built again.
#[derive(Clone, Debug, Default)]
pub(crate) struct RoomGesture {
    drag: Option<RoomDrag>,
    settle: Option<RoomSettle>,
    view: Option<ViewMotion>,
}

impl RoomGesture {
    /// Whether a speaker is held by the pointer or a finger, moved yet or
    /// not: the page is not built again under the hand.
    pub(crate) fn holding(&self) -> bool {
        self.drag.is_some()
    }

    /// Speaker `index` held at `at`, as a capture shows it mid drag: pulled
    /// onto the lines it is near when first drawn.
    #[must_use]
    pub(crate) fn held(index: usize, at: (i32, i32)) -> Self {
        Self {
            drag: Some(RoomDrag {
                index,
                grab: (0.0, 0.0),
                pressed_at: Point::ZERO,
                active: true,
                preview: at,
                guides: Vec::new(),
                touch: None,
                samples: [None, None],
                snapped_once: false,
            }),
            settle: None,
            view: None,
        }
    }
}

#[derive(Clone, Debug)]
struct RoomDrag {
    index: usize,
    /// From the speaker to where it was taken hold of, in millimetres.
    grab: (f64, f64),
    pressed_at: Point,
    /// Moved past the drag threshold: a press that never moves is a tap.
    active: bool,
    /// Where the held speaker stands: under the hand, pulled onto the lines
    /// it is near.
    preview: (i32, i32),
    guides: Vec<RoomGuide>,
    /// The finger holding it; `None` for the pointer.
    touch: Option<u64>,
    /// The last two places of the hand in the room and when, for the
    /// velocity the settle carries.
    samples: [Option<(Instant, (f64, f64))>; 2],
    /// A capture's held drag has been pulled onto its lines.
    snapped_once: bool,
}

impl RoomDrag {
    fn sample(&mut self, now: Instant, at: (f64, f64)) {
        self.samples = [self.samples[1], Some((now, at))];
    }

    /// Millimetres per second the hand moved at as it let go.
    fn velocity(&self) -> (f64, f64) {
        match self.samples {
            [Some((before, from)), Some((after, to))] => {
                let seconds = after
                    .checked_duration_since(before)
                    .map_or(0.0, |elapsed| elapsed.as_secs_f64());
                if seconds > f64::EPSILON && seconds < 0.1 {
                    ((to.0 - from.0) / seconds, (to.1 - from.1) / seconds)
                } else {
                    (0.0, 0.0)
                }
            }
            _ => (0.0, 0.0),
        }
    }
}

/// A dropped speaker springing from where it was let go to where it rests.
#[derive(Clone, Debug)]
struct RoomSettle {
    index: usize,
    from: (f64, f64),
    rest: (i32, i32),
    /// From 0 (let go) to 1 (at rest).
    progress: MotionValue<f64>,
}

pub(crate) struct RoomCanvas {
    /// Each speaker's stored place; written once, when a drag is let go.
    points: ui::LocalState<Vec<(i32, i32)>>,
    labels: Vec<String>,
    gesture: ui::LocalState<RoomGesture>,
    /// The speaker chosen here and in the pane's picker, by its index.
    selection: Option<ui::LocalState<String>>,
    rect: Rect,
    cursor: Option<Point>,
    hovered: Option<usize>,
    marker_radius: f64,
    snap_radius: f64,
    /// Room the canvas leaves on the right of a marker for its name: the
    /// widest name as drawn, measured on the first paint.
    label_room: f64,
    /// The smallest target a finger or pointer can hit.
    min_target: f64,
    motion: EffectiveMotion,
    /// What the canvas shows changed since its last paint.
    dirty: bool,
}

impl RoomCanvas {
    pub(crate) fn new(
        points: ui::LocalState<Vec<(i32, i32)>>,
        labels: Vec<String>,
        gesture: ui::LocalState<RoomGesture>,
    ) -> Self {
        Self {
            points,
            labels,
            gesture,
            selection: None,
            rect: Rect::ZERO,
            cursor: None,
            hovered: None,
            marker_radius: f64::from(ui::tokens::Metrics::BASE.icon_md) / 2.0,
            snap_radius: f64::from(ui::tokens::Metrics::BASE.icon_sm),
            label_room: 0.0,
            min_target: f64::from(ui::tokens::Metrics::BASE.min_pointer_target),
            motion: EffectiveMotion::default(),
            dirty: true,
        }
    }

    /// Share the selected speaker (its index) with the pane: pressing one
    /// selects it, and the selected one is drawn chosen.
    #[must_use]
    pub(crate) fn with_selection(mut self, selection: ui::LocalState<String>) -> Self {
        self.selection = Some(selection);
        self
    }

    fn selected(&self) -> Option<usize> {
        self.selection
            .as_ref()
            .and_then(|selection| selection.with(|selected| selected.parse().ok()))
    }

    /// The view that fits the speakers where they are stored.
    fn fitted(&self) -> View {
        View::fitting(
            self.stored()
                .into_iter()
                .map(|(x, y)| (f64::from(x), f64::from(y))),
        )
    }

    /// The part of the room shown at `now`: springing between framings,
    /// or fitted to the speakers at rest.
    fn view_at(&self, now: Instant) -> View {
        self.gesture.with(|gesture| match &gesture.view {
            Some(motion) => motion.from.toward(motion.to, motion.progress.get(now)),
            None => self.fitted(),
        })
    }

    /// Spring the view from where it is shown now to `to`.
    fn frame(&mut self, to: View) {
        let now = ui::motion::frame_now();
        let shown = self.view_at(now);
        let current = self
            .gesture
            .with(|gesture| gesture.view.as_ref().map(|motion| motion.to));
        if current == Some(to) || (current.is_none() && shown == to) {
            return;
        }
        let mut progress = MotionValue::new(0.0);
        progress.release(
            1.0,
            0.0,
            Snap::SPEC.timing(Direction::Enter, self.motion, MotionSource::Gesture),
            now,
        );
        self.gesture.update(|gesture| {
            gesture.view = Some(ViewMotion {
                from: shown,
                to,
                progress,
            });
        });
        self.dirty = true;
    }

    /// Where the room is drawn: inside the canvas, with room on the right
    /// of each marker for its name.
    fn inner(&self) -> Rect {
        let edge = self.marker_radius + 8.0;
        Rect::new(
            self.rect.x0 + edge,
            self.rect.y0 + edge,
            (self.rect.x1 - edge - self.label_room).max(self.rect.x0 + edge + 1.0),
            (self.rect.y1 - edge).max(self.rect.y0 + edge + 1.0),
        )
    }

    /// Millimetres per logical pixel and the room point at the canvas's
    /// middle, for the view shown at `now`.
    fn mapping(&self, now: Instant) -> (f64, (f64, f64)) {
        let view = self.view_at(now);
        let inner = self.inner();
        let scale = ((view.x1 - view.x0) / inner.width())
            .max((view.y1 - view.y0) / inner.height())
            .max(f64::EPSILON);
        (
            scale,
            (
                f64::midpoint(view.x0, view.x1),
                f64::midpoint(view.y0, view.y1),
            ),
        )
    }

    /// Millimetres per logical pixel, now.
    pub(crate) fn scale(&self) -> f64 {
        self.mapping(ui::motion::frame_now()).0
    }

    fn to_screen(&self, (x, y): (f64, f64)) -> Point {
        let (scale, (cx, cy)) = self.mapping(ui::motion::frame_now());
        let middle = self.inner().center();
        Point::new(middle.x + (x - cx) / scale, middle.y - (y - cy) / scale)
    }

    pub(crate) fn to_room(&self, point: Point) -> (f64, f64) {
        let (scale, (cx, cy)) = self.mapping(ui::motion::frame_now());
        let middle = self.inner().center();
        (
            cx + (point.x - middle.x) * scale,
            cy + (middle.y - point.y) * scale,
        )
    }

    /// The snapping threshold in millimetres: a fixed reach on screen.
    fn snap_threshold(&self) -> i32 {
        round_to_i32(self.snap_radius * self.scale())
    }

    /// How far apart two markers must stand, in millimetres: they never
    /// cover each other or the listener.
    fn gap(&self) -> i32 {
        round_to_i32(self.marker_radius * 2.0 * self.scale())
    }

    fn stored(&self) -> Vec<(i32, i32)> {
        self.points.with(Clone::clone)
    }

    fn others(&self, index: usize) -> Vec<(i32, i32)> {
        self.stored()
            .into_iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .map(|(_, point)| point)
            .collect()
    }

    /// Where each speaker is drawn this frame, in millimetres, and whether
    /// it is held.
    fn drawn_at(&self, now: Instant) -> Vec<((f64, f64), bool)> {
        let gesture = self.gesture.with(Clone::clone);
        let stored = self.stored();
        stored
            .iter()
            .enumerate()
            .map(|(index, &(x, y))| {
                if let Some(drag) = gesture.drag.as_ref().filter(|drag| drag.active)
                    && drag.index == index
                {
                    let (px, py) = drag.preview;
                    return ((f64::from(px), f64::from(py)), true);
                }
                if let Some(settle) = gesture.settle.as_ref().filter(|settle| {
                    settle.index == index && stored.get(index) == Some(&settle.rest)
                }) {
                    let progress = settle.progress.get(now);
                    let rest = (f64::from(settle.rest.0), f64::from(settle.rest.1));
                    return (
                        (
                            settle.from.0 + (rest.0 - settle.from.0) * progress,
                            settle.from.1 + (rest.1 - settle.from.1) * progress,
                        ),
                        false,
                    );
                }
                ((f64::from(x), f64::from(y)), false)
            })
            .collect()
    }

    /// The speaker drawn under `point`, nearest first, within reach of a
    /// finger as well as the pointer.
    fn speaker_at(&self, point: Point) -> Option<usize> {
        let reach = (self.marker_radius * 1.6).max(self.min_target / 2.0);
        self.drawn_at(ui::motion::frame_now())
            .iter()
            .enumerate()
            .map(|(index, (at, _))| (index, self.to_screen(*at).distance(point)))
            .filter(|(_, distance)| *distance <= reach)
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(index, _)| index)
    }

    /// A press or touch at `point`: the speaker there becomes the selected
    /// one and is picked up. Whether a speaker was there.
    fn press(&mut self, point: Point, touch: Option<u64>) -> bool {
        if self.gesture.with(|gesture| gesture.drag.is_some()) {
            return false;
        }
        let Some(index) = self.speaker_at(point) else {
            return false;
        };
        let selected = index.to_string();
        if let Some(selection) = &self.selection
            && selection.with(|current| *current != selected)
        {
            selection.set(selected);
        }
        let stored = self.stored();
        let Some(&(x, y)) = stored.get(index) else {
            return true;
        };
        let at = self.to_room(point);
        let now = ui::motion::frame_now();
        let mut drag = RoomDrag {
            index,
            grab: (at.0 - f64::from(x), at.1 - f64::from(y)),
            pressed_at: point,
            active: false,
            preview: (x, y),
            guides: Vec::new(),
            touch,
            samples: [None, None],
            snapped_once: true,
        };
        drag.sample(now, at);
        self.gesture.update(|gesture| {
            // Taking hold of a speaker stops any settle where it is drawn.
            gesture.settle = None;
            gesture.drag = Some(drag);
        });
        self.dirty = true;
        true
    }

    /// The hand moved to `point`: the held speaker follows it 1:1, pulled
    /// onto the lines it is near. Only the drawing changes.
    fn drag_to(&mut self, point: Point) -> bool {
        let Some(mut drag) = self.gesture.with(|gesture| gesture.drag.clone()) else {
            return false;
        };
        if !drag.active
            && (point - drag.pressed_at).hypot() < ui::widget::drag::REORDER_THRESHOLD_PX
        {
            return true;
        }
        drag.active = true;
        let at = self.to_room(point);
        drag.sample(ui::motion::frame_now(), at);
        let room = f64::from(ROOM_EXTENT_MM);
        let moved = (
            round_to_i32((at.0 - drag.grab.0).clamp(-room, room)),
            round_to_i32((at.1 - drag.grab.1).clamp(-room, room)),
        );
        let snapped = snap(moved, &self.others(drag.index), self.snap_threshold());
        drag.preview = snapped.at;
        drag.guides = snapped.guides;
        let preview = (f64::from(drag.preview.0), f64::from(drag.preview.1));
        self.gesture.update(|gesture| gesture.drag = Some(drag));
        // Near the edge the view grows to keep the speaker in sight, on the
        // settle spring; it never shrinks under the hand.
        let target = self
            .gesture
            .with(|gesture| gesture.view.as_ref().map(|motion| motion.to))
            .unwrap_or_else(|| self.view_at(ui::motion::frame_now()));
        let grown = target.holding(preview);
        if grown != target {
            self.frame(grown);
        }
        self.dirty = true;
        true
    }

    /// Let go: the held speaker settles into the nearest free place on a
    /// spring that carries the hand's velocity, and that place is stored
    /// once.
    fn release(&mut self) -> bool {
        let Some(drag) = self.gesture.with(|gesture| gesture.drag.clone()) else {
            return false;
        };
        self.gesture.update(|gesture| gesture.drag = None);
        self.dirty = true;
        if !drag.active {
            return true;
        }
        let rest = rest(drag.preview, &self.others(drag.index), self.gap());
        self.points.update(|points| {
            if let Some(point) = points.get_mut(drag.index) {
                *point = rest;
            }
        });
        let now = ui::motion::frame_now();
        let from = (f64::from(drag.preview.0), f64::from(drag.preview.1));
        let travel = Vec2::new(f64::from(rest.0) - from.0, f64::from(rest.1) - from.1);
        let distance = travel.hypot2();
        let mut progress = MotionValue::new(0.0);
        if distance > f64::EPSILON {
            let timing = Snap::SPEC.timing(Direction::Enter, self.motion, MotionSource::Gesture);
            // The hand's velocity along the way to the rest place, in
            // progress per second, carried in up to the spring's own rate:
            // a throw never overshoots, so there is no bounce.
            let (vx, vy) = drag.velocity();
            let along = travel.dot(Vec2::new(vx, vy)) / distance;
            let along = match timing {
                Timing::Spring(spring) => along.clamp(0.0, spring.omega()),
                Timing::Instant => 0.0,
            };
            progress.release(1.0, along, timing, now);
        } else {
            progress.snap(1.0);
        }
        self.gesture.update(|gesture| {
            gesture.settle = Some(RoomSettle {
                index: drag.index,
                from,
                rest,
                progress,
            });
        });
        // The view refits the set as it stands now.
        let fitted = self.fitted();
        self.frame(fitted);
        true
    }

    /// Drop a drag without moving anything (Escape, a cancelled touch).
    fn cancel(&mut self) -> bool {
        if self.gesture.with(|gesture| gesture.drag.is_none()) {
            return false;
        }
        self.gesture.update(|gesture| gesture.drag = None);
        self.dirty = true;
        true
    }

    fn hover(&mut self, point: Option<Point>) -> bool {
        let hovered = point
            .filter(|point| self.rect.contains(*point))
            .and_then(|point| self.speaker_at(point));
        if hovered == self.hovered {
            return false;
        }
        self.hovered = hovered;
        self.dirty = true;
        true
    }

    fn settling(&self, now: Instant) -> bool {
        self.gesture.with(|gesture| {
            gesture
                .settle
                .as_ref()
                .is_some_and(|settle| settle.progress.is_moving(now))
                || gesture
                    .view
                    .as_ref()
                    .is_some_and(|view| view.progress.is_moving(now))
        })
    }

    /// A capture's held drag, pulled onto the lines it is near the first
    /// time the canvas knows its size, as a real press and move would be.
    fn snap_held_capture(&self) {
        let Some(drag) = self
            .gesture
            .with(|gesture| gesture.drag.clone())
            .filter(|drag| !drag.snapped_once)
        else {
            return;
        };
        let snapped = snap(
            drag.preview,
            &self.others(drag.index),
            self.snap_threshold(),
        );
        self.gesture.update(|gesture| {
            if let Some(held) = gesture.drag.as_mut() {
                held.preview = snapped.at;
                held.guides = snapped.guides;
                held.snapped_once = true;
            }
        });
    }

    fn draw_room(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let colors = &theme.tokens.colors;
        let origin = self.to_screen((0.0, 0.0));
        for ring in [1_000, 2_000, 3_000, 4_000, 5_000] {
            // ratchet-allow(raw_paint): the room canvas draws the room and its speakers, content rather than chrome
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                colors.overlay_border_subtle,
                None,
                &Circle::new(origin, f64::from(ring) / self.scale()),
            );
        }
        // ratchet-allow(raw_paint): the room canvas draws the room and its speakers, content rather than chrome
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.overlay_border_subtle,
            None,
            &Line::new(
                Point::new(origin.x, self.rect.y0),
                Point::new(origin.x, self.rect.y1),
            ),
        );
    }

    /// The canvas's ground, a step above the page.
    fn ground(theme: &Theme) -> vello::peniko::Color {
        theme.tokens.colors.lift(0.1)
    }

    /// The lines a held speaker is snapped to.
    fn draw_guides(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let guides = self.gesture.with(|gesture| {
            gesture
                .drag
                .as_ref()
                .filter(|drag| drag.active)
                .map(|drag| drag.guides.clone())
                .unwrap_or_default()
        });
        // Each line runs the canvas's whole width or height, so it reads at
        // a glance, as a ruler's guide does.
        for guide in guides {
            let at = self.to_screen((f64::from(guide.at), f64::from(guide.at)));
            let (from, to) = if guide.vertical {
                (
                    Point::new(at.x, self.rect.y0),
                    Point::new(at.x, self.rect.y1),
                )
            } else {
                (
                    Point::new(self.rect.x0, at.y),
                    Point::new(self.rect.x1, at.y),
                )
            };
            ui::chrome::paint_alignment_guide(scene, transform, from, to, theme);
        }
    }

    /// The speakers, each with its name, the selected one ringed.
    fn draw_speakers(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        now: Instant,
    ) {
        let colors = &theme.tokens.colors;
        let caption = theme.text(ui::TextRole::Caption);
        let ground = Self::ground(theme);
        let selected = self.selected();
        let mut drawn: Vec<(usize, (f64, f64), bool)> = self
            .drawn_at(now)
            .into_iter()
            .enumerate()
            .map(|(index, (at, held))| (index, at, held))
            .collect();
        // The held speaker paints last so it rides above the others.
        drawn.sort_by_key(|(_, _, held)| *held);
        for (index, at, held) in drawn {
            let center = self.to_screen(at);
            // ratchet-allow(raw_paint): the room canvas draws the room and its speakers, content rather than chrome
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                colors.presence.series(index),
                None,
                &Circle::new(center, self.marker_radius),
            );
            let state = ui::chrome::SelectionState::new(
                selected == Some(index),
                self.hovered == Some(index),
                held,
            );
            if state.active || state.hover || state.pressed {
                // The design system's selection, as a ring round the marker.
                let ring = self.marker_radius + f64::from(theme.tokens.spacing.s1);
                ui::chrome::SelectionTier {
                    fill: vello::peniko::Color::TRANSPARENT,
                    ..ui::chrome::selection_tier_on(theme, state, ground)
                }
                .paint(
                    scene,
                    transform,
                    Rect::from_center_size(center, (ring * 2.0, ring * 2.0)),
                    ring,
                );
            }
            if let Some(label) = self.labels.get(index) {
                let ink = if selected == Some(index) {
                    colors.text_primary
                } else {
                    colors.text_dim
                };
                text_renderer.draw_style(
                    scene,
                    label,
                    ink,
                    transform.pre_translate(
                        (
                            center.x + self.marker_radius + f64::from(theme.tokens.spacing.s2),
                            center.y - f64::from(caption.size) / 2.0 - 1.0,
                        )
                            .into(),
                    ),
                    Some(theme.tokens.metrics.chooser_width),
                    &caption,
                );
            }
        }
    }

    fn position_text(&self, index: usize) -> String {
        self.stored()
            .get(index)
            .map(|&point| super::position_text(point))
            .unwrap_or_default()
    }
}

impl Widget for RoomCanvas {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.marker_radius = f64::from(theme.tokens.metrics.icon_md) / 2.0;
        self.snap_radius = f64::from(theme.tokens.metrics.icon_sm);
        self.min_target = f64::from(theme.tokens.metrics.min_pointer_target);
        self.motion = theme.motion();
        let node = ui::widget::TaffyTreeExt::leaf_node(
            taffy,
            taffy::Style {
                // As tall as a utility panel is wide: room for the speakers
                // around the listener without the page scrolling past it.
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::length(theme.tokens.metrics.panel_width_utility),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    /// Selection, hover and drag change the paint, not this leaf's size.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    ui::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        self.motion = theme.motion();
        let caption = theme.text(ui::TextRole::Caption);
        // The names are fixed for the canvas's life: measured on the first
        // paint, never per frame of a drag.
        if self.label_room <= 0.0 {
            self.label_room = self
                .labels
                .iter()
                .map(|label| f64::from(text_renderer.measure_style(label, None, &caption).0))
                .fold(0.0, f64::max)
                + f64::from(theme.tokens.spacing.s2);
        }
        self.snap_held_capture();
        let now = ui::motion::frame_now();
        let colors = &theme.tokens.colors;
        let radius = f64::from(theme.tokens.radii.panel);
        let ground = Self::ground(theme);
        ui::surface::card(scene, transform, self.rect, radius, ground);
        // A speaker held past the canvas's edge is cut there, never drawn
        // over the rows around it.
        ui::surface::push_card_clip(scene, transform, self.rect, radius);
        self.draw_room(scene, transform, theme);
        let origin = self.to_screen((0.0, 0.0));
        // ratchet-allow(raw_paint): the room canvas draws the room and its speakers, content rather than chrome
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.text_dim,
            None,
            &Circle::new(origin, f64::from(theme.tokens.spacing.s1)),
        );
        text_renderer.draw_style(
            scene,
            &t("speakers-room-you"),
            colors.text_dim,
            transform.pre_translate(
                (
                    origin.x + f64::from(theme.tokens.spacing.s2),
                    origin.y + f64::from(theme.tokens.spacing.s1),
                )
                    .into(),
            ),
            None,
            &caption,
        );
        self.draw_guides(scene, transform, theme);
        self.draw_speakers(scene, transform, text_renderer, theme, now);
        scene.pop_layer();
        // A settle that has come to rest is over: the canvas is idle again.
        let rested = self.gesture.with(|gesture| {
            gesture
                .settle
                .as_ref()
                .is_some_and(|settle| !settle.progress.is_moving(now))
        });
        if rested {
            self.gesture.update(|gesture| gesture.settle = None);
        }
        // A view that reached the fit of the set at rest is the fit again.
        let fitted = self.fitted();
        let framed = self.gesture.with(|gesture| {
            gesture.drag.is_none()
                && gesture
                    .view
                    .as_ref()
                    .is_some_and(|view| !view.progress.is_moving(now) && view.to == fitted)
        });
        if framed {
            self.gesture.update(|gesture| gesture.view = None);
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.settling(ui::motion::frame_now())
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    /// Frames while a dropped speaker settles; none at rest, none for a
    /// held drag (each move of the hand asks for its own frame).
    fn needs_animation_frame(&self) -> bool {
        self.settling(ui::motion::frame_now())
    }

    /// A held speaker keeps the pointer wherever it goes.
    fn wants_pointer_capture(&self) -> bool {
        self.gesture.with(|gesture| gesture.drag.is_some())
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let touch_held = self
            .gesture
            .with(|gesture| gesture.drag.as_ref().and_then(|drag| drag.touch));
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = ctx.logical(position);
                self.cursor = Some(point);
                if touch_held.is_some() {
                    // A finger holds the speaker; the pointer a host makes
                    // of the same finger follows it already.
                    *ctx.handled = true;
                } else if self.drag_to(point) || self.hover(Some(point)) {
                    *ctx.handled = true;
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.cursor = None;
                let _ = self.hover(None);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(point) = self.cursor
                    && self.rect.contains(point)
                    && self.press(point, None)
                {
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if touch_held.is_none() && self.release() {
                    *ctx.handled = true;
                }
            }
            WindowEvent::Touch(touch) => {
                let point = ctx.logical(touch.location);
                let handled = match touch.phase {
                    TouchPhase::Started => {
                        self.rect.contains(point) && self.press(point, Some(touch.id))
                    }
                    TouchPhase::Moved if touch_held == Some(touch.id) => self.drag_to(point),
                    TouchPhase::Ended if touch_held == Some(touch.id) => self.release(),
                    TouchPhase::Cancelled if touch_held == Some(touch.id) => self.cancel(),
                    _ => false,
                };
                if handled {
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape)
                    && self.cancel() =>
            {
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let selected = self.selected();
        let mut group = AccessNode::new(Role::Group)
            .with_name(t("speakers-room-name"))
            .with_description(t("speakers-layout-intro"))
            .with_bounds(self.rect);
        let drawn = self.drawn_at(ui::motion::frame_now());
        for (index, label) in self.labels.iter().enumerate() {
            let at = drawn.get(index).map_or((0.0, 0.0), |(at, _)| *at);
            let center = self.to_screen(at);
            group.children.push(
                AccessNode::new(Role::Image)
                    .with_name(label.clone())
                    .with_description(self.position_text(index))
                    .with_bounds(Rect::from_center_size(
                        center,
                        (self.marker_radius * 2.0, self.marker_radius * 2.0),
                    ))
                    .with_state(AccessState::new().selected(selected == Some(index))),
            );
        }
        Some(group)
    }

    fn name(&self) -> &'static str {
        "RoomCanvas"
    }

    fn test_id(&self) -> Option<&str> {
        Some("speakers.room")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_speaker_snaps_to_the_centre_line_and_the_mirror_of_another() {
        let near_centre = snap((120, 2_400), &[], 150);
        assert_eq!(near_centre.at, (0, 2_400));
        assert_eq!(
            near_centre.guides,
            [RoomGuide {
                vertical: true,
                at: 0,
            }]
        );
        // Near the mirror of a speaker on the left, and its row: a pair
        // standing symmetric.
        let left = (-1_500, 2_000);
        let pair = snap((1_430, 2_090), &[left], 150);
        assert_eq!(pair.at, (1_500, 2_000));
        assert_eq!(pair.guides.len(), 2);
        assert!(
            pair.guides
                .iter()
                .any(|guide| !guide.vertical && guide.at == 2_000)
        );
        assert!(
            pair.guides
                .iter()
                .any(|guide| guide.vertical && guide.at == 1_500)
        );
        // Far from every line: it stays where the hand is.
        assert_eq!(snap((900, 3_100), &[left], 150).at, (900, 3_100));
    }

    #[test]
    fn a_let_go_speaker_rests_inside_the_room_and_on_nothing_else() {
        let gap = 400;
        let others = [(0, 2_000)];
        // Dropped on another speaker: it moves out to touch it, no more.
        let dropped = rest((0, 2_100), &others, gap);
        let distance = f64::from(dropped.0).hypot(f64::from(dropped.1 - 2_000));
        assert!((distance - f64::from(gap)).abs() <= 1.0, "{dropped:?}");
        // Dropped on the listener: never on them.
        let on_you = rest((10, -10), &[], gap);
        assert!(f64::from(on_you.0).hypot(f64::from(on_you.1)) >= f64::from(gap) - 1.0);
        // Dropped beyond the room: back at its edge.
        assert_eq!(
            rest((ROOM_EXTENT_MM + 900, 1_000), &[], gap),
            (ROOM_EXTENT_MM, 1_000)
        );
        // Free: it stays.
        assert_eq!(rest((1_200, 1_800), &others, gap), (1_200, 1_800));
    }

    fn points() -> Vec<(i32, i32)> {
        vec![(-1_848, 765), (-765, 1_848), (765, 1_848), (1_848, 765)]
    }

    fn canvas() -> (RoomCanvas, ui::LocalState<String>) {
        let selection = ui::LocalState::new("0".to_owned());
        let mut canvas = RoomCanvas::new(
            ui::LocalState::new(points()),
            ["Desk", "Shelf", "Bookcase", "Kitchen"]
                .map(str::to_owned)
                .to_vec(),
            ui::LocalState::new(RoomGesture::default()),
        )
        .with_selection(selection.clone());
        canvas.rect = Rect::new(0.0, 0.0, 720.0, 320.0);
        (canvas, selection)
    }

    fn on_screen(canvas: &RoomCanvas, index: usize) -> Point {
        let (x, y) = points()[index];
        canvas.to_screen((f64::from(x), f64::from(y)))
    }

    #[test]
    fn the_canvas_maps_screen_and_room_both_ways() {
        let (canvas, _) = canvas();
        let screen = canvas.to_screen((1_500.0, -1_000.0));
        let back = canvas.to_room(screen);
        assert!(
            (back.0 - 1_500.0).abs() < 1.0 && (back.1 + 1_000.0).abs() < 1.0,
            "{back:?}"
        );
        assert!(
            canvas.to_screen((0.0, 2_000.0)).y < canvas.to_screen((0.0, 0.0)).y,
            "ahead is up"
        );
    }

    #[test]
    fn pressing_a_speaker_selects_it_in_the_selection_the_picker_shares() {
        let (mut canvas, selection) = canvas();
        assert!(canvas.press(on_screen(&canvas, 2), None));
        assert_eq!(selection.with(Clone::clone), "2");
        let node = canvas.accessibility().unwrap();
        assert!(
            node.children[2].state.selected,
            "the chosen speaker says so"
        );
        assert!(!node.children[0].state.selected);
        assert!(canvas.release(), "a press without a drag is just a choice");
        assert_eq!(canvas.stored(), points(), "and moves nothing");
        selection.set("3".into());
        assert!(canvas.accessibility().unwrap().children[3].state.selected);
    }

    #[test]
    fn a_drag_repaints_every_move_and_stores_only_on_release() {
        let (mut canvas, _) = canvas();
        let start = on_screen(&canvas, 1);
        assert!(canvas.press(start, None));
        canvas.mark_drawn();
        let stored = canvas.points.revision();
        for step in 1..=24 {
            let point = start + Vec2::new(4.0 * f64::from(step), 3.0 * f64::from(step));
            assert!(canvas.drag_to(point));
            assert!(canvas.needs_redraw(), "move {step} asks for its frame");
            assert!(!canvas.is_layout_dirty(), "move {step} lays nothing out");
            canvas.mark_drawn();
            assert_eq!(
                canvas.points.revision(),
                stored,
                "move {step} stores nothing"
            );
            // 1:1: the held speaker is where the hand put it, or pulled
            // onto a line within the snapping reach.
            let (held, _) = canvas.drawn_at(ui::motion::frame_now())[1];
            let hand = canvas.to_room(point);
            let grab = canvas.to_room(start);
            let free = (
                f64::from(points()[1].0) + hand.0 - grab.0,
                f64::from(points()[1].1) + hand.1 - grab.1,
            );
            let reach = f64::from(canvas.snap_threshold()) + 1.0;
            assert!(
                (held.0 - free.0).abs() <= reach && (held.1 - free.1).abs() <= reach,
                "move {step}: {held:?} strays from the hand at {free:?}"
            );
        }
        assert!(canvas.release());
        assert_eq!(
            canvas.points.revision(),
            stored + 1,
            "the place is stored once"
        );
        assert_ne!(canvas.stored()[1], points()[1]);
    }

    #[test]
    fn a_finger_drags_a_speaker_like_the_pointer() {
        fn touch(phase: TouchPhase, point: Point) -> WindowEvent {
            WindowEvent::Touch(winit::event::Touch {
                device_id: winit::event::DeviceId::dummy(),
                phase,
                location: winit::dpi::PhysicalPosition::new(point.x, point.y),
                force: None,
                id: 7,
            })
        }
        let drop = |by_touch: bool| {
            let (mut canvas, selection) = canvas();
            let start = on_screen(&canvas, 1);
            let path = [
                start,
                start + Vec2::new(-30.0, 20.0),
                start + Vec2::new(-80.0, 60.0),
            ];
            if by_touch {
                let send = |canvas: &mut RoomCanvas, event: &WindowEvent| {
                    ui::testing::Dispatch::new().send(canvas, event).handled
                };
                assert!(send(&mut canvas, &touch(TouchPhase::Started, path[0])));
                for point in &path[1..] {
                    assert!(send(&mut canvas, &touch(TouchPhase::Moved, *point)));
                }
                assert!(send(&mut canvas, &touch(TouchPhase::Ended, path[2])));
            } else {
                assert!(canvas.press(path[0], None));
                for point in &path[1..] {
                    assert!(canvas.drag_to(*point));
                }
                assert!(canvas.release());
            }
            (canvas.stored()[1], selection.with(Clone::clone))
        };
        let by_finger = drop(true);
        assert_eq!(by_finger, drop(false));
        assert_eq!(by_finger.1, "1", "the finger selected what it moved");
        assert_ne!(by_finger.0, points()[1], "and moved it");
    }

    #[test]
    fn a_dropped_speaker_springs_into_place_then_asks_for_nothing() {
        let (mut canvas, _) = canvas();
        assert!(!canvas.needs_animation_frame(), "idle costs nothing");
        let start = on_screen(&canvas, 1);
        assert!(canvas.press(start, None));
        // Let go on the speaker beside it: it rests clear of it.
        assert!(canvas.drag_to(on_screen(&canvas, 2)));
        let (held, _) = canvas.drawn_at(ui::motion::frame_now())[1];
        // The gap at the scale it was let go at: the view refits after.
        let gap = canvas.gap();
        assert!(canvas.release());
        let released = ui::motion::frame_now();
        let rest = canvas.stored()[1];
        let apart = f64::from(rest.0 - points()[2].0).hypot(f64::from(rest.1 - points()[2].1));
        assert!(
            apart >= f64::from(gap) - 1.0,
            "{rest:?} covers its neighbour"
        );
        assert!(canvas.needs_animation_frame(), "the settle asks for frames");
        let (start_at, _) = canvas.drawn_at(released)[1];
        assert!(
            (start_at.0 - held.0).abs() < 2.0 && (start_at.1 - held.1).abs() < 2.0,
            "it starts where it was let go: {start_at:?} vs {held:?}"
        );
        let later = released + ui::time::Duration::from_secs(2);
        let (end_at, _) = canvas.drawn_at(later)[1];
        assert!(
            (end_at.0 - f64::from(rest.0)).abs() < 1e-6
                && (end_at.1 - f64::from(rest.1)).abs() < 1e-6,
            "and rests where it settles"
        );
        assert!(!canvas.settling(later), "then it asks for no frames");
    }

    /// The view fits the speakers: they spread across the canvas rather
    /// than clustering in its middle, and a set standing close together is
    /// not magnified past a minimum.
    #[test]
    fn the_view_fits_the_speakers_and_grows_when_one_is_dragged_out() {
        let (mut canvas, _) = canvas();
        let inner = canvas.inner();
        let screen: Vec<Point> = (0..4).map(|index| on_screen(&canvas, index)).collect();
        let left = screen.iter().map(|point| point.x).fold(f64::MAX, f64::min);
        let right = screen.iter().map(|point| point.x).fold(f64::MIN, f64::max);
        let top = screen.iter().map(|point| point.y).fold(f64::MAX, f64::min);
        let listener = canvas.to_screen((0.0, 0.0));
        let used = (right - left).max(listener.y - top);
        assert!(
            used >= inner.width().min(inner.height()) * 0.6,
            "the set fills the canvas: {used} of {inner:?}"
        );
        let tight = View::fitting([(0.0, 300.0), (200.0, 300.0)]);
        assert!(tight.x1 - tight.x0 >= 2.0 * MIN_HALF_VIEW_MM - 1.0);

        let before = canvas.view_at(ui::motion::frame_now());
        let start = on_screen(&canvas, 3);
        assert!(canvas.press(start, None));
        assert!(canvas.drag_to(Point::new(canvas.rect.x1 - 2.0, start.y)));
        let target = canvas
            .gesture
            .with(|gesture| gesture.view.as_ref().map(|motion| motion.to))
            .expect("the view springs to a wider framing");
        assert!(target.x1 > before.x1, "{target:?} grows past {before:?}");
        assert!(canvas.needs_animation_frame(), "smoothly, on the spring");
        assert!(canvas.release());
    }
}
