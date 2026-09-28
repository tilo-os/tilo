//! A composing input method, end to end on the wire.
//!
//! Two real Wayland connections share one compositor `State`: a text field
//! (`zwp_text_input_v3`, as GTK, Qt, Chromium and winit speak it) and an input
//! method (`zwp_input_method_v2`, as Fcitx5 speaks it) accepted with the
//! composer's trusted credential. The input method grabs the keyboard, sees
//! the physical key the field would otherwise have received, shows preedit,
//! commits text, and opens its candidate window, which must land at the
//! field's caret. The same harness proves the gate: a public client and the
//! on-screen keyboard's credential cannot bind the input method while the
//! composer owns the seat, and handing the seat back disconnects the
//! composer.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "event counters, a 5 s deadline and a 300x40 buffer size, all far from overflow"
)]

use std::os::fd::{FromRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use shell::compositor::input_method::TrustedInputMethod;
use shell::compositor::state::{ClientState, State};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3, zwp_text_input_v3,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2, zwp_input_method_manager_v2, zwp_input_method_v2,
    zwp_input_popup_surface_v2,
};

/// Everything either connection observed.
#[derive(Default)]
struct Peer {
    globals: Vec<(u32, String, u32)>,
    // Text field side.
    text_input_entered: bool,
    preedit: Vec<Option<String>>,
    committed: Vec<Option<String>>,
    text_input_done: u32,
    keys: Vec<u32>,
    // Input method side.
    activated: u32,
    im_done: u32,
    unavailable: bool,
    grabbed_keys: Vec<(u32, bool)>,
    popup_rectangles: Vec<(i32, i32, i32, i32)>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Peer {
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

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Peer {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key { key, .. } = event {
            state.keys.push(key);
        }
    }
}

impl Dispatch<zwp_text_input_v3::ZwpTextInputV3, ()> for Peer {
    fn event(
        state: &mut Self,
        _: &zwp_text_input_v3::ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_text_input_v3::Event::Enter { .. } => state.text_input_entered = true,
            zwp_text_input_v3::Event::PreeditString { text, .. } => state.preedit.push(text),
            zwp_text_input_v3::Event::CommitString { text } => state.committed.push(text),
            zwp_text_input_v3::Event::Done { .. } => state.text_input_done += 1,
            _ => {}
        }
    }
}

impl Dispatch<zwp_input_method_v2::ZwpInputMethodV2, ()> for Peer {
    fn event(
        state: &mut Self,
        _: &zwp_input_method_v2::ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => state.activated += 1,
            zwp_input_method_v2::Event::Done => state.im_done += 1,
            zwp_input_method_v2::Event::Unavailable => state.unavailable = true,
            _ => {}
        }
    }
}

impl Dispatch<zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2, ()> for Peer {
    fn event(
        state: &mut Self,
        _: &zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_input_method_keyboard_grab_v2::Event::Key {
            key,
            state: pressed,
            ..
        } = event
        {
            state.grabbed_keys.push((
                key,
                matches!(pressed, WEnum::Value(wl_keyboard::KeyState::Pressed)),
            ));
        }
    }
}

impl Dispatch<zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2, ()> for Peer {
    fn event(
        state: &mut Self,
        _: &zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
        event: zwp_input_popup_surface_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_input_popup_surface_v2::Event::TextInputRectangle {
            x,
            y,
            width,
            height,
        } = event
        {
            state.popup_rectangles.push((x, y, width, height));
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Peer {
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

impl Dispatch<xdg_surface::XdgSurface, ()> for Peer {
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

delegate_noop!(Peer: ignore wl_compositor::WlCompositor);
delegate_noop!(Peer: ignore wl_surface::WlSurface);
delegate_noop!(Peer: ignore wl_seat::WlSeat);
delegate_noop!(Peer: ignore wl_shm::WlShm);
delegate_noop!(Peer: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Peer: ignore wl_buffer::WlBuffer);
delegate_noop!(Peer: ignore wl_callback::WlCallback);
delegate_noop!(Peer: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Peer: ignore zwp_text_input_manager_v3::ZwpTextInputManagerV3);
delegate_noop!(Peer: ignore zwp_input_method_manager_v2::ZwpInputMethodManagerV2);

struct Connected {
    /// The compositor's handle for this connection.
    server: smithay::reexports::wayland_server::Client,
    conn: Connection,
    queue: EventQueue<Peer>,
    peer: Peer,
}

impl Connected {
    fn bind<I: Proxy + 'static>(&self, interface: &str, version: u32) -> Option<I>
    where
        Peer: Dispatch<I, ()>,
    {
        let (name, _, advertised) = self
            .peer
            .globals
            .iter()
            .find(|(_, name, _)| name == interface)?
            .clone();
        let registry = self.conn.display().get_registry(&self.queue.handle(), ());
        Some(registry.bind(name, version.min(advertised), &self.queue.handle(), ()))
    }

    fn sees(&self, interface: &str) -> bool {
        self.peer
            .globals
            .iter()
            .any(|(_, name, _)| name == interface)
    }
}

struct Harness {
    display: Display<State>,
    state: State,
    clients: Vec<Connected>,
    _event_loop: EventLoop<'static, State>,
}

impl Harness {
    fn new() -> Self {
        let event_loop = EventLoop::try_new().expect("calloop");
        let display = Display::new().expect("display");
        let state = State::new(event_loop.handle(), display.handle());
        Self {
            display,
            state,
            clients: Vec::new(),
            _event_loop: event_loop,
        }
    }

    /// Connect a client with `credential`, as the matching socket acceptor
    /// would, and read its registry. Returns its index.
    fn connect(&mut self, credential: Option<TrustedInputMethod>) -> usize {
        let (server, client) = UnixStream::pair().expect("socket pair");
        let data = credential.map_or_else(
            || Arc::new(ClientState::default()),
            ClientState::input_method_arc,
        );
        let inserted = self
            .display
            .handle()
            .insert_client(server, data)
            .expect("client insert");
        if let Some(kind) = credential {
            self.state
                .register_trusted_input_method_client(&inserted, kind);
        }
        client.set_nonblocking(true).expect("nonblocking");
        let conn = Connection::from_socket(client).expect("client connection");
        let queue = conn.new_event_queue::<Peer>();
        let _registry = conn.display().get_registry(&queue.handle(), ());
        self.clients.push(Connected {
            server: inserted,
            conn,
            queue,
            peer: Peer::default(),
        });
        self.pump();
        self.clients.len() - 1
    }

    /// Pump until `done` holds, failing with `what` after a generous bound:
    /// buffer and role requests can take more than one exchange to land.
    fn pump_until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        while !done(self) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            self.pump();
            std::thread::yield_now();
        }
    }

    fn pump(&mut self) {
        for _ in 0..8 {
            for client in &mut self.clients {
                let _ = client.queue.flush();
            }
            let _ = self.display.dispatch_clients(&mut self.state);
            let _ = self.display.flush_clients();
            for client in &mut self.clients {
                if let Some(guard) = client.conn.prepare_read() {
                    let _ = guard.read();
                }
                let _ = client.queue.dispatch_pending(&mut client.peer);
            }
        }
    }
}

fn foreign_id(state: &State, app_id: &str) -> u64 {
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

/// Attach an opaque `width`×`height` shm buffer to `surface` and commit.
fn attach_buffer(client: &Connected, surface: &wl_surface::WlSurface, width: i32, height: i32) {
    use std::os::fd::AsFd as _;
    let shm = client.bind::<wl_shm::WlShm>("wl_shm", 1).expect("wl_shm");
    let stride = width * 4;
    let len = usize::try_from(stride * height).expect("buffer size");
    // SAFETY: a null-terminated name and a valid flag word are the whole
    // contract of `memfd_create`.
    let raw = unsafe { libc::memfd_create(c"tilo-input-method-wire".as_ptr(), 0) };
    assert!(
        raw >= 0,
        "memfd_create: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: `memfd_create` just handed us this descriptor.
    let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    file.set_len(len as u64).expect("size the shm pool");
    let qh = client.queue.handle();
    let pool = shm.create_pool(
        file.as_fd(),
        i32::try_from(len).expect("pool size"),
        &qh,
        (),
    );
    let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
    surface.attach(Some(&buffer), 0, 0);
    surface.damage(0, 0, width, height);
    surface.commit();
}

const INPUT_METHOD_MANAGER: &str = "zwp_input_method_manager_v2";
const VIRTUAL_KEYBOARD_MANAGER: &str = "zwp_virtual_keyboard_manager_v1";

#[test]
fn only_the_seat_owner_kind_sees_the_input_method_globals() {
    let mut h = Harness::new();
    h.state
        .set_input_method_owner(Some(TrustedInputMethod::Composer));
    let public = h.connect(None);
    let osk = h.connect(Some(TrustedInputMethod::OnScreenKeyboard));
    let composer = h.connect(Some(TrustedInputMethod::Composer));
    for global in [INPUT_METHOD_MANAGER, VIRTUAL_KEYBOARD_MANAGER] {
        assert!(
            !h.clients[public].sees(global),
            "public client saw {global}"
        );
        assert!(
            !h.clients[osk].sees(global),
            "the on-screen keyboard saw {global} while the composer owns the seat"
        );
        assert!(
            h.clients[composer].sees(global),
            "the composer lacks {global}"
        );
    }
    assert!(
        h.clients[public].sees("zwp_text_input_manager_v3"),
        "text fields stay public"
    );
}

#[allow(
    clippy::too_many_lines,
    reason = "one transcript from keyboard grab to committed text and candidate placement"
)]
#[test]
fn composer_grabs_keys_shows_preedit_commits_text_and_places_candidates_at_the_caret() {
    let mut h = Harness::new();
    h.state
        .set_output_layout(&[shell::compositor::backend::Output::new(
            shell::compositor::backend::OutputId(0),
            (1920, 1080),
            1.0,
        )]);
    h.state
        .set_input_method_owner(Some(TrustedInputMethod::Composer));

    // The text field: a toplevel with a keyboard and a text-input object.
    let app = h.connect(None);
    let (_app_surface, text_input, _app_xdg, _app_top, _keyboard) = {
        let client = &h.clients[app];
        let qh = client.queue.handle();
        let compositor = client
            .bind::<wl_compositor::WlCompositor>("wl_compositor", 4)
            .expect("compositor");
        let wm = client
            .bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3)
            .expect("xdg shell");
        let seat = client.bind::<wl_seat::WlSeat>("wl_seat", 9).expect("seat");
        let keyboard = seat.get_keyboard(&qh, ());
        let manager = client
            .bind::<zwp_text_input_manager_v3::ZwpTextInputManagerV3>(
                "zwp_text_input_manager_v3",
                1,
            )
            .expect("text-input-v3 is public");
        let text_input = manager.get_text_input(&seat, &qh, ());
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm.get_xdg_surface(&surface, &qh, ());
        let top = xdg.get_toplevel(&qh, ());
        top.set_app_id("org.example.Editor".into());
        (surface, text_input, xdg, top, keyboard)
    };
    h.pump();
    let field = foreign_id(&h.state, "org.example.Editor");
    shell::compositor::foreign::store()
        .note_placement(field, vello::kurbo::Rect::new(100.0, 200.0, 900.0, 1000.0));

    // The composer binds the input method and grabs the keyboard.
    let im_client = h.connect(Some(TrustedInputMethod::Composer));
    let (input_method, _grab, im_compositor) = {
        let client = &h.clients[im_client];
        let qh = client.queue.handle();
        let seat = client.bind::<wl_seat::WlSeat>("wl_seat", 9).expect("seat");
        let manager = client
            .bind::<zwp_input_method_manager_v2::ZwpInputMethodManagerV2>(INPUT_METHOD_MANAGER, 1)
            .expect("the composer may bind the input method");
        let input_method = manager.get_input_method(&seat, &qh, ());
        let grab = input_method.grab_keyboard(&qh, ());
        let compositor = client
            .bind::<wl_compositor::WlCompositor>("wl_compositor", 4)
            .expect("compositor");
        (input_method, grab, compositor)
    };
    h.pump();
    assert!(
        !h.clients[im_client].peer.unavailable,
        "the seat's slot was free"
    );

    // Focusing the field enters its text input; enabling it with a caret
    // rectangle activates the input method.
    h.state.set_client_keyboard_focus(Some(field));
    h.pump();
    assert!(h.clients[app].peer.text_input_entered);
    text_input.enable();
    text_input.set_cursor_rectangle(40, 30, 2, 20);
    text_input.commit();
    h.pump();
    assert_eq!(
        h.clients[im_client].peer.activated, 1,
        "input method activated"
    );

    // The candidate window, parented to the focused field.
    let _popup_surface = {
        let client = &h.clients[im_client];
        let qh = client.queue.handle();
        let surface = im_compositor.create_surface(&qh, ());
        let _popup = input_method.get_input_popup_surface(&surface, &qh, ());
        attach_buffer(client, &surface, 300, 40);
        surface
    };
    h.pump_until("the candidate window to be placed", |h| {
        !h.state.input_method_popup_surfaces_for_tests().is_empty()
    });

    // A physical key reaches the input method, not the field.
    h.clients[app].peer.keys.clear();
    // evdev KEY_N: the first letter of "ni".
    h.state.deliver_client_key(49, true, true);
    h.state.deliver_client_key(49, false, true);
    h.pump();
    assert_eq!(
        h.clients[im_client].peer.grabbed_keys,
        [(49, true), (49, false)],
        "the grab sees the key"
    );
    assert!(
        h.clients[app].peer.keys.is_empty(),
        "a grabbed key never reaches the field directly: {:?}",
        h.clients[app].peer.keys
    );

    // Preedit, then the composed text.
    let serial = h.clients[im_client].peer.im_done;
    input_method.set_preedit_string("ni".into(), 2, 2);
    input_method.commit(serial);
    h.pump();
    assert_eq!(h.clients[app].peer.preedit.last(), Some(&Some("ni".into())));
    let serial = h.clients[im_client].peer.im_done;
    input_method.set_preedit_string(String::new(), 0, 0);
    input_method.commit_string("你".into());
    input_method.commit(serial);
    h.pump();
    assert_eq!(h.clients[app].peer.committed, [Some("你".to_string())]);
    assert!(h.clients[app].peer.text_input_done >= 2);

    // The candidate window sits right below the caret: field origin
    // (100, 200) + caret (40, 30), one caret height (20) further down.
    let candidates = h.state.input_method_popup_surfaces_for_tests();
    assert_eq!(candidates.len(), 1, "one candidate window: {candidates:?}");
    assert_eq!(
        candidates[0].1,
        vello::kurbo::Rect::new(140.0, 250.0, 440.0, 290.0)
    );

    // Moving the caret moves the candidates.
    text_input.set_cursor_rectangle(200, 60, 2, 20);
    text_input.commit();
    h.pump();
    assert_eq!(
        h.clients[im_client].peer.popup_rectangles.last(),
        Some(&(200, 60, 2, 20))
    );
    let candidates = h.state.input_method_popup_surfaces_for_tests();
    assert_eq!(candidates.len(), 1, "still one candidate window");
    assert_eq!(
        candidates[0].1,
        vello::kurbo::Rect::new(300.0, 280.0, 600.0, 320.0)
    );

    // Handing the seat to the on-screen keyboard disconnects the composer.
    h.state
        .set_input_method_owner(Some(TrustedInputMethod::OnScreenKeyboard));
    // The display reaps a killed connection on its next dispatch; any
    // client traffic (here the field's own sync) is one.
    let _sync = h.clients[app]
        .conn
        .display()
        .sync(&h.clients[app].queue.handle(), ());
    h.pump();
    let composer_id = h.clients[im_client].server.id();
    assert!(
        h.display
            .handle()
            .backend_handle()
            .get_client_data(composer_id)
            .is_err(),
        "the previous owner's connection is cut"
    );
    assert!(
        h.display
            .handle()
            .backend_handle()
            .get_client_data(h.clients[app].server.id())
            .is_ok(),
        "the text field stays connected"
    );
}
