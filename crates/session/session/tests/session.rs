//! The native session on the headless backend with a real Wayland client:
//! its window becomes a tab of the desk's strip, shown where the strip lays
//! that tab out; the chrome above the windows draws the top bar; closing
//! the tab asks the client to close.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::time::Duration;

use tilo_compositor::{Compositor, OutputConfig};
use tilo_desktop_model::TileContent;
use tilo_session::{Options, Session};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_keyboard,
    wl_keyboard::WlKeyboard, wl_pointer, wl_pointer::WlPointer, wl_registry,
    wl_registry::WlRegistry, wl_seat, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_surface::XdgSurface, xdg_toplevel, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

#[derive(Default)]
struct Seen {
    globals: Vec<String>,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    configured: bool,
    pointer_inside: bool,
    buttons: Vec<(u32, bool)>,
    keyboard_focused: bool,
    keys: Vec<(u32, bool)>,
    frames_done: u32,
    closed: bool,
    size: Option<(i32, i32)>,
}

/// One client with one toplevel of a solid colour.
struct TestClient {
    connection: Connection,
    queue: EventQueue<Seen>,
    seen: Seen,
    surface: Option<WlSurface>,
    toplevel: Option<XdgToplevel>,
}

impl TestClient {
    fn connect(compositor: &mut Compositor) -> Self {
        let socket = compositor.connect_client().expect("client socket");
        let connection = Connection::from_socket(socket).expect("connection");
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        Self {
            connection,
            queue,
            seen: Seen::default(),
            surface: None,
            toplevel: None,
        }
    }

    /// Send what is queued, read what arrived, without blocking.
    fn exchange(&mut self) {
        self.connection.flush().expect("flush");
        if let Some(guard) = self.connection.prepare_read() {
            // Nothing to read is fine: WouldBlock.
            let _ = guard.read();
        }
        self.queue
            .dispatch_pending(&mut self.seen)
            .expect("dispatch");
        self.connection.flush().expect("flush");
    }

    fn open_window(&mut self, app_id: &str) {
        let qh = self.queue.handle();
        let surface = self
            .seen
            .compositor
            .as_ref()
            .expect("wl_compositor")
            .create_surface(&qh, ());
        let xdg = self
            .seen
            .wm_base
            .as_ref()
            .expect("xdg_wm_base")
            .get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id(app_id.into());
        surface.commit();
        self.surface = Some(surface);
        self.toplevel = Some(toplevel);
    }

    /// Attach a buffer of one colour (RGBA) and commit it with a frame
    /// callback.
    fn show(&self, width: i32, height: i32, rgba: [u8; 4]) {
        let qh = self.queue.handle();
        let stride = width * 4;
        let len = usize::try_from(stride * height).unwrap();
        let fd = rustix::fs::memfd_create("tilo-compositor-test", rustix::fs::MemfdFlags::CLOEXEC)
            .unwrap();
        let mut file = std::fs::File::from(fd);
        // ARGB8888 is B, G, R, A in memory.
        let pixel = [rgba[2], rgba[1], rgba[0], rgba[3]];
        file.write_all(&pixel.repeat(len / 4)).unwrap();
        let pool = self.seen.shm.as_ref().expect("wl_shm").create_pool(
            file.as_fd(),
            stride * height,
            &qh,
            (),
        );
        let buffer =
            pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
        let surface = self.surface.as_ref().expect("surface");
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.frame(&qh, ());
        surface.commit();
        pool.destroy();
    }
}

/// Let the session and clients talk until `done` holds.
fn pump(
    session: &mut Session,
    clients: &mut [&mut TestClient],
    mut done: impl FnMut(&mut Session, &[&mut TestClient]) -> bool,
) {
    for _ in 0..500 {
        for client in clients.iter_mut() {
            client.exchange();
        }
        session.step(Duration::from_millis(2)).expect("step");
        for client in clients.iter_mut() {
            client.exchange();
        }
        if done(session, clients) {
            return;
        }
    }
    panic!("condition not reached");
}

fn session() -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// Connect a client and open a toplevel of `app_id`, drawn at the size it
/// was configured to, and wait until it is mapped.
fn mapped(session: &mut Session, app_id: &str) -> TestClient {
    let mut client = TestClient::connect(session.compositor());
    pump(session, &mut [&mut client], |_, clients| {
        clients[0].seen.wm_base.is_some() && clients[0].seen.seat.is_some()
    });
    client.open_window(app_id);
    pump(session, &mut [&mut client], |_, clients| {
        clients[0].seen.configured
    });
    let (w, h) = client
        .seen
        .size
        .filter(|(w, h)| *w > 0 && *h > 0)
        .unwrap_or((300, 200));
    client.show(w, h, [200, 40, 40, 255]);
    let app = app_id.to_owned();
    pump(session, &mut [&mut client], |session, _| {
        window(session.compositor(), &app).is_some()
    });
    client
}

/// The window of `app` and its rectangle `(x, y, w, h)`.
fn window(compositor: &Compositor, app: &str) -> Option<(u64, (i32, i32, i32, i32))> {
    compositor
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some(app))
        .map(|(info, rect)| (info.id, rect))
}

/// The tab of the desk's tree showing window `id`.
fn tab_of(session: &Session, id: u64) -> Option<tilo_desktop_model::TabId> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|tab| matches!(tab.tile, TileContent::Foreign { surface_id, .. } if surface_id == id))
            .map(|tab| tab.id)
    })
}

#[test]
fn a_window_becomes_a_tab_of_the_strip_and_shows_where_the_strip_lays_it_out() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let _client = mapped(&mut session, "org.example.Terminal");
    let (id, rect) = window(session.compositor(), "org.example.Terminal").unwrap();
    assert!(
        tab_of(&session, id).is_some(),
        "the window has a tab in the desk's tree"
    );
    let layout = session.desktop().with_policy(|policy| policy.layout());
    let (_, shown) = layout
        .shown
        .iter()
        .find(|(window, _)| *window == id)
        .expect("the window's tab is shown");
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a window's place on a screen"
    )]
    let at = (shown.x0.round() as i32, shown.y0.round() as i32);
    assert_eq!(
        (rect.0, rect.1),
        at,
        "the window sits where the strip laid its tab out"
    );
    assert!(
        rect.1 >= tilo_desktop::topbar::TOPBAR_HEIGHT_PX as i32,
        "under the top bar: {rect:?}"
    );
}

#[test]
fn the_top_bar_draws_above_the_desk() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    session.step(Duration::from_millis(1)).unwrap();
    let names = session.chrome_names();
    assert!(!names.is_empty(), "the chrome names what it shows");
    let frame = session.compositor().screenshot("HEADLESS-1").unwrap();
    let band = (0..frame.width)
        .step_by(4)
        .filter_map(|x| frame.pixel(x, 12))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(band.len() > 2, "the bar draws in its band: {names:?}");
}

#[test]
fn closing_a_windows_tab_asks_its_client_to_close() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let mut client = mapped(&mut session, "org.example.Editor");
    let (id, _) = window(session.compositor(), "org.example.Editor").unwrap();
    let tab = tab_of(&session, id).unwrap();
    // As the window bar's close does: the tab goes from the desk's tree.
    let mut tree = session.handle().read(Clone::clone);
    assert!(tree.close_tab(tab).is_some());
    session.handle().replace_tree(tree);
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.closed
    });
}

impl Dispatch<WlRegistry, ()> for Seen {
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
            seen.globals.push(interface.clone());
            match interface.as_str() {
                "wl_compositor" => {
                    seen.compositor = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "wl_shm" => seen.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => seen.wm_base = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => seen.seat = Some(registry.bind(name, version.min(7), qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Seen {
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

impl Dispatch<XdgSurface, ()> for Seen {
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
            seen.configured = true;
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Close => seen.closed = true,
            xdg_toplevel::Event::Configure { width, height, .. } => {
                seen.size = Some((width, height));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for Seen {
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
            if capabilities.contains(wl_seat::Capability::Pointer) && seen.pointer.is_none() {
                seen.pointer = Some(seat.get_pointer(qh, ()));
            }
            if capabilities.contains(wl_seat::Capability::Keyboard) && seen.keyboard.is_none() {
                seen.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { .. } => seen.pointer_inside = true,
            wl_pointer::Event::Leave { .. } => seen.pointer_inside = false,
            wl_pointer::Event::Button { button, state, .. } => {
                seen.buttons.push((
                    button,
                    state == WEnum::Value(wl_pointer::ButtonState::Pressed),
                ));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Seen {
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
                seen.keys
                    .push((key, state == WEnum::Value(wl_keyboard::KeyState::Pressed)));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlCallback,
        _: wayland_client::protocol::wl_callback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        seen.frames_done += 1;
    }
}

delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore WlShm);
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore WlSurface);
