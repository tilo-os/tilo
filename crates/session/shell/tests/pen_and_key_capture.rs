//! A drawing app gets the pen itself over tablet-v2 while an app without a
//! tablet seat sees it as a pointer; a VM that inhibits shortcuts gets the
//! shell's chords until focus leaves or the release chord hands them back.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness failures are assertions"
)]

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use wayland_client::protocol::{wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1, zwp_keyboard_shortcuts_inhibitor_v1,
};
use wayland_protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2, zwp_tablet_seat_v2, zwp_tablet_tool_v2, zwp_tablet_v2,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, NativeKeyCode, PhysicalKey};

use shell::compositor::backend::{
    BackendEvent, TabletDevice, TabletInput, TabletPhase, TabletTool, TabletToolKind,
};
use shell::compositor::frame::FrameBuilder;
use shell::compositor::state::State;

#[derive(Debug, PartialEq)]
enum Wire {
    ToolAdded,
    ProximityIn,
    Pressure(u32),
    Tilt(f64, f64),
    Down,
    Up,
    Button(u32, bool),
    Active,
    Inactive,
    Key(u32),
}

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    wire: Vec<Wire>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            state.globals.push((name, interface, version));
        }
    }
}

impl Dispatch<zwp_tablet_seat_v2::ZwpTabletSeatV2, ()> for Client {
    fn event(
        state: &mut Self,
        _: &zwp_tablet_seat_v2::ZwpTabletSeatV2,
        event: zwp_tablet_seat_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_tablet_seat_v2::Event::ToolAdded { .. } = event {
            state.wire.push(Wire::ToolAdded);
        }
    }

    event_created_child!(Client, zwp_tablet_seat_v2::ZwpTabletSeatV2, [
        zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (zwp_tablet_v2::ZwpTabletV2, ()),
        zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (zwp_tablet_tool_v2::ZwpTabletToolV2, ()),
    ]);
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

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Client {
    fn event(
        _: &mut Self,
        base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for Client {
    fn event(
        _: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}

wayland_client::delegate_noop!(Client: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(Client: ignore wl_surface::WlSurface);
wayland_client::delegate_noop!(Client: ignore wl_seat::WlSeat);
wayland_client::delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
wayland_client::delegate_noop!(Client: ignore zwp_tablet_manager_v2::ZwpTabletManagerV2);
wayland_client::delegate_noop!(Client: ignore zwp_tablet_v2::ZwpTabletV2);
wayland_client::delegate_noop!(Client: ignore zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1);

struct Harness {
    display: Display<State>,
    state: State,
    fb: FrameBuilder,
    queue: wayland_client::EventQueue<Client>,
    client: Client,
    conn: Connection,
    _event_loop: EventLoop<'static, State>,
}

impl Harness {
    fn new() -> Self {
        let event_loop = EventLoop::try_new().expect("calloop");
        let display = Display::new().expect("display");
        let state = State::new(event_loop.handle(), display.handle());
        let fb = FrameBuilder::with_handle(state.workspace_tree.clone(), false, (1280, 800));
        let (server, client) = UnixStream::pair().expect("socket pair");
        display
            .handle()
            .insert_client(
                server,
                Arc::new(shell::compositor::state::ClientState::default()),
            )
            .expect("client insert");
        client.set_nonblocking(true).expect("nonblocking");
        let conn = Connection::from_socket(client).expect("client connection");
        let queue = conn.new_event_queue::<Client>();
        let mut harness = Self {
            display,
            state,
            fb,
            queue,
            client: Client::default(),
            conn,
            _event_loop: event_loop,
        };
        let _registry = harness
            .conn
            .display()
            .get_registry(&harness.queue.handle(), ());
        harness.pump();
        harness
    }

    fn pump(&mut self) {
        for _ in 0..6 {
            let _ = self.queue.flush();
            let _ = self.display.dispatch_clients(&mut self.state);
            let _ = self.display.flush_clients();
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read();
            }
            let _ = self.queue.dispatch_pending(&mut self.client);
        }
    }

    fn bind<I: Proxy + 'static>(&self, interface: &str, version: u32) -> I
    where
        Client: Dispatch<I, ()>,
    {
        let (name, _, advertised) = self
            .client
            .globals
            .iter()
            .find(|(_, name, _)| name == interface)
            .unwrap_or_else(|| panic!("{interface} is advertised"))
            .clone();
        let registry = self.conn.display().get_registry(&self.queue.handle(), ());
        registry.bind(name, version.min(advertised), &self.queue.handle(), ())
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
        let id = self
            .state
            .workspace_tree
            .read()
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter())
            .find_map(|tab| match &tab.tile {
                shell::shell::state::TileContent::Foreign {
                    app_id: actual,
                    surface_id,
                } if actual == app_id => Some(*surface_id),
                _ => None,
            })
            .expect("foreign surface was adopted");
        (surface, id)
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
        &mut h.state,
        &h.fb,
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
            &mut h.state,
            &h.fb,
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
        &mut h.state,
        &h.fb,
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
        &mut h.state,
        &h.fb,
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

fn key(logical: Key, pressed: bool) -> WindowEvent {
    // SAFETY: `KeyEvent` keeps platform data private; the input adapters
    // build it the same way.
    let mut event: KeyEvent = unsafe { std::mem::zeroed() };
    event.logical_key = logical;
    event.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    event.location = KeyLocation::Standard;
    event.state = if pressed {
        ElementState::Pressed
    } else {
        ElementState::Released
    };
    WindowEvent::KeyboardInput {
        // SAFETY: an inert device id, as the DRM adapter uses.
        device_id: unsafe { std::mem::zeroed() },
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
        &mut h.state,
        &mut h.fb,
        &key(Key::Named(NamedKey::Tab), true),
        &super_mods,
    );
    let _ = shell::compositor::client_input::route_input(
        &mut h.state,
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
