//! What a Wayland client sees when a remote participant types into it.
//!
//! The client decodes its `wl_keyboard` stream into text with a US layout
//! (Shift from the modifiers event), so the assertion is on the text the
//! person typed, exactly. A remote key repeats only as its sender repeats
//! it: the client's own repeat is off while a participant types, so a
//! release that crosses the network late cannot type the key meanwhile.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness failures are assertions"
)]

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use shell::network::{InputEvent, ShellView, StreamingShare};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use vello::kurbo::Rect;
use wayland_client::protocol::{wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, KeyLocation, PhysicalKey};

const SHIFT: u32 = 42;
const ROWS: [(&str, &str, u32); 4] = [
    ("1234567890", "!@#$%^&*()", 2),
    ("qwertyuiop", "QWERTYUIOP", 16),
    ("asdfghjkl;", "ASDFGHJKL:", 30),
    ("zxcvbnm,./", "ZXCVBNM<>?", 44),
];

/// The US-layout evdev keycode for `c`, and whether it takes Shift.
fn stroke(c: char) -> (u32, bool) {
    if c == ' ' {
        return (57, false);
    }
    ROWS.iter()
        .find_map(|(plain, shifted, first)| {
            let at = |row: &str| {
                row.chars()
                    .position(|other| other == c)
                    .map(|at| first.checked_add(u32::try_from(at).unwrap()).unwrap())
            };
            at(plain)
                .map(|code| (code, false))
                .or_else(|| at(shifted).map(|code| (code, true)))
        })
        .expect("a US key types it")
}

/// What `code` types on a US layout.
fn character(code: u32, shift: bool) -> Option<char> {
    if code == 57 {
        return Some(' ');
    }
    ROWS.iter().find_map(|(plain, shifted, first)| {
        let at = usize::try_from(code.checked_sub(*first)?).ok()?;
        (if shift { shifted } else { plain }).chars().nth(at)
    })
}

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    shift: bool,
    text: String,
    repeat_rate: Option<i32>,
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
            wl_keyboard::Event::Modifiers { mods_depressed, .. } => {
                state.shift = mods_depressed & 1 != 0;
            }
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(wl_keyboard::KeyState::Pressed),
                ..
            } => {
                if let Some(c) = character(key, state.shift) {
                    state.text.push(c);
                }
            }
            wl_keyboard::Event::RepeatInfo { rate, .. } => state.repeat_rate = Some(rate),
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

    fn bind<I: Proxy + 'static>(&self, interface: &str, version: u32) -> I
    where
        Client: Dispatch<I, ()>,
    {
        let (name, _, advertised) = self
            .client
            .globals
            .iter()
            .find(|(_, name, _)| name == interface)
            .expect("advertised global")
            .clone();
        let registry = self.conn.display().get_registry(&self.queue.handle(), ());
        registry.bind(name, version.min(advertised), &self.queue.handle(), ())
    }
}

fn foreign_id(state: &shell::compositor::state::State, app_id: &str) -> u64 {
    state
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
        .expect("foreign surface was adopted")
}

fn key(keysym: u32, pressed: bool) -> InputEvent {
    InputEvent::Key {
        keysym,
        pressed,
        modifiers: 0,
    }
}

/// A key of the machine's own keyboard, as the backend hands it over.
fn local_key(code: winit::keyboard::KeyCode, pressed: bool) -> WindowEvent {
    // `KeyEvent` keeps platform data private; zero it and set what the
    // compositor reads.
    let mut event: KeyEvent = unsafe { std::mem::zeroed() };
    event.logical_key = Key::Character("a".into());
    event.physical_key = PhysicalKey::Code(code);
    event.location = KeyLocation::Standard;
    event.state = if pressed {
        ElementState::Pressed
    } else {
        ElementState::Released
    };
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event,
        is_synthetic: false,
    }
}

#[test]
fn a_remote_participant_types_exactly_its_text_and_repeats_only_as_its_sender_does() {
    let mut h = Harness::new();
    let _registry = h.conn.display().get_registry(&h.queue.handle(), ());
    h.pump();
    let compositor = h.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
    let wm = h.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
    let seat = h.bind::<wl_seat::WlSeat>("wl_seat", 9);
    let _keyboard = seat.get_keyboard(&h.queue.handle(), ());
    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("org.example.Writer".into());
    h.pump();

    let window = foreign_id(&h.state, "org.example.Writer");
    h.state.set_client_keyboard_focus(Some(window));
    h.pump();
    let configured = h.client.repeat_rate.expect("the seat's repeat info");
    assert!(configured > 0, "clients repeat the machine's own keys");

    let peer = 9;
    h.state
        .share_registry
        .insert(peer, window, StreamingShare::new());
    h.state.remote_input_firewall.publish_view(ShellView::new(
        vec![(window, Rect::new(0.0, 0.0, 800.0, 600.0))],
        Some(window),
        false,
        false,
    ));
    assert!(h.state.apply_remote_input(
        peer,
        window,
        InputEvent::KeyboardFocus {
            surface: window,
            focused: true,
        },
    ));

    let text = "Hello, World! example.com: 42?";
    for c in text.chars() {
        let (code, shift) = stroke(c);
        if shift {
            assert!(h.state.apply_remote_input(peer, window, key(SHIFT, true)));
        }
        assert!(h.state.apply_remote_input(peer, window, key(code, true)));
        assert!(h.state.apply_remote_input(peer, window, key(code, false)));
        if shift {
            assert!(h.state.apply_remote_input(peer, window, key(SHIFT, false)));
        }
    }
    h.pump();
    assert_eq!(
        h.client.text, text,
        "every stroke, in order, with its Shift"
    );
    assert_eq!(
        h.client.repeat_rate,
        Some(0),
        "the client does not repeat a remote key on its own"
    );

    // The sender holds "o" and repeats it twice; then Shift, which never repeats.
    h.client.text.clear();
    let (o, _) = stroke('o');
    for _ in 0..3 {
        assert!(h.state.apply_remote_input(peer, window, key(o, true)));
    }
    assert!(h.state.apply_remote_input(peer, window, key(o, false)));
    assert!(h.state.apply_remote_input(peer, window, key(SHIFT, true)));
    let _ = h.state.apply_remote_input(peer, window, key(SHIFT, true));
    assert!(h.state.apply_remote_input(peer, window, key(SHIFT, false)));
    h.pump();
    assert_eq!(h.client.text, "ooo");

    // The person at the machine types: clients repeat its keys again.
    let mut frame = shell::compositor::frame::FrameBuilder::new((1600, 900));
    for pressed in [true, false] {
        let _ = shell::compositor::client_input::route_input(
            &mut h.state,
            &mut frame,
            &local_key(winit::keyboard::KeyCode::KeyA, pressed),
            &Modifiers::default(),
        );
    }
    h.pump();
    assert_eq!(h.client.repeat_rate, Some(configured));
}
