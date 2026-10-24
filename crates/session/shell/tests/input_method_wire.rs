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
    wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3, zwp_text_input_v3,
};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2, zwp_input_method_manager_v2, zwp_input_method_v2,
    zwp_input_popup_surface_v2,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};

use crate::support::wayland::{Globals as _, foreign_id};

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
    /// What the on-screen keyboard reads of the input method's events.
    keyboard_events: Vec<osk::imclient::ImEvent>,
    activated: u32,
    im_done: u32,
    unavailable: bool,
    grabbed_keys: Vec<(u32, bool)>,
    popup_rectangles: Vec<(i32, i32, i32, i32)>,
}

crate::support::wayland::test_client!(Peer);

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
            zwp_input_method_v2::Event::Activate => {
                state.activated += 1;
                state.keyboard_events.push(osk::imclient::ImEvent::Activate);
            }
            zwp_input_method_v2::Event::ContentType { hint, purpose } => {
                state
                    .keyboard_events
                    .push(osk::imclient::ImEvent::ContentType {
                        hint: u32::from(hint),
                        purpose: u32::from(purpose),
                    });
            }
            zwp_input_method_v2::Event::Done => {
                state.im_done += 1;
                state.keyboard_events.push(osk::imclient::ImEvent::Done);
            }
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
delegate_noop!(Peer: ignore zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1);
delegate_noop!(Peer: ignore zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1);

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
        let (name, version) = self.peer.global(interface, version)?;
        let registry = self.conn.display().get_registry(&self.queue.handle(), ());
        Some(registry.bind(name, version, &self.queue.handle(), ()))
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

#[test]
fn chrome_field_activates_the_input_method_and_receives_its_commit() {
    let mut h = Harness::new();
    h.state
        .set_input_method_owner(Some(TrustedInputMethod::OnScreenKeyboard));
    let keyboard = h.connect(Some(TrustedInputMethod::OnScreenKeyboard));
    let input_method = {
        let client = &h.clients[keyboard];
        let qh = client.queue.handle();
        let seat = client.bind::<wl_seat::WlSeat>("wl_seat", 1).expect("seat");
        client
            .bind::<zwp_input_method_manager_v2::ZwpInputMethodManagerV2>(INPUT_METHOD_MANAGER, 1)
            .expect("trusted keyboard may bind")
            .get_input_method(&seat, &qh, ())
    };
    h.pump();
    let mut proxy = shell::compositor::shell_text_input::ShellTextInput::connect(&mut h.state)
        .expect("chrome stand-in");
    let field = shell::compositor::shell_text_input::FieldRequest {
        content: ui::ime::InputContent::TEXT,
        x: 420,
        y: 100,
        width: 1,
        height: 24,
    };
    let deadline = tilo_test_env::hang_deadline();
    while h.clients[keyboard].peer.activated == 0 || !h.state.text_input_active() {
        assert!(
            std::time::Instant::now() < deadline,
            "chrome field never activated"
        );
        proxy.sync(&mut h.state, Some(field));
        h.pump();
    }
    assert!(proxy.holds_focus(&h.state));
    input_method.commit_string("q".into());
    input_method.commit(h.clients[keyboard].peer.im_done);
    let mut events = Vec::new();
    while events.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "chrome commit never arrived"
        );
        h.pump();
        events.extend(proxy.sync(&mut h.state, Some(field)));
    }
    assert!(events.iter().any(|event| matches!(event,
        winit::event::WindowEvent::Ime(winit::event::Ime::Commit(text)) if text == "q"
    )));
    proxy.sync(&mut h.state, None);
    h.pump();
    assert!(!proxy.holds_focus(&h.state));
    assert!(!h.state.text_input_active());
}

/// The on-screen keyboard's terminal keys reach one of the shell's own
/// fields as real keys: Ctrl+C arrives as the C key with Control held, and an
/// arrow with nothing held.
#[test]
fn a_virtual_keyboard_chord_reaches_a_chrome_terminal_with_its_modifiers() {
    use osk::inject::{Modifiers, Symbol};
    use winit::event::{ElementState, WindowEvent};
    use winit::keyboard::{Key, NamedKey};
    let mut h = Harness::new();
    h.state
        .set_input_method_owner(Some(TrustedInputMethod::OnScreenKeyboard));
    let keyboard = h.connect(Some(TrustedInputMethod::OnScreenKeyboard));
    let (_input_method, virtual_keyboard) = {
        let client = &h.clients[keyboard];
        let qh = client.queue.handle();
        let seat = client.bind::<wl_seat::WlSeat>("wl_seat", 1).expect("seat");
        let input_method = client
            .bind::<zwp_input_method_manager_v2::ZwpInputMethodManagerV2>(INPUT_METHOD_MANAGER, 1)
            .expect("trusted keyboard may bind")
            .get_input_method(&seat, &qh, ());
        let virtual_keyboard = client
            .bind::<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1>(
                VIRTUAL_KEYBOARD_MANAGER,
                1,
            )
            .expect("trusted keyboard may bind")
            .create_virtual_keyboard(&seat, &qh, ());
        (input_method, virtual_keyboard)
    };
    h.pump();
    let mut proxy = shell::compositor::shell_text_input::ShellTextInput::connect(&mut h.state)
        .expect("chrome stand-in");
    let field = shell::compositor::shell_text_input::FieldRequest {
        content: ui::ime::InputContent::TERMINAL,
        x: 420,
        y: 100,
        width: 1,
        height: 24,
    };
    let deadline = tilo_test_env::hang_deadline();
    while !proxy.holds_focus(&h.state) {
        assert!(
            std::time::Instant::now() < deadline,
            "chrome terminal never focused"
        );
        proxy.sync(&mut h.state, Some(field));
        h.pump();
    }

    let codes = upload_keymap(&virtual_keyboard, &[Symbol::Unicode('c'), Symbol::Up]);
    let ctrl = Modifiers {
        ctrl: true,
        alt: false,
    };
    let (c, up) = (codes[0], codes[1]);
    virtual_keyboard.modifiers(ctrl.xkb_mask(), 0, 0, 0);
    virtual_keyboard.key(0, c, 1);
    virtual_keyboard.key(1, c, 0);
    virtual_keyboard.modifiers(0, 0, 0, 0);
    virtual_keyboard.key(2, up, 1);
    virtual_keyboard.key(3, up, 0);

    let mut keys = Vec::new();
    while keys.len() < 4 {
        assert!(
            std::time::Instant::now() < deadline,
            "the keys never reached the chrome field: {keys:?}"
        );
        h.pump();
        proxy.sync(&mut h.state, Some(field));
        keys.extend(h.state.take_shell_field_keys());
    }
    let presses: Vec<(Key, bool, bool)> = keys
        .iter()
        .filter_map(|(event, modifiers)| match event {
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                Some((
                    event.logical_key.clone(),
                    modifiers.state().control_key(),
                    modifiers.state().alt_key(),
                ))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        presses,
        [
            (Key::Character("c".into()), true, false),
            (Key::Named(NamedKey::ArrowUp), false, false),
        ]
    );
}

/// Hand `virtual_keyboard` the keymap the on-screen keyboard builds for
/// `symbols`, returning each symbol's wire keycode.
fn upload_keymap(
    virtual_keyboard: &zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
    symbols: &[osk::inject::Symbol],
) -> Vec<u32> {
    use std::os::fd::AsFd as _;
    let (keymap, codes) = osk::inject::build_keymap(symbols);
    let mut bytes = keymap.into_bytes();
    bytes.push(0);
    // SAFETY: a null-terminated name and a valid flag word are the whole
    // contract of `memfd_create`.
    let raw = unsafe { libc::memfd_create(c"tilo-osk-keymap".as_ptr(), 0) };
    assert!(
        raw >= 0,
        "memfd_create: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: `memfd_create` just handed us this descriptor.
    let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    std::io::Write::write_all(&mut &file, &bytes).expect("write the keymap");
    virtual_keyboard.keymap(
        1,
        file.as_fd(),
        u32::try_from(bytes.len()).expect("keymap size"),
    );
    // The wire carries evdev codes: XKB's minus 8.
    codes.into_iter().map(|(_, code)| code - 8).collect()
}

/// The content type a focused field of an app asks for, as the toolkit
/// derives it for the native text input.
fn app_field(input: ui::widget::TextInput) -> shell::compositor::shell_text_input::FieldRequest {
    let mut app = ui::headless::FrameHarness::for_root(
        ui::State::new(),
        input.with_id("field"),
        (400, 80),
        ui::Theme::dark(),
    );
    app.perform_named("field", &ui::a11y::Action::Focus)
        .expect("focus the field");
    app.ime_window()
        .expect("a focused field asks for the input method")
}

/// A field as an app builds it, the keyboard layout it must reach, and
/// whether it is secret.
type FieldCase = (fn() -> ui::widget::TextInput, osk::InputPurpose, bool);

#[test]
fn an_email_field_reaches_the_on_screen_keyboard_as_an_email_field() {
    use osk::InputPurpose;
    use osk::imclient::{ImState, content_hint, may_learn};
    use ui::widget::{TextInput, TextInputKind};
    let secret = content_hint::HIDDEN_TEXT | content_hint::SENSITIVE_DATA;
    // Each field is built for its case: four are too large for the stack.
    let cases: [FieldCase; 4] = [
        (
            || TextInput::local().with_kind(TextInputKind::Email),
            InputPurpose::Email,
            false,
        ),
        (
            || TextInput::local().with_kind(TextInputKind::Phone),
            InputPurpose::Telephone,
            false,
        ),
        (
            || TextInput::local().with_secret(true),
            InputPurpose::Password,
            true,
        ),
        (TextInput::local, InputPurpose::Text, false),
    ];
    for (input, purpose, hidden) in cases {
        let field = app_field(input());
        let mut h = Harness::new();
        h.state
            .set_input_method_owner(Some(TrustedInputMethod::OnScreenKeyboard));
        let keyboard = h.connect(Some(TrustedInputMethod::OnScreenKeyboard));
        let _input_method = {
            let client = &h.clients[keyboard];
            let qh = client.queue.handle();
            let seat = client.bind::<wl_seat::WlSeat>("wl_seat", 1).expect("seat");
            client
                .bind::<zwp_input_method_manager_v2::ZwpInputMethodManagerV2>(
                    INPUT_METHOD_MANAGER,
                    1,
                )
                .expect("trusted keyboard may bind")
                .get_input_method(&seat, &qh, ())
        };
        h.pump();
        let mut proxy = shell::compositor::shell_text_input::ShellTextInput::connect(&mut h.state)
            .expect("chrome stand-in");
        let mut keyboard_state = ImState::new();
        let mut read = 0;
        let deadline = tilo_test_env::hang_deadline();
        while !keyboard_state.visible() || keyboard_state.purpose() != purpose {
            assert!(
                std::time::Instant::now() < deadline,
                "the keyboard never showed the {purpose:?} layout: {:?}",
                h.clients[keyboard].peer.keyboard_events
            );
            proxy.sync(&mut h.state, Some(field));
            h.pump();
            for event in &h.clients[keyboard].peer.keyboard_events[read..] {
                keyboard_state.reduce(*event);
            }
            read = h.clients[keyboard].peer.keyboard_events.len();
        }
        assert_eq!(
            keyboard_state.hints() & secret == secret,
            hidden,
            "{purpose:?}: hidden and sensitive exactly for a secret"
        );
        assert_eq!(
            may_learn(keyboard_state.purpose(), keyboard_state.hints()),
            !hidden
        );
    }
}

#[test]
fn a_late_keyboard_activates_an_already_enabled_chrome_field() {
    let mut h = Harness::new();
    h.state
        .set_input_method_owner(Some(TrustedInputMethod::OnScreenKeyboard));
    let mut proxy = shell::compositor::shell_text_input::ShellTextInput::connect(&mut h.state)
        .expect("chrome stand-in");
    let field = shell::compositor::shell_text_input::FieldRequest {
        content: ui::ime::InputContent::TEXT,
        x: 420,
        y: 100,
        width: 1,
        height: 24,
    };
    let deadline = tilo_test_env::hang_deadline();
    while !proxy.holds_focus(&h.state) {
        assert!(
            std::time::Instant::now() < deadline,
            "chrome field never focused"
        );
        proxy.sync(&mut h.state, Some(field));
        h.pump();
    }
    // Drain enter and the client enable/commit before starting the keyboard.
    // Smithay cannot activate that request until an input method exists.
    h.pump();
    proxy.sync(&mut h.state, Some(field));
    h.pump();
    let keyboard = h.connect(Some(TrustedInputMethod::OnScreenKeyboard));
    let input_method = {
        let client = &h.clients[keyboard];
        let qh = client.queue.handle();
        let seat = client.bind::<wl_seat::WlSeat>("wl_seat", 1).expect("seat");
        client
            .bind::<zwp_input_method_manager_v2::ZwpInputMethodManagerV2>(INPUT_METHOD_MANAGER, 1)
            .expect("trusted keyboard may bind")
            .get_input_method(&seat, &qh, ())
    };
    h.pump();
    // Promoting the keyboard surface reannounces the already-focused field.
    h.state.reannounce_text_input_focus();
    while h.clients[keyboard].peer.activated == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "late keyboard never activated"
        );
        proxy.sync(&mut h.state, Some(field));
        h.pump();
    }
    input_method.commit_string("q".into());
    input_method.commit(h.clients[keyboard].peer.im_done);
    let mut events = Vec::new();
    while events.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "late keyboard commit never arrived"
        );
        h.pump();
        events.extend(proxy.sync(&mut h.state, Some(field)));
    }
    assert!(events.iter().any(|event| matches!(event,
        winit::event::WindowEvent::Ime(winit::event::Ime::Commit(text)) if text == "q"
    )));
}

#[test]
fn dictation_reaches_native_chrome_as_one_corrected_ime_commit() {
    assert_dictation_native_commit(&["rough".into()], "東京 👋");
}

/// Recorded, literal public-engine results, replayed through production editing.
/// These fixtures prove insertion/undo, not a new acoustic measurement.
#[derive(serde::Deserialize)]
pub(super) struct PublicAotTranscript {
    pub case: String,
    pub raw_final: String,
    all_drafts: Vec<PublicAotDraft>,
}
#[derive(serde::Deserialize)]
struct PublicAotDraft {
    text: String,
}
impl PublicAotTranscript {
    pub fn drafts(&self) -> Vec<String> {
        self.all_drafts
            .iter()
            .map(|draft| draft.text.clone())
            .collect()
    }
}
pub(super) fn public_aot_transcripts() -> Vec<PublicAotTranscript> {
    #[derive(serde::Deserialize)]
    struct Ledger {
        qualification_only: bool,
        production_activation: bool,
        results: Vec<PublicAotTranscript>,
    }
    let ledger: Ledger = serde_json::from_str(include_str!(
        "../../../../docs/architecture/dictation-whistle-aot-review.json"
    ))
    .expect("source-pinned public AOT qualification ledger");
    assert!(ledger.qualification_only && !ledger.production_activation);
    assert_eq!(
        ledger
            .results
            .iter()
            .map(|row| row.case.as_str())
            .collect::<Vec<_>>(),
        ["en_us-0", "de_de-0", "fr_fr-0", "es_419-0"]
    );
    for row in &ledger.results {
        assert!(!row.raw_final.is_empty() && !row.all_drafts.is_empty());
    }
    ledger.results
}

pub(super) fn public_image_transcripts() -> Vec<PublicAotTranscript> {
    let rows = image_transcripts(
        include_str!("../../../../docs/architecture/dictation-whistle-aot-image.json"),
        1,
    );
    assert_eq!(
        rows.iter().map(|row| row.case.as_str()).collect::<Vec<_>>(),
        ["de_de-0", "en_us-0", "es_419-0", "fr_fr-0"]
    );
    rows
}

pub(super) fn public_long_image_transcripts() -> Vec<PublicAotTranscript> {
    let rows = image_transcripts(
        include_str!("../../../../docs/architecture/dictation-whistle-aot-image-120s.json"),
        11,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].case, "fr_fr-0");
    rows
}

fn image_transcripts(source: &str, repetitions: usize) -> Vec<PublicAotTranscript> {
    #[derive(serde::Deserialize)]
    struct Ledger {
        qualification_only: bool,
        production_activation: bool,
        production_source_handler: bool,
        results: Vec<ResultRow>,
    }
    #[derive(serde::Deserialize)]
    struct ResultRow {
        case: String,
        mode: String,
        exit: i32,
        repeat_audio: usize,
        submitted_seconds: f64,
        replies: Vec<Reply>,
    }
    #[derive(serde::Deserialize)]
    struct Reply {
        kind: String,
        text: Option<String>,
    }
    let ledger: Ledger = serde_json::from_str(source)
        .expect("actual production-handler public qualification ledger");
    assert!(ledger.qualification_only && !ledger.production_activation);
    assert!(ledger.production_source_handler);
    let mut rows = Vec::new();
    for result in ledger.results {
        if result.mode != "complete" || result.exit != 0 || result.repeat_audio != repetitions {
            continue;
        }
        assert!(result.submitted_seconds <= 120.0);
        if repetitions == 11 {
            assert_eq!(result.submitted_seconds, 120.0);
        }
        let mut drafts = Vec::new();
        let mut finals = Vec::new();
        for reply in result.replies {
            if reply.kind == "draft" {
                drafts.push(PublicAotDraft {
                    text: reply.text.expect("literal recorded draft"),
                });
            } else if reply.kind == "done" {
                finals.push(reply.text.expect("literal recorded final"));
            }
        }
        assert_eq!(finals.len(), 1, "one observed production final per case");
        assert!(!drafts.is_empty());
        rows.push(PublicAotTranscript {
            case: result.case,
            raw_final: finals.remove(0),
            all_drafts: drafts,
        });
    }
    rows.sort_by(|a, b| a.case.cmp(&b.case));
    rows
}

#[test]
fn public_image_transcripts_reach_native_notes_with_selection_undo_and_redo() {
    for row in public_image_transcripts() {
        assert_dictation_native_commit(&row.drafts(), &row.raw_final);
    }
}

#[test]
fn public_image_transcripts_full_session_reaches_native_notes_with_owned_undo() {
    for row in public_long_image_transcripts() {
        assert_dictation_native_commit(&row.drafts(), &row.raw_final);
    }
}

#[test]
fn public_aot_transcripts_reach_native_notes_with_selection_undo_and_redo() {
    for row in public_aot_transcripts() {
        assert_dictation_native_commit(&row.drafts(), &row.raw_final);
    }
}

fn assert_dictation_native_commit(drafts: &[String], final_text: &str) {
    let field = shell::compositor::shell_text_input::FieldRequest {
        content: ui::ime::InputContent::TEXT,
        x: 420,
        y: 100,
        width: 1,
        height: 24,
    };
    let committed = receive_native_dictation(drafts, final_text, field);
    assert_native_field_selection_and_undo(&committed);
}

#[test]
fn native_document_paragraphs_have_one_owned_undo_and_redo() {
    for paragraph in ["First café.\n\nSecond 👋", "\n\n", "First café.\n"] {
        assert_native_field_selection_and_undo(paragraph);
    }
}

#[test]
fn native_multiline_intent_reaches_dictation_through_the_existing_bridge() {
    use ui::Widget;
    struct ParagraphField(ui::widget::TextInput);
    impl ui::widget::Decorator for ParagraphField {
        type Inner = ui::widget::TextInput;
        fn inner(&self) -> &Self::Inner {
            &self.0
        }
        fn inner_mut(&mut self) -> &mut Self::Inner {
            &mut self.0
        }
    }
    let mut input = ui::widget::TextInput::local().multiline(true);
    input.set_focus(true);
    let mut decorated = ParagraphField(ui::widget::TextInput::local().multiline(true));
    Widget::set_focus(&mut decorated, true);
    let mut editor = ui::widget::CodeEditor::local();
    editor.set_focused(true);
    editor.set_rect(vello::kurbo::Rect::new(0.0, 0.0, 400.0, 200.0));
    let paragraph = "Premier paragraphe.\n\nDeuxième paragraphe 👋";
    for widget in [
        &input as &dyn Widget,
        &decorated as &dyn Widget,
        &editor as &dyn Widget,
    ] {
        let field = ui::ime::ImeWindowState::from_widget(widget).unwrap();
        assert!(field.content.hints.multiline);
        assert_eq!(
            receive_native_dictation(&["Provisional\nparagraph".into()], paragraph, field),
            paragraph
        );
    }
}

fn receive_native_dictation(
    drafts: &[String],
    final_text: &str,
    field: shell::compositor::shell_text_input::FieldRequest,
) -> String {
    use shell::compositor::shell_text_input::ShellTextInput;
    let mut h = Harness::new();
    let mut proxy = ShellTextInput::connect(&mut h.state).expect("native chrome bridge");
    let deadline = tilo_test_env::hang_deadline();
    while !proxy.holds_focus(&h.state) {
        assert!(
            std::time::Instant::now() < deadline,
            "native field did not take focus"
        );
        proxy.sync(&mut h.state, Some(field));
        h.pump();
    }
    let id = h
        .state
        .prepare_dictation(std::time::Instant::now())
        .unwrap();
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "native field did not activate"
        );
        h.pump();
        proxy.sync(&mut h.state, Some(field));
        if h.state
            .advance_dictation_preparation(id, std::time::Instant::now())
            .unwrap()
        {
            break;
        }
    }
    for draft in drafts {
        h.state
            .update_dictation(id, draft.clone(), std::time::Instant::now())
            .unwrap();
        h.pump();
        assert!(
            !proxy
                .sync(&mut h.state, Some(field))
                .iter()
                .any(|event| matches!(
                    event,
                    winit::event::WindowEvent::Ime(winit::event::Ime::Commit(_))
                )),
            "a provisional engine hypothesis must never edit the field"
        );
    }
    h.state
        .update_dictation(id, final_text.into(), std::time::Instant::now())
        .unwrap();
    h.state
        .finish_dictation(id, std::time::Instant::now())
        .unwrap();
    let mut commits = Vec::new();
    while commits.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "native commit did not arrive"
        );
        h.pump();
        for event in proxy.sync(&mut h.state, Some(field)) {
            if let winit::event::WindowEvent::Ime(winit::event::Ime::Commit(text)) = event {
                commits.push(text);
            }
        }
    }
    assert_eq!(commits, [final_text]);
    assert!(
        h.state
            .check_dictation(id, std::time::Instant::now())
            .is_err()
    );
    proxy.sync(&mut h.state, None);
    h.pump();
    assert!(!proxy.holds_focus(&h.state));
    drop(proxy);
    drop(h);
    commits.into_iter().next().unwrap()
}

/// Use the event produced by the actual Wayland bridge, then Notes' owned
/// action dispatcher and system history, without a legacy direct-document edit.
fn assert_native_field_selection_and_undo(transcript: &str) {
    let mut fixture = NativeNotes::new();
    fixture
        .driver
        .invoke_action(
            &tilo_actions::ActionAddress::new("notes.notebook", "new-note"),
            serde_json::json!({"title": "Dictation"})
                .as_object()
                .unwrap(),
            false,
            None,
        )
        .unwrap();
    let page = fixture
        .records
        .read(&fixture.checkout.id, |doc| {
            notes::read::list_pages(doc).first().unwrap().id
        })
        .unwrap();
    fixture
        .driver
        .invoke_action(
            &tilo_actions::ActionAddress::new("notes.notebook", "open-note"),
            serde_json::json!({ "page": page }).as_object().unwrap(),
            false,
            None,
        )
        .unwrap();
    fixture
        .driver
        .perform_named("block_editor", &ui::a11y::Action::Focus)
        .unwrap();
    fixture.driver.commit_text("before old after ");
    fixture
        .driver
        .perform_named(
            "block_editor",
            &ui::a11y::Action::SetSelection {
                anchor: 7,
                focus: 10,
            },
        )
        .unwrap();
    let before = fixture
        .records
        .read_history(&fixture.checkout.id, |_, history| history.revision())
        .unwrap();
    let paragraph_commit = if transcript.contains('\n') {
        let field = fixture
            .driver
            .ime_window()
            .expect("actual Notes editing field");
        assert!(
            field.content.hints.multiline,
            "the original Notes field declares paragraph intent"
        );
        let committed =
            receive_native_dictation(&["provisional paragraph".into()], transcript, field);
        assert_eq!(committed, transcript);
        Some(committed)
    } else {
        None
    };
    let transcript = paragraph_commit.as_deref().unwrap_or(transcript);
    fixture.driver.commit_text(transcript);
    let expected = format!("before {transcript} after ");
    fixture.assert_text(page, &expected);
    fixture.driver.set_modifiers(&["ctrl".into()]);
    fixture.driver.key("z");
    fixture.driver.set_modifiers(&[]);
    fixture.assert_body(page, "before old after ");
    fixture
        .driver
        .set_modifiers(&["ctrl".into(), "shift".into()]);
    fixture.driver.key("z");
    fixture.driver.set_modifiers(&[]);
    fixture.assert_text(page, &expected);
    assert!(
        fixture
            .records
            .read_history(&fixture.checkout.id, |_, history| { history.revision() })
            .unwrap()
            > before,
        "dictation must enter owned persistent history"
    );
    fixture.driver.close_components();
    let reports = fixture.driver.take_component_close_reports();
    assert!(!reports.is_empty(), "the real Notes component must close");
    for report in reports {
        assert!(report.record_error.is_none());
        assert!(report.view_error.is_none());
        assert!(report.teardown_error.is_none());
    }
}

struct NativeNotes {
    driver: ui::headless::FrameHarness,
    records: ui::record::RecordStore,
    checkout: ui::record::RecordCheckout,
    _owners: ui::record::RecordOwners,
    _temp: tempfile::TempDir,
}
impl NativeNotes {
    fn new() -> Self {
        use std::{cell::RefCell, rc::Rc};
        use ui::application::{HostContext, HostContextInputs, LaunchTarget, RestoreInput};
        use ui::permissions::{PermissionContext, Role, UserId};
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().join("state");
        let mut owners = ui::record::RecordOwners::new(state_root.clone());
        let records = owners.open("so.tilo.notes", "dictation-wire").unwrap();
        let target = LaunchTarget::New;
        let context = HostContext::prepare(
            notes::definition(),
            HostContextInputs {
                records: records.clone(),
                views: Rc::new(
                    ui::application_view::InstanceViewStore::new(
                        state_root,
                        "fixture-device",
                        "dictation-wire",
                    )
                    .unwrap(),
                ),
                device_id: "fixture-device".into(),
                instance_id: 11,
                permissions: Rc::new(RefCell::new(PermissionContext::member(
                    "dictation-wire-author",
                    Role::Owner,
                ))),
                author: UserId("dictation-wire-author".into()),
                action_context: tilo_actions::ActionContext::default(),
            },
            &target,
        )
        .unwrap();
        let component = notes::definition()
            .instantiate(context, target, RestoreInput::None)
            .unwrap();
        let checkout = component.record().clone();
        let fonts = ui::font_catalog::FontCatalog::controlled_fixture(
            ui::font_catalog::checkout_root(),
            "en-US",
        );
        let mut driver = ui::headless::FrameHarness::from_component(
            component,
            (1000, 700),
            ui::application::ComponentRunnerInputs {
                theme: ui::Theme::dark(),
                text_renderer: ui::TextRenderer::with_font_catalog(&fonts).unwrap(),
            },
        )
        .unwrap();
        driver.set_platform_conventions(ui::PlatformConventions::new(ui::Platform::Linux));
        Self {
            driver,
            records,
            checkout,
            _owners: owners,
            _temp: temp,
        }
    }
    fn assert_text(&self, page: notes::PageId, expected: &str) {
        if !expected.contains('\n') {
            self.assert_body(page, expected);
            return;
        }
        self.records
            .read(&self.checkout.id, |doc| {
                notes::read::validate(doc).unwrap();
                let blocks = notes::read::page_block_snapshots(doc, page);
                let actual = blocks
                    .iter()
                    .map(|(_, body)| body.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(actual, expected.split('\n').collect::<Vec<_>>());
            })
            .unwrap();
    }

    fn assert_body(&self, page: notes::PageId, expected: &str) {
        self.records
            .read(&self.checkout.id, |doc| {
                notes::read::validate(doc).unwrap();
                let blocks = notes::read::page_block_snapshots(doc, page);
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks.first().unwrap().1, expected);
            })
            .unwrap();
    }
}

fn prepare_native_field_dictation(
    h: &mut Harness,
    proxy: &mut shell::compositor::shell_text_input::ShellTextInput,
    field: shell::compositor::shell_text_input::FieldRequest,
    identity: Option<shell::compositor::shell_text_input::NativeFieldId>,
    deadline: std::time::Instant,
) -> shell::compositor::dictation::SessionId {
    let id = h
        .state
        .prepare_dictation(std::time::Instant::now())
        .unwrap();
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "native field did not activate"
        );
        h.pump();
        proxy.sync_for_field(&mut h.state, Some(field), identity);
        if h.state
            .advance_dictation_preparation(id, std::time::Instant::now())
            .unwrap()
        {
            return id;
        }
    }
}

#[test]
fn dictation_cancels_when_native_field_identity_changes_at_identical_geometry() {
    use shell::compositor::shell_text_input::{FieldRequest, NativeFieldId, ShellTextInput};
    let mut h = Harness::new();
    let mut proxy = ShellTextInput::connect(&mut h.state).expect("native chrome bridge");
    let field = FieldRequest {
        content: ui::ime::InputContent::TEXT,
        x: 420,
        y: 100,
        width: 1,
        height: 24,
    };
    let first = Some(NativeFieldId::Widget(1));
    let second = Some(NativeFieldId::Widget(2));
    let deadline = tilo_test_env::hang_deadline();
    while !proxy.holds_focus(&h.state) {
        assert!(
            std::time::Instant::now() < deadline,
            "field did not take focus"
        );
        proxy.sync_for_field(&mut h.state, Some(field), first);
        h.pump();
    }
    let stale = prepare_native_field_dictation(&mut h, &mut proxy, field, first, deadline);
    h.state
        .update_dictation(stale, "must not move".into(), std::time::Instant::now())
        .unwrap();
    let mut events = proxy.sync_for_field(&mut h.state, Some(field), second);
    assert!(
        h.state
            .finish_dictation(stale, std::time::Instant::now())
            .is_err()
    );
    assert!(
        h.state
            .update_dictation(stale, "late".into(), std::time::Instant::now())
            .is_err()
    );
    h.pump();
    events.extend(proxy.sync_for_field(&mut h.state, Some(field), second));
    assert!(!events.iter().any(|event| matches!(
        event,
        winit::event::WindowEvent::Ime(winit::event::Ime::Commit(_))
    )));
    let next = prepare_native_field_dictation(&mut h, &mut proxy, field, second, deadline);
    assert_ne!(next, stale);
    h.state
        .update_dictation(next, "successor".into(), std::time::Instant::now())
        .unwrap();
    h.state
        .finish_dictation(next, std::time::Instant::now())
        .unwrap();
    let mut commits = Vec::new();
    while commits.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "successor commit did not arrive"
        );
        h.pump();
        for event in proxy.sync_for_field(&mut h.state, Some(field), second) {
            if let winit::event::WindowEvent::Ime(winit::event::Ime::Commit(text)) = event {
                commits.push(text);
            }
        }
    }
    assert_eq!(commits, ["successor"]);
    drop(proxy);
    drop(h);
}
