//! Touchscreens and touchpad gestures, routed.
//!
//! A touch on a hosted surface reaches it as a pointer press (hosts take
//! pointer input only); every other touch goes to the client surface under
//! it through the seat's touch handle and stays with that surface until the
//! finger lifts. Touchpad gestures of one or two fingers belong to the client
//! under the pointer (`zwp_pointer_gestures_v1`: a browser's pinch-to-zoom);
//! three or more are offered to the desktop first
//! ([`crate::WindowPolicy::gesture`]).

use smithay::input::pointer::{
    GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
    GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
};
use smithay::input::touch::{DownEvent, MotionEvent, UpEvent};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Serial};

use crate::gestures;
use crate::input::{BTN_LEFT, GestureEvent};
use crate::state::State;

/// A client surface and its origin in the logical layout.
type Focus = Option<(WlSurface, Point<f64, Logical>)>;

/// Where each finger on the screen went down.
#[derive(Debug, Default)]
pub(crate) struct Touches {
    /// Fingers on a hosted surface, by slot: the host's index and where
    /// the finger is.
    hosts: Vec<(u32, usize, Point<f64, Logical>)>,
    /// Fingers on client surfaces, by slot: the surface and its origin in
    /// the logical layout as the finger went down.
    clients: Vec<(u32, Focus)>,
    /// The gesture in flight belongs to the desktop.
    gesture_to_desktop: bool,
}

impl Touches {
    /// A finger holding a hosted surface, taken from it: the hosted app
    /// starts a drag that follows it. Its slot, the host and where it is.
    pub(crate) fn take_host_finger(&mut self) -> Option<(u32, usize, Point<f64, Logical>)> {
        self.hosts.pop()
    }
}

impl State {
    pub(crate) fn touch_down(
        &mut self,
        slot: u32,
        at: Point<f64, Logical>,
        serial: Serial,
        time: u32,
    ) {
        let at = self.clamp_to_outputs(at);
        if !self.locked()
            && let Some(index) = self.host_under(at)
            && let Some(host) = self.hosts.get_mut(index)
        {
            host.pointer_motion(at.x, at.y);
            host.pointer_button(BTN_LEFT, true);
            self.touch.hosts.push((slot, index, at));
            return;
        }
        let focus = if self.locked() {
            self.lock_surface_under(at)
        } else {
            self.focus_on_press(at, serial);
            self.surface_under(at)
                .map(|(_, surface, origin)| (surface, origin))
        };
        self.touch.clients.retain(|(held, _)| *held != slot);
        self.touch.clients.push((slot, focus.clone()));
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        touch.down(
            self,
            focus,
            &DownEvent {
                slot: Some(slot).into(),
                location: at,
                serial,
                time,
            },
        );
    }

    pub(crate) fn touch_motion(&mut self, slot: u32, at: Point<f64, Logical>, time: u32) {
        let at = self.clamp_to_outputs(at);
        if self.drag_finger() == Some(slot) {
            self.drag_touch_motion(slot, at, time);
            return;
        }
        if let Some((_, index, last)) = self
            .touch
            .hosts
            .iter_mut()
            .find(|(held, _, _)| *held == slot)
        {
            *last = at;
            if let Some(host) = self.hosts.get_mut(*index) {
                host.pointer_motion(at.x, at.y);
            }
            return;
        }
        let focus = self
            .touch
            .clients
            .iter()
            .find(|(held, _)| *held == slot)
            .and_then(|(_, focus)| focus.clone());
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        touch.motion(
            self,
            focus,
            &MotionEvent {
                slot: Some(slot).into(),
                location: at,
                time,
            },
        );
    }

    pub(crate) fn touch_up(&mut self, slot: u32, serial: Serial, time: u32) {
        if self.drag_finger() == Some(slot) {
            self.drag_touch_up(slot, serial, time);
            return;
        }
        if let Some(position) = self
            .touch
            .hosts
            .iter()
            .position(|(held, _, _)| *held == slot)
        {
            let (_, index, _) = self.touch.hosts.remove(position);
            if let Some(host) = self.hosts.get_mut(index) {
                host.pointer_button(BTN_LEFT, false);
                host.pointer_leave();
            }
            return;
        }
        self.touch.clients.retain(|(held, _)| *held != slot);
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        touch.up(
            self,
            &UpEvent {
                slot: Some(slot).into(),
                serial,
                time,
            },
        );
    }

    pub(crate) fn touch_frame(&mut self) {
        if let Some(touch) = self.seat.get_touch() {
            touch.frame(self);
        }
    }

    pub(crate) fn touch_cancel(&mut self) {
        for (_, index, _) in std::mem::take(&mut self.touch.hosts) {
            if let Some(host) = self.hosts.get_mut(index) {
                host.pointer_button(BTN_LEFT, false);
                host.pointer_leave();
            }
        }
        self.touch.clients.clear();
        let finger = self.drag_finger();
        if let Some(touch) = self.seat.get_touch() {
            touch.cancel(self);
        }
        if finger.is_some() {
            self.drag_cancelled();
        }
    }

    pub(crate) fn gesture(&mut self, gesture: GestureEvent, serial: Serial, time: u32) {
        if let Some(fingers) = gesture.fingers() {
            self.touch.gesture_to_desktop =
                gestures::claims(fingers) && !self.locked() && self.policy.gesture(&gesture);
            if self.touch.gesture_to_desktop {
                return;
            }
        } else if self.touch.gesture_to_desktop {
            self.policy.gesture(&gesture);
            if gesture.ends() {
                self.touch.gesture_to_desktop = false;
            }
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        match gesture {
            GestureEvent::SwipeBegin { fingers } => pointer.gesture_swipe_begin(
                self,
                &GestureSwipeBeginEvent {
                    serial,
                    time,
                    fingers,
                },
            ),
            GestureEvent::SwipeUpdate { dx, dy } => pointer.gesture_swipe_update(
                self,
                &GestureSwipeUpdateEvent {
                    time,
                    delta: Point::from((dx, dy)),
                },
            ),
            GestureEvent::SwipeEnd { cancelled } => pointer.gesture_swipe_end(
                self,
                &GestureSwipeEndEvent {
                    serial,
                    time,
                    cancelled,
                },
            ),
            GestureEvent::PinchBegin { fingers } => pointer.gesture_pinch_begin(
                self,
                &GesturePinchBeginEvent {
                    serial,
                    time,
                    fingers,
                },
            ),
            GestureEvent::PinchUpdate {
                dx,
                dy,
                scale,
                rotation,
            } => pointer.gesture_pinch_update(
                self,
                &GesturePinchUpdateEvent {
                    time,
                    delta: Point::from((dx, dy)),
                    scale,
                    rotation,
                },
            ),
            GestureEvent::PinchEnd { cancelled } => pointer.gesture_pinch_end(
                self,
                &GesturePinchEndEvent {
                    serial,
                    time,
                    cancelled,
                },
            ),
            GestureEvent::HoldBegin { fingers } => pointer.gesture_hold_begin(
                self,
                &GestureHoldBeginEvent {
                    serial,
                    time,
                    fingers,
                },
            ),
            GestureEvent::HoldEnd { cancelled } => pointer.gesture_hold_end(
                self,
                &GestureHoldEndEvent {
                    serial,
                    time,
                    cancelled,
                },
            ),
        }
    }
}
