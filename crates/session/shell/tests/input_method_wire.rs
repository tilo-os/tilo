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

#[test]
fn second_input_method_resource_retires_the_trusted_connection() {
    let mut h = Harness::new();
    h.state
        .set_input_method_owner(Some(TrustedInputMethod::Composer));
    let composer = h.connect(Some(TrustedInputMethod::Composer));
    let (manager, seat) = {
        let client = &h.clients[composer];
        (
            client
                .bind::<zwp_input_method_manager_v2::ZwpInputMethodManagerV2>(
                    INPUT_METHOD_MANAGER,
                    1,
                )
                .expect("trusted composer sees the manager"),
            client.bind::<wl_seat::WlSeat>("wl_seat", 9).expect("seat"),
        )
    };
    let qh = h.clients[composer].queue.handle();
    let _first = manager.get_input_method(&seat, &qh, ());
    h.pump();
    assert!(
        h.display
            .handle()
            .backend_handle()
            .get_client_data(h.clients[composer].server.id())
            .is_ok(),
        "one admitted resource remains connected"
    );
    let _unavailable_second = manager.get_input_method(&seat, &qh, ());
    h.pump();
    assert!(
        h.display
            .handle()
            .backend_handle()
            .get_client_data(h.clients[composer].server.id())
            .is_err(),
        "an unavailable second resource cannot mutate observed preedit state"
    );
    assert!(!h.state.input_method_preedit_active_for_tests());
}

#[test]
fn destroying_the_manager_keeps_its_live_input_method_connected() {
    let mut h = Harness::new();
    h.state
        .set_input_method_owner(Some(TrustedInputMethod::Composer));
    let composer = h.connect(Some(TrustedInputMethod::Composer));
    let (manager, seat) = {
        let client = &h.clients[composer];
        (
            client
                .bind::<zwp_input_method_manager_v2::ZwpInputMethodManagerV2>(
                    INPUT_METHOD_MANAGER,
                    1,
                )
                .expect("trusted composer sees the manager"),
            client.bind::<wl_seat::WlSeat>("wl_seat", 9).expect("seat"),
        )
    };
    let qh = h.clients[composer].queue.handle();
    let _input_method = manager.get_input_method(&seat, &qh, ());
    h.pump();

    manager.destroy();
    h.pump();

    assert!(
        h.display
            .handle()
            .backend_handle()
            .get_client_data(h.clients[composer].server.id())
            .is_ok(),
        "destroying the manager is not a second input-method resource"
    );
    let replacement_manager = h.clients[composer]
        .bind::<zwp_input_method_manager_v2::ZwpInputMethodManagerV2>(INPUT_METHOD_MANAGER, 1)
        .expect("manager global remains available to the trusted connection");
    let _unavailable_second =
        replacement_manager.get_input_method(&seat, &h.clients[composer].queue.handle(), ());
    h.pump();
    assert!(
        h.display
            .handle()
            .backend_handle()
            .get_client_data(h.clients[composer].server.id())
            .is_err(),
        "the first input method remained tracked after its manager was destroyed"
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
    let observed = h.state.seat_focus_json();
    assert_eq!(observed["input_method_connected"].as_bool(), Some(true));
    assert_eq!(
        observed["input_method_keyboard_grabbed"].as_bool(),
        Some(true)
    );
    assert_eq!(
        observed["input_method_preedit_active"].as_bool(),
        Some(false)
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

    // A second native window of the same app may eagerly enable its text
    // input before it receives the seat. Smithay rejects that resource while
    // the primary resource is active; it must not advance the compositor's
    // accepted input-method serial or poison the genuine composer commit.
    let unfocused_text_input = {
        let client = &h.clients[app];
        let qh = client.queue.handle();
        let seat = client.bind::<wl_seat::WlSeat>("wl_seat", 9).expect("seat");
        client
            .bind::<zwp_text_input_manager_v3::ZwpTextInputManagerV3>(
                "zwp_text_input_manager_v3",
                1,
            )
            .expect("text-input manager")
            .get_text_input(&seat, &qh, ())
    };
    let serial = h.clients[im_client].peer.im_done;
    unfocused_text_input.enable();
    unfocused_text_input.commit();
    h.pump();
    assert_eq!(
        h.clients[im_client].peer.im_done, serial,
        "an unfocused sibling text input cannot advance the input-method serial"
    );

    // A preedit paired with a stale serial reaches Smithay's discard path,
    // but must not become compositor authority for a floating handoff.
    input_method.set_preedit_string("stale".into(), 5, 5);
    input_method.commit(serial.saturating_add(1));
    h.pump();
    assert!(
        !h.state.input_method_preedit_active_for_tests(),
        "an invalid input-method commit serial cannot authorize continuation"
    );

    // Preedit, then the composed text.
    input_method.set_preedit_string("ni".into(), 2, 2);
    h.pump();
    assert!(
        !h.state.input_method_preedit_active_for_tests(),
        "staging a new preedit cannot create authority before its valid commit"
    );
    input_method.commit(serial);
    h.pump();
    assert_eq!(h.clients[app].peer.preedit.last(), Some(&Some("ni".into())));
    assert!(h.state.input_method_preedit_active_for_tests());
    assert_eq!(
        h.state.seat_focus_json()["input_method_preedit_active"].as_bool(),
        Some(true),
        "the closed seat observation follows the accepted preedit"
    );
    let serial = h.clients[im_client].peer.im_done;
    // Requests are double-buffered. A clear can arrive in a different
    // dispatch from its Commit, while a floating handoff is reconciled.
    // The already accepted composition owns that interval until a valid
    // transaction commits the clear; a stale serial cannot end it either.
    let done_before_clear = h.clients[app].peer.text_input_done;
    input_method.set_preedit_string(String::new(), 0, 0);
    h.pump();
    assert!(
        h.state.input_method_preedit_active_for_tests(),
        "a staged clear must preserve the previously accepted composition"
    );
    assert_eq!(h.clients[app].peer.text_input_done, done_before_clear);
    input_method.commit(serial.saturating_add(1));
    h.pump();
    assert!(
        h.state.input_method_preedit_active_for_tests(),
        "a stale clear transaction cannot retire accepted preedit authority"
    );
    input_method.set_preedit_string(String::new(), 0, 0);
    h.pump();
    assert!(h.state.input_method_preedit_active_for_tests());
    input_method.commit_string("你".into());
    input_method.commit(serial);
    h.pump();
    assert_eq!(h.clients[app].peer.committed, [Some("你".to_string())]);
    assert!(!h.state.input_method_preedit_active_for_tests());
    assert_eq!(
        h.state.seat_focus_json()["input_method_preedit_active"].as_bool(),
        Some(false),
        "committing clears the closed preedit observation"
    );
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

    // A current serial alone is not an active text-input context. Once the
    // field disables, a preedit cannot authorize floating continuation.
    text_input.disable();
    text_input.commit();
    h.pump();
    let serial = h.clients[im_client].peer.im_done;
    input_method.set_preedit_string("inactive".into(), 8, 8);
    input_method.commit(serial);
    h.pump();
    assert!(
        !h.state.input_method_preedit_active_for_tests(),
        "a valid serial without an active text input is not accepted"
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
        purpose: winit::window::ImePurpose::Normal,
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

#[test]
fn a_late_keyboard_activates_an_already_enabled_chrome_field() {
    let mut h = Harness::new();
    h.state
        .set_input_method_owner(Some(TrustedInputMethod::OnScreenKeyboard));
    let mut proxy = shell::compositor::shell_text_input::ShellTextInput::connect(&mut h.state)
        .expect("chrome stand-in");
    let field = shell::compositor::shell_text_input::FieldRequest {
        purpose: winit::window::ImePurpose::Normal,
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
    assert!(
        !h.state.text_input_active(),
        "the early enable/commit cannot activate before an input method exists"
    );
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
    // Binding the input method must reannounce the already-focused field.
    // The original enable/commit arrived before Smithay had an input-method
    // instance and was therefore discarded; requiring an unrelated surface
    // promotion here would leave a real editor inactive indefinitely.
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
    use shell::compositor::shell_text_input::{FieldRequest, ShellTextInput};
    let mut h = Harness::new();
    let mut proxy = ShellTextInput::connect(&mut h.state).expect("native chrome bridge");
    let field = FieldRequest {
        purpose: winit::window::ImePurpose::Normal,
        x: 420,
        y: 100,
        width: 1,
        height: 24,
    };
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
    h.state
        .update_dictation(id, "rough".into(), std::time::Instant::now())
        .unwrap();
    h.pump();
    assert!(
        !proxy
            .sync(&mut h.state, Some(field))
            .iter()
            .any(|event| matches!(
                event,
                winit::event::WindowEvent::Ime(winit::event::Ime::Commit(_))
            ))
    );
    h.state
        .update_dictation(id, "東京 👋".into(), std::time::Instant::now())
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
    assert_eq!(commits, ["東京 👋"]);
    assert_native_field_selection_and_undo(commits.first().unwrap());
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
}

/// Use the event produced by the actual Wayland bridge, then Notes' owned
/// action dispatcher and system history, without a legacy direct-document edit.
fn assert_native_field_selection_and_undo(transcript: &str) {
    let mut fixture = NativeNotes::new();
    fixture
        .driver
        .invoke_action(
            &tilo_actions::ActionAddress::new("notes.notebook", "new-page"),
            serde_json::json!({"value": "Dictation"})
                .as_object()
                .unwrap(),
            false,
            None,
        )
        .unwrap();
    let page = fixture
        .records
        .read(&fixture.checkout.id, |doc| {
            notes::doc::NotesDoc::list_pages(doc).first().unwrap().id
        })
        .unwrap();
    fixture
        .driver
        .invoke_action(
            &tilo_actions::ActionAddress::new(notes::actions::page_resource_id(page), "open"),
            &serde_json::Map::new(),
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
    fixture.driver.commit_text(transcript);
    let expected = format!("before {transcript} after ");
    fixture.assert_body(page, &expected);
    fixture.driver.set_modifiers(&["ctrl".into()]);
    fixture.driver.key("z");
    fixture.driver.set_modifiers(&[]);
    fixture.assert_body(page, "before old after ");
    fixture
        .driver
        .set_modifiers(&["ctrl".into(), "shift".into()]);
    fixture.driver.key("z");
    fixture.driver.set_modifiers(&[]);
    fixture.assert_body(page, &expected);
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
    driver: ui::headless::Driver,
    records: ui::record::RecordStore,
    checkout: ui::record::RecordCheckout,
    _owners: ui::record::RecordOwners,
    _temp: tempfile::TempDir,
}
impl NativeNotes {
    fn new() -> Self {
        use std::{cell::RefCell, rc::Rc};
        use ui::application::{HostContext, HostContextInputs, LaunchTarget, RestoreInput};
        use ui::permissions::{Actor, PermissionContext, Role, UserId};
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
                permissions: Rc::new(RefCell::new(PermissionContext::from_capabilities(
                    Actor::person(Some(Role::Owner), "dictation-wire-author"),
                    vec![],
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
        let mut driver = ui::headless::Driver::from_component(
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
    fn assert_body(&self, page: notes::PageId, expected: &str) {
        self.records
            .read(&self.checkout.id, |doc| {
                notes::notebook::validate(doc).unwrap();
                let blocks = notes::doc::NotesDoc::page_block_snapshots(doc, page);
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks.first().unwrap().1, expected);
            })
            .unwrap();
    }
}

#[test]
fn dictation_cancels_when_native_field_identity_changes_at_identical_geometry() {
    use shell::compositor::shell_text_input::{FieldRequest, NativeFieldId, ShellTextInput};
    let mut h = Harness::new();
    let mut proxy = ShellTextInput::connect(&mut h.state).expect("native chrome bridge");
    let field = FieldRequest {
        purpose: winit::window::ImePurpose::Normal,
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
    let stale = h
        .state
        .prepare_dictation(std::time::Instant::now())
        .unwrap();
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "field did not activate"
        );
        h.pump();
        proxy.sync_for_field(&mut h.state, Some(field), first);
        if h.state
            .advance_dictation_preparation(stale, std::time::Instant::now())
            .unwrap()
        {
            break;
        }
    }
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
    let next = h
        .state
        .prepare_dictation(std::time::Instant::now())
        .unwrap();
    assert_ne!(next, stale);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "successor did not activate"
        );
        h.pump();
        proxy.sync_for_field(&mut h.state, Some(field), second);
        if h.state
            .advance_dictation_preparation(next, std::time::Instant::now())
            .unwrap()
        {
            break;
        }
    }
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
