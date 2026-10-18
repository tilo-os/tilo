//! The row's half of resizing windows on a strip: finding the window edge
//! under the pointer, showing it, and carrying out the drag.
//!
//! The model itself is [`crate::widget::edge_resize`]: whose edge a point is
//! ([`seam_owner`]) and what a drag sets ([`EdgeDrag::resize`]). This file
//! only feeds it the strip's geometry and applies its answer, so the hover
//! highlight, the press and every frame of the drag ask the same rule.

use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::{DRAG_EDGE_BAND_PX, DRAG_EDGE_MAX_STEP, DRAG_EDGE_PAN_PX_PER_S, GroupRow};
use crate::time::{Duration, Instant};
use crate::widget::edge_resize::{
    DraggedWindow, EdgeDrag, EdgeGrab, ResizeMode, Seam, WindowEdge, seam_owner,
};
use crate::widget::{RESIZE_HANDLE_PX, Widget, WindowGroup};

/// An edge resize in flight, with its windows named by id: a window may
/// close or move while the pointer is down.
#[derive(Clone, Copy, Debug)]
pub(super) struct RowEdgeDrag {
    drag: EdgeDrag,
    grabbed_id: u64,
    other_id: Option<u64>,
    /// The lane whose pan the drag holds.
    lane: usize,
    /// The pointer's x when pressed, and now.
    press_x: f64,
    pointer_x: f64,
    /// The lane's pan when pressed.
    origin_scroll: f64,
    /// How far the strip has auto-scrolled under a pointer resting at the
    /// display's edge (positive: its canvas moved left).
    autoscroll: f64,
    /// When the auto-scroll last advanced; `None` outside the edge bands.
    edge_tick: Option<Instant>,
}

impl RowEdgeDrag {
    /// How far the dragged edge has been carried across the strip's canvas.
    fn travel(&self) -> f64 {
        self.pointer_x - self.press_x + self.autoscroll
    }
}

impl GroupRow {
    /// The window edge a press at `p` would resize, by the seam rule
    /// ([`seam_owner`]): on a seam, the focused window's edge, else the
    /// half's; at either end of a lane, its outer window's outer edge.
    /// `None` off every edge, on a maximized or fullscreen window, or where
    /// the viewport clips the edge away.
    pub fn edge_grab_at(&self, p: Point) -> Option<EdgeGrab> {
        if self.hidden || !self.rect.contains(p) {
            return None;
        }
        // The windows with edges to grab, in strip order: index, lane, rect.
        let shown: Vec<(usize, usize, Rect)> = self
            .groups
            .iter()
            .enumerate()
            .filter(|(i, g)| {
                !self.layout_hides_group(*i)
                    && self.fullscreen_group != Some(g.id)
                    && !g.maximized
                    && !g.is_closing()
                    && g.get_rect().width() > 0.0
            })
            .map(|(i, g)| (i, self.lane_index_of(i), g.get_rect()))
            .collect();
        let lane = shown
            .iter()
            .find(|(_, _, r)| {
                r.inflate(RESIZE_HANDLE_PX + super::HANDLE_SLOT_PX, 0.0)
                    .contains(p)
            })
            .map(|&(_, lane, _)| lane)?;
        if !self.pan_of_lane(lane).viewport.contains(p) {
            return None;
        }
        // A window's `+` sits in its bar at the trailing edge: a press there
        // opens a tab rather than grabbing the edge.
        if !self.external_navigation
            && shown
                .iter()
                .any(|&(i, ..)| self.groups.get(i).is_some_and(|g| g.new_tab_hit(p)))
        {
            return None;
        }
        let adjacent = |a: &(usize, usize, Rect), b: &(usize, usize, Rect)| {
            a.1 == b.1 && a.0.checked_add(1) == Some(b.0)
        };
        for pair in shown.windows(2) {
            let [a, b] = pair else { continue };
            if !adjacent(a, b) {
                continue;
            }
            let (left, right) = (a.2, b.2);
            let zone = Rect::new(
                left.x1 - RESIZE_HANDLE_PX,
                left.y0.min(right.y0),
                right.x0 + RESIZE_HANDLE_PX,
                left.y1.max(right.y1),
            );
            if zone.contains(p) {
                let focused = |i: usize| self.groups.get(i).is_some_and(WindowGroup::focused);
                let seam = Seam {
                    left: a.0,
                    left_rect: left,
                    left_focused: focused(a.0),
                    right_rect: right,
                    right_focused: focused(b.0),
                };
                return Some(seam_owner(&seam, p.x));
            }
        }
        for (k, window) in shown.iter().enumerate() {
            let (i, _, r) = *window;
            let first = k
                .checked_sub(1)
                .and_then(|j| shown.get(j))
                .is_none_or(|before| !adjacent(before, window));
            let last = shown
                .get(k.saturating_add(1))
                .is_none_or(|next| !adjacent(window, next));
            let leading = Rect::new(r.x0, r.y0, r.x0 + RESIZE_HANDLE_PX, r.y1);
            let trailing = Rect::new(r.x1 - RESIZE_HANDLE_PX, r.y0, r.x1, r.y1);
            if first && leading.contains(p) {
                return Some(EdgeGrab {
                    group: i,
                    edge: WindowEdge::Leading,
                });
            }
            if last && trailing.contains(p) {
                return Some(EdgeGrab {
                    group: i,
                    edge: WindowEdge::Trailing,
                });
            }
        }
        None
    }

    /// The window a Shift+drag on `grab` trades width with: the one across
    /// the seam, in the same lane. An outer edge has none.
    fn zero_sum_partner(&self, grab: EdgeGrab) -> Option<usize> {
        let other = grab.across(self.groups.len())?;
        (self.lane_index_of(other) == self.lane_index_of(grab.group)
            && !self.layout_hides_group(other)
            && self.groups.get(other).is_some_and(|g| !g.is_closing()))
        .then_some(other)
    }

    /// Whether an edge resize is in flight.
    pub fn edge_resize_in_flight(&self) -> bool {
        self.edge_drag.is_some()
    }

    /// The edge a press would resize now, and whether Shift is held.
    pub fn edge_hover(&self) -> Option<(EdgeGrab, bool)> {
        self.edge_hover
    }

    /// Another gesture owns the pointer: a tab or window being dragged, the
    /// strip being panned by hand.
    fn edge_resize_blocked(&self) -> bool {
        self.group_drag.is_some()
            || self.pointer_pan.is_some_and(|pan| pan.engaged)
            || self.groups.iter().any(WindowGroup::drag_in_flight)
    }

    /// Show which window a press at `p` would change: the owner's edge grip,
    /// and with Shift held the grip of the window it would trade with.
    fn hover_edge(&mut self, p: Point, shift: bool) {
        let hover = if self.edge_resize_blocked() {
            None
        } else {
            self.edge_grab_at(p).map(|grab| (grab, shift))
        };
        if hover != self.edge_hover {
            self.edge_hover = hover;
            self.show_edge_hover();
        }
    }

    fn show_edge_hover(&mut self) {
        let mut lit: Vec<Option<WindowEdge>> = vec![None; self.groups.len()];
        if let Some((grab, shift)) = self.edge_hover {
            if let Some(slot) = lit.get_mut(grab.group) {
                *slot = Some(grab.edge);
            }
            if shift
                && let Some(other) = self.zero_sum_partner(grab)
                && let Some(slot) = lit.get_mut(other)
            {
                *slot = Some(grab.edge.opposite());
            }
        }
        for (group, edge) in self.groups.iter_mut().zip(lit) {
            group.set_edge_highlight(edge);
        }
    }

    /// The row's edge-resize input: hover, press, drag, release, cancel.
    /// Sets `handled` for what it claims.
    pub(super) fn handle_edge_resize(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        let shift = ctx.modifiers.state().shift_key();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if let Some(drag) = self.edge_drag.as_mut() {
                    drag.pointer_x = p.x;
                    self.apply_edge_drag();
                    *ctx.handled = true;
                } else {
                    self.hover_edge(p, shift);
                }
            }
            WindowEvent::ModifiersChanged(modifiers) if self.edge_drag.is_none() => {
                self.hover_edge(self.last_cursor, modifiers.state().shift_key());
            }
            WindowEvent::CursorLeft { .. } if self.edge_drag.is_none() => {
                if self.edge_hover.take().is_some() {
                    self.show_edge_hover();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.edge_drag.is_none() && !self.edge_resize_blocked() => {
                // Found again at the press rather than trusted from the last
                // move, so a press never resizes a window the hover did not
                // show.
                if let Some(grab) = self.edge_grab_at(self.last_cursor) {
                    self.begin_edge_drag(grab, shift, self.last_cursor.x);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.edge_drag.is_some() => {
                self.end_edge_drag(true);
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.edge_drag.is_some()
                    && event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                self.end_edge_drag(false);
                *ctx.handled = true;
            }
            // Focus loss also reaches the apps, so their own input is
            // cancelled too.
            WindowEvent::Focused(false) if self.edge_drag.is_some() => self.end_edge_drag(false),
            _ => {}
        }
    }

    fn begin_edge_drag(&mut self, grab: EdgeGrab, shift: bool, x: f64) {
        let Some(grabbed_id) = self.groups.get(grab.group).map(|g| g.id) else {
            return;
        };
        let other = if shift {
            self.zero_sum_partner(grab)
        } else {
            None
        };
        let mut window = |i: usize, moving: WindowEdge| -> Option<(u64, DraggedWindow)> {
            let group = self.groups.get_mut(i)?;
            group.begin_edge_drag(moving);
            Some((
                group.id,
                DraggedWindow {
                    group: i,
                    width: group.edge_drag_origin_px()?,
                    limits: group.width_limits(),
                },
            ))
        };
        let Some((_, grabbed)) = window(grab.group, grab.edge) else {
            return;
        };
        let other = other.and_then(|i| window(i, grab.edge.opposite()));
        let lane = self.lane_index_of(grab.group);
        let pan = self.pan_of_lane_mut(lane);
        // The drag holds the pan from here: a glide or snap in flight stops
        // where it is, under the pointer.
        let origin_scroll = pan.scroll_x;
        pan.jump_to(origin_scroll);
        pan.absolute = false;
        self.edge_drag = Some(RowEdgeDrag {
            drag: EdgeDrag {
                edge: grab.edge,
                grabbed,
                zero_sum_with: other.map(|(_, window)| window),
            },
            grabbed_id,
            other_id: other.map(|(id, _)| id),
            lane,
            press_x: x,
            pointer_x: x,
            origin_scroll,
            autoscroll: 0.0,
            edge_tick: None,
        });
        self.edge_hover = None;
        self.show_edge_hover();
    }

    /// Put the windows at the widths the drag asks for this frame, and the
    /// lane's pan where it keeps the grabbed window's opposite edge still.
    fn apply_edge_drag(&mut self) {
        let Some(drag) = self.edge_drag else {
            return;
        };
        let index = |id: u64| self.groups.iter().position(|g| g.id == id);
        let (Some(grabbed), other) = (index(drag.grabbed_id), drag.other_id.map(index)) else {
            // The window closed under the pointer: nothing left to resize.
            self.edge_drag = None;
            return;
        };
        let resize = drag.drag.resize(drag.travel());
        if let Some(group) = self.groups.get_mut(grabbed) {
            group.set_live_width(resize.width);
        }
        if let (Some(Some(i)), Some(width)) = (other, resize.other_width)
            && let Some(group) = self.groups.get_mut(i)
        {
            group.set_live_width(width);
        }
        let pan = self.pan_of_lane_mut(drag.lane);
        pan.scroll_x = drag.origin_scroll + resize.pan + drag.autoscroll;
        self.scroll_dirty = true;
    }

    fn end_edge_drag(&mut self, commit: bool) {
        let Some(drag) = self.edge_drag.take() else {
            return;
        };
        let mut settle = 0.0;
        for id in std::iter::once(drag.grabbed_id).chain(drag.other_id) {
            let Some(group) = self.groups.iter_mut().find(|g| g.id == id) else {
                continue;
            };
            let change = group.end_edge_drag(commit);
            // The stored share resolves to whole pixels on its own grid, a
            // pixel or two off the live width; a leading-edge push keeps its
            // trailing edge where it was through that too.
            if id == drag.grabbed_id
                && drag.drag.mode() == ResizeMode::Push
                && drag.drag.edge == WindowEdge::Leading
            {
                settle = change;
            }
        }
        let now = crate::time::now();
        let omega = self.move_omega;
        let reduce_motion = self.reduce_motion;
        let pan = self.pan_of_lane_mut(drag.lane);
        pan.scroll_x = if commit {
            pan.scroll_x + settle
        } else {
            drag.origin_scroll
        };
        // While the pointer was down the strip held the fixed edge, even
        // past its start or with a window cut while all of them fit. Let go,
        // it eases to where the strip rests by its own framing rule rather
        // than jumping there on the next layout.
        let rest = pan
            .strip_frame_target(&pan.laid_spans)
            .unwrap_or(pan.scroll_x)
            .clamp(0.0, pan.pan_max_scroll_x());
        if reduce_motion {
            pan.jump_to(rest);
        } else if (rest - pan.scroll_x).abs() > super::SNAP_SETTLED_PX {
            pan.ease_to(rest, now, omega);
        }
        self.scroll_dirty = true;
        self.hover_edge(self.last_cursor, false);
    }

    /// Advance the auto-scroll of an edge drag resting at a display edge to
    /// `now`: the strip pans under the pointer so a window can grow past the
    /// screen, while the edge stays under the pointer. It only pans while
    /// that still changes a width, so a window stopped at its limit does not
    /// carry the strip off into empty canvas. Returns whether anything moved.
    pub(super) fn advance_edge_autoscroll(&mut self, now: Instant) -> bool {
        let Some(mut drag) = self.edge_drag else {
            return false;
        };
        let velocity = self.edge_autoscroll_velocity(&drag);
        if velocity.abs() < f64::EPSILON {
            drag.edge_tick = None;
            self.edge_drag = Some(drag);
            return false;
        }
        let step = drag
            .edge_tick
            .map_or(Duration::ZERO, |last| now.saturating_duration_since(last))
            .min(DRAG_EDGE_MAX_STEP);
        drag.edge_tick = Some(now);
        let before = drag.drag.resize(drag.travel());
        let mut next = drag;
        next.autoscroll += velocity * step.as_secs_f64();
        let after = drag.drag.resize(next.travel());
        let pan = self.pan_of_lane(drag.lane);
        let scroll = drag.origin_scroll + after.pan + next.autoscroll;
        let current = pan.scroll_x;
        // Never further past the strip's start or end than it already is.
        // The end is where the last layout put it, moved by what this step
        // adds to the windows' widths.
        let grown = after.width - before.width + after.other_width.unwrap_or(0.0)
            - before.other_width.unwrap_or(0.0);
        let within =
            scroll >= current.min(0.0) && scroll <= current.max(pan.pan_max_scroll_x() + grown);
        if after == before || !within {
            self.edge_drag = Some(drag);
            return false;
        }
        self.edge_drag = Some(next);
        self.apply_edge_drag();
        true
    }

    /// Whether an edge drag rests in a display-edge band, so the next frame
    /// auto-scrolls.
    pub(super) fn edge_autoscroll_pending(&self) -> bool {
        self.edge_drag
            .is_some_and(|drag| self.edge_autoscroll_velocity(&drag).abs() >= f64::EPSILON)
    }

    /// How fast (px/s, negative leftwards) the strip pans under an edge drag
    /// whose pointer rests in the lane's edge band: from nothing at the
    /// band's inner side to full speed against the edge.
    fn edge_autoscroll_velocity(&self, drag: &RowEdgeDrag) -> f64 {
        let view = self.pan_of_lane(drag.lane).viewport;
        if view.width() <= DRAG_EDGE_BAND_PX * 2.0 {
            return 0.0;
        }
        let into_left = DRAG_EDGE_BAND_PX - (drag.pointer_x - view.x0);
        let into_right = DRAG_EDGE_BAND_PX - (view.x1 - drag.pointer_x);
        if into_left > 0.0 {
            -DRAG_EDGE_PAN_PX_PER_S * (into_left / DRAG_EDGE_BAND_PX).min(1.0)
        } else if into_right > 0.0 {
            DRAG_EDGE_PAN_PX_PER_S * (into_right / DRAG_EDGE_BAND_PX).min(1.0)
        } else {
            0.0
        }
    }
}
