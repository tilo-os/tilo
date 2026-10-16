//! Pen tablets for clients.
//!
//! A client that asked for a tablet seat (`zwp_tablet_manager_v2`) gets the
//! tool itself: proximity, tip, buttons, pressure, tilt, distance, rotation,
//! slider and wheel. Every other client sees the pen as a pointer, with the
//! tip acting as the primary button and the barrel buttons as secondary and
//! middle, so a pen still works in an app that knows nothing about tablets.

use std::collections::HashSet;

use smithay::backend::input::{
    ButtonState, TabletToolCapabilities, TabletToolDescriptor, TabletToolType,
};
use smithay::reexports::wayland_protocols::wp::tablet::zv2::server::zwp_tablet_manager_v2::{
    self, ZwpTabletManagerV2,
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::{Client, DataInit, Dispatch, DisplayHandle};
use smithay::utils::Point;
use smithay::wayland::tablet_manager::{TabletDescriptor, TabletManagerState, TabletSeatTrait};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::compositor::backend::{TabletInput, TabletPhase, TabletToolKind};
use crate::compositor::state::State;

/// Linux button codes (`linux/input-event-codes.h`) a pen reports, or
/// that its buttons are remapped to in a tablet's settings.
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;
pub const BTN_SIDE: u32 = 0x113;
pub const BTN_EXTRA: u32 = 0x114;
/// The pen barrel button nearest the tip.
pub const BTN_STYLUS: u32 = 0x14b;
/// The pen barrel button further from the tip.
pub const BTN_STYLUS2: u32 = 0x14c;
/// A third barrel button, on pens that have one.
pub const BTN_STYLUS3: u32 = 0x149;

/// Clients that asked for a tablet seat, and the pads announced to them.
#[derive(Default)]
pub struct TabletClients {
    clients: HashSet<ClientId>,
    pub(crate) pads: crate::compositor::tablet_pad::TabletPads,
    /// The tablet-aware surface (and its global origin) a pen stroke
    /// started in, while the tip is down.
    pub(crate) stroke: Option<(u64, (f64, f64))>,
}

impl TabletClients {
    pub(crate) fn client_ids(&self) -> impl Iterator<Item = &ClientId> {
        self.clients.iter()
    }
}

/// Records which clients ask for a tablet seat, then hands the request to
/// Smithay's tablet manager unchanged.
pub struct TabletSeatTracker;

impl Dispatch<ZwpTabletManagerV2, (), State> for TabletSeatTracker {
    fn request(
        state: &mut State,
        client: &Client,
        resource: &ZwpTabletManagerV2,
        request: zwp_tablet_manager_v2::Request,
        data: &(),
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, State>,
    ) {
        let new_seat = matches!(
            request,
            zwp_tablet_manager_v2::Request::GetTabletSeat { .. }
        );
        if new_seat {
            state.tablet_clients.clients.insert(client.id());
        }
        <TabletManagerState as Dispatch<ZwpTabletManagerV2, (), State>>::request(
            state, client, resource, request, data, dh, data_init,
        );
        if new_seat {
            // Smithay announced the pens; the pads are announced here.
            state.announce_tablet_pads();
        }
    }
}

impl State {
    /// Whether the client owning `surface_id` takes tablet input itself.
    pub fn surface_takes_tablet_input(&self, surface_id: u64) -> bool {
        self.client_input
            .surface(surface_id)
            .and_then(smithay::reexports::wayland_server::Resource::client)
            .is_some_and(|client| self.tablet_clients.clients.contains(&client.id()))
    }

    /// Deliver one tool event over `zwp_tablet_tool_v2` to `target`
    /// (`(surface id, global origin)`), or take the tool out of proximity of
    /// whatever it was over when `target` is `None`.
    pub fn forward_tablet(&mut self, input: &TabletInput, target: Option<(u64, (f64, f64))>) {
        let dh = self.display_handle.clone();
        let tablet_seat = self.seat.tablet_seat();
        let tablet = tablet_seat.add_tablet::<Self>(
            &dh,
            &TabletDescriptor {
                name: input.device.name.clone(),
                usb_id: input.device.usb_id,
                syspath: input.device.syspath.clone(),
            },
        );
        let tool = tablet_seat.add_tool::<Self>(self, &dh, &tool_descriptor(input));
        let focus = target.and_then(|(surface_id, origin)| {
            self.client_input
                .surface(surface_id)
                .cloned()
                .map(|surface| (surface, Point::from(origin)))
        });
        let serial = self.next_serial();
        let time = input.time_msec;
        let position = Point::from(input.position);
        if matches!(input.phase, TabletPhase::ProximityOut) || focus.is_none() {
            tool.proximity_out(time);
            return;
        }
        if input.tool.pressure {
            tool.pressure(input.pressure);
        }
        if input.tool.distance {
            tool.distance(input.distance);
        }
        if input.tool.tilt {
            tool.tilt(input.tilt);
        }
        if input.tool.rotation {
            tool.rotation(input.rotation);
        }
        if input.tool.slider {
            tool.slider_position(input.slider);
        }
        if input.tool.wheel && (input.wheel.0 != 0.0 || input.wheel.1 != 0) {
            tool.wheel(input.wheel.0, input.wheel.1);
        }
        tool.motion(position, focus, &tablet, serial, time);
        match input.phase {
            TabletPhase::TipDown => tool.tip_down(serial, time),
            TabletPhase::TipUp => tool.tip_up(time),
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
}

fn tool_descriptor(input: &TabletInput) -> TabletToolDescriptor {
    let tool = input.tool;
    let mut capabilities = TabletToolCapabilities::empty();
    for (present, capability) in [
        (tool.pressure, TabletToolCapabilities::PRESSURE),
        (tool.distance, TabletToolCapabilities::DISTANCE),
        (tool.tilt, TabletToolCapabilities::TILT),
        (tool.rotation, TabletToolCapabilities::ROTATION),
        (tool.slider, TabletToolCapabilities::SLIDER),
        (tool.wheel, TabletToolCapabilities::WHEEL),
    ] {
        if present {
            capabilities |= capability;
        }
    }
    TabletToolDescriptor {
        tool_type: match tool.kind {
            TabletToolKind::Pen => TabletToolType::Pen,
            TabletToolKind::Eraser => TabletToolType::Eraser,
            TabletToolKind::Brush => TabletToolType::Brush,
            TabletToolKind::Pencil => TabletToolType::Pencil,
            TabletToolKind::Airbrush => TabletToolType::Airbrush,
            TabletToolKind::Mouse => TabletToolType::Mouse,
            TabletToolKind::Lens => TabletToolType::Lens,
            TabletToolKind::Totem => TabletToolType::Totem,
            TabletToolKind::Unknown => TabletToolType::Unknown,
        },
        hardware_serial: tool.serial,
        hardware_id_wacom: tool.wacom_id,
        capabilities,
    }
}

/// The pointer events a pen stands for in an app without tablet support:
/// hover moves the pointer, the tip is the primary button and the barrel
/// buttons are secondary and middle. Leaving proximity moves nothing.
pub fn tablet_pointer_events(input: &TabletInput) -> Vec<WindowEvent> {
    tool_pointer_events(input, Some(BTN_LEFT))
}

/// Like [`tablet_pointer_events`], but the tip presses the pointer button
/// `tip` (a Linux button code), or nothing when `None`: an eraser mapped to
/// a mouse button, or to a shortcut that runs on its own.
pub fn tool_pointer_events(input: &TabletInput, tip: Option<u32>) -> Vec<WindowEvent> {
    let moved = ui::synthetic_input::cursor_moved(input.position.0, input.position.1);
    let button = |button: MouseButton, pressed: bool| {
        ui::synthetic_input::mouse_input(
            if pressed {
                ElementState::Pressed
            } else {
                ElementState::Released
            },
            button,
        )
    };
    let press = |code: Option<u32>, pressed: bool| match code.and_then(pointer_button) {
        Some(mouse) => vec![moved.clone(), button(mouse, pressed)],
        None => vec![moved.clone()],
    };
    match input.phase {
        TabletPhase::ProximityOut => Vec::new(),
        TabletPhase::TipDown => press(tip, true),
        TabletPhase::TipUp => press(tip, false),
        TabletPhase::Button { button, pressed } => press(Some(button), pressed),
        TabletPhase::ProximityIn | TabletPhase::Motion => vec![moved],
    }
}

/// The pointer button a pen button code clicks: the barrel buttons are
/// secondary and middle, and the codes a tablet's settings remap them to
/// are those mouse buttons.
fn pointer_button(code: u32) -> Option<MouseButton> {
    match code {
        BTN_LEFT => Some(MouseButton::Left),
        BTN_STYLUS | BTN_RIGHT => Some(MouseButton::Right),
        BTN_STYLUS2 | BTN_MIDDLE => Some(MouseButton::Middle),
        BTN_SIDE => Some(MouseButton::Back),
        BTN_EXTRA => Some(MouseButton::Forward),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::backend::{TabletDevice, TabletTool};

    fn pen(phase: TabletPhase) -> TabletInput {
        TabletInput {
            device: TabletDevice {
                name: "Wacom Intuos".into(),
                usb_id: Some((0x056a, 0x0374)),
                syspath: None,
            },
            tool: TabletTool {
                kind: TabletToolKind::Pen,
                serial: 0x0dea_dbee,
                wacom_id: 0x0802,
                pressure: true,
                distance: true,
                tilt: true,
                rotation: false,
                slider: false,
                wheel: false,
            },
            phase,
            position: (812.5, 403.25),
            pressure: 0.42,
            distance: 0.0,
            tilt: (12.0, -30.0),
            rotation: 0.0,
            slider: 0.0,
            wheel: (0.0, 0),
            time_msec: 7,
        }
    }

    fn kinds(events: &[WindowEvent]) -> Vec<String> {
        events
            .iter()
            .map(|event| match event {
                WindowEvent::CursorMoved { position, .. } => {
                    format!("move {} {}", position.x, position.y)
                }
                WindowEvent::MouseInput { state, button, .. } => format!("{button:?} {state:?}"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_pen_without_tablet_support_acts_as_a_pointer() {
        assert_eq!(
            kinds(&tablet_pointer_events(&pen(TabletPhase::Motion))),
            ["move 812.5 403.25"]
        );
        assert_eq!(
            kinds(&tablet_pointer_events(&pen(TabletPhase::TipDown))),
            ["move 812.5 403.25", "Left Pressed"]
        );
        assert_eq!(
            kinds(&tablet_pointer_events(&pen(TabletPhase::TipUp))),
            ["move 812.5 403.25", "Left Released"]
        );
        assert_eq!(
            kinds(&tablet_pointer_events(&pen(TabletPhase::Button {
                button: BTN_STYLUS,
                pressed: true
            }))),
            ["move 812.5 403.25", "Right Pressed"]
        );
        assert_eq!(
            kinds(&tablet_pointer_events(&pen(TabletPhase::Button {
                button: BTN_STYLUS2,
                pressed: false
            }))),
            ["move 812.5 403.25", "Middle Released"]
        );
        assert!(tablet_pointer_events(&pen(TabletPhase::ProximityOut)).is_empty());
        for (code, expected) in [
            (BTN_LEFT, "Left Pressed"),
            (BTN_RIGHT, "Right Pressed"),
            (BTN_MIDDLE, "Middle Pressed"),
            (BTN_SIDE, "Back Pressed"),
            (BTN_EXTRA, "Forward Pressed"),
        ] {
            assert_eq!(
                kinds(&tablet_pointer_events(&pen(TabletPhase::Button {
                    button: code,
                    pressed: true
                }))),
                ["move 812.5 403.25", expected],
                "a barrel button remapped to a mouse button clicks it"
            );
        }
    }

    #[test]
    fn an_eraser_is_a_tool_of_its_own_and_can_press_another_button() {
        let mut eraser = pen(TabletPhase::TipDown);
        eraser.tool.kind = TabletToolKind::Eraser;
        let descriptor = tool_descriptor(&eraser);
        assert_eq!(descriptor.tool_type, TabletToolType::Eraser);
        assert_ne!(
            descriptor,
            tool_descriptor(&pen(TabletPhase::TipDown)),
            "the eraser end is announced apart from the tip"
        );
        assert_eq!(
            kinds(&tool_pointer_events(&eraser, Some(BTN_RIGHT))),
            ["move 812.5 403.25", "Right Pressed"]
        );
        assert_eq!(
            kinds(&tool_pointer_events(&eraser, None)),
            ["move 812.5 403.25"],
            "an eraser running a shortcut clicks nothing"
        );
        eraser.phase = TabletPhase::Button {
            button: BTN_STYLUS3,
            pressed: true,
        };
        assert_eq!(
            kinds(&tablet_pointer_events(&eraser)),
            ["move 812.5 403.25"],
            "a third barrel button left as itself clicks nothing"
        );
    }

    #[test]
    fn the_tool_descriptor_carries_the_reported_axes() {
        let descriptor = tool_descriptor(&pen(TabletPhase::Motion));
        assert_eq!(descriptor.tool_type, TabletToolType::Pen);
        assert_eq!(descriptor.hardware_serial, 0x0dea_dbee);
        assert_eq!(
            descriptor.capabilities,
            TabletToolCapabilities::PRESSURE
                | TabletToolCapabilities::DISTANCE
                | TabletToolCapabilities::TILT
        );
    }
}
