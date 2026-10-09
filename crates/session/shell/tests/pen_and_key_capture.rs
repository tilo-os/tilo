//! A drawing app gets the pen and the pad's rings and strips itself over
//! tablet-v2 while an app without a tablet seat sees the pen as a pointer
//! and the rings as scrolling; the pad map shows the drawing app's labels
//! for the controls, or Tilo's mapping where it gave none; a VM that inhibits shortcuts gets the
//! shell's chords until focus leaves or the release chord hands them back.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness failures are assertions"
)]

use std::ops::{Deref, DerefMut};

use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_pointer, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, event_created_child};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1, zwp_keyboard_shortcuts_inhibitor_v1,
};
use wayland_protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2, zwp_tablet_pad_group_v2, zwp_tablet_pad_ring_v2,
    zwp_tablet_pad_strip_v2, zwp_tablet_pad_v2, zwp_tablet_seat_v2, zwp_tablet_tool_v2,
    zwp_tablet_v2,
};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use settings_store::hardware::{PadDial, TabletConfig};
use shell::compositor::backend::{
    BackendEvent, PadAction, PadDescription, PadEvent, PadGroup, TabletDevice, TabletInput,
    TabletPhase, TabletTool, TabletToolKind,
};
use shell::compositor::frame::FrameBuilder;
use shell::compositor::tablet_pad::{DialSteps, dial_events};

use crate::support::wayland;

#[derive(Debug, PartialEq)]
enum Wire {
    ToolAdded,
    ProximityIn,
    ProximityOut,
    Pressure(u32),
    Tilt(f64, f64),
    Down,
    Up,
    Button(u32, bool),
    Active,
    Inactive,
    Key(u32),
    PadAdded,
    PadRing,
    PadStrip,
    PadDone,
    PadEnter,
    PadLeave,
    PadButton(u32, bool),
    ModeSwitch(u32),
    RingAngle(f64),
    RingStop,
    RingFrame,
    StripPosition(u32),
    StripFrame,
    Axis,
}

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    wire: Vec<Wire>,
    pads: Vec<zwp_tablet_pad_v2::ZwpTabletPadV2>,
    rings: Vec<zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2>,
    /// The serial of the pad's last `enter` or `mode_switch`, which
    /// feedback names.
    pad_serial: u32,
}

crate::support::wayland::test_client!(Client);

impl Dispatch<zwp_tablet_seat_v2::ZwpTabletSeatV2, ()> for Client {
    fn event(
        state: &mut Self,
        _: &zwp_tablet_seat_v2::ZwpTabletSeatV2,
        event: zwp_tablet_seat_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_seat_v2::Event::ToolAdded { .. } => state.wire.push(Wire::ToolAdded),
            zwp_tablet_seat_v2::Event::PadAdded { id } => {
                state.pads.push(id);
                state.wire.push(Wire::PadAdded);
            }
            _ => {}
        }
    }

    event_created_child!(Client, zwp_tablet_seat_v2::ZwpTabletSeatV2, [
        zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (zwp_tablet_v2::ZwpTabletV2, ()),
        zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (zwp_tablet_tool_v2::ZwpTabletToolV2, ()),
        zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => (zwp_tablet_pad_v2::ZwpTabletPadV2, ()),
    ]);
}

impl Dispatch<zwp_tablet_pad_v2::ZwpTabletPadV2, ()> for Client {
    fn event(
        state: &mut Self,
        _: &zwp_tablet_pad_v2::ZwpTabletPadV2,
        event: zwp_tablet_pad_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_pad_v2::Event::Done => state.wire.push(Wire::PadDone),
            zwp_tablet_pad_v2::Event::Enter { serial, .. } => {
                state.pad_serial = serial;
                state.wire.push(Wire::PadEnter);
            }
            zwp_tablet_pad_v2::Event::Leave { .. } => state.wire.push(Wire::PadLeave),
            zwp_tablet_pad_v2::Event::Button {
                button,
                state: pressed,
                ..
            } => state.wire.push(Wire::PadButton(
                button,
                pressed == wayland_client::WEnum::Value(zwp_tablet_pad_v2::ButtonState::Pressed),
            )),
            _ => {}
        }
    }

    event_created_child!(Client, zwp_tablet_pad_v2::ZwpTabletPadV2, [
        zwp_tablet_pad_v2::EVT_GROUP_OPCODE => (zwp_tablet_pad_group_v2::ZwpTabletPadGroupV2, ()),
    ]);
}

impl Dispatch<zwp_tablet_pad_group_v2::ZwpTabletPadGroupV2, ()> for Client {
    fn event(
        state: &mut Self,
        _: &zwp_tablet_pad_group_v2::ZwpTabletPadGroupV2,
        event: zwp_tablet_pad_group_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_pad_group_v2::Event::Ring { ring } => {
                state.rings.push(ring);
                state.wire.push(Wire::PadRing);
            }
            zwp_tablet_pad_group_v2::Event::Strip { .. } => state.wire.push(Wire::PadStrip),
            zwp_tablet_pad_group_v2::Event::ModeSwitch { mode, serial, .. } => {
                state.pad_serial = serial;
                state.wire.push(Wire::ModeSwitch(mode));
            }
            _ => {}
        }
    }

    event_created_child!(Client, zwp_tablet_pad_group_v2::ZwpTabletPadGroupV2, [
        zwp_tablet_pad_group_v2::EVT_RING_OPCODE => (zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2, ()),
        zwp_tablet_pad_group_v2::EVT_STRIP_OPCODE => (zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2, ()),
    ]);
}

impl Dispatch<zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2, ()> for Client {
    fn event(
        state: &mut Self,
        _: &zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2,
        event: zwp_tablet_pad_ring_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_pad_ring_v2::Event::Angle { degrees } => {
                state.wire.push(Wire::RingAngle(degrees));
            }
            zwp_tablet_pad_ring_v2::Event::Stop => state.wire.push(Wire::RingStop),
            zwp_tablet_pad_ring_v2::Event::Frame { .. } => state.wire.push(Wire::RingFrame),
            _ => {}
        }
    }
}

impl Dispatch<zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2, ()> for Client {
    fn event(
        state: &mut Self,
        _: &zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2,
        event: zwp_tablet_pad_strip_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_pad_strip_v2::Event::Position { position } => {
                state.wire.push(Wire::StripPosition(position));
            }
            zwp_tablet_pad_strip_v2::Event::Frame { .. } => state.wire.push(Wire::StripFrame),
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Axis { .. } = event {
            state.wire.push(Wire::Axis);
        }
    }
}

impl Dispatch<zwp_tablet_tool_v2::ZwpTabletToolV2, ()> for Client {
    fn event(
        state: &mut Self,
        _: &zwp_tablet_tool_v2::ZwpTabletToolV2,
        event: zwp_tablet_tool_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_tool_v2::Event::ProximityIn { .. } => state.wire.push(Wire::ProximityIn),
            zwp_tablet_tool_v2::Event::ProximityOut => state.wire.push(Wire::ProximityOut),
            zwp_tablet_tool_v2::Event::Pressure { pressure } => {
                state.wire.push(Wire::Pressure(pressure));
            }
            zwp_tablet_tool_v2::Event::Tilt { tilt_x, tilt_y } => {
                state.wire.push(Wire::Tilt(tilt_x, tilt_y));
            }
            zwp_tablet_tool_v2::Event::Down { .. } => state.wire.push(Wire::Down),
            zwp_tablet_tool_v2::Event::Up => state.wire.push(Wire::Up),
            zwp_tablet_tool_v2::Event::Button {
                button,
                state: pressed,
                ..
            } => {
                state.wire.push(Wire::Button(
                    button,
                    pressed
                        == wayland_client::WEnum::Value(zwp_tablet_tool_v2::ButtonState::Pressed),
                ));
            }
            _ => {}
        }
    }
}

impl Dispatch<zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
        event: zwp_keyboard_shortcuts_inhibitor_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Active => state.wire.push(Wire::Active),
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Inactive => {
                state.wire.push(Wire::Inactive);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key {
            key,
            state: wayland_client::WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
        {
            state.wire.push(Wire::Key(key));
        }
    }
}

wayland_client::delegate_noop!(Client: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(Client: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(Client: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(Client: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(Client: ignore wl_surface::WlSurface);
wayland_client::delegate_noop!(Client: ignore wl_seat::WlSeat);
wayland_client::delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
wayland_client::delegate_noop!(Client: ignore zwp_tablet_manager_v2::ZwpTabletManagerV2);
wayland_client::delegate_noop!(Client: ignore zwp_tablet_v2::ZwpTabletV2);
wayland_client::delegate_noop!(Client: ignore zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1);

struct Harness {
    wl: wayland::Harness<Client>,
    fb: FrameBuilder,
}

impl Deref for Harness {
    type Target = wayland::Harness<Client>;
    fn deref(&self) -> &Self::Target {
        &self.wl
    }
}

impl DerefMut for Harness {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.wl
    }
}

impl Harness {
    fn new() -> Self {
        let wl = wayland::Harness::new();
        let fb = FrameBuilder::with_handle(wl.state.workspace_tree.clone(), false, (1280, 800));
        Self { wl, fb }
    }

    /// Open a toplevel and return its surface and compositor id, laid out.
    fn open(&mut self, app_id: &str) -> (wl_surface::WlSurface, u64) {
        let compositor = self.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
        let wm = self.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
        let qh = self.queue.handle();
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id(app_id.into());
        self.pump();
        let _ = self.fb.paint_settled();
        // Layout sends the opening xdg configure. Acknowledge it before
        // the caller can attach its first buffer.
        self.pump();
        let id = self.foreign_id(app_id);
        (surface, id)
    }

    /// Give `surface` a first buffer, so it is mapped and can take focus.
    fn map(&mut self, surface: &wl_surface::WlSurface, surface_id: u64) {
        self.map_sized(surface, surface_id, (64, 64));
    }

    fn map_sized(
        &mut self,
        surface: &wl_surface::WlSurface,
        surface_id: u64,
        (width, height): (i32, i32),
    ) {
        use std::os::fd::{AsFd as _, FromRawFd as _, OwnedFd};
        let stride = width.checked_mul(4).expect("ARGB stride");
        let len = usize::try_from(stride.checked_mul(height).expect("ARGB extent"))
            .expect("positive ARGB extent");
        let shm = self.bind::<wl_shm::WlShm>("wl_shm", 1);
        // SAFETY: a null-terminated name and a valid flag word are the whole
        // contract of `memfd_create`.
        let raw = unsafe { libc::memfd_create(c"tilo-pen-capture".as_ptr(), 0) };
        assert!(
            raw >= 0,
            "memfd_create: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: `memfd_create` just handed us this descriptor.
        let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
        file.set_len(len as u64).expect("size the shm pool");
        let qh = self.queue.handle();
        let pool = shm.create_pool(
            file.as_fd(),
            i32::try_from(len).expect("pool size"),
            &qh,
            (),
        );
        let buffer =
            pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, width, height);
        surface.commit();
        self.pump();
        assert!(
            shell::compositor::foreign::store().is_mapped(surface_id),
            "the applied non-null commit maps for input before pixel presentation"
        );
        let _ = self.fb.paint_settled();
        assert!(
            shell::compositor::foreign::store().is_mapped(surface_id),
            "the real first-buffer commit maps the configured toplevel"
        );
    }

    /// The centre of the app's tile, where a pen lands on it.
    fn centre_of(&self, surface_id: u64) -> (f64, f64) {
        let chrome = self.fb.chrome();
        let tree = chrome.tree();
        let hit = shell::shell::input_policy::foreign_tiles(&tree, self.fb.host())
            .into_iter()
            .find(|hit| hit.surface_id == surface_id)
            .expect("the app is on screen");
        drop(tree);
        drop(chrome);
        (hit.rect.center().x, hit.rect.center().y)
    }
}

fn pen(phase: TabletPhase, position: (f64, f64)) -> TabletInput {
    TabletInput {
        device: TabletDevice {
            name: "Test tablet".into(),
            usb_id: Some((0x056a, 0x0374)),
            syspath: None,
        },
        tool: TabletTool {
            kind: TabletToolKind::Pen,
            serial: 42,
            wacom_id: 0x0802,
            pressure: true,
            distance: false,
            tilt: true,
            rotation: false,
            slider: false,
            wheel: false,
        },
        phase,
        position,
        pressure: 0.5,
        distance: 0.0,
        tilt: (10.0, -20.0),
        rotation: 0.0,
        slider: 0.0,
        wheel: (0.0, 0),
        time_msec: 1,
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one pen over one app, through each way it can be routed"
)]
fn a_tablet_aware_app_gets_pressure_and_tilt_and_others_get_a_pointer() {
    let mut h = Harness::new();
    let (_surface, app) = h.open("org.example.Draw");
    let centre = h.centre_of(app);

    // Before the app asks for a tablet seat the pen is a pointer to it.
    let fallback = shell::compositor::client_input::route_tablet_event(
        &mut h.wl.state,
        &mut h.fb,
        &pen(TabletPhase::TipDown, centre),
        false,
    );
    assert!(
        matches!(
            fallback.as_slice(),
            [
                BackendEvent::Input(WindowEvent::CursorMoved { .. }),
                BackendEvent::Input(WindowEvent::MouseInput { .. })
            ]
        ),
        "an app without tablet support sees the tip as a click"
    );

    let manager = h.bind::<zwp_tablet_manager_v2::ZwpTabletManagerV2>("zwp_tablet_manager_v2", 1);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _tablet_seat = manager.get_tablet_seat(&seat, &h.queue.handle(), ());
    h.pump();
    assert!(h.state.surface_takes_tablet_input(app));
    assert!(
        h.client.wire.contains(&Wire::ToolAdded),
        "a new tablet seat learns about the pen already in use"
    );
    h.client.wire.clear();

    for phase in [
        TabletPhase::ProximityIn,
        TabletPhase::TipDown,
        TabletPhase::TipUp,
    ] {
        let routed = shell::compositor::client_input::route_tablet_event(
            &mut h.wl.state,
            &mut h.fb,
            &pen(phase, centre),
            false,
        );
        assert!(
            routed.is_empty(),
            "the tablet-aware app takes {phase:?} itself"
        );
        h.pump();
    }
    assert!(h.client.wire.contains(&Wire::ProximityIn));
    assert!(
        h.client.wire.contains(&Wire::Pressure(32768)),
        "{:?}",
        h.client.wire
    );
    assert!(h.client.wire.contains(&Wire::Tilt(10.0, -20.0)));
    let down = h.client.wire.iter().position(|w| *w == Wire::Down);
    let up = h.client.wire.iter().position(|w| *w == Wire::Up);
    assert!(down.is_some() && down < up, "{:?}", h.client.wire);
    h.client.wire.clear();

    // A barrel button, as the backend reports it after the tablet's
    // settings remapped it, reaches the app as a tool button.
    let routed = shell::compositor::client_input::route_tablet_event(
        &mut h.wl.state,
        &mut h.fb,
        &pen(
            TabletPhase::Button {
                button: 0x14b,
                pressed: true,
            },
            centre,
        ),
        false,
    );
    assert!(routed.is_empty());
    h.pump();
    assert!(
        h.client.wire.contains(&Wire::Button(0x14b, true)),
        "{:?}",
        h.client.wire
    );
    h.client.wire.clear();

    // On the lock screen the pen is the pointer, even over a tablet-aware
    // app, and the app sees none of it.
    let locked = shell::compositor::client_input::route_tablet_event(
        &mut h.wl.state,
        &mut h.fb,
        &pen(TabletPhase::TipDown, centre),
        true,
    );
    assert!(
        matches!(
            locked.as_slice(),
            [
                BackendEvent::Input(WindowEvent::CursorMoved { .. }),
                BackendEvent::Input(WindowEvent::MouseInput { .. })
            ]
        ),
        "the lock screen gets the tip as a click"
    );
    h.pump();
    let wire = std::mem::take(&mut h.client.wire);
    drop(h);
    assert!(
        !wire
            .iter()
            .any(|w| matches!(w, Wire::Down | Wire::Button(..))),
        "{wire:?}"
    );
}

#[test]
fn a_pen_over_a_tablet_aware_app_still_moves_the_cursor_focuses_and_holds_its_stroke() {
    let mut h = Harness::new();
    let (surface, app) = h.open("org.example.Draw");
    h.map(&surface, app);
    let centre = h.centre_of(app);
    let manager = h.bind::<zwp_tablet_manager_v2::ZwpTabletManagerV2>("zwp_tablet_manager_v2", 1);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let _tablet_seat = manager.get_tablet_seat(&seat, &h.queue.handle(), ());
    h.pump();
    h.state.set_client_keyboard_focus(None);
    h.client.wire.clear();
    let route = |h: &mut Harness, phase, at| {
        let routed = shell::compositor::client_input::route_tablet_event(
            &mut h.wl.state,
            &mut h.fb,
            &pen(phase, at),
            false,
        );
        h.pump();
        routed
    };

    assert!(route(&mut h, TabletPhase::ProximityIn, centre).is_empty());
    assert_eq!(
        h.state.client_input.pointer_position(),
        Some(centre),
        "the cursor is drawn where the pen hovers"
    );
    assert_eq!(
        h.state.client_input.keyboard_focus(),
        None,
        "hovering focuses nothing"
    );

    assert!(route(&mut h, TabletPhase::TipDown, centre).is_empty());
    assert_eq!(
        h.state.client_input.keyboard_focus(),
        Some(app),
        "touching down focuses the window, as a click does; mapped={}, active={:?}, cursor={:?}",
        shell::compositor::foreign::store().is_mapped(app),
        shell::shell::input_policy::active_foreign_surface(&h.state.workspace_tree.read()),
        h.state.client_input.cursor(),
    );

    // Off the window's edge mid-stroke, the stroke stays with the app.
    let outside = (-100.0, -100.0);
    assert!(route(&mut h, TabletPhase::Motion, outside).is_empty());
    assert!(route(&mut h, TabletPhase::TipUp, outside).is_empty());
    assert!(h.client.wire.contains(&Wire::Up), "{:?}", h.client.wire);
    assert!(
        !h.client.wire.contains(&Wire::ProximityOut),
        "{:?}",
        h.client.wire
    );

    // Lifted, the pen over nothing leaves the app.
    let _ = route(&mut h, TabletPhase::Motion, outside);
    let wire = std::mem::take(&mut h.client.wire);
    drop(h);
    assert!(wire.ends_with(&[Wire::ProximityOut]), "{wire:?}");
}

#[test]
fn an_app_that_declines_the_pad_keeps_the_pads_own_mapping() {
    let mut h = Harness::new();
    let (_surface, app) = h.open("org.example.Pens");
    let centre = h.centre_of(app);
    let qh = h.queue.handle();
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _pointer = seat.get_pointer(&qh, ());
    let manager = h.bind::<zwp_tablet_manager_v2::ZwpTabletManagerV2>("zwp_tablet_manager_v2", 1);
    let _tablet_seat = manager.get_tablet_seat(&seat, &qh, ());
    h.pump();
    h.state.set_client_keyboard_focus(Some(app));
    h.state.add_tablet_pad(test_pad());
    h.pump();
    assert!(h.client.wire.contains(&Wire::PadAdded));
    // The app takes pens but not pads.
    for pad in h.client.pads.drain(..) {
        pad.destroy();
    }
    h.pump();
    h.client.wire.clear();

    let no_mods = winit::event::Modifiers::default();
    let _ = shell::compositor::client_input::route_input(
        &mut h.wl.state,
        &mut h.fb,
        &WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(centre.0, centre.1),
        },
        &no_mods,
    );
    let config = TabletConfig::default();
    let mut steps = DialSteps::default();
    let mut turned = Vec::new();
    for angle in [10.0, 40.0] {
        turned = shell::compositor::client_input::route_pad_event(
            &mut h.wl.state,
            &h.fb,
            pad_event(
                &mut steps,
                &config,
                ring(Some(angle)),
                Some((PadDial::Ring(0), Some(angle))),
            ),
            false,
        );
        h.pump();
    }
    let wire = std::mem::take(&mut h.client.wire);
    drop(h);
    assert!(
        matches!(
            turned.as_slice(),
            [BackendEvent::Input(WindowEvent::MouseWheel { .. })]
        ),
        "the ring scrolls, as in an app without tablet support: {turned:?}"
    );
    assert!(
        !wire.contains(&Wire::PadAdded),
        "a declined pad is not announced again: {wire:?}"
    );
}

fn test_pad() -> PadDescription {
    PadDescription {
        key: "056a:0374:Test tablet Pad".into(),
        device: TabletDevice {
            name: "Test tablet Pad".into(),
            usb_id: Some((0x056a, 0x0374)),
            syspath: None,
        },
        buttons: 4,
        groups: vec![PadGroup {
            buttons: vec![0, 1, 2, 3],
            mode_buttons: vec![3],
            rings: vec![0],
            strips: vec![0],
            modes: 2,
        }],
        layout: None,
    }
}

/// A pad event as the backend reports it, with the fallback it builds from
/// the pad's saved settings for `dial` in mode 0.
fn pad_event(
    steps: &mut DialSteps,
    config: &TabletConfig,
    action: PadAction,
    dial: Option<(PadDial, Option<f64>)>,
) -> PadEvent {
    let fallback = dial.map_or_else(Vec::new, |(dial, position)| {
        dial_events(
            config.dial_action(dial, 0),
            steps.advance("056a:0374:Test tablet Pad", dial, position),
            |_| panic!("a scrolling ring presses no key"),
        )
    });
    PadEvent {
        pad: test_pad().key,
        action,
        time_msec: 9,
        fallback,
    }
}

fn ring(angle: Option<f64>) -> PadAction {
    PadAction::Ring {
        ring: 0,
        angle,
        finger: true,
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one pad over one app, before and after it takes tablet input"
)]
fn a_tablet_aware_app_gets_pad_rings_and_strips_and_others_scroll() {
    let mut h = Harness::new();
    let (_surface, app) = h.open("org.example.Draw");
    let centre = h.centre_of(app);
    let qh = h.queue.handle();
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _pointer = seat.get_pointer(&qh, ());
    h.pump();
    h.state.set_client_keyboard_focus(Some(app));
    h.state.add_tablet_pad(test_pad());
    h.pump();
    let config = TabletConfig::default();
    let mut steps = DialSteps::default();

    // Before the app asks for a tablet seat, turning the ring scrolls what
    // is under the pointer, as a new pad's rings do by default.
    let no_mods = winit::event::Modifiers::default();
    let _ = shell::compositor::client_input::route_input(
        &mut h.wl.state,
        &mut h.fb,
        &WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(centre.0, centre.1),
        },
        &no_mods,
    );
    let landed = shell::compositor::client_input::route_pad_event(
        &mut h.wl.state,
        &h.fb,
        pad_event(
            &mut steps,
            &config,
            ring(Some(10.0)),
            Some((PadDial::Ring(0), Some(10.0))),
        ),
        false,
    );
    assert!(landed.is_empty(), "a touch landing moves nothing yet");
    let turned = shell::compositor::client_input::route_pad_event(
        &mut h.wl.state,
        &h.fb,
        pad_event(
            &mut steps,
            &config,
            ring(Some(40.0)),
            Some((PadDial::Ring(0), Some(40.0))),
        ),
        false,
    );
    assert!(
        matches!(
            turned.as_slice(),
            [BackendEvent::Input(WindowEvent::MouseWheel { .. })]
        ),
        "an app without tablet support gets the ring as a wheel: {turned:?}"
    );
    for event in turned {
        if let BackendEvent::Input(event) = event {
            let _ = shell::compositor::client_input::route_input(
                &mut h.wl.state,
                &mut h.fb,
                &event,
                &no_mods,
            );
        }
    }
    h.pump();
    assert!(
        h.client.wire.contains(&Wire::Axis),
        "the fallback scroll reaches the app under the pointer: {:?}",
        h.client.wire
    );
    assert!(!h.client.wire.contains(&Wire::RingAngle(40.0)));
    h.client.wire.clear();

    // Taking a tablet seat announces the pad already attached.
    let manager = h.bind::<zwp_tablet_manager_v2::ZwpTabletManagerV2>("zwp_tablet_manager_v2", 1);
    let _tablet_seat = manager.get_tablet_seat(&seat, &qh, ());
    h.pump();
    for announced in [Wire::PadAdded, Wire::PadRing, Wire::PadStrip, Wire::PadDone] {
        assert!(
            h.client.wire.contains(&announced),
            "{announced:?} in {:?}",
            h.client.wire
        );
    }
    h.client.wire.clear();

    let route = |h: &mut Harness, event: PadEvent| {
        let routed =
            shell::compositor::client_input::route_pad_event(&mut h.wl.state, &h.fb, event, false);
        h.pump();
        routed
    };
    let routed = route(
        &mut h,
        pad_event(
            &mut steps,
            &config,
            ring(Some(80.0)),
            Some((PadDial::Ring(0), Some(80.0))),
        ),
    );
    assert!(
        routed.is_empty(),
        "the tablet-aware app takes the ring itself"
    );
    assert_eq!(
        h.client.wire,
        [
            Wire::PadEnter,
            Wire::ModeSwitch(0),
            Wire::RingAngle(80.0),
            Wire::RingFrame
        ],
        "the pad enters the focused app, tells it the mode, then turns"
    );
    h.client.wire.clear();

    let routed = route(
        &mut h,
        pad_event(
            &mut steps,
            &config,
            PadAction::Strip {
                strip: 0,
                position: Some(0.25),
                finger: true,
            },
            Some((PadDial::Strip(0), Some(0.25))),
        ),
    );
    assert!(routed.is_empty());
    let _ = route(
        &mut h,
        pad_event(
            &mut steps,
            &config,
            ring(None),
            Some((PadDial::Ring(0), None)),
        ),
    );
    let _ = route(
        &mut h,
        pad_event(
            &mut steps,
            &config,
            PadAction::ModeSwitch {
                group: 0,
                mode: 1,
                modes: 2,
            },
            None,
        ),
    );
    let _ = route(
        &mut h,
        pad_event(
            &mut steps,
            &config,
            PadAction::Button {
                button: 2,
                pressed: true,
            },
            None,
        ),
    );
    assert_eq!(
        h.client.wire,
        [
            Wire::StripPosition(16384),
            Wire::StripFrame,
            Wire::RingStop,
            Wire::RingFrame,
            Wire::ModeSwitch(1),
            Wire::PadButton(2, true)
        ]
    );
    h.client.wire.clear();

    // On the lock screen the ring's mapping applies again and the app
    // loses the pad. The next touch lands at the top and turns back.
    let landed = pad_event(
        &mut steps,
        &config,
        ring(Some(0.0)),
        Some((PadDial::Ring(0), Some(0.0))),
    );
    assert!(landed.fallback.is_empty());
    let locked = shell::compositor::client_input::route_pad_event(
        &mut h.wl.state,
        &h.fb,
        pad_event(
            &mut steps,
            &config,
            ring(Some(330.0)),
            Some((PadDial::Ring(0), Some(330.0))),
        ),
        true,
    );
    h.pump();
    let wire = std::mem::take(&mut h.client.wire);
    drop(h);
    assert!(
        matches!(
            locked.as_slice(),
            [BackendEvent::Input(WindowEvent::MouseWheel { .. })]
        ),
        "{locked:?}"
    );
    assert_eq!(wire, [Wire::PadLeave], "{wire:?}");
}

/// One app with a tablet seat and keyboard focus, the test pad entered into
/// it by a first touch of the ring.
fn pad_in_tablet_app(h: &mut Harness) -> u64 {
    let (_surface, app) = h.open("org.example.Draw");
    let qh = h.queue.handle();
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let manager = h.bind::<zwp_tablet_manager_v2::ZwpTabletManagerV2>("zwp_tablet_manager_v2", 1);
    let _tablet_seat = manager.get_tablet_seat(&seat, &qh, ());
    h.pump();
    h.state.set_client_keyboard_focus(Some(app));
    h.state.add_tablet_pad(test_pad());
    h.pump();
    let routed = shell::compositor::client_input::route_pad_event(
        &mut h.wl.state,
        &h.fb,
        pad_event(
            &mut DialSteps::default(),
            &TabletConfig::default(),
            ring(Some(10.0)),
            None,
        ),
        false,
    );
    h.pump();
    assert!(routed.is_empty(), "the tablet-aware app takes the ring");
    assert!(
        h.client.wire.contains(&Wire::PadEnter),
        "{:?}",
        h.client.wire
    );
    h.client.wire.clear();
    app
}

#[test]
fn the_pad_map_shows_the_focused_apps_labels() {
    use shell::shell::pad_map::{PadControl, PadMapAction};
    let mut h = Harness::new();
    let _app = pad_in_tablet_app(&mut h);
    let serial = h.client.pad_serial;
    let pad = h
        .client
        .pads
        .first()
        .cloned()
        .expect("the pad was announced");
    let ring_object = h.client.rings.first().cloned().expect("its ring too");
    pad.set_feedback(0, "Undo".into(), serial);
    pad.set_feedback(1, "Stale".into(), serial.wrapping_add(100));
    pad.set_feedback(2, "Never shown".into(), serial);
    ring_object.set_feedback("Brush size".into(), serial);
    h.pump();

    let mut config = TabletConfig::default();
    config
        .pad_buttons
        .insert(2, settings_store::hardware::ShortcutAction::Search);
    let key = test_pad().key;
    let map = h
        .state
        .tablet_pad_map(&key, &config)
        .expect("the pad is attached");
    assert_eq!(
        map.action(PadControl::Button(0)),
        Some(&PadMapAction::App("Undo".into()))
    );
    assert_eq!(
        map.action(PadControl::Ring(0)),
        Some(&PadMapAction::App("Brush size".into()))
    );
    assert_eq!(
        map.action(PadControl::Button(1)),
        Some(&PadMapAction::AppDecides),
        "a label naming an old serial is ignored"
    );
    assert_eq!(
        map.action(PadControl::Button(2)),
        Some(&PadMapAction::Shortcut(
            settings_store::hardware::ShortcutAction::Search
        )),
        "a mapped shortcut never reaches the app, whatever it says"
    );
    assert_eq!(
        map.action(PadControl::Strip(0)),
        Some(&PadMapAction::AppDecides)
    );
    assert!(
        map.narration().contains("Button 1: Undo"),
        "{}",
        map.narration()
    );

    // A mode switch asks the app for new labels: the old ones go.
    let _ = shell::compositor::client_input::route_pad_event(
        &mut h.wl.state,
        &h.fb,
        pad_event(
            &mut DialSteps::default(),
            &config,
            PadAction::ModeSwitch {
                group: 0,
                mode: 1,
                modes: 2,
            },
            None,
        ),
        false,
    );
    h.pump();
    let map = h.state.tablet_pad_map(&key, &config).expect("attached");
    assert_eq!(
        map.action(PadControl::Button(0)),
        Some(&PadMapAction::AppDecides)
    );
    pad.set_feedback(0, "Redo".into(), h.client.pad_serial);
    h.pump();
    let map = h.state.tablet_pad_map(&key, &config).expect("attached");
    drop(h);
    assert_eq!(
        map.action(PadControl::Button(0)),
        Some(&PadMapAction::App("Redo".into()))
    );
}

#[test]
fn the_pad_map_falls_back_to_tilos_mapping() {
    use settings_store::hardware::{PadDialAction, ShortcutAction};
    use shell::shell::pad_map::{PadControl, PadMapAction};
    let mut h = Harness::new();
    let (_surface, app) = h.open("org.example.Plain");
    h.state.set_client_keyboard_focus(Some(app));
    h.state.add_tablet_pad(test_pad());
    let mut config = TabletConfig {
        pad_map_button: Some(1),
        ..TabletConfig::default()
    };
    config
        .pad_buttons
        .insert(0, ShortcutAction::WorkspaceOverview);
    config.set_dial_action(PadDial::Ring(0), 0, PadDialAction::Zoom);
    config.set_dial_action(PadDial::Ring(0), 1, PadDialAction::Volume);
    let key = test_pad().key;

    let map = h.state.tablet_pad_map(&key, &config).expect("attached");
    assert_eq!(
        map.action(PadControl::Button(0)),
        Some(&PadMapAction::Shortcut(ShortcutAction::WorkspaceOverview))
    );
    assert_eq!(
        map.action(PadControl::Button(1)),
        Some(&PadMapAction::ShowsMap)
    );
    assert_eq!(
        map.action(PadControl::Button(2)),
        Some(&PadMapAction::Nothing),
        "an app without tablet support gets nothing from an unmapped button"
    );
    assert_eq!(
        map.action(PadControl::Button(3)),
        Some(&PadMapAction::ModeSwitch { mode: 0, modes: 2 })
    );
    assert_eq!(
        map.action(PadControl::Ring(0)),
        Some(&PadMapAction::Dial(PadDialAction::Zoom))
    );
    assert_eq!(
        map.action(PadControl::Strip(0)),
        Some(&PadMapAction::Dial(PadDialAction::Scroll))
    );
    assert_eq!(map.name, "Test tablet Pad");
    assert!(map.layout.is_none(), "an unknown model is listed");
    assert!(
        map.narration().contains("Ring 1: Zoom"),
        "{}",
        map.narration()
    );

    // The ring's action follows its group's mode.
    let fallback = shell::compositor::client_input::route_pad_event(
        &mut h.wl.state,
        &h.fb,
        pad_event(
            &mut DialSteps::default(),
            &config,
            PadAction::ModeSwitch {
                group: 0,
                mode: 1,
                modes: 2,
            },
            None,
        ),
        false,
    );
    assert!(fallback.is_empty());
    let map = h.state.tablet_pad_map(&key, &config).expect("attached");
    assert_eq!(
        map.action(PadControl::Ring(0)),
        Some(&PadMapAction::Dial(PadDialAction::Volume))
    );
    assert_eq!(
        map.action(PadControl::Button(3)),
        Some(&PadMapAction::ModeSwitch { mode: 1, modes: 2 })
    );
    h.state.remove_tablet_pad(&key);
    let gone = h.state.tablet_pad_map(&key, &config).is_none();
    drop(h);
    assert!(gone, "a pad that went away has no map");
}

#[test]
fn a_pad_leaves_the_app_as_soon_as_keyboard_focus_does() {
    let mut h = Harness::new();
    let app = pad_in_tablet_app(&mut h);
    let (_other_surface, other) = h.open("org.example.Other");

    h.state.set_client_keyboard_focus(Some(other));
    h.pump();
    assert_eq!(
        h.client.wire,
        [Wire::PadLeave],
        "the app hears leave once focus moves, before any pad event"
    );
    h.client.wire.clear();

    h.state.set_client_keyboard_focus(Some(app));
    h.pump();
    assert!(
        !h.client.wire.contains(&Wire::PadEnter),
        "the pad enters again at its next touch: {:?}",
        h.client.wire
    );
    h.state.set_client_keyboard_focus(None);
    h.pump();
    let wire = std::mem::take(&mut h.client.wire);
    drop(h);
    assert!(
        !wire.contains(&Wire::PadLeave),
        "no second leave for a pad that is not in: {wire:?}"
    );
}

fn key(logical: Key, pressed: bool) -> WindowEvent {
    let event = ui::synthetic_input::key_event(
        logical,
        None,
        if pressed {
            ElementState::Pressed
        } else {
            ElementState::Released
        },
    );
    WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event,
        is_synthetic: false,
    }
}

#[test]
fn an_inhibiting_vm_gets_super_chords_until_focus_or_the_release_chord_takes_them_back() {
    let mut h = Harness::new();
    let (surface, vm) = h.open("org.example.Vm");
    let manager = h
        .bind::<zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1>(
            "zwp_keyboard_shortcuts_inhibit_manager_v1",
            1,
        );
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let _inhibitor = manager.inhibit_shortcuts(&surface, &seat, &h.queue.handle(), ());
    h.pump();
    assert_eq!(
        h.state.sync_shortcut_inhibit(true),
        None,
        "nothing for an unfocused app"
    );

    h.state.set_client_keyboard_focus(Some(vm));
    h.pump();
    assert_eq!(h.state.sync_shortcut_inhibit(true), Some(true));
    h.pump();
    assert!(h.client.wire.contains(&Wire::Active));
    h.client.wire.clear();

    // Super+Tab is the overview; while inhibited it reaches the VM.
    let super_mods = Modifiers::from(ModifiersState::SUPER);
    let routed = shell::compositor::client_input::route_input(
        &mut h.wl.state,
        &mut h.fb,
        &key(Key::Named(NamedKey::Tab), true),
        &super_mods,
    );
    let _ = shell::compositor::client_input::route_input(
        &mut h.wl.state,
        &mut h.fb,
        &key(Key::Named(NamedKey::Tab), false),
        &super_mods,
    );
    h.pump();
    assert!(routed, "the inhibiting VM takes the overview chord");
    assert!(
        h.client.wire.contains(&Wire::Key(15)),
        "{:?}",
        h.client.wire
    );
    h.client.wire.clear();

    // Chrome taking input withdraws it; so does the release chord.
    assert_eq!(h.state.sync_shortcut_inhibit(false), Some(false));
    assert_eq!(h.state.sync_shortcut_inhibit(true), Some(true));
    assert!(h.state.suspend_shortcut_inhibit());
    assert_eq!(
        h.state.sync_shortcut_inhibit(true),
        None,
        "handed-back shortcuts stay with the shell while focus stays put"
    );
    h.pump();
    let released = h.client.wire.contains(&Wire::Inactive);
    let still_active = h.state.shortcut_inhibit.is_active();
    drop(h);
    assert!(released);
    assert!(!still_active);
}

#[test]
fn a_non_null_shm_commit_maps_for_input_before_async_conversion_finishes() {
    let mut harness = Harness::new();
    let (surface, id) = harness.open("org.tilo.test.pending-shm-input");
    // A valid larger buffer keeps conversion in flight across the wire pump.
    // Mapping must follow the applied commit, not worker scheduling or paint.
    harness.map_sized(&surface, id, (2048, 2048));
}
