//! What windows ask of the window manager: to be moved or resized with the
//! pointer, maximized, made fullscreen or minimized. Wayland toplevels and
//! X11 windows ask the same things the same way here.
//!
//! The [`WindowPolicy`](crate::WindowPolicy) is asked first
//! ([`WindowPolicy::request`](crate::WindowPolicy::request)): a desktop of
//! strips and tabs answers them itself; granted, the compositor does what a
//! floating window manager does: a pointer grab that moves or resizes the
//! window, or the window covering its output until it asks to be restored.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "smithay geometry: logical coordinates of outputs and windows, far from i32 limits"
)]

use smithay::desktop::Window;
use smithay::input::SeatHandler;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
    RelativeMotionEvent,
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Serial};

use crate::policy::{ResizeEdges, WindowRequest};
use crate::state::State;

/// The left mouse button's evdev code.
const BTN_LEFT: u32 = 0x110;

/// The smallest a resize makes a window.
const MIN_SIZE: (i32, i32) = (64, 48);

/// `start` resized by dragging `edges` by `delta`: the dragged edges move,
/// the opposite ones stay, and the window keeps at least `min`.
pub(crate) fn resized(
    start: Rectangle<i32, Logical>,
    edges: ResizeEdges,
    delta: Point<i32, Logical>,
    min: (i32, i32),
) -> Rectangle<i32, Logical> {
    let mut rect = start;
    if edges.right {
        rect.size.w = (start.size.w + delta.x).max(min.0);
    }
    if edges.left {
        let w = (start.size.w - delta.x).max(min.0);
        rect.loc.x = start.loc.x + start.size.w - w;
        rect.size.w = w;
    }
    if edges.bottom {
        rect.size.h = (start.size.h + delta.y).max(min.1);
    }
    if edges.top {
        let h = (start.size.h - delta.y).max(min.1);
        rect.loc.y = start.loc.y + start.size.h - h;
        rect.size.h = h;
    }
    rect
}

/// How a window covers its output.
enum Cover {
    Maximized,
    /// Fullscreen, on the named output or its own.
    Fullscreen(Option<String>),
}

/// Where a window was before it was maximized or made fullscreen.
struct Restore(std::cell::Cell<Option<Rectangle<i32, Logical>>>);

impl State {
    /// Whether a fullscreen window covers `output`.
    pub(crate) fn fullscreen_on(&self, output: &smithay::output::Output) -> bool {
        let Some(area) = self.space.output_geometry(output) else {
            return false;
        };
        self.space.elements().any(|window| {
            let fullscreen = window.toplevel().is_some_and(|toplevel| {
                toplevel
                    .current_state()
                    .states
                    .contains(xdg_toplevel::State::Fullscreen)
            }) || window
                .x11_surface()
                .is_some_and(smithay::xwayland::X11Surface::is_fullscreen);
            fullscreen
                && self
                    .space
                    .element_geometry(window)
                    .is_some_and(|geometry| geometry.contains_rect(area))
        })
    }

    /// A window asked for `request` (`serial`: the press it starts from,
    /// for a move or resize).
    pub(crate) fn window_request(
        &mut self,
        window: &Window,
        request: &WindowRequest,
        serial: Option<Serial>,
    ) {
        let Some(info) = crate::state::describe(window) else {
            return;
        };
        if !self.policy.request(&info, request) {
            // The policy answers it itself; a toplevel still needs its
            // configure.
            if let Some(toplevel) = window.toplevel() {
                toplevel.send_pending_configure();
            }
            self.rearrange();
            return;
        }
        match request {
            WindowRequest::Move => self.start_grab(window, None, serial),
            WindowRequest::Resize(edges) => self.start_grab(window, Some(*edges), serial),
            WindowRequest::Maximize(on) => self.cover(window, *on, &Cover::Maximized),
            WindowRequest::Fullscreen { on, output } => {
                self.cover(window, *on, &Cover::Fullscreen(output.clone()));
            }
            WindowRequest::Minimize => {}
        }
    }

    /// Start moving (`edges` `None`) or resizing `window` with the pointer,
    /// while the button that asked stays pressed. A Wayland client names
    /// the press (`serial`), which must be the pointer's; an X11 client asks
    /// while holding a button, without a serial of ours.
    fn start_grab(&mut self, window: &Window, edges: Option<ResizeEdges>, serial: Option<Serial>) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let start = match serial {
            Some(serial) if !pointer.has_grab(serial) => return,
            _ => pointer.grab_start_data().unwrap_or_else(|| GrabStartData {
                focus: None,
                button: BTN_LEFT,
                location: pointer.current_location(),
            }),
        };
        let Some(location) = self.space.element_location(window) else {
            return;
        };
        let rect = Rectangle::new(location, window.geometry().size);
        self.set_grab(window, edges, start, rect);
    }

    fn set_grab(
        &mut self,
        window: &Window,
        edges: Option<ResizeEdges>,
        start: GrabStartData<Self>,
        rect: Rectangle<i32, Logical>,
    ) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let grab = WindowGrab {
            start,
            window: window.clone(),
            initial: rect,
            edges,
        };
        if let Some(toplevel) = window.toplevel()
            && edges.is_some()
        {
            toplevel.with_pending_state(|state| state.states.set(xdg_toplevel::State::Resizing));
            toplevel.send_pending_configure();
        }
        pointer.set_grab(
            self,
            grab,
            SERIAL_COUNTER.next_serial(),
            smithay::input::pointer::Focus::Clear,
        );
    }

    /// Make `window` cover its output (`fullscreen`: the named one, else the
    /// one it is on) or put it back where it was.
    fn cover(&mut self, window: &Window, on: bool, how: &Cover) {
        window
            .user_data()
            .insert_if_missing(|| Restore(std::cell::Cell::new(None)));
        let Some(restore) = window.user_data().get::<Restore>() else {
            return;
        };
        let Some(location) = self.space.element_location(window) else {
            return;
        };
        let current = Rectangle::new(location, window.geometry().size);
        let target = if on {
            let named = match how {
                Cover::Fullscreen(Some(name)) => Some(name.clone()),
                _ => None,
            };
            let output = named
                .and_then(|name| {
                    self.space
                        .outputs()
                        .find(|output| output.name() == name)
                        .cloned()
                })
                .or_else(|| self.space.outputs_for_element(window).into_iter().next())
                .or_else(|| self.space.outputs().next().cloned());
            let Some(geometry) = output.and_then(|output| self.space.output_geometry(&output))
            else {
                return;
            };
            if restore.0.get().is_none() {
                restore.0.set(Some(current));
            }
            geometry
        } else {
            match restore.0.take() {
                Some(rect) => rect,
                None => current,
            }
        };
        let fullscreen = matches!(how, Cover::Fullscreen(_));
        let state = if fullscreen {
            xdg_toplevel::State::Fullscreen
        } else {
            xdg_toplevel::State::Maximized
        };
        if let Some(toplevel) = window.toplevel() {
            toplevel.with_pending_state(|pending| {
                if on {
                    pending.states.set(state);
                } else {
                    pending.states.unset(state);
                }
                pending.size = Some(target.size);
            });
            toplevel.send_pending_configure();
        }
        if let Some(x11) = window.x11_surface() {
            let _ = if fullscreen {
                x11.set_fullscreen(on)
            } else {
                x11.set_maximized(on)
            };
            let _ = x11.configure(target);
        }
        self.space.map_element(window.clone(), target.loc, true);
    }
}

/// A pointer grab that moves (`edges` `None`) or resizes a window.
struct WindowGrab {
    start: GrabStartData<State>,
    window: Window,
    initial: Rectangle<i32, Logical>,
    edges: Option<ResizeEdges>,
}

impl WindowGrab {
    fn follow(&self, data: &mut State, location: Point<f64, Logical>) {
        let delta = (location - self.start.location).to_i32_round::<i32>();
        match self.edges {
            None => {
                let to = self.initial.loc + delta;
                if let Some(x11) = self.window.x11_surface() {
                    let _ = x11.configure(Rectangle::new(to, self.initial.size));
                }
                data.space.map_element(self.window.clone(), to, true);
            }
            Some(edges) => {
                let rect = resized(self.initial, edges, delta, MIN_SIZE);
                if let Some(toplevel) = self.window.toplevel() {
                    toplevel.with_pending_state(|state| state.size = Some(rect.size));
                    toplevel.send_pending_configure();
                }
                if let Some(x11) = self.window.x11_surface() {
                    let _ = x11.configure(rect);
                }
                if rect.loc != self.initial.loc || self.window.x11_surface().is_some() {
                    data.space.map_element(self.window.clone(), rect.loc, true);
                }
            }
        }
    }

    fn finish(&self) {
        if let Some(toplevel) = self.window.toplevel()
            && self.edges.is_some()
        {
            toplevel.with_pending_state(|state| state.states.unset(xdg_toplevel::State::Resizing));
            toplevel.send_pending_configure();
        }
    }
}

impl PointerGrab<State> for WindowGrab {
    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // No client sees the pointer while it drags a window.
        handle.motion(data, None, event);
        self.follow(data, event.location);
    }

    fn relative_motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, None, event);
    }

    fn button(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        if handle.current_pressed().is_empty() {
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        details: AxisFrame,
    ) {
        handle.axis(data, details);
    }

    fn frame(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &GrabStartData<State> {
        &self.start
    }

    fn unset(&mut self, _data: &mut State) {
        self.finish();
    }
}

/// The edges an `xdg_toplevel.resize` names.
pub(crate) fn xdg_edges(edge: xdg_toplevel::ResizeEdge) -> ResizeEdges {
    use xdg_toplevel::ResizeEdge as E;
    ResizeEdges {
        top: matches!(edge, E::Top | E::TopLeft | E::TopRight),
        bottom: matches!(edge, E::Bottom | E::BottomLeft | E::BottomRight),
        left: matches!(edge, E::Left | E::TopLeft | E::BottomLeft),
        right: matches!(edge, E::Right | E::TopRight | E::BottomRight),
    }
}

/// The edges an X11 `_NET_WM_MOVERESIZE` names.
pub(crate) fn x11_edges(edge: smithay::xwayland::xwm::ResizeEdge) -> ResizeEdges {
    use smithay::xwayland::xwm::ResizeEdge as E;
    ResizeEdges {
        top: matches!(edge, E::Top | E::TopLeft | E::TopRight),
        bottom: matches!(edge, E::Bottom | E::BottomLeft | E::BottomRight),
        left: matches!(edge, E::Left | E::TopLeft | E::BottomLeft),
        right: matches!(edge, E::Right | E::TopRight | E::BottomRight),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::utils::Size;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    #[test]
    fn dragging_an_edge_moves_it_and_keeps_the_opposite_one() {
        let start = rect(100, 100, 400, 300);
        let right_bottom = ResizeEdges {
            right: true,
            bottom: true,
            ..ResizeEdges::default()
        };
        assert_eq!(
            resized(start, right_bottom, Point::from((50, -20)), MIN_SIZE),
            rect(100, 100, 450, 280)
        );
        let top_left = ResizeEdges {
            top: true,
            left: true,
            ..ResizeEdges::default()
        };
        assert_eq!(
            resized(start, top_left, Point::from((50, 20)), MIN_SIZE),
            rect(150, 120, 350, 280),
            "the bottom-right corner stays put"
        );
    }

    #[test]
    fn a_resize_never_shrinks_below_the_minimum_and_the_far_edge_stays() {
        let start = rect(0, 0, 200, 200);
        let left = ResizeEdges {
            left: true,
            ..ResizeEdges::default()
        };
        assert_eq!(
            resized(start, left, Point::from((500, 0)), MIN_SIZE),
            rect(200 - MIN_SIZE.0, 0, MIN_SIZE.0, 200)
        );
    }
}
