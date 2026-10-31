//! Where a sheet sits: the display the person is working on, the first one
//! when that display is gone, and above a window that fills the display
//! without that window keeping the keyboard.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::time::Duration;

use tilo_actions::ResourceSpec;
use tilo_compositor::{Compositor, InputEvent, OutputConfig, Screenshot};
use tilo_desktop::desktop_actions::DISPLAY_KIND;
use tilo_session::{Options, Session};
use tilo_sheets::Seat as _;
use tilo_sheets::power::PowerSeat;
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_keyboard, wl_keyboard::WlKeyboard,
    wl_registry, wl_registry::WlRegistry, wl_seat, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_surface::XdgSurface, xdg_toplevel, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};

const WIDTH: i32 = 1000;
const HEIGHT: i32 = 700;

/// The colour the compositor paints every window of the test client with.
const WINDOW: [u8; 4] = [40, 40, 200, 255];
/// The same colour as the bytes of an `ARGB8888` buffer hold them.
const WINDOW_BYTES: [u8; 4] = [200, 40, 40, 255];

/// What a key sends, evdev codes.
const KEY_A: u32 = 30;
const KEY_ESCAPE: u32 = 1;

/// What one toplevel was told.
#[derive(Default)]
struct Toplevel {
    configured: bool,
    size: Option<(i32, i32)>,
}

#[derive(Default)]
struct Seen {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    seat: Option<WlSeat>,
    keyboard: Option<WlKeyboard>,
    toplevels: Vec<Toplevel>,
    /// Whether the keyboard is on one of this client's surfaces.
    keyboard_focused: bool,
    /// Every key the client was delivered, as the window sees it.
    keys: Vec<(u32, bool)>,
}

/// One client with toplevels of a solid colour.
struct TestClient {
    connection: Connection,
    queue: EventQueue<Seen>,
    seen: Seen,
    windows: Vec<(WlSurface, XdgToplevel)>,
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
            windows: Vec::new(),
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

    /// Open toplevel number `self.windows.len()` of `app_id`, set up by
    /// `before` before its first commit. Returns its index.
    fn open(&mut self, app_id: &str, before: impl FnOnce(&XdgToplevel)) -> usize {
        let index = self.windows.len();
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
            .get_xdg_surface(&surface, &qh, index);
        let toplevel = xdg.get_toplevel(&qh, index);
        toplevel.set_app_id(app_id.into());
        before(&toplevel);
        surface.commit();
        self.seen.toplevels.push(Toplevel::default());
        self.windows.push((surface, toplevel));
        index
    }

    /// Attach a buffer of one colour to toplevel `index` and commit it.
    fn show(&self, index: usize, width: i32, height: i32) {
        let qh = self.queue.handle();
        let stride = width * 4;
        let len = usize::try_from(stride * height).unwrap();
        let fd =
            rustix::fs::memfd_create("tilo-sheet-placement-test", rustix::fs::MemfdFlags::CLOEXEC)
                .unwrap();
        let mut file = std::fs::File::from(fd);
        file.write_all(&WINDOW_BYTES.repeat(len / 4)).unwrap();
        let pool = self.seen.shm.as_ref().expect("wl_shm").create_pool(
            file.as_fd(),
            stride * height,
            &qh,
            (),
        );
        let buffer =
            pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
        let surface = &self.windows[index].0;
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.commit();
        pool.destroy();
    }

    /// Press and release `keycode` on the client's keyboard, and let the
    /// session deliver it.
    fn tap(&mut self, session: &mut Session, keycode: u32) {
        for pressed in [true, false] {
            session
                .compositor()
                .input(InputEvent::Key { keycode, pressed })
                .expect("the key");
            self.exchange();
            session.step(Duration::from_millis(1)).expect("step");
            self.exchange();
        }
    }
}

/// Let the session and `client` talk until `done` holds.
#[track_caller]
fn pump(
    session: &mut Session,
    client: &mut TestClient,
    mut done: impl FnMut(&mut Session, &TestClient) -> bool,
) {
    for _ in 0..500 {
        client.exchange();
        session.step(Duration::from_millis(2)).expect("step");
        client.exchange();
        if done(session, client) {
            return;
        }
    }
    panic!("condition not reached");
}

/// A session on `outputs`, each `WIDTH` x `HEIGHT` at the place it names.
fn session_at(outputs: &[OutputConfig]) -> Session {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            ..Options::default()
        },
        outputs,
    )
    .expect("session");
    session.step(Duration::from_millis(1)).unwrap();
    session
}

/// The displays `HEADLESS-1` and further, `WIDTH` x `HEIGHT` at `places`.
fn displays_at(places: &[(i32, i32)]) -> Session {
    let outputs: Vec<OutputConfig> = places
        .iter()
        .enumerate()
        .map(|(index, place)| {
            let mut output = OutputConfig::new(format!("HEADLESS-{}", index + 1), WIDTH, HEIGHT);
            output.position = *place;
            output
        })
        .collect();
    session_at(&outputs)
}

/// Two displays side by side.
fn docked() -> Session {
    displays_at(&[(0, 0), (WIDTH, 0)])
}

fn client(session: &mut Session) -> TestClient {
    let mut client = TestClient::connect(session.compositor());
    pump(session, &mut client, |_, client| {
        client.seen.wm_base.is_some() && client.seen.shm.is_some() && client.seen.seat.is_some()
    });
    // The seat tells the client its capabilities on its first event.
    pump(session, &mut client, |_, client| {
        client.seen.keyboard.is_some()
    });
    client
}

/// Open toplevel of `app_id` (set up by `before`), drawn at the size it
/// was configured to, else `fallback`, and wait until it is mapped.
fn map(
    session: &mut Session,
    client: &mut TestClient,
    app_id: &str,
    fallback: (i32, i32),
    before: impl FnOnce(&XdgToplevel),
) -> usize {
    let index = client.open(app_id, before);
    pump(session, client, |_, client| {
        client.seen.toplevels[index].configured
    });
    let (w, h) = client.seen.toplevels[index]
        .size
        .filter(|(w, h)| *w > 0 && *h > 0)
        .unwrap_or(fallback);
    client.show(index, w, h);
    let app = app_id.to_owned();
    pump(session, client, |session, _| {
        window(session.compositor(), &app).is_some()
    });
    index
}

/// The window of `app` and its rectangle `(x, y, w, h)`.
fn window(compositor: &Compositor, app: &str) -> Option<(u64, (i32, i32, i32, i32))> {
    compositor
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some(app))
        .map(|(info, rect)| (info.id, rect))
}

fn rect(session: &mut Session, app: &str) -> (i32, i32, i32, i32) {
    window(session.compositor(), app).expect("mapped").1
}

/// The displays the desk publishes, in reading order.
fn displays(session: &Session) -> Vec<ResourceSpec> {
    fn find(resources: &[ResourceSpec], hit: &dyn Fn(&ResourceSpec) -> bool) -> Vec<ResourceSpec> {
        resources
            .iter()
            .flat_map(|resource| {
                let mut found = find(&resource.children, hit);
                if hit(resource) {
                    found.insert(0, resource.clone());
                }
                found
            })
            .collect()
    }
    find(&session.resources(), &|resource| {
        resource.kind == DISPLAY_KIND
    })
}

fn focuses(session: &mut Session, client: &mut TestClient, index: usize) {
    let display = displays(session)[index].id.clone();
    session
        .invoke(display.as_str(), "focus", &serde_json::Map::new())
        .unwrap_or_else(|error| panic!("{display:?} focus: {error:?}"));
    for _ in 0..4 {
        client.exchange();
        session.step(Duration::from_millis(1)).unwrap();
    }
}

/// A frame of `output`, once the session has drawn what it has.
fn shot(session: &mut Session, output: &str) -> Screenshot {
    session
        .compositor()
        .screenshot(output)
        .unwrap_or_else(|error| panic!("a frame of {output}: {error}"))
}

fn pixel(frame: &Screenshot, x: i32, y: i32) -> [u8; 4] {
    let x = u32::try_from(x).expect("on the output");
    let y = u32::try_from(y).expect("on the output");
    frame.pixel(x, y).expect("a pixel of the frame")
}

/// The point at the middle of `power`'s panel, in its own coordinates.
fn panel_centre(power: &PowerSeat) -> (i32, i32) {
    let panel = power.panel();
    assert!(
        panel.x1 > panel.x0 && panel.y1 > panel.y0,
        "the menu drew a panel: {:?}",
        (panel.x0, panel.y0, panel.x1, panel.y1)
    );
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a point on a display, well inside its bounds"
    )]
    let centre = (
        f64::midpoint(panel.x0, panel.x1).round() as i32,
        f64::midpoint(panel.y0, panel.y1).round() as i32,
    );
    centre
}

/// The rectangle of the display `HEADLESS-2`, the right one of two.
fn right() -> (i32, i32, u32, u32) {
    (
        WIDTH,
        0,
        u32::try_from(WIDTH).unwrap(),
        u32::try_from(HEIGHT).unwrap(),
    )
}

/// Where every sheet puts itself: on the display the person is working on.
#[test]
fn a_sheet_opens_on_the_display_in_focus() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    assert_eq!(displays(&session).len(), 2, "the desk tells both apart");
    focuses(&mut session, &mut client, 1);

    let before_left = shot(&mut session, "HEADLESS-1");
    let before_right = shot(&mut session, "HEADLESS-2");

    let power = session.power_menu().expect("power sheet");
    power.presentation().open(false);
    pump(&mut session, &mut client, |session, _| {
        session
            .power_menu()
            .expect("power sheet")
            .presentation()
            .place()
            == Some(right())
    });
    let power = session.power_menu().expect("power sheet");
    assert!(
        power.presentation().is_open(),
        "the menu is up for the person"
    );
    assert_eq!(
        power.presentation().place(),
        Some(right()),
        "the sheet covers the display in focus"
    );
    let (cx, cy) = panel_centre(power);
    for _ in 0..4 {
        client.exchange();
        session.step(Duration::from_millis(1)).unwrap();
    }

    let after_right = shot(&mut session, "HEADLESS-2");
    let after_left = shot(&mut session, "HEADLESS-1");
    assert_ne!(
        pixel(&after_right, cx, cy),
        pixel(&before_right, cx, cy),
        "the sheet is drawn on the display in focus, at ({cx}, {cy}) of it"
    );
    assert_eq!(
        pixel(&after_left, cx, cy),
        pixel(&before_left, cx, cy),
        "and nothing of it reaches the other display"
    );
}

/// A window maximised over the whole display stays under the sheet, and the
/// sheet, not the window, gets the person's keys while it is up.
#[test]
fn a_window_filling_the_display_keeps_the_sheet_on_top_and_takes_no_keys() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_at(&[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)]);
    let mut client = client(&mut session);
    let editor = map(
        &mut session,
        &mut client,
        "org.example.Editor",
        (300, 200),
        |_| {},
    );
    client.windows[editor].1.set_maximized();

    // The compositor hands the client the size it fills, which the client
    // then draws at: nothing of the display shows around it.
    let before = client.seen.toplevels[editor].size;
    pump(&mut session, &mut client, |_, client| {
        client.seen.toplevels[editor].size != before
    });
    if let Some((w, h)) = client.seen.toplevels[editor].size
        && w > 0
        && h > 0
    {
        client.show(editor, w, h);
    }
    pump(&mut session, &mut client, |session, _| {
        let (x, y, w, h) = rect(session, "org.example.Editor");
        x == 0 && w == WIDTH && y + h == HEIGHT
    });
    pump(&mut session, &mut client, |_, client| {
        client.seen.keyboard_focused
    });

    // The window has the keyboard to itself before the sheet comes up.
    client.tap(&mut session, KEY_A);
    pump(&mut session, &mut client, |_, client| {
        client.seen.keys.contains(&(KEY_A, true))
    });
    assert!(
        !client.seen.keys.iter().any(|(key, _)| *key == KEY_ESCAPE),
        "no key of the sheet's reached the window yet: {:?}",
        client.seen.keys
    );

    let before = shot(&mut session, "HEADLESS-1");
    let power = session.power_menu().expect("power sheet");
    power.presentation().open(false);
    pump(&mut session, &mut client, |session, _| {
        session.power_menu().expect("power sheet").panel().x1 > 0.0
    });
    let (cx, cy) = panel_centre(session.power_menu().expect("power sheet"));
    let (wx, wy, ww, wh) = rect(&mut session, "org.example.Editor");
    assert!(
        cx >= wx && cx < wx + ww && cy >= wy && cy < wy + wh,
        "the menu's middle ({cx}, {cy}) is over the window {:?}",
        (wx, wy, ww, wh)
    );
    for _ in 0..4 {
        client.exchange();
        session.step(Duration::from_millis(1)).unwrap();
    }
    let after = shot(&mut session, "HEADLESS-1");
    assert_eq!(
        pixel(&before, cx, cy),
        WINDOW,
        "the window filled the display at ({cx}, {cy}) before the sheet"
    );
    assert_ne!(
        pixel(&after, cx, cy),
        WINDOW,
        "the sheet paints over the maximised window"
    );
    let (x, y, w, h) = rect(&mut session, "org.example.Editor");
    assert_eq!(
        (x, w, y + h),
        (0, WIDTH, HEIGHT),
        "and the window still fills the display"
    );

    // Escape is the sheet's while it is up: the window sees nothing of it.
    let seen_before = client.seen.keys.clone();
    client.tap(&mut session, KEY_ESCAPE);
    pump(&mut session, &mut client, |session, _| {
        !session
            .power_menu()
            .expect("power sheet")
            .presentation()
            .is_open()
    });
    assert_eq!(
        client.seen.keys, seen_before,
        "Escape closed the sheet and never reached the window"
    );

    // The window is still the client with focus, and takes keys again once
    // the sheet is gone.
    assert!(client.seen.keyboard_focused, "the window kept the seat");
    client.tap(&mut session, KEY_A);
    pump(&mut session, &mut client, |_, client| {
        client
            .seen
            .keys
            .iter()
            .filter(|(key, pressed)| *key == KEY_A && *pressed)
            .count()
            >= 2
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

impl Dispatch<XdgSurface, usize> for Seen {
    fn event(
        seen: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            if let Some(toplevel) = seen.toplevels.get_mut(*index) {
                toplevel.configured = true;
            }
        }
    }
}

impl Dispatch<XdgToplevel, usize> for Seen {
    fn event(
        seen: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event
            && let Some(toplevel) = seen.toplevels.get_mut(*index)
        {
            toplevel.size = Some((width, height));
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
        if let wl_seat::Event::Capabilities { capabilities } = event {
            let WEnum::Value(wanted) = capabilities else {
                return;
            };
            if wanted.contains(wl_seat::Capability::Keyboard) && seen.keyboard.is_none() {
                seen.keyboard = Some(seat.get_keyboard(qh, ()));
            }
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
                let pressed = state == WEnum::Value(wl_keyboard::KeyState::Pressed);
                seen.keys.push((key, pressed));
            }
            _ => {}
        }
    }
}

delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore WlShm);
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore WlSurface);
