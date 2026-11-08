//! Touchscreens and touchpad gestures, routed.
//! A touch on a hosted surface reaches it as a pointer press (hosts take pointer input only); the
//! desktop is offered every touch first and may take it (the top edge's swipe); every other touch
//! goes to the client surface under it through the seat's touch handle and stays with that surface
//! until the finger lifts. Touchpad gestures of one or two fingers belong to the client under the
//! pointer (`zwp_pointer_gestures_v1`: a browser's pinch-to-zoom); three or more are offered to the
//! desktop first ([`crate::WindowPolicy::gesture`]).

use smithay::backend::input::ButtonState;
use smithay::input::pointer::{
    ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, MotionEvent as PointerMotionEvent,
};
use smithay::input::touch::{DownEvent, MotionEvent, UpEvent};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, SERIAL_COUNTER, Serial};

use crate::gestures;
use crate::input::{BTN_LEFT, GestureEvent};
use crate::policy::TouchSample;
use crate::state::State;

/// A client surface and its origin in the logical layout.
pub(crate) type Focus = Option<(WlSurface, Point<f64, Logical>)>;

/// Whether the client `surface` belongs to bound `wl_touch` . One that did not has the seat's touch
/// objects of no use: a finger reaches it as a pointer press at the same place, so a tap still
/// clicks.
fn binds_touch(surface: &WlSurface) -> bool {
    surface.client().is_some_and(|client| {
        client
            .get_data::<crate::state::ClientState>()
            .is_some_and(crate::state::ClientState::binds_touch)
    })
}

/// Where each finger on the screen went down.
#[derive(Debug, Default)]
pub(crate) struct Touches {
    /// Fingers on a hosted surface, by slot: the host's index and where the finger is.
    hosts: Vec<(u32, usize, Point<f64, Logical>)>,
    /// Fingers the desktop took, by slot and where the finger is ([`crate::WindowPolicy::touch`]):
    /// nothing under them sees them.
    pub(crate) desktop: Vec<(u32, Point<f64, Logical>)>,
    /// Fingers on client surfaces.
    pub(crate) clients: Vec<Finger>,
    /// The gesture in flight belongs to the desktop.
    gesture_to_desktop: bool,
}

/// A finger on a client surface.
#[derive(Debug)]
pub(crate) struct Finger {
    /// Its slot.
    pub(crate) slot: u32,
    /// The surface and its origin in the logical layout as it went down.
    pub(crate) focus: Focus,
    /// Where the finger is.
    pub(crate) at: Point<f64, Logical>,
    /// The client bound no `wl_touch` : the finger reaches it as a pointer press, which is the only
    /// way it can be typed on at all.
    pointer: bool,
    /// When it went down, for the times the pointer events carry.
    time: u32,
}

impl Touches {
    /// A finger holding a hosted surface, taken from it: the hosted app starts a drag that follows
    /// it. Its slot, the host and where it is.
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
        if !self.locked() && self.policy.touch(TouchSample::Down { x: at.x, y: at.y }) {
            self.touch.desktop.retain(|(held, _)| *held != slot);
            self.touch.desktop.push((slot, at));
            return;
        }
        if !self.locked()
            && let Some(index) = self.host_under(at)
            && let Some(host) = self.hosts.get_mut(index)
        {
            host.pointer_motion(at.x, at.y);
            host.pointer_button(BTN_LEFT, true);
            self.touch.hosts.push((slot, index, at));
            return;
        }
        let (focus, at) = if self.locked() {
            self.lock_surface_under(at).map_or((None, at), |under| {
                (Some((under.surface, under.origin)), under.at)
            })
        } else {
            self.focus_on_press(at, serial);
            (
                self.surface_under(at)
                    .map(|(_, surface, origin)| (surface, origin)),
                at,
            )
        };
        self.touch.clients.retain(|finger| finger.slot != slot);
        // One finger at a time holds the pointer down: a second one on a client that bound no touch
        // has no pointer of its own, and pressing it again would be a press the client never
        // lifted.
        let pointer = focus
            .as_ref()
            .is_some_and(|(surface, _)| !binds_touch(surface))
            && !self.touch.clients.iter().any(|finger| finger.pointer);
        self.touch.clients.push(Finger {
            slot,
            focus: focus.clone(),
            at,
            pointer,
            time,
        });
        if let Some((surface, _)) = focus.as_ref() {
            self.note_input_on(surface);
        }
        if pointer {
            self.touch_pointer(focus, at, Some(ButtonState::Pressed), serial, time);
            return;
        }
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
        // A finger on an X11 window is the press Xwayland runs its drags under, as the pointer's
        // is.
        self.x11_drag_motion(at);
    }

    pub(crate) fn touch_motion(&mut self, slot: u32, at: Point<f64, Logical>, time: u32) {
        let at = self.clamp_to_outputs(at);
        if self.drag_finger() == Some(slot) {
            self.drag_touch_motion(slot, at, time);
            return;
        }
        if let Some((_, last)) = self
            .touch
            .desktop
            .iter_mut()
            .find(|(held, _)| *held == slot)
        {
            *last = at;
            self.policy.touch(TouchSample::Move { x: at.x, y: at.y });
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
        let (focus, pointer) = self
            .touch
            .clients
            .iter_mut()
            .find(|finger| finger.slot == slot)
            .map(|finger| {
                finger.at = at;
                finger.time = time;
                (finger.focus.clone(), finger.pointer)
            })
            .unwrap_or_default();
        // The lock screen covers the display, stretched to it: it reads the point where it drew
        // what the finger is over.
        let at = if self.locked() {
            self.lock_point(at)
        } else {
            at
        };
        if pointer {
            self.touch_pointer(focus, at, None, SERIAL_COUNTER.next_serial(), time);
            return;
        }
        if let Some(touch) = self.seat.get_touch() {
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
        self.x11_drag_motion(at);
    }

    pub(crate) fn touch_up(&mut self, slot: u32, serial: Serial, time: u32) {
        if self.drag_finger() == Some(slot) {
            self.drag_touch_up(slot, serial, time);
            return;
        }
        if let Some(position) = self
            .touch
            .desktop
            .iter()
            .position(|(held, _)| *held == slot)
        {
            let (_, at) = self.touch.desktop.remove(position);
            self.policy.touch(TouchSample::Lift { x: at.x, y: at.y });
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
        let lifted = self
            .touch
            .clients
            .iter()
            .position(|finger| finger.slot == slot)
            .map(|index| self.touch.clients.remove(index));
        let at = lifted.as_ref().map_or(self.pointer, |finger| finger.at);
        if let Some(finger) = lifted.filter(|finger| finger.pointer) {
            let at = if self.locked() {
                self.lock_point(finger.at)
            } else {
                finger.at
            };
            self.touch_pointer(finger.focus, at, Some(ButtonState::Released), serial, time);
            self.x11_drag_motion(at);
            return;
        }
        if let Some(touch) = self.seat.get_touch() {
            touch.up(
                self,
                &UpEvent {
                    slot: Some(slot).into(),
                    serial,
                    time,
                },
            );
        }
        // The X11 drag the finger ran is over: its proxy comes down.
        self.x11_drag_motion(at);
    }

    pub(crate) fn touch_frame(&mut self) {
        if let Some(touch) = self.seat.get_touch() {
            touch.frame(self);
        }
    }

    pub(crate) fn touch_cancel(&mut self) {
        if !self.touch.desktop.is_empty() {
            self.touch.desktop.clear();
            self.policy.touch(TouchSample::Cancel);
        }
        for (_, index, _) in std::mem::take(&mut self.touch.hosts) {
            if let Some(host) = self.hosts.get_mut(index) {
                host.pointer_button(BTN_LEFT, false);
                host.pointer_leave();
            }
        }
        // A cancelled contact leaves no drag running: the proxy Xwayland raised for it comes down.
        let at = self
            .touch
            .clients
            .last()
            .map_or(self.pointer, |finger| finger.at);
        for finger in std::mem::take(&mut self.touch.clients) {
            if finger.pointer {
                let at = if self.locked() {
                    self.lock_point(finger.at)
                } else {
                    finger.at
                };
                self.touch_pointer(
                    finger.focus,
                    at,
                    Some(ButtonState::Released),
                    SERIAL_COUNTER.next_serial(),
                    finger.time,
                );
            }
        }
        let finger = self.drag_finger();
        if let Some(touch) = self.seat.get_touch() {
            touch.cancel(self);
        }
        if finger.is_some() {
            self.drag_cancelled();
        }
        self.x11_drag_motion(at);
    }

    /// A finger on a client that bound no `wl_touch` , as the pointer it can take: moved to the
    /// finger, and pressed there or lifted as `button` says.
    fn touch_pointer(
        &mut self,
        focus: Focus,
        at: Point<f64, Logical>,
        button: Option<ButtonState>,
        serial: Serial,
        time: u32,
    ) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        pointer.motion(
            self,
            focus,
            &PointerMotionEvent {
                location: at,
                serial,
                time,
            },
        );
        if let Some(state) = button {
            pointer.button(
                self,
                &ButtonEvent {
                    button: BTN_LEFT,
                    state,
                    serial,
                    time,
                },
            );
        }
        pointer.frame(self);
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
