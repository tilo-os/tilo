//! Pen tablets for clients.
//!
//! A client that asked for a tablet seat (`zwp_tablet_manager_v2`) gets the
//! tool itself: proximity, tip, buttons, pressure, tilt, distance, rotation,
//! slider and wheel. Every other client, and every hosted surface, sees the
//! pen as a pointer: the tip presses the primary button and the barrel
//! buttons the secondary and middle ones, so a pen works in an app that
//! knows nothing about tablets.
//!
//! A tablet's pad (its buttons, rings and strips) follows the keyboard
//! focus: the focused surface's client gets it when it took a tablet seat.
//! Otherwise [`State::tablet_pad`] says nobody took the event and the
//! backend acts on it itself (a ring scrolls).

use std::collections::HashSet;

use smithay::backend::input::{ButtonState, TabletToolCapabilities as Caps};
use smithay::reexports::wayland_protocols::wp::tablet::zv2::server::zwp_tablet_manager_v2::{
    self, ZwpTabletManagerV2,
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DataInit, Dispatch, DisplayHandle, Resource};
use smithay::utils::{Logical, Point, SERIAL_COUNTER, Serial};
use smithay::wayland::tablet_manager::{TabletManagerState, TabletSeatTrait};

use crate::input::{PadAction, TabletEvent, TabletPadEvent, TabletPhase};
use crate::state::State;

/// Linux button codes (`linux/input-event-codes.h`) a pen's buttons click
/// as a pointer.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;
const BTN_STYLUS: u32 = 0x14b;
const BTN_STYLUS2: u32 = 0x14c;

/// Clients that take tablet input, and the stroke in progress.
#[derive(Debug, Default)]
pub(crate) struct Tablets {
    clients: HashSet<ClientId>,
    /// The tablet-aware surface (and its origin) a stroke started on, while
    /// the tip is down: the stroke stays with it wherever the pen goes.
    stroke: Option<(WlSurface, Point<f64, Logical>)>,
    /// Where the tool was last, for events that carry no position.
    position: Point<f64, Logical>,
}

impl Dispatch<ZwpTabletManagerV2, (), State> for State {
    fn request(
        state: &mut State,
        client: &Client,
        resource: &ZwpTabletManagerV2,
        request: zwp_tablet_manager_v2::Request,
        data: &(),
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, State>,
    ) {
        if matches!(
            request,
            zwp_tablet_manager_v2::Request::GetTabletSeat { .. }
        ) {
            state.tablets.clients.insert(client.id());
        }
        <TabletManagerState as Dispatch<ZwpTabletManagerV2, (), State>>::request(
            state, client, resource, request, data, dh, data_init,
        );
    }
}

/// The pointer button a pen button code clicks for apps without tablet
/// support: the barrel buttons are secondary and middle, and the codes a
/// tablet's settings remap them to are those mouse buttons.
pub(crate) const fn pointer_button(code: u32) -> Option<u32> {
    match code {
        BTN_LEFT => Some(BTN_LEFT),
        BTN_STYLUS | BTN_RIGHT => Some(BTN_RIGHT),
        BTN_STYLUS2 | BTN_MIDDLE => Some(BTN_MIDDLE),
        BTN_SIDE => Some(BTN_SIDE),
        BTN_EXTRA => Some(BTN_EXTRA),
        _ => None,
    }
}

impl State {
    /// Whether the client owning `surface` takes tablet input itself.
    fn takes_tablet_input(&self, surface: &WlSurface) -> bool {
        surface
            .client()
            .is_some_and(|client| self.tablets.clients.contains(&client.id()))
    }

    /// The client surface the tool is over, if its client takes tablets.
    fn tablet_target(&self, at: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        if let Some(stroke) = &self.tablets.stroke {
            return Some(stroke.clone());
        }
        let under = if self.locked() {
            self.lock_surface_under(at)
                .map(|under| (under.surface, under.origin))
        } else if self.host_under(at).is_some() {
            None
        } else {
            self.surface_under(at)
                .map(|(_, surface, origin)| (surface, origin))
        };
        under.filter(|(surface, _)| self.takes_tablet_input(surface))
    }

    pub(crate) fn tablet_input(&mut self, event: &TabletEvent, serial: Serial, time: u32) {
        let at = if event.x.is_finite() && event.y.is_finite() {
            self.clamp_to_outputs(Point::from((event.x, event.y)))
        } else {
            self.tablets.position
        };
        self.tablets.position = at;
        let target = if event.as_pointer {
            None
        } else {
            self.tablet_target(at)
        };
        // The lock screen covers the display, stretched to it: it reads the
        // point where it drew what the tool is over.
        let at = if self.locked() {
            self.lock_point(at)
        } else {
            at
        };
        let dh = self.display.clone();
        let tablet_seat = self.seat.tablet_seat();
        let tablet = tablet_seat.add_tablet::<Self>(&dh, &event.tablet);
        let tool = tablet_seat.add_tool::<Self>(self, &dh, &event.tool);
        let Some(focus) = target else {
            // A pointer to everything else; the tool leaves any tablet-aware
            // surface it was over.
            tool.proximity_out(time);
            self.tablets.stroke = None;
            self.tablet_as_pointer(event, at, serial, time);
            return;
        };
        if event.phase == TabletPhase::ProximityOut {
            tool.proximity_out(time);
            self.tablets.stroke = None;
            return;
        }
        let capabilities = event.tool.capabilities;
        if capabilities.contains(Caps::PRESSURE) {
            tool.pressure(event.pressure);
        }
        if capabilities.contains(Caps::DISTANCE) {
            tool.distance(event.distance);
        }
        if capabilities.contains(Caps::TILT) {
            tool.tilt(event.tilt);
        }
        if capabilities.contains(Caps::ROTATION) {
            tool.rotation(event.rotation);
        }
        if capabilities.contains(Caps::SLIDER) {
            tool.slider_position(event.slider);
        }
        if capabilities.contains(Caps::WHEEL) && (event.wheel.0 != 0.0 || event.wheel.1 != 0) {
            tool.wheel(event.wheel.0, event.wheel.1);
        }
        if event.phase == TabletPhase::ProximityIn {
            tool.proximity_in(at, focus.clone(), &tablet, serial, time);
        } else {
            tool.motion(at, Some(focus.clone()), &tablet, serial, time);
        }
        match event.phase {
            TabletPhase::TipDown => {
                self.tablets.stroke = Some(focus);
                tool.tip_down(serial, time);
            }
            TabletPhase::TipUp => {
                self.tablets.stroke = None;
                tool.tip_up(time);
            }
            TabletPhase::Button { button, pressed } => tool.button(
                button,
                if pressed {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                },
                serial,
                time,
            ),
            TabletPhase::ProximityIn | TabletPhase::ProximityOut | TabletPhase::Motion => {}
        }
    }

    /// Pads follow the keyboard: the focused surface's client gets their
    /// buttons, rings and strips.
    pub(crate) fn pads_follow_keyboard(&self, focused: Option<&WlSurface>) {
        let time = self.now_ms();
        self.seat
            .tablet_seat()
            .set_pad_focus(focused, SERIAL_COUNTER.next_serial(), time);
    }

    /// One pad event from a backend that acts on what no client takes; see
    /// [`Self::tablet_pad`].
    pub(crate) fn pad_input(&mut self, event: &TabletPadEvent) -> bool {
        self.note_input();
        let time = self.now_ms();
        self.tablet_pad(event, SERIAL_COUNTER.next_serial(), time)
    }

    /// Route one pad event to the client the pad is focused on. Returns
    /// whether a client took it: `false` leaves a button, ring or strip to
    /// the compositor. Plugging, unplugging and mode switches are always
    /// taken.
    pub(crate) fn tablet_pad(&self, event: &TabletPadEvent, serial: Serial, time: u32) -> bool {
        let tablet_seat = self.seat.tablet_seat();
        if event.action == PadAction::Removed {
            tablet_seat.remove_pad(&event.pad);
            return true;
        }
        let dh = self.display.clone();
        // A pad seen first through an event (its plug went unnoticed) is
        // added then.
        let tablet = tablet_seat.add_tablet::<Self>(&dh, &event.tablet);
        let pad = tablet_seat.add_pad::<Self>(&dh, &event.pad, &tablet);
        let pressed = |pressed| {
            if pressed {
                ButtonState::Pressed
            } else {
                ButtonState::Released
            }
        };
        match event.action {
            PadAction::Added | PadAction::Removed => {}
            PadAction::Mode { group, mode } => {
                pad.mode_switch(group as usize, mode, serial, time);
            }
            _ if !pad.has_focused_client() => return false,
            PadAction::Button {
                button,
                pressed: down,
            } => {
                pad.button(button, pressed(down), time);
            }
            PadAction::Ring { ring, degrees } => pad.ring(ring, degrees, time),
            PadAction::Strip { strip, position } => pad.strip(strip, position, time),
        }
        true
    }

    /// The pointer events a tool stands for where tablets are not taken:
    /// hover moves the pointer, the tip presses [`TabletEvent::tip_button`]
    /// and the barrel buttons click their pointer buttons.
    fn tablet_as_pointer(
        &mut self,
        event: &TabletEvent,
        at: Point<f64, Logical>,
        serial: Serial,
        time: u32,
    ) {
        if event.phase == TabletPhase::ProximityOut {
            return;
        }
        self.pointer_motion(at, serial, time);
        let press = match event.phase {
            TabletPhase::TipDown => event.tip_button.map(|button| (button, true)),
            TabletPhase::TipUp => event.tip_button.map(|button| (button, false)),
            TabletPhase::Button { button, pressed } => {
                pointer_button(button).map(|button| (button, pressed))
            }
            TabletPhase::ProximityIn | TabletPhase::ProximityOut | TabletPhase::Motion => None,
        };
        if let Some((button, pressed)) = press {
            self.pointer_button(button, pressed, serial, time);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pen_buttons_click_their_pointer_buttons() {
        assert_eq!(pointer_button(BTN_STYLUS), Some(BTN_RIGHT));
        assert_eq!(pointer_button(BTN_STYLUS2), Some(BTN_MIDDLE));
        for code in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA] {
            assert_eq!(
                pointer_button(code),
                Some(code),
                "a barrel button remapped to a mouse button clicks it"
            );
        }
        assert_eq!(
            pointer_button(0x149),
            None,
            "a third barrel button left as itself clicks nothing"
        );
    }
}
