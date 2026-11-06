//! The sheet takes the person's input, whatever window is focused: its field
//! is typed into rather than the window behind it, Escape and a click outside
//! close it, a click inside reaches its own controls, and a sheet the session
//! closed stops being drawn.

use std::time::Duration;

use test_http::Reply;
use tilo_compositor::InputEvent;
use tilo_session::Session;
use tilo_sheets::Seat as _;
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_keyboard, wl_keyboard::WlKeyboard,
    wl_pointer, wl_pointer::WlPointer, wl_registry, wl_registry::WlRegistry, wl_seat,
    wl_seat::WlSeat, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_surface::XdgSurface, xdg_toplevel, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};

use crate::{
    CLUB_WORKSPACE, ask_to_join, club_entry, common, community_entry, community_site,
    invitation_for, joining_site, now_ms, open_directory, page_of, session, site, this_computer,
};

/// What one toplevel of a test client was told.
#[derive(Default)]
struct Toplevel {
    configured: bool,
    size: Option<(i32, i32)>,
}

#[derive(Default)]
struct ClientSeen {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    seat: Option<WlSeat>,
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    toplevel: Toplevel,
    /// Whether the keyboard is on the window.
    keyboard_focused: bool,
    /// Every key the window saw, as `(evdev code, pressed)`.
    keys: Vec<(u32, bool)>,
    /// Every pointer button the window saw, in evdev codes.
    buttons: Vec<u32>,
}

/// A window of an app on the session's desk: a Wayland client of this test
/// process's own, mapped where the desk places it. Kept up while it lives.
struct TestClient {
    connection: Connection,
    queue: EventQueue<ClientSeen>,
    seen: ClientSeen,
    _surface: WlSurface,
    _toplevel: XdgToplevel,
    _buffer: WlBuffer,
}

impl TestClient {
    /// Connect to `session`'s compositor, map a window of `app`, and wait
    /// until it has the keyboard.
    fn open(session: &mut Session, app: &str) -> Self {
        let socket = session
            .compositor()
            .connect_client()
            .expect("client socket");
        let connection = Connection::from_socket(socket).expect("connection");
        let mut queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        let mut seen = ClientSeen::default();
        let qh = queue.handle();
        // The globals.
        common::until(session, "the client's globals", |session| {
            exchange(&connection, &mut queue, &mut seen);
            session.step(Duration::from_millis(2)).expect("step");
            exchange(&connection, &mut queue, &mut seen);
            seen.compositor.is_some() && seen.wm_base.is_some() && seen.shm.is_some()
        });
        let surface = seen
            .compositor
            .as_ref()
            .expect("wl_compositor")
            .create_surface(&qh, ());
        let xdg = seen
            .wm_base
            .as_ref()
            .expect("xdg_wm_base")
            .get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id(app.to_owned());
        surface.commit();
        common::until(session, "the window's configuration", |session| {
            exchange(&connection, &mut queue, &mut seen);
            session.step(Duration::from_millis(2)).expect("step");
            exchange(&connection, &mut queue, &mut seen);
            seen.toplevel.configured
        });
        let (w, h) = seen
            .toplevel
            .size
            .filter(|(w, h)| *w > 0 && *h > 0)
            .unwrap_or((640, 400));
        let buffer = paint_window(&connection, &queue, &seen, &surface, w, h);
        let app = app.to_owned();
        common::until(session, "the window on the desk", |session| {
            exchange(&connection, &mut queue, &mut seen);
            session.step(Duration::from_millis(2)).expect("step");
            exchange(&connection, &mut queue, &mut seen);
            session
                .compositor()
                .windows()
                .iter()
                .any(|(info, _)| info.app_id.as_deref() == Some(app.as_str()))
        });
        common::until(session, "the window's keyboard focus", |session| {
            exchange(&connection, &mut queue, &mut seen);
            session.step(Duration::from_millis(2)).expect("step");
            exchange(&connection, &mut queue, &mut seen);
            seen.keyboard_focused
        });
        Self {
            connection,
            queue,
            seen,
            _surface: surface,
            _toplevel: toplevel,
            _buffer: buffer,
        }
    }

    /// Take what the session sent, without waiting for more.
    fn exchange(&mut self) {
        exchange(&self.connection, &mut self.queue, &mut self.seen);
    }
}

/// One turn of the client: what it queued out, what the session sent back in.
fn exchange(connection: &Connection, queue: &mut EventQueue<ClientSeen>, seen: &mut ClientSeen) {
    let _ = connection.flush();
    if let Some(guard) = connection.prepare_read() {
        let _ = guard.read();
    }
    let _ = queue.dispatch_pending(seen);
    let _ = connection.flush();
}

/// Attach one colour's buffer to `surface` and commit it.
fn paint_window(
    connection: &Connection,
    queue: &EventQueue<ClientSeen>,
    seen: &ClientSeen,
    surface: &WlSurface,
    width: i32,
    height: i32,
) -> WlBuffer {
    use std::io::Write as _;
    use std::os::fd::AsFd as _;

    let qh = queue.handle();
    let stride = width.checked_mul(4).expect("a window's stride");
    let size = stride.checked_mul(height).expect("a window's size");
    let len = usize::try_from(size).expect("a window's size");
    let fd = rustix::fs::memfd_create("directory-input-test", rustix::fs::MemfdFlags::CLOEXEC)
        .expect("a buffer's memory");
    let mut file = std::fs::File::from(fd);
    // ARGB8888 is B, G, R, A in memory.
    file.write_all(&[0x40u8, 0x60, 0x80, 0xff].repeat(len / 4))
        .expect("the window's contents");
    let pool = seen
        .shm
        .as_ref()
        .expect("the session's shared memory")
        .create_pool(file.as_fd(), size, &qh, ());
    let buffer = pool.create_buffer(
        0,
        width,
        height,
        stride,
        wayland_client::protocol::wl_shm::Format::Argb8888,
        &qh,
        (),
    );
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, width, height);
    surface.commit();
    pool.destroy();
    let _ = connection.flush();
    buffer
}

impl Dispatch<WlRegistry, ()> for ClientSeen {
    fn event(
        seen: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    seen.compositor = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "wl_shm" => seen.shm = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => seen.seat = Some(registry.bind(name, version.min(7), qh, ())),
                "xdg_wm_base" => seen.wm_base = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<XdgWmBase, ()> for ClientSeen {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
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

impl Dispatch<XdgSurface, ()> for ClientSeen {
    fn event(
        seen: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            seen.toplevel.configured = true;
        }
    }
}

impl Dispatch<XdgToplevel, ()> for ClientSeen {
    fn event(
        seen: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            seen.toplevel.size = Some((width, height));
        }
    }
}

impl Dispatch<WlSeat, ()> for ClientSeen {
    fn event(
        seen: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Keyboard) && seen.keyboard.is_none() {
                seen.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            if capabilities.contains(wl_seat::Capability::Pointer) && seen.pointer.is_none() {
                seen.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for ClientSeen {
    fn event(
        seen: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { .. } => seen.keyboard_focused = true,
            wl_keyboard::Event::Leave { .. } => seen.keyboard_focused = false,
            wl_keyboard::Event::Key { key, state, .. } => {
                let pressed = state == WEnum::Value(wl_keyboard::KeyState::Pressed);
                seen.keys.push((key, pressed));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for ClientSeen {
    fn event(
        seen: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button {
            button,
            state: WEnum::Value(wl_pointer::ButtonState::Pressed),
            ..
        } = event
        {
            seen.buttons.push(button);
        }
    }
}

delegate_noop!(ClientSeen: ignore WlCompositor);
delegate_noop!(ClientSeen: ignore WlShm);
delegate_noop!(ClientSeen: ignore WlShmPool);
delegate_noop!(ClientSeen: ignore WlBuffer);
delegate_noop!(ClientSeen: ignore WlSurface);

/// What a key sends, evdev codes.
const KEY_ESCAPE: u32 = 1;
const BTN_LEFT: u32 = 0x110;

fn press_key(session: &mut Session, keycode: u32, pressed: bool) {
    session
        .compositor()
        .input(InputEvent::Key { keycode, pressed })
        .expect("the key");
    session.step(Duration::from_millis(1)).expect("step");
}

fn tap_key(session: &mut Session, keycode: u32) {
    press_key(session, keycode, true);
    press_key(session, keycode, false);
}

/// Press and release the left button at `(x, y)` of the display.
fn click_at(session: &mut Session, x: f64, y: f64) {
    session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .expect("the pointer");
    session.step(Duration::from_millis(1)).expect("step");
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: BTN_LEFT,
                pressed,
            })
            .expect("the press");
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// The session with the directory's sheet up, and a client window focused
/// behind it.
fn sheet_over_a_window() -> (Session, TestClient) {
    let mut env = tilo_test_env::ScopedEnv::new();
    let (responder, page) = site();
    let _config = community_site(&mut env, &responder.url());
    *page.lock().expect("the page") =
        page_of(vec![community_entry(34, now_ms()), club_entry(now_ms())]);
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    let client = TestClient::open(&mut session, "org.example.Terminal");
    open_directory(&mut session);
    (session, client)
}

/// With a client window focused behind it, typing reaches the sheet's search
/// field and never the window: the list narrows to what was typed.
#[test]
fn typing_reaches_the_sheets_field_and_not_the_window_behind_it() {
    let _settled = ui::motion::testing::settled();
    let (mut session, mut client) = sheet_over_a_window();
    let seen_before = client.seen.keys.clone();
    // "salon" is on no listing, so the list says nothing matches.
    for keycode in [31u32, 30, 38, 24, 49] {
        tap_key(&mut session, keycode);
        client.exchange();
    }
    common::until(
        &mut session,
        "the search field to hold the typing",
        |session| {
            client.exchange();
            session.step(Duration::from_millis(2)).expect("step");
            client.exchange();
            session
                .workspace_directory()
                .is_some_and(|seat| seat.query() == "salon")
        },
    );
    assert_eq!(
        client.seen.keys, seen_before,
        "the window behind the sheet saw none of what was typed"
    );
    assert!(
        session
            .accessible_labels()
            .iter()
            .any(|label| label.contains("salon")),
        "the sheet says what was typed finds nothing: {:?}",
        session.accessible_labels()
    );
}

/// Escape closes the sheet while a client window is focused behind it, and
/// the window never sees the key.
#[test]
fn escape_closes_the_sheet_and_never_reaches_the_window() {
    let _settled = ui::motion::testing::settled();
    let (mut session, mut client) = sheet_over_a_window();
    assert!(
        session
            .workspace_directory()
            .is_some_and(|seat| seat.presentation().is_open()),
        "the sheet is up to begin with"
    );
    let seen_before = client.seen.keys.clone();
    tap_key(&mut session, KEY_ESCAPE);
    common::until(&mut session, "the sheet to close on Escape", |session| {
        client.exchange();
        session.step(Duration::from_millis(2)).expect("step");
        client.exchange();
        session
            .workspace_directory()
            .is_some_and(|seat| !seat.presentation().is_open())
    });
    assert_eq!(
        client.seen.keys, seen_before,
        "the window behind the sheet saw none of the Escape"
    );
    assert!(client.seen.keyboard_focused, "the window kept the seat");
}

/// A press outside the sheet's card is the sheet's, not the window's behind
/// it: it closes the sheet and the window sees no button.
#[test]
fn a_click_outside_the_sheet_never_reaches_the_window() {
    let _settled = ui::motion::testing::settled();
    let (mut session, mut client) = sheet_over_a_window();
    let buttons_before = client.seen.buttons.clone();
    // The sheet covers the whole display; a press at its corner is beside
    // the card and closes the sheet.
    click_at(&mut session, 1.0, 1.0);
    common::until(
        &mut session,
        "the press beside the card to close the sheet",
        |session| {
            client.exchange();
            session.step(Duration::from_millis(2)).expect("step");
            client.exchange();
            session
                .workspace_directory()
                .is_some_and(|seat| !seat.presentation().is_open())
        },
    );
    assert_eq!(
        client.seen.buttons, buttons_before,
        "the window behind the sheet saw none of the press"
    );
}

/// A press inside the sheet's card reaches its controls: the search field
/// takes the caret, and typing after it narrows the list.
#[test]
fn a_click_inside_the_sheet_reaches_its_controls() {
    let _settled = ui::motion::testing::settled();
    let (mut session, mut client) = sheet_over_a_window();
    // The sheet publishes its field as the screen reader reads it: the
    // shared search field, named by the words it stands for.
    let field = common::wait_for(&mut session, "the sheet's search field", |session| {
        session
            .step(Duration::from_millis(16))
            .expect("a turn of the session");
        sheet_control(session, "Search public workspaces, or paste a link or code")
    });
    let buttons_before = client.seen.buttons.clone();
    click_at(
        &mut session,
        field.0 + field.2 / 2.0,
        field.1 + field.3 / 2.0,
    );
    tap_key(&mut session, 30);
    client.exchange();
    common::until(
        &mut session,
        "the field to take the typed letter",
        |session| {
            client.exchange();
            session.step(Duration::from_millis(2)).expect("step");
            client.exchange();
            session
                .workspace_directory()
                .is_some_and(|seat| seat.query() == "a")
        },
    );
    assert_eq!(
        client.seen.buttons, buttons_before,
        "the press inside the card was the sheet's"
    );
}

/// Where the sheet's control named `name` stands, as the session's own
/// export publishes it: the window the sheet's app is on, and the node it
/// names there, in logical pixels.
fn sheet_control(session: &Session, name: &str) -> Option<(f64, f64, f64, f64)> {
    for (_, _, tree) in session.a11y_windows() {
        let found = tree.nodes.iter().find_map(|(_, node)| {
            let node_name = node.label()?;
            (node_name == name).then(|| node.bounds()).flatten()
        });
        if let Some(bounds) = found {
            return Some((
                bounds.x0,
                bounds.y0,
                bounds.x1 - bounds.x0,
                bounds.y1 - bounds.y0,
            ));
        }
    }
    None
}

/// Choosing Join closes the sheet once its action has started, leaving the
/// person where they are rather than a sheet behind.
#[test]
fn choosing_join_closes_the_sheet() {
    let _settled = ui::motion::testing::settled();
    let mut env = tilo_test_env::ScopedEnv::new();
    let (_identity, person) = this_computer(&mut env);
    let responder = joining_site(
        CLUB_WORKSPACE,
        vec![community_entry(34, now_ms()), club_entry(now_ms())],
        move |_| {
            Reply::status(200)
                .header("content-type", "application/json")
                .body(invitation_for(CLUB_WORKSPACE, person))
        },
    );
    let _config = community_site(&mut env, &responder.url());
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    ask_to_join(&mut session, CLUB_WORKSPACE);
    common::until(
        &mut session,
        "the sheet to close once the join started",
        |session| {
            session.step(Duration::from_millis(2)).expect("step");
            session
                .workspace_directory()
                .is_some_and(|seat| !seat.presentation().is_drawn())
        },
    );
}

/// A sheet the session closed stops being drawn: nothing is left on screen
/// taking no input.
#[test]
fn a_sheet_the_session_closed_stops_being_drawn() {
    let _settled = ui::motion::testing::settled();
    let mut env = tilo_test_env::ScopedEnv::new();
    let _config = community_site(&mut env, "http://127.0.0.1:1/");
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    let seat = session
        .workspace_directory()
        .expect("the session hosts the workspace directory")
        .clone();
    seat.presentation().close();
    common::until(
        &mut session,
        "the closed sheet to stop being drawn",
        |session| {
            session.step(Duration::from_millis(16)).expect("step");
            !seat.presentation().is_drawn()
        },
    );
}
