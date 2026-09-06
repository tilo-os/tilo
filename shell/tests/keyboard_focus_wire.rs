//! Wire-level keyboard focus regression tests.
//!
//! Smithay deliberately transfers every forwarded held key in the `enter`
//! accompanying a focus replacement.  The following release must therefore
//! go to the *new* focus, exactly once; sending it back to the old surface
//! would contradict the Wayland ordering and create a stuck key.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness failures are assertions"
)]

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use wayland_client::protocol::{wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, KeyLocation, NamedKey, NativeKeyCode, PhysicalKey};

#[derive(Debug, PartialEq, Eq)]
enum KeyboardWire {
    Enter { surface: u32, keys: Vec<u32> },
    Leave { surface: u32 },
    Key { key: u32, pressed: bool },
}

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    keyboard: Vec<KeyboardWire>,
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

fn surface_id(surface: &wl_surface::WlSurface) -> u32 {
    surface.id().protocol_id()
}

/// Construct the same minimal `WindowEvent` that the DRM/test-control input
/// adapters feed to the capture path. `KeyEvent` keeps platform data private,
/// hence zero-init plus the fields consumed by the compositor.
fn captured_key_event(logical: Key, pressed: bool) -> WindowEvent {
    let mut event: KeyEvent = unsafe { std::mem::zeroed() };
    event.logical_key = logical;
    event.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    event.location = KeyLocation::Standard;
    event.state = if pressed {
        ElementState::Pressed
    } else {
        ElementState::Released
    };
    event.repeat = false;
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event,
        is_synthetic: false,
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
        match event {
            wl_keyboard::Event::Enter { surface, keys, .. } => {
                let keys = keys
                    .chunks_exact(4)
                    .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("u32 key")))
                    .collect();
                state.keyboard.push(KeyboardWire::Enter {
                    surface: surface_id(&surface),
                    keys,
                });
            }
            wl_keyboard::Event::Leave { surface, .. } => {
                state.keyboard.push(KeyboardWire::Leave {
                    surface: surface_id(&surface),
                });
            }
            wl_keyboard::Event::Key {
                key,
                state: key_state,
                ..
            } => {
                state.keyboard.push(KeyboardWire::Key {
                    key,
                    pressed: matches!(key_state, WEnum::Value(wl_keyboard::KeyState::Pressed)),
                });
            }
            _ => {}
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

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);

struct Harness {
    display: Display<shell::compositor::state::State>,
    state: shell::compositor::state::State,
    queue: wayland_client::EventQueue<Client>,
    client: Client,
    conn: Connection,
    _event_loop: EventLoop<'static, shell::compositor::state::State>,
}

impl Harness {
    fn new() -> Self {
        let event_loop = EventLoop::try_new().expect("calloop");
        let display = Display::new().expect("display");
        let state = shell::compositor::state::State::new(event_loop.handle(), display.handle());
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
        Self {
            display,
            state,
            queue,
            client: Client::default(),
            conn,
            _event_loop: event_loop,
        }
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

    fn bind<I: Proxy + 'static>(&self, interface: &str, version: u32) -> Option<I>
    where
        Client: Dispatch<I, ()>,
    {
        let (name, _, advertised) = self
            .client
            .globals
            .iter()
            .find(|(_, name, _)| name == interface)?
            .clone();
        let registry = self.conn.display().get_registry(&self.queue.handle(), ());
        Some(registry.bind(name, version.min(advertised), &self.queue.handle(), ()))
    }
}

fn foreign_id(state: &shell::compositor::state::State, app_id: &str) -> u64 {
    state
        .workspace_tree
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
        .expect("foreign surface was adopted")
}

#[test]
fn focus_change_transfers_a_held_key_then_releases_it_once_at_the_new_focus() {
    let mut h = Harness::new();
    let _registry = h.conn.display().get_registry(&h.queue.handle(), ());
    h.pump();
    let compositor = h
        .bind::<wl_compositor::WlCompositor>("wl_compositor", 4)
        .expect("compositor");
    let wm = h
        .bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1)
        .expect("xdg shell");
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9).expect("seat");
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let qh = h.queue.handle();

    let first = compositor.create_surface(&qh, ());
    let first_xdg = wm.get_xdg_surface(&first, &qh, ());
    let first_top = first_xdg.get_toplevel(&qh, ());
    first_top.set_app_id("org.example.First".into());
    let second = compositor.create_surface(&qh, ());
    let second_xdg = wm.get_xdg_surface(&second, &qh, ());
    let second_top = second_xdg.get_toplevel(&qh, ());
    second_top.set_app_id("org.example.Second".into());
    h.pump();

    let first_id = foreign_id(&h.state, "org.example.First");
    let second_id = foreign_id(&h.state, "org.example.Second");
    h.state.set_client_keyboard_focus(Some(first_id));
    h.pump();
    h.client.keyboard.clear();

    // evdev KEY_BACKSPACE.  Smithay receives xkb keycode 22 internally but
    // the wire protocol correctly carries the evdev value 14.
    assert!(h.state.deliver_client_key(14, true, true));
    h.pump();
    h.state.set_client_keyboard_focus(Some(second_id));
    h.pump();
    assert!(h.state.deliver_client_key(14, false, true));
    h.pump();

    assert_eq!(
        h.client.keyboard,
        vec![
            KeyboardWire::Key {
                key: 14,
                pressed: true,
            },
            KeyboardWire::Leave {
                surface: surface_id(&first),
            },
            KeyboardWire::Enter {
                surface: surface_id(&second),
                keys: vec![14],
            },
            KeyboardWire::Key {
                key: 14,
                pressed: false,
            },
        ],
        "focus replacement must hand the held key to the new surface, then exactly one release follows there"
    );
}

/// A bounded real-socket stress pass for the failure pattern Electron users
/// described: editing keys held while focus moves in response to a layout or
/// activation revision.  Every press must get one release, regardless of
/// which toplevel owns focus at the moment it arrives.
#[test]
fn focus_handoff_soak_has_one_release_per_backspace_press() {
    const ROUNDS: usize = 128;
    const MAX_DISPATCH: Duration = Duration::from_millis(500);

    let mut h = Harness::new();
    let _registry = h.conn.display().get_registry(&h.queue.handle(), ());
    h.pump();
    let compositor = h
        .bind::<wl_compositor::WlCompositor>("wl_compositor", 4)
        .expect("compositor");
    let wm = h
        .bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1)
        .expect("xdg shell");
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9).expect("seat");
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let qh = h.queue.handle();

    let first = compositor.create_surface(&qh, ());
    let first_xdg = wm.get_xdg_surface(&first, &qh, ());
    let first_top = first_xdg.get_toplevel(&qh, ());
    first_top.set_app_id("org.example.SoakFirst".into());
    let second = compositor.create_surface(&qh, ());
    let second_xdg = wm.get_xdg_surface(&second, &qh, ());
    let second_top = second_xdg.get_toplevel(&qh, ());
    second_top.set_app_id("org.example.SoakSecond".into());
    h.pump();

    let first_id = foreign_id(&h.state, "org.example.SoakFirst");
    let second_id = foreign_id(&h.state, "org.example.SoakSecond");
    h.client.keyboard.clear();
    let mut max_dispatch = Duration::ZERO;

    for round in 0..ROUNDS {
        let (old, new) = if round % 2 == 0 {
            (first_id, second_id)
        } else {
            (second_id, first_id)
        };
        h.state.set_client_keyboard_focus(Some(old));
        h.pump();
        let started = Instant::now();
        assert!(h.state.deliver_client_key(14, true, true));
        h.pump();
        h.state.set_client_keyboard_focus(Some(new));
        h.pump();
        assert!(h.state.deliver_client_key(14, false, true));
        h.pump();
        max_dispatch = max_dispatch.max(started.elapsed());
    }

    let keys: Vec<_> = h
        .client
        .keyboard
        .iter()
        .filter_map(|event| match event {
            KeyboardWire::Key { key, pressed } => Some((*key, *pressed)),
            KeyboardWire::Enter { .. } | KeyboardWire::Leave { .. } => None,
        })
        .collect();
    assert_eq!(
        keys.len(),
        ROUNDS * 2,
        "every physical press/release must reach exactly one focused client"
    );
    for pair in keys.chunks_exact(2) {
        assert_eq!(pair, [(14, true), (14, false)]);
    }
    assert!(
        h.state
            .seat
            .get_keyboard()
            .expect("seat keyboard")
            .pressed_keys()
            .is_empty(),
        "no Backspace may remain pressed after the bounded focus soak"
    );
    eprintln!(
        "keyboard focus soak: {ROUNDS} handoffs, max press-to-release dispatch {max_dispatch:?}"
    );
    assert!(
        max_dispatch < MAX_DISPATCH,
        "socket dispatch stalled for {max_dispatch:?}; limit is {MAX_DISPATCH:?}"
    );
}

/// This is the precise seat sequence used at the lock boundary: the client
/// first owns held modifier/editing keys, lock revokes focus but retains the
/// seat's physical state, physical ups arrive while capture is active, then an app is focused
/// again.  No release or stale modifier may leak into either locked or newly
/// restored client input.
#[test]
fn lock_capture_boundary_hides_held_keys_and_restores_a_clean_seat() {
    let mut h = Harness::new();
    let _registry = h.conn.display().get_registry(&h.queue.handle(), ());
    h.pump();
    let compositor = h
        .bind::<wl_compositor::WlCompositor>("wl_compositor", 4)
        .expect("compositor");
    let wm = h
        .bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1)
        .expect("xdg shell");
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9).expect("seat");
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let qh = h.queue.handle();

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("org.example.LockBoundary".into());
    h.pump();
    let foreign_surface_id = foreign_id(&h.state, "org.example.LockBoundary");
    h.state.set_client_keyboard_focus(Some(foreign_surface_id));
    h.pump();
    h.client.keyboard.clear();

    assert!(h.state.deliver_client_key(42, true, true));
    assert!(h.state.deliver_client_key(14, true, true));
    h.pump();
    assert_eq!(
        h.client.keyboard,
        vec![
            KeyboardWire::Key {
                key: 42,
                pressed: true,
            },
            KeyboardWire::Key {
                key: 14,
                pressed: true,
            },
        ]
    );

    // Native `run()` calls this immediately when SessionManager enters lock:
    // client delivery stops, while Smithay retains the actual held keys until
    // the locked physical ups below.
    h.state.set_client_keyboard_focus(None);
    h.pump();
    h.client.keyboard.clear();

    // These are the physical ups which arrived while the lock owns input.
    // They advance Smithay's state but, with no focus, cannot be observed by
    // the old client.
    shell::compositor::client_input::consume_keyboard_while_captured(
        &mut h.state,
        &captured_key_event(Key::Named(NamedKey::Backspace), false),
    );
    shell::compositor::client_input::consume_keyboard_while_captured(
        &mut h.state,
        &captured_key_event(Key::Named(NamedKey::Shift), false),
    );
    h.pump();
    assert!(
        h.client.keyboard.is_empty(),
        "a locked client boundary must not emit a key up to the hidden app"
    );
    assert!(
        h.state
            .seat
            .get_keyboard()
            .expect("seat keyboard")
            .pressed_keys()
            .is_empty(),
        "Shift/Backspace must be cleared before unlock restores application focus"
    );

    h.state.set_client_keyboard_focus(Some(foreign_surface_id));
    h.pump();
    assert_eq!(
        h.client.keyboard,
        vec![KeyboardWire::Enter {
            surface: surface_id(&surface),
            keys: Vec::new(),
        }],
        "restored focus must not inherit a held modifier or editing key"
    );
    h.client.keyboard.clear();
    assert!(h.state.deliver_client_key(30, true, true));
    assert!(h.state.deliver_client_key(30, false, true));
    h.pump();
    assert_eq!(
        h.client.keyboard,
        vec![
            KeyboardWire::Key {
                key: 30,
                pressed: true,
            },
            KeyboardWire::Key {
                key: 30,
                pressed: false,
            },
        ],
        "typing after unlock must be a normal, balanced new key sequence"
    );
}
