//! Drag-to-arrange canvas for the Displays pane.
//!
//! Each attached, enabled display is drawn at its size on the shared desktop
//! (mode, rotation and scale), numbered the way the compositor numbers it
//! when asked to identify screens. The canvas is the page's one display
//! selection: pressing or tapping a display chooses it, as do the arrow
//! keys once the canvas has focus and a screen reader's activation of a
//! display's tile. Dragging one moves it 1:1 under the pointer or finger,
//! by transform alone: nothing is laid out or stored while it moves. Within
//! a small threshold its edges and centre snap magnetically onto another
//! display's, and an alignment guide shows each line it lines up with
//! ([`placement::snap_with_guides`]). On release it settles into the
//! nearest place where it touches another display and covers none
//! ([`placement::settle`]) with the paged-strip settle spring
//! (`motion::Snap`), carrying the hand's velocity; the drop is sent once, as
//! the output's exported `move` action, so a drag is exactly what an agent
//! or script can do. Ctrl with an arrow key moves the chosen display to the
//! next place that way ([`placement::step`]), settling as a drop does. At
//! rest the canvas asks for no frames.
//!
//! [`placement::snap_with_guides`]: crate::widget::display_arrangement::placement::snap_with_guides
//! [`placement::settle`]: crate::widget::display_arrangement::placement::settle
//! [`placement::step`]: crate::widget::display_arrangement::placement::step

pub use settings_store::display_placement as placement;
use tilo_sort::CompactSort as _;

use crate as ui;
use crate::color_compat::ColorCompat as _;
use crate::motion::{Direction, MotionValue, Snap, Transient};
use crate::num::f32_of_f64;
use crate::time::Instant;
use crate::tokens::{EffectiveMotion, MotionSource};
use crate::widget::{ModelPath, TaffyTreeExt};

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{ActionEmitter, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
use winit::event::{ElementState, MouseButton, TouchPhase, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::finger_path::FingerPath;
use placement::{Guide, Placement};

use crate::i18n::widget_format;

/// The action target of a display's tile, before its output key: a screen
/// reader activating it chooses that display.
const CHOOSE_TARGET: &str = "display:";

fn t(key: &str) -> String {
    widget_format(key, &[])
}

/// One display as the canvas draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct ArrangedDisplay {
    pub connector: String,
    /// Screens in one device move together without changing its native seat layout.
    pub movement_group: Option<String>,
    /// Certified shared identity and layout state; absent for native-only or legacy tiles.
    pub shared: Option<SharedDisplayState>,
    pub label: String,
    /// The number Identify shows on this display.
    pub number: usize,
    pub placement: Placement,
    pub primary: bool,
    /// Mirrors sit on their target and follow it, so only extended displays
    /// can be dragged.
    pub movable: bool,
}

/// State of one certified output in the person's shared layout.
/// Availability and inclusion are independent: an offline display keeps its place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedDisplayState {
    pub device: String,
    pub output: String,
    pub online: bool,
    pub included: bool,
}

impl ArrangedDisplay {
    fn status(&self) -> Option<String> {
        let shared = self.shared.as_ref()?;
        match (shared.online, shared.included) {
            (true, true) => None,
            (false, true) => Some(t("display-arrangement-offline")),
            (true, false) => Some(t("display-arrangement-excluded")),
            (false, false) => Some(t("display-arrangement-offline-excluded")),
        }
    }

    fn muted(&self) -> bool {
        self.shared
            .as_ref()
            .is_some_and(|shared| !shared.online || !shared.included)
    }
}

/// What the canvas is doing with the hand: a drag in progress, a dropped
/// display settling into place, and the part of the desktop the canvas
/// frames while either happens. Owned by the pane, not the widget, so it
/// survives the composition being rebuilt between frames (a selection or a
/// drop recomposes the pane).
#[derive(Clone, Debug, Default)]
pub struct ArrangementGesture {
    drag: Option<ArrangementDrag>,
    settle: Option<Settle>,
    /// The desktop rect the canvas frames, in desktop units: growing as a
    /// held display goes past the arrangement, settling back to fit it
    /// after the drop. `None` while the canvas simply fits the arrangement.
    view: Option<MotionValue<Rect>>,
}

impl ArrangementGesture {
    /// Whether a display is held by the pointer or a finger.
    pub fn dragging(&self) -> bool {
        self.drag.as_ref().is_some_and(|drag| drag.active)
    }

    /// A drag of `connector` held at `preview`, as a capture shows it mid
    /// drag: pulled onto the lines it is near and framed, as a real press
    /// and move would be.
    #[must_use]
    pub fn held(connector: &str, preview: Placement) -> Self {
        Self {
            drag: Some(ArrangementDrag {
                connector: connector.to_owned(),
                grab: (0.0, 0.0),
                pressed_at: Point::ZERO,
                pointer: None,
                held: Some(preview),
                active: true,
                touch: None,
                path_x: FingerPath::default(),
                path_y: FingerPath::default(),
            }),
            settle: None,
            view: None,
        }
    }
}

/// An in-progress drag.
#[derive(Clone, Debug)]
struct ArrangementDrag {
    connector: String,
    /// Where on the held display the hand took it, as a share of its width
    /// and height: that point stays under the hand however the canvas
    /// frames the desktop.
    grab: (f64, f64),
    pressed_at: Point,
    /// Where the hand is on the canvas; `None` for a capture's held drag.
    pointer: Option<Point>,
    /// A capture's held place, standing in for a hand.
    held: Option<Placement>,
    active: bool,
    /// The finger holding it; `None` for the pointer.
    touch: Option<u64>,
    /// The hand's path on the desktop, for the velocity the settle carries.
    path_x: FingerPath,
    path_y: FingerPath,
}

/// A dropped display springing from where it was let go to where it rests.
#[derive(Clone, Debug)]
struct Settle {
    connector: String,
    /// Where the display's top-left stood when it was let go.
    from: Point,
    /// Where it rests.
    placement: Placement,
    /// From 0 (let go) to 1 (at rest).
    progress: MotionValue<f64>,
}

/// Uniform scale plus offset from desktop units to canvas pixels:
/// `canvas = desktop * scale + offset` on each axis.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Mapping {
    scale: f64,
    offset: Vec2,
    snap_radius: f64,
}

impl Mapping {
    /// The mapping that shows `view` (desktop units) centred in `canvas`,
    /// inside `padding`.
    fn frame(canvas: Rect, view: Rect, padding: f64, snap_radius: f64) -> Self {
        let inner = canvas.inset(-padding);
        let scale = (inner.width() / view.width().max(1.0))
            .min(inner.height() / view.height().max(1.0))
            .max(f64::EPSILON);
        Self {
            scale,
            offset: Vec2::new(
                inner.center().x - view.center().x * scale,
                inner.center().y - view.center().y * scale,
            ),
            snap_radius,
        }
    }

    fn point(self, x: f64, y: f64) -> Point {
        Point::new(
            x * self.scale + self.offset.x,
            y * self.scale + self.offset.y,
        )
    }

    fn to_canvas(self, placement: Placement) -> Rect {
        self.at(placement, Vec2::ZERO)
    }

    /// `placement` moved by `shift` desktop units, on the canvas.
    fn at(self, placement: Placement, shift: Vec2) -> Rect {
        let top_left = self.point(
            f64::from(placement.x) + shift.x,
            f64::from(placement.y) + shift.y,
        );
        Rect::new(
            top_left.x,
            top_left.y,
            top_left.x + f64::from(placement.width) * self.scale,
            top_left.y + f64::from(placement.height) * self.scale,
        )
    }

    fn to_desktop(self, point: Point) -> (f64, f64) {
        (
            (point.x - self.offset.x) / self.scale,
            (point.y - self.offset.y) / self.scale,
        )
    }

    fn snap_threshold(self) -> i32 {
        round_to_i32(self.snap_radius / self.scale)
    }
}

/// `placement` as a desktop rect.
fn rect_of(placement: Placement) -> Rect {
    Rect::new(
        f64::from(placement.x),
        f64::from(placement.y),
        f64::from(placement.right()),
        f64::from(placement.bottom()),
    )
}

/// The desktop rect `displays` cover; a 1080p display's when there are none.
fn bounds_of<'a>(displays: impl IntoIterator<Item = &'a Placement>) -> Rect {
    displays
        .into_iter()
        .map(|placement| rect_of(*placement))
        .reduce(|all, one| all.union(one))
        .unwrap_or_else(|| Rect::new(0.0, 0.0, 1920.0, 1080.0))
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

pub struct DisplayArrangement {
    displays: Vec<ArrangedDisplay>,
    moves: Vec<(String, ActionEmitter<(i32, i32)>)>,
    gesture: ui::LocalState<ArrangementGesture>,
    /// The page's one display selection, chosen here.
    selection: Option<ui::LocalState<String>>,
    /// The display under the pointer, for the pane to flash its number on.
    pointed: Option<ui::LocalState<Option<String>>>,
    /// While Identify shows each display's number: when it showed and
    /// goes. The tiles show the same numbers in the accent meanwhile.
    identify: Option<(Instant, Instant)>,
    hovered: Option<String>,
    cursor: Option<Point>,
    padding: f64,
    snap_radius: f64,
    motion: EffectiveMotion,
    /// What the canvas shows changed since its last paint.
    dirty: bool,
    /// Keyboard focus: arrow keys move the selected display.
    focused: bool,
    rect: Rect,
}

impl DisplayArrangement {
    pub fn new(
        displays: Vec<ArrangedDisplay>,
        moves: Vec<(String, tilo_actions::ActionBinding<(i32, i32)>)>,
        gesture: ui::LocalState<ArrangementGesture>,
    ) -> Self {
        Self {
            displays,
            moves: moves
                .into_iter()
                .map(|(connector, binding)| (connector, ActionEmitter::new(binding)))
                .collect(),
            gesture,
            selection: None,
            pointed: None,
            identify: None,
            hovered: None,
            cursor: None,
            padding: f64::from(ui::tokens::Spacing::BASE.s6),
            snap_radius: f64::from(ui::tokens::Metrics::BASE.icon_sm),
            motion: EffectiveMotion::default(),
            dirty: true,
            focused: false,
            rect: Rect::ZERO,
        }
    }

    /// Share the selected display (by connector) with the pane: pressing a
    /// display selects it, and the selected one is drawn chosen.
    #[must_use]
    pub fn with_selection(mut self, selection: ui::LocalState<String>) -> Self {
        self.selection = Some(selection);
        self
    }

    /// Tell the pane which display the pointer is over, so it can flash that
    /// display's number on it.
    #[must_use]
    pub fn with_pointed(mut self, pointed: ui::LocalState<Option<String>>) -> Self {
        self.pointed = Some(pointed);
        self
    }

    /// Identify shows each display's number from `shown` until `until`;
    /// the tiles show theirs in the accent meanwhile.
    #[must_use]
    pub fn identifying(mut self, window: Option<(Instant, Instant)>) -> Self {
        self.identify = window;
        self
    }

    fn selected(&self) -> Option<String> {
        self.selection
            .as_ref()
            .map(|selection| selection.with(Clone::clone))
            .filter(|selected| !selected.is_empty())
    }

    /// The desktop rect the arrangement covers: what the canvas frames at
    /// rest, centred, inside its padding.
    fn fitted_view(&self) -> Rect {
        bounds_of(self.displays.iter().map(|display| &display.placement))
    }

    /// The desktop rect the canvas frames at `now`.
    fn view_at(&self, now: Instant) -> Rect {
        self.gesture
            .with(|gesture| gesture.view.as_ref().map(|view| view.get(now)))
            .unwrap_or_else(|| self.fitted_view())
    }

    /// Where the canvas maps the desktop this frame.
    fn mapping(&self) -> Mapping {
        self.mapping_at(crate::motion::frame_now())
    }

    fn mapping_at(&self, now: Instant) -> Mapping {
        Mapping::frame(self.rect, self.view_at(now), self.padding, self.snap_radius)
    }

    /// Where the held display stands under the hand when the canvas maps
    /// the desktop by `mapping`, pulled onto the lines it is near, and those
    /// lines. It never goes further from the others than its own size: a
    /// display cannot float detached, and the canvas need not grow past it.
    fn held_at(&self, drag: &ArrangementDrag, mapping: Mapping) -> (Placement, Vec<Guide>) {
        let Some(display) = self
            .displays
            .iter()
            .find(|display| display.connector == drag.connector)
        else {
            return (Placement::new(0, 0, 0, 0), Vec::new());
        };
        let moved = match (drag.held, drag.pointer) {
            (Some(held), _) => held,
            (None, Some(pointer)) if drag.active => {
                let (dx, dy) = mapping.to_desktop(pointer);
                display.placement.at(
                    round_to_i32(dx - drag.grab.0 * f64::from(display.placement.width)),
                    round_to_i32(dy - drag.grab.1 * f64::from(display.placement.height)),
                )
            }
            _ => return (display.placement, Vec::new()),
        };
        let others = self.others(&drag.connector);
        let group = self.group_bounds(&drag.connector, moved);
        let group = if others.is_empty() {
            group
        } else {
            let near = bounds_of(&others);
            let clamp = |value: i32, low: f64, high: f64| {
                round_to_i32(f64::from(value).clamp(low, high.max(low)))
            };
            group.at(
                clamp(group.x, near.x0 - f64::from(group.width), near.x1),
                clamp(group.y, near.y0 - f64::from(group.height), near.y1),
            )
        };
        let snapped = placement::snap_with_guides(group, &others, mapping.snap_threshold());
        (
            self.align_group(&drag.connector, moved, snapped.placement),
            snapped.guides,
        )
    }

    /// Grow the framed view so the held display stays in it: it springs
    /// out to take in the display wherever the hand holds it, and never
    /// shrinks while the display is held.
    fn follow_view(&self, now: Instant) {
        let Some(drag) = self
            .gesture
            .with(|gesture| gesture.drag.clone())
            .filter(|drag| drag.active)
        else {
            return;
        };
        let (held, _) = self.held_at(&drag, self.mapping_at(now));
        let wanted = self
            .fitted_view()
            .union(rect_of(self.group_bounds(&drag.connector, held)));
        let fitted = self.fitted_view();
        let timing = Snap::SPEC.timing(Direction::Enter, self.motion, MotionSource::Gesture);
        self.gesture.update(|gesture| {
            let view = gesture.view.get_or_insert_with(|| MotionValue::new(fitted));
            let target = view.target().union(wanted);
            if (target.area() - view.target().area()).abs() > 1.0 {
                view.set(target, timing, now);
            }
        });
    }

    /// Spring the framed view back to fit the arrangement.
    fn settle_view(&self, now: Instant) {
        let fitted = self.fitted_view();
        let timing = Snap::SPEC.timing(Direction::Enter, self.motion, MotionSource::Gesture);
        self.gesture.update(|gesture| {
            if let Some(view) = gesture.view.as_mut() {
                view.set(fitted, timing, now);
            }
        });
    }

    /// Whether `settle` still describes a display at the place it springs
    /// to: a drop the pane did not take (or another change since) ends it.
    fn settle_current(&self, settle: &Settle) -> bool {
        self.displays.iter().any(|display| {
            display.connector == settle.connector && display.placement == settle.placement
        })
    }

    fn moves_together(&self, connector: &str, display: &ArrangedDisplay) -> bool {
        display.connector == connector
            || self
                .displays
                .iter()
                .find(|item| item.connector == connector)
                .is_some_and(|item| {
                    item.movement_group.is_some() && item.movement_group == display.movement_group
                })
    }

    fn moved_placement(
        &self,
        connector: &str,
        destination: Placement,
        display: &ArrangedDisplay,
    ) -> Placement {
        let Some(selected) = self
            .displays
            .iter()
            .find(|item| item.connector == connector)
        else {
            return display.placement;
        };
        display.placement.at(
            display
                .placement
                .x
                .saturating_add(destination.x.saturating_sub(selected.placement.x)),
            display
                .placement
                .y
                .saturating_add(destination.y.saturating_sub(selected.placement.y)),
        )
    }

    /// Where each display is drawn this frame, in desktop units: the held
    /// preview for the one being dragged (and its device's other screens),
    /// the settling place for one just dropped, its stored placement for
    /// the rest; and whether it is held.
    fn drawn_at(&self, now: Instant) -> Vec<(ArrangedDisplay, Vec2, bool)> {
        let gesture = self.gesture.with(Clone::clone);
        let held = gesture
            .drag
            .as_ref()
            .filter(|drag| drag.active)
            .map(|drag| {
                (
                    drag.connector.clone(),
                    self.held_at(drag, self.mapping_at(now)).0,
                )
            });
        let settle = gesture
            .settle
            .as_ref()
            .filter(|settle| self.settle_current(settle))
            .map(|settle| {
                let progress = settle.progress.get(now);
                let rest = Point::new(f64::from(settle.placement.x), f64::from(settle.placement.y));
                (
                    settle.connector.clone(),
                    (settle.from - rest) * (1.0 - progress),
                )
            });
        self.displays
            .iter()
            .map(|display| match (&held, &settle) {
                (Some((connector, preview)), _) if self.moves_together(connector, display) => (
                    ArrangedDisplay {
                        placement: self.moved_placement(connector, *preview, display),
                        ..display.clone()
                    },
                    Vec2::ZERO,
                    true,
                ),
                (_, Some((connector, shift))) if self.moves_together(connector, display) => {
                    (display.clone(), *shift, false)
                }
                _ => (display.clone(), Vec2::ZERO, false),
            })
            .collect()
    }

    #[cfg(test)]
    fn drawn(&self) -> Vec<(ArrangedDisplay, bool)> {
        self.drawn_at(crate::motion::frame_now())
            .into_iter()
            .map(|(display, _, held)| (display, held))
            .collect()
    }

    fn others(&self, connector: &str) -> Vec<Placement> {
        self.displays
            .iter()
            .filter(|display| !self.moves_together(connector, display))
            .map(|display| display.placement)
            .collect()
    }

    fn group_bounds(&self, connector: &str, destination: Placement) -> Placement {
        let screens = self
            .displays
            .iter()
            .filter(|display| self.moves_together(connector, display))
            .map(|display| self.moved_placement(connector, destination, display))
            .collect::<Vec<_>>();
        let x = screens
            .iter()
            .map(|rect| rect.x)
            .min()
            .unwrap_or(destination.x);
        let y = screens
            .iter()
            .map(|rect| rect.y)
            .min()
            .unwrap_or(destination.y);
        let right = screens
            .iter()
            .map(|rect| rect.right())
            .max()
            .unwrap_or(destination.right());
        let bottom = screens
            .iter()
            .map(|rect| rect.bottom())
            .max()
            .unwrap_or(destination.bottom());
        Placement::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
    }

    fn align_group(
        &self,
        connector: &str,
        destination: Placement,
        settled: Placement,
    ) -> Placement {
        let bounds = self.group_bounds(connector, destination);
        destination.at(
            destination
                .x
                .saturating_add(settled.x.saturating_sub(bounds.x)),
            destination
                .y
                .saturating_add(settled.y.saturating_sub(bounds.y)),
        )
    }

    /// The display drawn under `point`, topmost first: the last drawn wins
    /// where rectangles meet.
    fn display_at(&self, point: Point) -> Option<ArrangedDisplay> {
        let mapping = self.mapping();
        self.displays
            .iter()
            .rev()
            .find(|display| mapping.to_canvas(display.placement).contains(point))
            .cloned()
    }

    /// A press or touch at `point`: the display there becomes the selected
    /// one, and one that can move is picked up. Whether a display was there.
    fn press(&mut self, point: Point) -> bool {
        self.press_with(point, None)
    }

    fn press_with(&mut self, point: Point, touch: Option<u64>) -> bool {
        if self.gesture.with(|gesture| gesture.drag.is_some()) {
            return false;
        }
        let Some(display) = self.display_at(point) else {
            return false;
        };
        if let Some(selection) = &self.selection
            && selection.with(|selected| *selected != display.connector)
        {
            selection.set(display.connector.clone());
        }
        self.dirty = true;
        if !display.movable {
            return true;
        }
        let now = crate::motion::frame_now();
        let mapping = self.mapping_at(now);
        let view = self.view_at(now);
        let (dx, dy) = mapping.to_desktop(point);
        let mut path_x = FingerPath::default();
        let mut path_y = FingerPath::default();
        path_x.push(now, dx);
        path_y.push(now, dy);
        let share = |offset: f64, length: i32| offset / f64::from(length.max(1));
        self.gesture.update(|gesture| {
            // Taking hold of a display stops any settle where it is drawn,
            // and the canvas keeps framing what it shows.
            gesture.settle = None;
            gesture.view = Some(MotionValue::new(view));
            gesture.drag = Some(ArrangementDrag {
                connector: display.connector.clone(),
                grab: (
                    share(dx - f64::from(display.placement.x), display.placement.width),
                    share(
                        dy - f64::from(display.placement.y),
                        display.placement.height,
                    ),
                ),
                pressed_at: point,
                pointer: Some(point),
                held: None,
                active: false,
                touch,
                path_x,
                path_y,
            });
        });
        true
    }

    /// The hand moved to `point`: the held display follows it 1:1, pulled
    /// onto the lines it is near, and the view grows to keep it in sight.
    /// Only the drawing changes.
    fn drag_to(&mut self, point: Point) -> bool {
        let Some(drag) = self.gesture.with(|gesture| gesture.drag.clone()) else {
            return false;
        };
        if !drag.active
            && (point - drag.pressed_at).hypot() < ui::widget::drag::REORDER_THRESHOLD_PX
        {
            return true;
        }
        let now = crate::motion::frame_now();
        let (dx, dy) = self.mapping_at(now).to_desktop(point);
        self.gesture.update(|gesture| {
            if let Some(drag) = gesture.drag.as_mut() {
                drag.active = true;
                drag.pointer = Some(point);
                drag.path_x.push(now, dx);
                drag.path_y.push(now, dy);
            }
        });
        self.follow_view(now);
        self.dirty = true;
        true
    }

    /// Let go: the held display settles into the nearest valid place on a
    /// spring that carries the hand's velocity, the view settles back to fit
    /// the arrangement, and that place is sent once as the display's move.
    fn release(&mut self) -> bool {
        let Some(drag) = self.gesture.with(|gesture| gesture.drag.clone()) else {
            return false;
        };
        let now = crate::motion::frame_now();
        let mapping = self.mapping_at(now);
        let (preview, _) = self.held_at(&drag, mapping);
        self.gesture.update(|gesture| gesture.drag = None);
        self.dirty = true;
        if !drag.active {
            self.settle_view(now);
            return true;
        }
        let bounds = placement::settle(
            self.group_bounds(&drag.connector, preview),
            &self.others(&drag.connector),
            mapping.snap_threshold(),
        );
        let settled = self.align_group(&drag.connector, preview, bounds);
        let velocity = Vec2::new(
            drag.path_x.release_velocity(now),
            drag.path_y.release_velocity(now),
        );
        self.place(&drag.connector, preview, settled, velocity, now);
        true
    }

    /// A key on the focused canvas: an arrow chooses the next display that
    /// way; with Ctrl it moves the chosen display instead (as Ctrl+arrow
    /// moves a Quick Settings tile). Whether the canvas took the key: an
    /// arrow with nowhere to go is still the canvas's, so focus does not
    /// wander off on it.
    fn key(&mut self, key: &Key, control: bool) -> bool {
        let way = match key {
            Key::Named(NamedKey::ArrowLeft) => placement::Step::Left,
            Key::Named(NamedKey::ArrowRight) => placement::Step::Right,
            Key::Named(NamedKey::ArrowUp) => placement::Step::Up,
            Key::Named(NamedKey::ArrowDown) => placement::Step::Down,
            _ => return false,
        };
        if control {
            self.step(way);
        } else {
            self.select_toward(way);
        }
        true
    }

    /// Choose the display nearest the chosen one's centre that way (the
    /// first display when none is chosen). Whether the choice changed.
    fn select_toward(&mut self, way: placement::Step) -> bool {
        let Some(selection) = self.selection.clone() else {
            return false;
        };
        let centre = |placement: Placement| {
            (
                f64::from(placement.x) + f64::from(placement.width) / 2.0,
                f64::from(placement.y) + f64::from(placement.height) / 2.0,
            )
        };
        let current = self.selected().and_then(|connector| {
            self.displays
                .iter()
                .find(|display| display.connector == connector)
                .map(|display| (connector, centre(display.placement)))
        });
        let next = match current {
            None => self
                .displays
                .first()
                .map(|display| display.connector.clone()),
            Some((connector, (cx, cy))) => self
                .displays
                .iter()
                .filter(|display| display.connector != connector)
                .filter_map(|display| {
                    let (x, y) = centre(display.placement);
                    let (along, across) = match way {
                        placement::Step::Left => (cx - x, y - cy),
                        placement::Step::Right => (x - cx, y - cy),
                        placement::Step::Up => (cy - y, x - cx),
                        placement::Step::Down => (y - cy, x - cx),
                    };
                    // Ahead that way; straight ahead before off to a side.
                    (along > 0.0).then(|| (along + 2.0 * across.abs(), display))
                })
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, display)| display.connector.clone()),
        };
        let Some(next) = next else {
            return false;
        };
        if selection.with(|selected| *selected == next) {
            return false;
        }
        selection.set(next);
        self.dirty = true;
        true
    }

    /// Choose `connector`, as a screen reader's activation of its tile does.
    fn choose(&mut self, connector: &str) -> bool {
        let Some(selection) = &self.selection else {
            return false;
        };
        if !self
            .displays
            .iter()
            .any(|display| display.connector == connector)
        {
            return false;
        }
        if selection.with(|selected| selected != connector) {
            selection.set(connector.to_owned());
            self.dirty = true;
        }
        true
    }

    /// An arrow key moves the selected display to the next place that way
    /// ([`placement::step`]), settling there as a drop does. Whether it
    /// moved.
    fn step(&mut self, way: placement::Step) -> bool {
        let Some(connector) = self.selected() else {
            return false;
        };
        if self.gesture.with(ArrangementGesture::dragging) {
            return false;
        }
        let Some(from) = self
            .displays
            .iter()
            .find(|display| display.connector == connector)
            .map(|display| display.placement)
        else {
            return false;
        };
        let Some(bounds) = placement::step(
            self.group_bounds(&connector, from),
            &self.others(&connector),
            way,
        ) else {
            return false;
        };
        let settled = self.align_group(&connector, from, bounds);
        let now = crate::motion::frame_now();
        self.place(&connector, from, settled, Vec2::ZERO, now);
        self.dirty = true;
        true
    }

    /// Send `connector`'s new place once and spring it there from `from`,
    /// carrying `velocity` (desktop units per second), then frame the view
    /// to the new arrangement.
    fn place(
        &mut self,
        connector: &str,
        from: Placement,
        settled: Placement,
        velocity: Vec2,
        now: Instant,
    ) {
        let unchanged = self
            .displays
            .iter()
            .any(|display| display.connector == connector && display.placement == settled);
        if !unchanged
            && let Some((_, emitter)) = self
                .moves
                .iter_mut()
                .find(|(candidate, _)| *candidate == connector)
        {
            let _ = emitter.emit((settled.x, settled.y));
        }
        let positions = self
            .displays
            .iter()
            .map(|display| {
                if self.moves_together(connector, display) {
                    self.moved_placement(connector, settled, display)
                } else {
                    display.placement
                }
            })
            .collect::<Vec<_>>();
        for (display, position) in self.displays.iter_mut().zip(positions) {
            display.placement = position;
        }
        let from = Point::new(f64::from(from.x), f64::from(from.y));
        let travel = Point::new(f64::from(settled.x), f64::from(settled.y)) - from;
        let distance = travel.hypot2();
        let mut progress = MotionValue::new(0.0);
        let timing = Snap::SPEC.timing(Direction::Enter, self.motion, MotionSource::Gesture);
        if distance > f64::EPSILON {
            // The hand's velocity along the way to the rest place, in
            // progress per second. A critically damped settle passes its
            // rest only when thrown faster than its own rate, so a throw
            // carries in up to that rate and never overshoots: no bounce.
            let along = travel.dot(velocity) / distance;
            let along = match timing {
                crate::motion::Timing::Spring(spring) => along.clamp(0.0, spring.omega()),
                crate::motion::Timing::Instant => 0.0,
            };
            progress.release(1.0, along, timing, now);
        } else {
            progress.snap(1.0);
        }
        self.gesture.update(|gesture| {
            gesture.settle = Some(Settle {
                connector: connector.to_owned(),
                from,
                placement: settled,
                progress,
            });
        });
        self.settle_view(now);
    }

    /// Drop a drag without moving anything (Escape, a cancelled touch).
    fn cancel(&mut self) -> bool {
        if self.gesture.with(|gesture| gesture.drag.is_none()) {
            return false;
        }
        self.gesture.update(|gesture| gesture.drag = None);
        self.settle_view(crate::motion::frame_now());
        self.dirty = true;
        true
    }

    /// The pointer is over `point` (or left the canvas): the display under
    /// it is told to the pane once per display. Whether that changed.
    fn hover(&mut self, point: Option<Point>) -> bool {
        let hovered = point
            .filter(|point| self.rect.contains(*point))
            .and_then(|point| self.display_at(point))
            .map(|display| display.connector);
        if hovered == self.hovered {
            return false;
        }
        self.hovered.clone_from(&hovered);
        if let Some(pointed) = &self.pointed {
            pointed.set(hovered);
        }
        self.dirty = true;
        true
    }

    fn settling(&self, now: Instant) -> bool {
        self.gesture.with(|gesture| {
            gesture
                .settle
                .as_ref()
                .is_some_and(|settle| self.settle_current(settle) && settle.progress.is_moving(now))
                || gesture
                    .view
                    .as_ref()
                    .is_some_and(|view| view.is_moving(now))
        })
    }

    fn identify_moving(&self, now: Instant) -> bool {
        self.identify.is_some_and(|window| {
            Transient::is_moving(Transient::READOUT, window, now, self.motion)
        })
    }

    /// How strongly the tiles' numbers take the accent: with Identify's
    /// cards, arriving and fading as they do.
    fn identify_level(&self, now: Instant) -> f32 {
        self.identify.map_or(0.0, |window| {
            Transient::opacity(Transient::READOUT, window, now, self.motion)
        })
    }

    fn draw_display(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        display: &ArrangedDisplay,
        state: TileState,
    ) {
        let colors = &theme.tokens.colors;
        let ground = colors.lift(0.1);
        let radius = f64::from(theme.tokens.radii.control);
        let selection = ui::chrome::SelectionState::new(state.selected, state.hovered, state.held);
        // A tile is a control with its own floor; the chosen one takes the
        // design system's selection: the accent-tinted lift with its
        // hairline and glow.
        let tier = ui::chrome::selection_tier_on(theme, selection, ground);
        let fill = if display.muted() {
            colors.lift_on(ground, 0.1)
        } else if state.selected {
            tier.fill
        } else if display.movable {
            ui::chrome::selection_floor_ramp(theme, selection, ground)
        } else {
            colors.lift_on(ground, ui::chrome::shell::FLOOR_IDLE_T / 2.0)
        };
        let shape = RoundedRect::from_rect(rect, radius);
        scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
        if state.selected || state.held {
            ui::chrome::SelectionTier {
                fill: vello::peniko::Color::TRANSPARENT,
                ..tier
            }
            .paint(scene, transform, rect, radius);
        } else {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                ui::surface::edge_on(fill),
                None,
                &ui::surface::edge_outline(rect, radius),
            );
        }
        let number = display.number.to_string();
        let number_style = theme.text(ui::TextRole::Title).strong().numeric();
        let (nw, nh) = text_renderer.measure_style(&number, None, &number_style);
        let number_at = transform.pre_translate(
            (
                rect.center().x - f64::from(nw) / 2.0,
                rect.center().y - f64::from(nh) / 2.0 - f64::from(theme.tokens.spacing.s1),
            )
                .into(),
        );
        let ink = if display.muted() {
            colors.text_dim
        } else {
            colors.text_primary
        };
        if state.identify < 1.0 {
            text_renderer.draw_style(scene, &number, ink, number_at, None, &number_style);
        }
        if state.identify > 0.0 {
            text_renderer.draw_style(
                scene,
                &number,
                colors.intent.accent_ink.with_alpha_factor(state.identify),
                number_at,
                None,
                &number_style,
            );
        }
        let caption_style = theme.text(ui::TextRole::Caption);
        let caption = display.status().map_or_else(
            || display.label.clone(),
            |status| {
                widget_format(
                    "display-arrangement-caption-status",
                    &[
                        ("status", status.into()),
                        ("name", display.label.clone().into()),
                    ],
                )
            },
        );
        let label = text_renderer.elide_to_width_style(
            &caption,
            f32_of_f64(rect.width() - f64::from(theme.tokens.spacing.s3)),
            &caption_style,
        );
        let (lw, _) = text_renderer.measure_style(&label, None, &caption_style);
        text_renderer.draw_style(
            scene,
            &label,
            colors.text_dim,
            transform.pre_translate(
                (
                    rect.center().x - f64::from(lw) / 2.0,
                    rect.center().y + f64::from(nh) / 2.0 - 4.0,
                )
                    .into(),
            ),
            None,
            &caption_style,
        );
        if display.primary {
            // Primary marker: a small bar along the top edge, the place the
            // top bar lives on that display.
            let bar = Rect::new(rect.x0 + 6.0, rect.y0 + 5.0, rect.x1 - 6.0, rect.y0 + 8.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.text_primary,
                None,
                &RoundedRect::from_rect(bar, f64::from(theme.tokens.radii.control)),
            );
        }
    }
}

/// How one tile stands this frame.
#[derive(Clone, Copy, Debug, Default)]
struct TileState {
    selected: bool,
    hovered: bool,
    held: bool,
    /// How strongly its number takes the accent (Identify showing).
    identify: f32,
}

impl Widget for DisplayArrangement {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.padding = f64::from(theme.tokens.spacing.s6);
        self.snap_radius = f64::from(theme.tokens.metrics.icon_sm);
        self.motion = theme.motion();
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(theme.tokens.metrics.panel_width_utility * 9.0 / 8.0),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    /// Value, hover and drag change the paint, not this leaf's intrinsic size.
    /// Theme, viewport and input-capability changes rebuild the host layout.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.motion = theme.motion();
        let now = crate::motion::frame_now();
        let colors = &theme.tokens.colors;
        let radius = f64::from(theme.tokens.radii.panel);
        let ground = colors.lift(0.1);
        ui::surface::card(scene, transform, self.rect, radius, ground);
        // A held display keeps the view growing to take it in (a capture's
        // held drag is framed so on its first frame).
        self.follow_view(now);
        // A display held past the canvas's edge is cut there, never drawn
        // over the rows around it.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &RoundedRect::from_rect(self.rect, radius),
        );
        let mapping = self.mapping_at(now);
        let selected = self.selected();
        let identify = self.identify_level(now);
        let mut drawn = self.drawn_at(now);
        // The held display paints last so it rides above the others.
        drawn.compact_sort_by_key(|(_, _, held)| *held);
        for (display, shift, held) in &drawn {
            let rect = mapping.at(display.placement, *shift);
            let chosen = selected.as_deref() == Some(display.connector.as_str());
            if chosen && self.focused {
                crate::widget::paint_focus_ring(
                    scene,
                    transform,
                    rect,
                    f64::from(theme.tokens.radii.control),
                    theme,
                );
            }
            Self::draw_display(
                scene,
                transform,
                text_renderer,
                theme,
                rect,
                display,
                TileState {
                    selected: chosen,
                    hovered: self.hovered.as_deref() == Some(display.connector.as_str()),
                    held: *held,
                    identify,
                },
            );
        }
        let guides = self
            .gesture
            .with(|gesture| gesture.drag.clone())
            .filter(|drag| drag.active)
            .map(|drag| self.held_at(&drag, mapping).1)
            .unwrap_or_default();
        let reach = f64::from(theme.tokens.spacing.s2);
        for guide in guides {
            let at = f64::from(guide.at);
            let (from, to) = if guide.vertical {
                (
                    mapping.point(at, f64::from(guide.from)) - Vec2::new(0.0, reach),
                    mapping.point(at, f64::from(guide.to)) + Vec2::new(0.0, reach),
                )
            } else {
                (
                    mapping.point(f64::from(guide.from), at) - Vec2::new(reach, 0.0),
                    mapping.point(f64::from(guide.to), at) + Vec2::new(reach, 0.0),
                )
            };
            ui::chrome::paint_alignment_guide(scene, transform, from, to, theme);
        }
        scene.pop_layer();
        // A settle that has come to rest is over: the canvas is idle again.
        let rested = self.gesture.with(|gesture| {
            gesture.settle.as_ref().is_some_and(|settle| {
                !settle.progress.is_moving(now) || !self.settle_current(settle)
            })
        });
        if rested {
            self.gesture.update(|gesture| gesture.settle = None);
        }
        // So is a view back at the arrangement's fit with nothing held.
        let fitted = self.fitted_view();
        let framed = self.gesture.with(|gesture| {
            gesture.drag.is_none()
                && gesture
                    .view
                    .as_ref()
                    .is_some_and(|view| !view.is_moving(now) && view.target() == fitted)
        });
        if framed {
            self.gesture.update(|gesture| gesture.view = None);
        }
        if drawn.is_empty() {
            let message = t("display-arrangement-empty");
            let style = theme.text(ui::TextRole::Body);
            let (w, h) = text_renderer.measure_style(&message, None, &style);
            text_renderer.draw_style(
                scene,
                &message,
                colors.text_dim,
                transform.pre_translate(
                    (
                        self.rect.center().x - f64::from(w) / 2.0,
                        self.rect.center().y - f64::from(h) / 2.0,
                    )
                        .into(),
                ),
                None,
                &style,
            );
        }
    }

    /// A screen reader's action on a display's tile: activating or focusing
    /// it chooses that display.
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        let Some(connector) = target.strip_prefix(CHOOSE_TARGET) else {
            return false;
        };
        matches!(action, ui::a11y::Action::Activate | ui::a11y::Action::Focus)
            && self.choose(connector)
    }

    /// Focusable when there is a display to choose or move: arrow keys
    /// choose, and with Ctrl move, the chosen one.
    fn is_focusable(&self) -> bool {
        self.displays.len() > 1 && (self.selection.is_some() || !self.moves.is_empty())
    }

    fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn needs_redraw(&self) -> bool {
        let now = crate::motion::frame_now();
        self.dirty || self.settling(now) || self.identify_moving(now)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    /// Frames while a dropped display settles, the view springs to frame
    /// the arrangement, or Identify's numbers fade; none at rest, and a held
    /// drag otherwise asks for its frames by each move of the hand.
    fn needs_animation_frame(&self) -> bool {
        let now = crate::motion::frame_now();
        self.settling(now) || self.identify_moving(now)
    }

    /// While Identify's numbers are held: when they start to fade, then
    /// when they go.
    fn next_wake_deadline(&self) -> Option<Instant> {
        let (_, until) = self.identify?;
        let now = crate::motion::frame_now();
        let fade = Transient::exit_starts(Transient::READOUT, until, self.motion);
        if now < fade {
            Some(fade)
        } else if now < until {
            Some(until)
        } else {
            None
        }
    }

    fn serve_deadline(&mut self, _now: Instant) -> bool {
        self.dirty = true;
        true
    }

    /// A held display keeps the pointer wherever it goes.
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
                    // A finger holds the display; the pointer a host makes
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
                    && self.press(point)
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
                        self.rect.contains(point) && self.press_with(point, Some(touch.id))
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
            WindowEvent::KeyboardInput { event, .. }
                if self.focused && event.state == ElementState::Pressed =>
            {
                let modifiers = ctx.modifiers.state();
                if self.key(&event.logical_key, modifiers.control_key()) {
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        let mapping = self.mapping();
        let selected = self.selected();
        let mut group = AccessNode::new(Role::Group)
            .with_name(t("display-arrangement-name"))
            .with_description(t("display-arrangement-description"))
            .with_bounds(self.rect);
        for display in &self.displays {
            let key = if display.primary {
                "display-arrangement-item-primary"
            } else {
                "display-arrangement-item"
            };
            let chosen = selected.as_deref() == Some(display.connector.as_str());
            let node = AccessNode::new(if self.selection.is_some() {
                Role::RadioButton
            } else {
                Role::Image
            })
            .with_name(widget_format(
                key,
                &[
                    (
                        "number",
                        ui::i18n::ArgValue::Text(display.number.to_string()),
                    ),
                    ("name", ui::i18n::ArgValue::Text(display.label.clone())),
                ],
            ))
            .with_description(widget_format(
                "display-arrangement-item-position",
                &[
                    (
                        "x",
                        ui::i18n::ArgValue::Text(display.placement.x.to_string()),
                    ),
                    (
                        "y",
                        ui::i18n::ArgValue::Text(display.placement.y.to_string()),
                    ),
                    (
                        "width",
                        ui::i18n::ArgValue::Text(display.placement.width.to_string()),
                    ),
                    (
                        "height",
                        ui::i18n::ArgValue::Text(display.placement.height.to_string()),
                    ),
                ],
            ))
            .with_bounds(mapping.to_canvas(display.placement))
            .with_state(ui::a11y::AccessState::new().selected(chosen));
            // A screen reader chooses a display by activating its tile.
            let node = if self.selection.is_some() {
                let node = node.with_action_target(format!("{CHOOSE_TARGET}{}", display.connector));
                let state = node.state.checked(chosen);
                node.with_state(state)
            } else {
                node
            };
            let mut node = if let Some(status) = display.status() {
                let position = node.description.clone().unwrap_or_default();
                node.with_description(widget_format(
                    "display-arrangement-item-status",
                    &[("position", position.into()), ("status", status.into())],
                ))
            } else {
                node
            };
            if let Some(shared) = &display.shared {
                // Length-prefix the device component so separators inside IDs cannot alias.
                node = node.with_semantic_key(format!(
                    "shared-display:{}:{}{}",
                    shared.device.len(),
                    shared.device,
                    shared.output
                ));
            }
            group.children.push(node);
        }
        Some(group)
    }

    fn name(&self) -> &'static str {
        "DisplayArrangement"
    }

    fn test_id(&self) -> Option<&str> {
        Some("display.arrangement")
    }
}

impl ui::widget::controls::ActionControls for DisplayArrangement {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.moves
            .iter_mut()
            .find_map(|(_, emitter)| emitter.take())
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        for (_, emitter) in &mut self.moves {
            emitter.refresh_availability(allowed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, tilo_resource::Effect)]
    #[effect(operation = "test.display")]
    enum TestEffect {
        #[effect(returns, row)]
        MoveDisplay { display: String, x: i32, y: i32 },
    }

    fn displays() -> Vec<ArrangedDisplay> {
        vec![
            ArrangedDisplay {
                connector: "eDP-1".into(),
                movement_group: None,
                shared: None,
                label: "Built-in display".into(),
                number: 1,
                placement: Placement::new(0, 0, 1920, 1080),
                primary: true,
                movable: true,
            },
            ArrangedDisplay {
                connector: "HDMI-A-1".into(),
                movement_group: None,
                shared: None,
                label: "Desk monitor".into(),
                number: 2,
                placement: Placement::new(1920, 0, 1920, 1080),
                primary: false,
                movable: true,
            },
        ]
    }

    fn canvas() -> DisplayArrangement {
        let moves = displays()
            .iter()
            .map(|display| {
                use tilo_resource::Effect as _;
                let spec = TestEffect::declarations()
                    .into_iter()
                    .find(|decl| decl.id == "move-display")
                    .expect("moving a display is an effect")
                    .spec("test.display");
                let resource =
                    ui::tilo_actions::Resource::new("settings.root", "settings.root", "Settings")
                        .with_action(spec.clone());
                let connector = display.connector.clone();
                let binding = ui::tilo_actions::ActionBinding::fallible(
                    &resource,
                    &spec,
                    move |(x, y): (i32, i32)| {
                        TestEffect::MOVE_DISPLAY.arguments(&(connector.clone(), x, y))
                    },
                )
                .expect("the move effect is exported");
                (display.connector.clone(), binding)
            })
            .collect();
        let mut canvas = DisplayArrangement::new(
            displays(),
            moves,
            ui::LocalState::new(ArrangementGesture::default()),
        );
        canvas.rect = Rect::new(0.0, 0.0, 600.0, 220.0);
        canvas
    }

    #[test]
    fn certified_shared_tiles_expose_independent_availability_and_inclusion() {
        let mut canvas = canvas();
        canvas.displays[0].shared = Some(SharedDisplayState {
            device: "laptop".into(),
            output: "edid:panel".into(),
            online: false,
            included: true,
        });
        canvas.displays[1].shared = Some(SharedDisplayState {
            device: "desk".into(),
            output: "edid:panel".into(),
            online: true,
            included: false,
        });
        let before = canvas.accessibility().unwrap();
        assert!(
            before.children[0]
                .description
                .as_ref()
                .unwrap()
                .contains("Offline")
        );
        assert!(
            !before.children[0]
                .description
                .as_ref()
                .unwrap()
                .contains("Not in layout")
        );
        assert!(
            before.children[1]
                .description
                .as_ref()
                .unwrap()
                .contains("Not in layout")
        );
        assert!(
            !before.children[1]
                .description
                .as_ref()
                .unwrap()
                .contains("Offline")
        );
        assert_ne!(
            before.children[0].semantic_key,
            before.children[1].semantic_key
        );
        let mapping = canvas.mapping();
        assert!(canvas.press(mapping.to_canvas(canvas.displays[1].placement).center()));
        assert!(canvas.drag_to(mapping.to_canvas(canvas.displays[0].placement).center()));
        let preview = canvas.drawn();
        assert_eq!(preview[1].0.shared, canvas.displays[1].shared);
        let after = canvas.accessibility().unwrap();
        assert_eq!(
            before.children[1].semantic_key,
            after.children[1].semantic_key
        );
        canvas.displays[1].shared.as_mut().unwrap().online = false;
        let offline_excluded = canvas.accessibility().unwrap();
        let description = offline_excluded.children[1].description.as_ref().unwrap();
        assert!(description.contains("Offline") && description.contains("not in the layout"));
    }

    #[test]
    fn retained_arrangement_hover_preserves_geometry_without_remeasurement() {
        let mut host =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(canvas()), (800, 600));
        host.profile_frame();
        assert!(!host.last_relayout().measured);
        let rect = host
            .targets()
            .get("display.arrangement")
            .copied()
            .expect("the actual arrangement is mounted");
        host.send(&crate::synthetic_input::cursor_moved(
            rect.center().x,
            rect.center().y,
        ));
        assert!(!host.last_relayout().measured);
        host.verify_retained_geometry().unwrap();
        let nodes = host.layout_node_ids();
        host.resize((1200, 720)).unwrap();
        assert!(!host.last_relayout().rebuilt && host.last_relayout().measured);
        assert_eq!(host.layout_node_ids(), nodes, "resize keeps the tree");
        assert!(
            host.targets()
                .get("display.arrangement")
                .copied()
                .expect("the actual arrangement is mounted")
                .width()
                > rect.width()
        );
        host.verify_retained_geometry().unwrap();
        host.set_theme(ui::Theme::light());
        assert!(host.last_relayout().rebuilt);
        host.verify_retained_geometry().unwrap();
    }

    #[test]
    fn dragging_a_display_below_the_other_emits_its_settled_position() {
        let mut canvas = canvas();
        let mapping = canvas.mapping();
        let monitor = mapping.to_canvas(displays()[1].placement);
        let panel = mapping.to_canvas(displays()[0].placement);
        // One frame's instant throughout: the view grows to take in the held
        // display, so a drop read later than its move lands where the grown
        // view puts the hand, which a loaded test machine would make vary.
        let _frame = crate::motion::pin_frame();
        assert!(canvas.press(monitor.center()));
        let target = Point::new(
            panel.center().x + 3.0,
            panel.y1 + monitor.height() / 2.0 + 4.0,
        );
        assert!(canvas.drag_to(target));
        assert!(canvas.release());

        let invocation = ui::widget::controls::take_control_invocation(&mut canvas)
            .expect("the drop is sent as the display's move action");
        assert_eq!(invocation.address.action, "move-display");
        let x = invocation.arguments["x"].as_i64().unwrap();
        let y = invocation.arguments["y"].as_i64().unwrap();
        assert_eq!((x, y), (0, 1080), "snapped flush under the panel");
        assert!(ui::widget::controls::take_control_invocation(&mut canvas).is_none());
    }

    #[test]
    fn device_screens_preview_and_drop_together_preserving_native_seat_geometry() {
        let mut canvas = canvas();
        canvas.displays[0].movable = false;
        canvas.displays[1].movement_group = Some("peer".into());
        canvas.displays.push(ArrangedDisplay {
            connector: "peer/DP-2".into(),
            movement_group: Some("peer".into()),
            shared: None,
            label: "Second peer screen".into(),
            number: 3,
            placement: Placement::new(1920, 1080, 1280, 720),
            primary: false,
            movable: true,
        });
        let mapping = canvas.mapping();
        let first = mapping.to_canvas(canvas.displays[1].placement);
        let panel = mapping.to_canvas(canvas.displays[0].placement);
        assert!(canvas.press(first.center()));
        assert!(canvas.drag_to(Point::new(
            panel.center().x,
            panel.y1 + first.height() / 2.0 + 4.0
        )));
        let drawn = canvas.drawn();
        assert_eq!(drawn.iter().filter(|(_, dragging)| *dragging).count(), 2);
        assert_eq!(drawn[2].0.placement.x, drawn[1].0.placement.x);
        assert_eq!(drawn[2].0.placement.y - drawn[1].0.placement.y, 1080);
        assert!(canvas.release());
        assert_eq!(
            canvas.displays[2].placement.x,
            canvas.displays[1].placement.x
        );
        assert_eq!(
            canvas.displays[2].placement.y - canvas.displays[1].placement.y,
            1080
        );
        assert_eq!(canvas.displays[2].placement.width, 1280);
        let invocation = ui::widget::controls::take_control_invocation(&mut canvas).unwrap();
        assert_eq!(invocation.address.action, "move-display");
        assert!(ui::widget::controls::take_control_invocation(&mut canvas).is_none());
    }

    #[test]
    fn immovable_local_screen_still_constrains_peer_drag() {
        let mut canvas = canvas();
        canvas.displays[0].movable = false;
        let mapping = canvas.mapping();
        let local = mapping.to_canvas(canvas.displays[0].placement);
        let peer = mapping.to_canvas(canvas.displays[1].placement);
        assert!(
            canvas.press(local.center()),
            "an immovable screen is still chosen"
        );
        assert!(
            canvas.gesture.with(|gesture| gesture.drag.is_none()),
            "but never picked up"
        );
        assert!(!canvas.release());
        assert!(canvas.press(peer.center()));
        assert!(canvas.drag_to(local.center()));
        assert!(canvas.release());
        assert!(
            !canvas.displays[0]
                .placement
                .overlaps(canvas.displays[1].placement)
        );
        assert!(ui::widget::controls::take_control_invocation(&mut canvas).is_some());
    }

    #[test]
    fn a_click_without_movement_changes_nothing() {
        let mut canvas = canvas();
        let monitor = canvas.mapping().to_canvas(displays()[1].placement);
        assert!(canvas.press(monitor.center()));
        assert!(canvas.drag_to(monitor.center() + (1.0, 1.0)));
        assert!(canvas.release());
        assert!(ui::widget::controls::take_control_invocation(&mut canvas).is_none());
    }

    #[test]
    fn every_display_is_described_for_assistive_technology() {
        let node = canvas().accessibility().unwrap();
        let names: Vec<String> = node
            .children
            .iter()
            .filter_map(|child| child.name.clone())
            .collect();
        assert_eq!(
            names,
            [
                "Display 1: Built-in display, main display",
                "Display 2: Desk monitor"
            ]
        );
    }

    #[test]
    fn arrow_keys_move_the_selected_display_from_the_keyboard() {
        let (mut canvas, selection) = selected_canvas();
        selection.set("HDMI-A-1".into());
        assert!(canvas.is_focusable(), "Tab reaches the canvas");
        canvas.mark_drawn();
        canvas.set_focus(true);
        assert!(canvas.needs_redraw(), "the focus ring shows");
        canvas.mark_drawn();
        assert!(canvas.step(placement::Step::Left));
        let invocation = ui::widget::controls::take_control_invocation(&mut canvas)
            .expect("the step is sent as the display's move");
        assert_eq!(invocation.address.action, "move-display");
        assert_eq!(
            (
                invocation.arguments["x"].as_i64().unwrap(),
                invocation.arguments["y"].as_i64().unwrap()
            ),
            (-1920, 0),
            "to the panel's other side, in line"
        );
        assert!(ui::widget::controls::take_control_invocation(&mut canvas).is_none());
        assert!(canvas.needs_redraw(), "it springs there");
        assert!(canvas.step(placement::Step::Right), "and back");
        let back = ui::widget::controls::take_control_invocation(&mut canvas).unwrap();
        assert_eq!(back.arguments["x"].as_i64(), Some(1920));
        assert!(
            !canvas.step(placement::Step::Right),
            "nothing further that way moves nothing"
        );
        assert!(ui::widget::controls::take_control_invocation(&mut canvas).is_none());
    }

    #[test]
    fn every_display_is_chosen_and_moved_from_the_keyboard() {
        let (canvas, selection) = selected_canvas();
        let mut host = ui::headless::FrameHarness::for_root(
            ui::State::new(),
            canvas,
            (800, 400),
            ui::theme::Theme::dark(),
        );
        host.profile_frame();
        assert!(host.tab(false).is_some(), "Tab reaches the canvas");
        host.key("ArrowRight");
        assert_eq!(selection.with(Clone::clone), "HDMI-A-1", "an arrow chooses");
        host.key("ArrowRight");
        assert_eq!(
            selection.with(Clone::clone),
            "HDMI-A-1",
            "nothing further that way keeps the choice"
        );
        let place_of = |host: &mut ui::headless::FrameHarness, connector: &str| {
            host.root_mut::<DisplayArrangement>()
                .displays
                .iter()
                .find(|display| display.connector == connector)
                .map(|display| (display.placement.x, display.placement.y))
        };
        assert_eq!(
            place_of(&mut host, "eDP-1"),
            Some((0, 0)),
            "choosing moves nothing"
        );
        assert_eq!(place_of(&mut host, "HDMI-A-1"), Some((1920, 0)));
        host.key("ArrowLeft");
        assert_eq!(selection.with(Clone::clone), "eDP-1");
        host.set_modifiers(&["ctrl".to_owned()]);
        host.key("ArrowRight");
        // (The move is sent as the display's move action, as
        // `arrow_keys_move_the_selected_display_from_the_keyboard` shows.)
        assert_eq!(
            place_of(&mut host, "eDP-1"),
            Some((3840, 0)),
            "Ctrl+arrow moves the panel past the monitor's right side"
        );
        assert_eq!(selection.with(Clone::clone), "eDP-1", "and stays chosen");

        // A screen reader: each display's tile is a choice it can activate.
        let canvas = host.root_mut::<DisplayArrangement>();
        let tiles = canvas.accessibility().unwrap().children;
        assert_eq!(tiles.len(), 2);
        for tile in &tiles {
            assert_eq!(tile.role, ui::a11y::Role::RadioButton);
            assert!(
                tile.action_target.is_some(),
                "{:?} can be activated",
                tile.name
            );
        }
        let monitor = tiles
            .iter()
            .find(|tile| !tile.state.selected)
            .and_then(|tile| tile.action_target.clone())
            .unwrap();
        assert_eq!(monitor, format!("{CHOOSE_TARGET}HDMI-A-1"));
        assert!(canvas.choose("HDMI-A-1"));
        assert_eq!(selection.with(Clone::clone), "HDMI-A-1");
        assert!(
            !canvas.choose("Virtual-1"),
            "never a display it does not show"
        );
    }

    fn selected_canvas() -> (DisplayArrangement, ui::LocalState<String>) {
        let selection = ui::LocalState::new("eDP-1".to_owned());
        let canvas = canvas().with_selection(selection.clone());
        (canvas, selection)
    }

    #[test]
    fn pressing_a_display_selects_it_in_the_shared_selection() {
        let (mut canvas, selection) = selected_canvas();
        let monitor = canvas.mapping().to_canvas(displays()[1].placement);
        assert!(canvas.press(monitor.center()));
        assert_eq!(selection.with(Clone::clone), "HDMI-A-1");
        let node = canvas.accessibility().unwrap();
        assert!(node.children[1].state.selected, "the chosen tile says so");
        assert!(!node.children[0].state.selected);
        assert!(canvas.release(), "a press without a drag is just a choice");
        assert!(ui::widget::controls::take_control_invocation(&mut canvas).is_none());
        // A choice made another way (keys, a screen reader) shows too.
        selection.set("eDP-1".into());
        assert!(canvas.accessibility().unwrap().children[0].state.selected);
    }

    #[test]
    fn a_drag_repaints_every_move_and_writes_only_on_release() {
        let mut canvas = canvas();
        let mapping = canvas.mapping();
        let monitor = mapping.to_canvas(displays()[1].placement);
        assert!(canvas.press(monitor.center()));
        canvas.mark_drawn();
        let mut writes = 0;
        for step in 1..=24 {
            let point =
                monitor.center() + Vec2::new(-12.0 * f64::from(step), 6.0 * f64::from(step));
            assert!(canvas.drag_to(point));
            assert!(
                canvas.needs_redraw(),
                "move {step} asks for its frame: the host repaints a pointer move only then"
            );
            assert!(!canvas.is_layout_dirty(), "move {step} lays nothing out");
            writes +=
                usize::from(ui::widget::controls::take_control_invocation(&mut canvas).is_some());
            canvas.mark_drawn();
            // Between moves it asks for nothing, unless the view is still
            // springing out to keep the held display in sight.
            let view_growing = canvas.gesture.with(|gesture| {
                gesture
                    .view
                    .as_ref()
                    .is_some_and(|view| view.is_moving(crate::motion::frame_now()))
            });
            assert!(
                !canvas.needs_redraw() || view_growing,
                "move {step} asks for nothing more"
            );
            // 1:1: away from every line, the held display is exactly where
            // the hand put it.
            let (held, _) = canvas.drawn().into_iter().find(|(_, held)| *held).unwrap();
            let (dx, dy) = mapping.to_desktop(point);
            let grab = mapping.to_desktop(monitor.center());
            let free = (
                displays()[1].placement.x + round_to_i32(dx - grab.0),
                displays()[1].placement.y + round_to_i32(dy - grab.1),
            );
            let snapped = (held.placement.x, held.placement.y);
            let threshold = mapping.snap_threshold();
            assert!(
                (snapped.0 - free.0).abs() <= threshold && (snapped.1 - free.1).abs() <= threshold,
                "move {step}: {snapped:?} strays from the hand at {free:?}"
            );
        }
        assert_eq!(writes, 0, "nothing is written while the display moves");
        assert!(canvas.release());
        assert!(ui::widget::controls::take_control_invocation(&mut canvas).is_some());
        assert!(ui::widget::controls::take_control_invocation(&mut canvas).is_none());
    }

    #[test]
    fn a_dragged_display_moves_without_a_relayout() {
        let bare = DisplayArrangement::new(
            displays(),
            Vec::new(),
            ui::LocalState::new(ArrangementGesture::default()),
        );
        let mut host =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(bare), (800, 600));
        host.profile_frame();
        let rect = host
            .targets()
            .get("display.arrangement")
            .copied()
            .expect("the arrangement is mounted");
        let rebuilt = host.rebuilt_frames();
        let start = Point::new(rect.x0 + rect.width() * 0.75, rect.center().y);
        host.send(&crate::synthetic_input::cursor_moved(start.x, start.y));
        host.send(&crate::synthetic_input::mouse_press());
        let frames = host.relayout_frames();
        for step in 1..=12 {
            host.send(&crate::synthetic_input::cursor_moved(
                start.x - 6.0 * f64::from(step),
                start.y + 3.0 * f64::from(step),
            ));
            let relayout = host.last_relayout();
            assert!(
                !relayout.rebuilt && !relayout.measured,
                "move {step} relaid the canvas out: {relayout:?}"
            );
        }
        assert!(
            host.relayout_frames() >= frames + 12,
            "every move of the hand paints a frame"
        );
        assert_eq!(
            host.rebuilt_frames(),
            rebuilt,
            "no move recomposed anything"
        );
        host.send(&crate::synthetic_input::mouse_release());
        host.verify_retained_geometry().unwrap();
    }

    #[test]
    fn a_finger_drags_a_display_like_the_pointer() {
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
            let mut canvas = canvas();
            let mapping = canvas.mapping();
            let monitor = mapping.to_canvas(displays()[1].placement);
            let panel = mapping.to_canvas(displays()[0].placement);
            let target = Point::new(
                panel.center().x + 3.0,
                panel.y1 + monitor.height() / 2.0 + 4.0,
            );
            let path = [
                monitor.center(),
                monitor.center() - Vec2::new(30.0, -20.0),
                target,
            ];
            if by_touch {
                let dispatch = || crate::testing::Dispatch::new();
                assert!(
                    dispatch()
                        .send(&mut canvas, &touch(TouchPhase::Started, path[0]))
                        .handled
                );
                for point in &path[1..] {
                    assert!(
                        dispatch()
                            .send(&mut canvas, &touch(TouchPhase::Moved, *point))
                            .handled
                    );
                }
                assert!(
                    dispatch()
                        .send(&mut canvas, &touch(TouchPhase::Ended, target))
                        .handled
                );
            } else {
                assert!(canvas.press(path[0]));
                for point in &path[1..] {
                    assert!(canvas.drag_to(*point));
                }
                assert!(canvas.release());
            }
            let invocation = ui::widget::controls::take_control_invocation(&mut canvas)
                .expect("the drop is sent");
            (
                invocation.arguments["x"].as_i64().unwrap(),
                invocation.arguments["y"].as_i64().unwrap(),
            )
        };
        assert_eq!(drop(true), drop(false));
        assert_eq!(drop(true), (0, 1080));
    }

    #[test]
    fn a_dropped_display_springs_into_place_then_asks_for_nothing() {
        let mut canvas = canvas();
        let mapping = canvas.mapping();
        let monitor = mapping.to_canvas(displays()[1].placement);
        assert!(!canvas.needs_animation_frame(), "idle costs nothing");
        // One frame's instant from the press to the drop: the settle starts
        // at the drop's instant, and reading it a moment later, as a loaded
        // test machine does, would find it already on its way.
        let _frame = crate::motion::pin_frame();
        assert!(canvas.press(monitor.center()));
        // Let go overlapping the panel: it rests clear of it.
        let over = mapping.to_canvas(displays()[0].placement).center();
        assert!(canvas.drag_to(over));
        let held = canvas
            .drawn()
            .into_iter()
            .find(|(_, held)| *held)
            .unwrap()
            .0
            .placement;
        assert!(canvas.release());
        let released = crate::motion::frame_now();
        let rest = canvas.displays[1].placement;
        assert_ne!(rest, held);
        assert!(!rest.overlaps(canvas.displays[0].placement));
        assert!(canvas.needs_animation_frame(), "the settle asks for frames");
        let (_, shift, _) = canvas.drawn_at(released)[1].clone();
        let start = Point::new(f64::from(rest.x), f64::from(rest.y)) + shift;
        assert!(
            (start.x - f64::from(held.x)).abs() < 2.0 && (start.y - f64::from(held.y)).abs() < 2.0,
            "it starts where it was let go: {start:?} vs {held:?}"
        );
        let later = released + crate::time::Duration::from_secs(2);
        let (_, shift, _) = canvas.drawn_at(later)[1].clone();
        assert!(shift.hypot() < 1e-6, "and rests where it settles");
        assert!(!canvas.settling(later), "then it asks for no frames");
    }

    #[test]
    fn the_view_grows_to_keep_a_held_display_in_sight_then_settles_back() {
        let mut canvas = canvas();
        let fitted = canvas.mapping();
        let rest = fitted
            .to_canvas(displays()[0].placement)
            .union(fitted.to_canvas(displays()[1].placement));
        assert!(
            (rest.center().x - canvas.rect.center().x).abs() < 1.0
                && (rest.center().y - canvas.rect.center().y).abs() < 1.0,
            "the arrangement is centred: {rest:?} in {:?}",
            canvas.rect
        );
        let monitor = fitted.to_canvas(displays()[1].placement);
        assert!(canvas.press(monitor.center()));
        let panel = fitted.to_canvas(displays()[0].placement);
        assert!(canvas.drag_to(Point::new(panel.center().x, canvas.rect.y1 + 40.0)));
        let later = crate::motion::frame_now() + crate::time::Duration::from_secs(2);
        let grown = canvas.mapping_at(later);
        assert!(grown.scale < fitted.scale, "the view grew");
        let drawn = canvas.drawn_at(later);
        let (held, _, _) = drawn.iter().find(|(_, _, held)| *held).unwrap();
        let tile = grown.to_canvas(held.placement);
        assert!(
            canvas.rect.inflate(1.0, 1.0).union(tile) == canvas.rect.inflate(1.0, 1.0),
            "the held display stays in sight: {tile:?} in {:?}",
            canvas.rect
        );
        assert!(canvas.release());
        let after = crate::motion::frame_now() + crate::time::Duration::from_secs(2);
        let settled = canvas.mapping_at(after);
        let expected = Mapping::frame(
            canvas.rect,
            canvas.fitted_view(),
            canvas.padding,
            canvas.snap_radius,
        );
        assert!(
            (settled.scale - expected.scale).abs() < 1e-6,
            "it settles back to fit the new arrangement"
        );
        assert!(!canvas.settling(after), "and then asks for nothing");
    }
}
