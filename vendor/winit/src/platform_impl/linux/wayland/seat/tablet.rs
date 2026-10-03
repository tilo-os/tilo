//! Pen tablets.
//!
//! Binds `zwp_tablet_manager_v2` and takes a tablet seat for every `wl_seat`.
//! A compositor then reports a stylus to this client as a tablet tool rather
//! than as a pointer, so the tool's events are turned back into the pointer
//! events every window understands — moving the cursor, the tip as the left
//! button and the barrel buttons as the right and middle ones — each
//! preceded by a [`WindowEvent::PenSample`] carrying pressure, tilt and
//! whether the eraser end is in use.
//!
//! Pads are declined: their objects are destroyed as soon as they arrive, so
//! a compositor knows to apply its own mapping of the pad's buttons, rings
//! and strips instead of sending them here.

use std::sync::Mutex;

use sctk::compositor::SurfaceData;
use sctk::globals::GlobalData;
use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::{
    delegate_dispatch, event_created_child, Connection, Dispatch, Proxy, QueueHandle, WEnum,
};
use sctk::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use sctk::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use sctk::reexports::protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2::ZwpTabletManagerV2,
    zwp_tablet_pad_dial_v2::ZwpTabletPadDialV2,
    zwp_tablet_pad_group_v2::{self, ZwpTabletPadGroupV2},
    zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2,
    zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2,
    zwp_tablet_pad_v2::{self, ZwpTabletPadV2},
    zwp_tablet_seat_v2::{self, ZwpTabletSeatV2},
    zwp_tablet_tool_v2::{self, ZwpTabletToolV2},
    zwp_tablet_v2::{self, ZwpTabletV2},
};

use crate::dpi::{LogicalPosition, PhysicalPosition};
use crate::event::{ElementState, MouseButton, WindowEvent};
use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::{self, DeviceId, WindowId};

/// Linux button codes (`linux/input-event-codes.h`) a tool reports.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;
/// The barrel button nearest the tip.
const BTN_STYLUS: u32 = 0x14b;
/// The barrel button further from the tip.
const BTN_STYLUS2: u32 = 0x14c;

/// The tablet manager, and the cursor shapes a tool shows over a window.
pub struct TabletState {
    manager: ZwpTabletManagerV2,
    cursor_shapes: Option<WpCursorShapeManagerV1>,
}

impl TabletState {
    /// Bind the tablet manager.
    pub fn new(
        globals: &GlobalList,
        queue_handle: &QueueHandle<WinitState>,
    ) -> Result<Self, BindError> {
        // Version 1: version 2 adds pad dials and bus types, which a client
        // that declines pads has no use for.
        let manager = globals.bind(queue_handle, 1..=1, GlobalData)?;
        let cursor_shapes = globals.bind(queue_handle, 1..=1, GlobalData).ok();
        Ok(Self { manager, cursor_shapes })
    }

    /// The tablet seat of `seat`.
    pub fn tablet_seat(
        &self,
        seat: &sctk::reexports::client::protocol::wl_seat::WlSeat,
        queue_handle: &QueueHandle<WinitState>,
    ) -> ZwpTabletSeatV2 {
        self.manager.get_tablet_seat(seat, queue_handle, GlobalData)
    }
}

/// One tablet tool: what it is, where it is and what it holds down.
#[derive(Debug, Default)]
pub struct TabletToolData {
    inner: Mutex<ToolState>,
}

#[derive(Debug, Default)]
struct ToolState {
    eraser: bool,
    /// The window the tool is over, `None` when out of proximity or over a
    /// surface that is no window's own.
    window: Option<WindowId>,
    /// Surface-local, in logical pixels.
    position: (f64, f64),
    pressure: f64,
    tilt: (f64, f64),
    /// The pointer buttons the tool holds down in `window`.
    held: Vec<MouseButton>,
    /// What changed since the last `frame`.
    pending: Pending,
    /// The cursor shape of this tool, made on its first proximity.
    cursor: Option<WpCursorShapeDeviceV1>,
}

#[derive(Debug, Default)]
struct Pending {
    /// Came into proximity of this window.
    entered: Option<WindowId>,
    /// Came into proximity of a surface that is no window's own.
    entered_elsewhere: bool,
    left: bool,
    moved: bool,
    down: bool,
    up: bool,
    buttons: Vec<(MouseButton, bool)>,
}

impl ToolState {
    /// The pointer events one `frame` of the tool stands for, each preceded
    /// by the sample it was made with. `scale_factor` gives a window's scale
    /// factor, `None` when it is gone.
    fn frame(
        &mut self,
        device_id: crate::event::DeviceId,
        scale_factor: impl Fn(WindowId) -> Option<f64>,
    ) -> Vec<(WindowId, WindowEvent)> {
        let pending = std::mem::take(&mut self.pending);
        let mut events = Vec::new();
        if pending.entered.is_some() || pending.entered_elsewhere {
            self.window = pending.entered;
            self.held.clear();
        }
        let Some(window) = self.window else {
            return events;
        };
        let Some(scale_factor) = scale_factor(window) else {
            self.window = None;
            self.held.clear();
            return events;
        };
        let moved = pending.moved || pending.entered.is_some();
        if pending.entered.is_some() {
            events.push((window, WindowEvent::CursorEntered { device_id }));
        }
        let sample = WindowEvent::PenSample {
            device_id,
            pressure: self.pressure,
            tilt: self.tilt,
            eraser: self.eraser,
        };
        let pointer = |events: &mut Vec<(WindowId, WindowEvent)>, event| {
            events.push((window, sample.clone()));
            events.push((window, event));
        };
        let button = |button, state| WindowEvent::MouseInput { device_id, state, button };
        if moved {
            let position: PhysicalPosition<f64> =
                LogicalPosition::new(self.position.0, self.position.1).to_physical(scale_factor);
            pointer(&mut events, WindowEvent::CursorMoved { device_id, position });
        }
        let mut changes = Vec::with_capacity(pending.buttons.len() + 2);
        if pending.down {
            changes.push((MouseButton::Left, true));
        }
        changes.extend(pending.buttons);
        if pending.up {
            changes.push((MouseButton::Left, false));
        }
        if pending.left {
            changes.extend(self.held.iter().map(|held| (*held, false)));
        }
        for (mouse, pressed) in changes {
            let holding = self.held.contains(&mouse);
            if pressed && !holding {
                self.held.push(mouse);
                pointer(&mut events, button(mouse, ElementState::Pressed));
            } else if !pressed && holding {
                self.held.retain(|held| *held != mouse);
                pointer(&mut events, button(mouse, ElementState::Released));
            }
        }
        if pending.left {
            self.window = None;
            events.push((window, WindowEvent::CursorLeft { device_id }));
        }
        events
    }
}

/// The pointer button a tool button clicks: the barrel buttons are the
/// secondary and middle buttons, and the mouse buttons a compositor may
/// remap them to are themselves.
fn tool_button(code: u32) -> Option<MouseButton> {
    match code {
        BTN_LEFT => Some(MouseButton::Left),
        BTN_STYLUS | BTN_RIGHT => Some(MouseButton::Right),
        BTN_STYLUS2 | BTN_MIDDLE => Some(MouseButton::Middle),
        BTN_SIDE => Some(MouseButton::Back),
        BTN_EXTRA => Some(MouseButton::Forward),
        _ => None,
    }
}

/// The protocol's pressure, `0..=65535`, as `0.0..=1.0`.
fn pressure_fraction(pressure: u32) -> f64 {
    (f64::from(pressure) / 65535.0).clamp(0.0, 1.0)
}

/// The window whose own surface `surface` is, if any.
fn window_of(
    state: &WinitState,
    surface: &sctk::reexports::client::protocol::wl_surface::WlSurface,
) -> Option<WindowId> {
    let data = surface.data::<SurfaceData>()?;
    // A tool over a subsurface such as a client-side decoration is not over
    // the window's content.
    if data.parent_surface().is_some_and(|parent| parent != surface) {
        return None;
    }
    let window_id = wayland::make_wid(surface);
    state.windows.borrow().contains_key(&window_id).then_some(window_id)
}

impl Dispatch<ZwpTabletManagerV2, GlobalData, WinitState> for TabletState {
    fn event(
        _: &mut WinitState,
        _: &ZwpTabletManagerV2,
        _: <ZwpTabletManagerV2 as Proxy>::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        // The manager has no events.
    }
}

impl Dispatch<ZwpTabletSeatV2, GlobalData, WinitState> for TabletState {
    fn event(
        _: &mut WinitState,
        _: &ZwpTabletSeatV2,
        event: zwp_tablet_seat_v2::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        if let zwp_tablet_seat_v2::Event::PadAdded { id } = event {
            id.destroy();
        }
    }

    event_created_child!(WinitState, ZwpTabletSeatV2, [
        zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (ZwpTabletV2, GlobalData),
        zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (ZwpTabletToolV2, TabletToolData::default()),
        zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => (ZwpTabletPadV2, GlobalData),
    ]);
}

impl Dispatch<ZwpTabletV2, GlobalData, WinitState> for TabletState {
    fn event(
        _: &mut WinitState,
        tablet: &ZwpTabletV2,
        event: zwp_tablet_v2::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        if let zwp_tablet_v2::Event::Removed = event {
            tablet.destroy();
        }
    }
}

impl Dispatch<ZwpTabletToolV2, TabletToolData, WinitState> for TabletState {
    fn event(
        state: &mut WinitState,
        tool: &ZwpTabletToolV2,
        event: zwp_tablet_tool_v2::Event,
        data: &TabletToolData,
        _: &Connection,
        queue_handle: &QueueHandle<WinitState>,
    ) {
        let mut inner = data.inner.lock().unwrap();
        match event {
            zwp_tablet_tool_v2::Event::Type { tool_type } => {
                inner.eraser = tool_type == WEnum::Value(zwp_tablet_tool_v2::Type::Eraser);
            },
            zwp_tablet_tool_v2::Event::ProximityIn { serial, surface, .. } => {
                match window_of(state, &surface) {
                    Some(window) => inner.pending.entered = Some(window),
                    None => inner.pending.entered_elsewhere = true,
                }
                inner.pending.left = false;
                inner.pressure = 0.0;
                if inner.cursor.is_none() {
                    inner.cursor =
                        state.tablet.as_ref().and_then(|tablet| tablet.cursor_shapes.as_ref()).map(
                            |shapes| shapes.get_tablet_tool_v2(tool, queue_handle, GlobalData),
                        );
                }
                if let Some(cursor) = &inner.cursor {
                    cursor.set_shape(serial, Shape::Default);
                }
            },
            zwp_tablet_tool_v2::Event::ProximityOut => inner.pending.left = true,
            zwp_tablet_tool_v2::Event::Down { .. } => inner.pending.down = true,
            zwp_tablet_tool_v2::Event::Up => inner.pending.up = true,
            zwp_tablet_tool_v2::Event::Motion { x, y } => {
                inner.position = (x, y);
                inner.pending.moved = true;
            },
            zwp_tablet_tool_v2::Event::Pressure { pressure } => {
                inner.pressure = pressure_fraction(pressure);
                inner.pending.moved = true;
            },
            zwp_tablet_tool_v2::Event::Tilt { tilt_x, tilt_y } => {
                inner.tilt = (tilt_x, tilt_y);
                inner.pending.moved = true;
            },
            zwp_tablet_tool_v2::Event::Button { button, state: button_state, .. } => {
                if let Some(mouse) = tool_button(button) {
                    let pressed =
                        button_state == WEnum::Value(zwp_tablet_tool_v2::ButtonState::Pressed);
                    inner.pending.buttons.push((mouse, pressed));
                }
            },
            zwp_tablet_tool_v2::Event::Frame { .. } => {
                let device_id =
                    crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(DeviceId));
                let windows = state.windows.borrow();
                let events = inner.frame(device_id, |window| {
                    windows.get(&window).map(|window| window.lock().unwrap().scale_factor())
                });
                drop(windows);
                for (window, event) in events {
                    state.events_sink.push_window_event(event, window);
                }
            },
            zwp_tablet_tool_v2::Event::Removed => {
                if let Some(cursor) = inner.cursor.take() {
                    cursor.destroy();
                }
                tool.destroy();
            },
            _ => {},
        }
    }
}

impl Dispatch<ZwpTabletPadV2, GlobalData, WinitState> for TabletState {
    fn event(
        _: &mut WinitState,
        _: &ZwpTabletPadV2,
        event: zwp_tablet_pad_v2::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        // A declined pad's description may already be on its way.
        if let zwp_tablet_pad_v2::Event::Group { pad_group } = event {
            pad_group.destroy();
        }
    }

    event_created_child!(WinitState, ZwpTabletPadV2, [
        zwp_tablet_pad_v2::EVT_GROUP_OPCODE => (ZwpTabletPadGroupV2, GlobalData),
    ]);
}

impl Dispatch<ZwpTabletPadGroupV2, GlobalData, WinitState> for TabletState {
    fn event(
        _: &mut WinitState,
        _: &ZwpTabletPadGroupV2,
        event: zwp_tablet_pad_group_v2::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        match event {
            zwp_tablet_pad_group_v2::Event::Ring { ring } => ring.destroy(),
            zwp_tablet_pad_group_v2::Event::Strip { strip } => strip.destroy(),
            zwp_tablet_pad_group_v2::Event::Dial { dial } => dial.destroy(),
            _ => {},
        }
    }

    event_created_child!(WinitState, ZwpTabletPadGroupV2, [
        zwp_tablet_pad_group_v2::EVT_RING_OPCODE => (ZwpTabletPadRingV2, GlobalData),
        zwp_tablet_pad_group_v2::EVT_STRIP_OPCODE => (ZwpTabletPadStripV2, GlobalData),
        zwp_tablet_pad_group_v2::EVT_DIAL_OPCODE => (ZwpTabletPadDialV2, GlobalData),
    ]);
}

/// The controls of a declined pad, destroyed as they arrive.
macro_rules! declined_pad_control {
    ($interface:ty) => {
        impl Dispatch<$interface, GlobalData, WinitState> for TabletState {
            fn event(
                _: &mut WinitState,
                _: &$interface,
                _: <$interface as Proxy>::Event,
                _: &GlobalData,
                _: &Connection,
                _: &QueueHandle<WinitState>,
            ) {
            }
        }
    };
}

declined_pad_control!(ZwpTabletPadRingV2);
declined_pad_control!(ZwpTabletPadStripV2);
declined_pad_control!(ZwpTabletPadDialV2);

delegate_dispatch!(WinitState: [ZwpTabletManagerV2: GlobalData] => TabletState);
delegate_dispatch!(WinitState: [ZwpTabletSeatV2: GlobalData] => TabletState);
delegate_dispatch!(WinitState: [ZwpTabletV2: GlobalData] => TabletState);
delegate_dispatch!(WinitState: [ZwpTabletToolV2: TabletToolData] => TabletState);
delegate_dispatch!(WinitState: [ZwpTabletPadV2: GlobalData] => TabletState);
delegate_dispatch!(WinitState: [ZwpTabletPadGroupV2: GlobalData] => TabletState);
delegate_dispatch!(WinitState: [ZwpTabletPadRingV2: GlobalData] => TabletState);
delegate_dispatch!(WinitState: [ZwpTabletPadStripV2: GlobalData] => TabletState);
delegate_dispatch!(WinitState: [ZwpTabletPadDialV2: GlobalData] => TabletState);

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> crate::event::DeviceId {
        crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(DeviceId))
    }

    fn describe(events: &[(WindowId, WindowEvent)]) -> Vec<String> {
        events
            .iter()
            .map(|(_, event)| match event {
                WindowEvent::PenSample { pressure, tilt, eraser, .. } => {
                    format!(
                        "pen {pressure:.2} {:?} {}",
                        tilt,
                        if *eraser { "eraser" } else { "tip" }
                    )
                },
                WindowEvent::CursorMoved { position, .. } => {
                    format!("move {} {}", position.x, position.y)
                },
                WindowEvent::MouseInput { state, button, .. } => format!("{button:?} {state:?}"),
                WindowEvent::CursorEntered { .. } => "enter".into(),
                WindowEvent::CursorLeft { .. } => "leave".into(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    fn frame(tool: &mut ToolState) -> Vec<String> {
        describe(&tool.frame(device(), |_| Some(2.0)))
    }

    #[test]
    fn a_stroke_is_pointer_input_each_preceded_by_its_sample() {
        let window = WindowId::from(7);
        let mut tool = ToolState::default();

        tool.pending.entered = Some(window);
        tool.position = (10.0, 20.0);
        tool.tilt = (30.0, -15.0);
        assert_eq!(frame(&mut tool), ["enter", "pen 0.00 (30.0, -15.0) tip", "move 20 40"]);

        tool.pending.down = true;
        tool.pressure = pressure_fraction(32768);
        tool.pending.moved = true;
        assert_eq!(
            frame(&mut tool),
            [
                "pen 0.50 (30.0, -15.0) tip",
                "move 20 40",
                "pen 0.50 (30.0, -15.0) tip",
                "Left Pressed",
            ]
        );

        // Pressing harder in place is a motion with the new pressure.
        tool.pressure = 1.0;
        tool.pending.moved = true;
        assert_eq!(frame(&mut tool), ["pen 1.00 (30.0, -15.0) tip", "move 20 40"]);

        tool.pending.up = true;
        tool.pressure = 0.0;
        assert_eq!(frame(&mut tool), ["pen 0.00 (30.0, -15.0) tip", "Left Released"]);

        tool.pending.left = true;
        assert_eq!(frame(&mut tool), ["leave"]);
        assert!(frame(&mut tool).is_empty(), "out of proximity the tool is nowhere");
    }

    #[test]
    fn leaving_proximity_releases_what_the_tool_held() {
        let window = WindowId::from(7);
        let mut tool = ToolState { eraser: true, ..ToolState::default() };
        tool.pending.entered = Some(window);
        tool.pending.down = true;
        tool.pending.buttons.push((tool_button(BTN_STYLUS).unwrap(), true));
        tool.pressure = 0.25;
        assert_eq!(
            frame(&mut tool),
            [
                "enter",
                "pen 0.25 (0.0, 0.0) eraser",
                "move 0 0",
                "pen 0.25 (0.0, 0.0) eraser",
                "Left Pressed",
                "pen 0.25 (0.0, 0.0) eraser",
                "Right Pressed",
            ]
        );
        tool.pending.left = true;
        assert_eq!(
            frame(&mut tool),
            [
                "pen 0.25 (0.0, 0.0) eraser",
                "Left Released",
                "pen 0.25 (0.0, 0.0) eraser",
                "Right Released",
                "leave",
            ]
        );
    }

    #[test]
    fn a_tool_over_no_window_reports_nothing() {
        let mut tool = ToolState::default();
        tool.pending.entered_elsewhere = true;
        tool.pending.moved = true;
        tool.pending.down = true;
        assert!(frame(&mut tool).is_empty());

        // A window that went away mid-stroke takes the stroke with it.
        tool.pending.entered = Some(WindowId::from(9));
        assert!(describe(&tool.frame(device(), |_| None)).is_empty());
        assert!(tool.window.is_none());
    }

    #[test]
    fn barrel_buttons_are_the_secondary_and_middle_buttons() {
        assert_eq!(tool_button(BTN_STYLUS), Some(MouseButton::Right));
        assert_eq!(tool_button(BTN_STYLUS2), Some(MouseButton::Middle));
        assert_eq!(tool_button(BTN_LEFT), Some(MouseButton::Left));
        assert_eq!(tool_button(0x149), None, "a third barrel button clicks nothing");
        assert_eq!(pressure_fraction(65535), 1.0);
        assert_eq!(pressure_fraction(0), 0.0);
    }
}
