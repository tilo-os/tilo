//! The tilolang shell's bar on every display: one bar per output, standing on its own
//! display's rectangle (each part follows an output added, moved or taken away), not stretched
//! across the displays' bounding box as one; and a bar steps aside on the display a fullscreen
//! window owns, from the signal the compositor names those displays by.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::time::Duration;

use tilo_compositor::{Compositor, OutputChange, OutputConfig};
use tilo_session::{Options, Session};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry, wl_registry::WlRegistry, wl_shm,
    wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_surface::XdgSurface, xdg_toplevel, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};

const WIDTH: i32 = 1000;
const HEIGHT: i32 = 700;
const PERSON: &str = "Pat";

fn options() -> Options {
    Options {
        presentation: ui::headless::fixture_runner_inputs,
        person: PERSON.to_owned(),
        collab: false,
        ..Options::default()
    }
}

/// A session on `outputs`, each `WIDTH` x `HEIGHT` at its place.
fn session(places: &[(i32, i32)]) -> Session {
    let outputs: Vec<OutputConfig> = places
        .iter()
        .enumerate()
        .map(|(index, place)| {
            let mut output = OutputConfig::new(format!("HEADLESS-{}", index + 1), WIDTH, HEIGHT);
            output.position = *place;
            output
        })
        .collect();
    let mut session = Session::headless(options(), &outputs).expect("session");
    session.step(Duration::from_millis(1)).unwrap();
    session
}

/// Run frames until `done` holds.
#[track_caller]
fn until(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) {
    for _ in 0..2000 {
        session.step(Duration::from_millis(1)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!(
        "condition not reached; the bars say: {:?}",
        session.shell_bars()
    );
}

/// Where the bar on `output` stands, `(x, y, w, h)`, and whether it stepped aside: `None`
/// while no bar stands there.
fn bar(session: &Session, output: &str) -> Option<((i32, i32, u32, u32), bool)> {
    session
        .shell_bars()
        .into_iter()
        .find(|bar| bar.output == output)
        .map(|bar| (bar.rect, bar.aside))
}

#[test]
fn every_display_has_a_bar_on_its_own_rectangle() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(&[(0, 0), (WIDTH, 0)]);
    until(&mut session, |session| session.shell_bars().len() == 2);
    let bars = session.shell_bars();
    let names: Vec<&str> = bars.iter().map(|bar| bar.output.as_str()).collect();
    assert_eq!(names, ["HEADLESS-1", "HEADLESS-2"], "one bar per display");
    for bar in &bars {
        assert!(
            !bar.aside,
            "no window covers {}: its bar stands",
            bar.output
        );
    }
    let first = bar(&session, "HEADLESS-1")
        .expect("the first display's bar")
        .0;
    let second = bar(&session, "HEADLESS-2")
        .expect("the second display's bar")
        .0;
    assert_eq!(
        (first.0, first.1, first.2),
        (0, 0, WIDTH as u32),
        "the first bar stands on the first display, its band high"
    );
    assert_eq!(
        (second.0, second.1, second.2),
        (WIDTH, 0, WIDTH as u32),
        "the second bar stands on the second display, not stretched across both"
    );
    assert_eq!(first.3, second.3, "both bands are the bar's own height");
    assert!(
        second.0 >= WIDTH,
        "the second bar is on its display, beyond the first: {second:?}"
    );
}

#[test]
fn a_display_added_later_gets_its_own_bar() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(&[(0, 0)]);
    until(&mut session, |session| session.shell_bars().len() == 1);
    assert!(bar(&session, "HEADLESS-1").is_some());
    // A second display comes: its bar is made, on its rectangle.
    let mut output = OutputConfig::new("HEADLESS-2", WIDTH, HEIGHT);
    output.position = (WIDTH, 0);
    session.compositor().add_output(&output).unwrap();
    until(&mut session, |session| session.shell_bars().len() == 2);
    let second = bar(&session, "HEADLESS-2")
        .expect("the new display's bar")
        .0;
    assert_eq!(
        (second.0, second.2),
        (WIDTH, WIDTH as u32),
        "the added display's bar stands on the added display"
    );
}

#[test]
fn a_display_turned_off_takes_its_bar_and_brings_it_back() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(&[(0, 0), (WIDTH, 0)]);
    until(&mut session, |session| session.shell_bars().len() == 2);
    let offered = session.resources().len();
    session
        .compositor()
        .configure_outputs(&[OutputChange::disable("HEADLESS-2")])
        .unwrap();
    until(&mut session, |session| session.shell_bars().len() == 1);
    assert!(
        bar(&session, "HEADLESS-1").is_some(),
        "the display left on keeps its bar"
    );
    assert!(
        session.resources().len() < offered,
        "the gone display's bar offers nothing on the door"
    );
    session
        .compositor()
        .configure_outputs(&[OutputChange::enable("HEADLESS-2")])
        .unwrap();
    until(&mut session, |session| session.shell_bars().len() == 2);
    assert_eq!(
        session.resources().len(),
        offered,
        "the display back has its own bar again, not another one beside it"
    );
}

#[test]
fn a_bar_steps_aside_on_the_display_a_fullscreen_window_owns() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(&[(0, 0), (WIDTH, 0)]);
    until(&mut session, |session| session.shell_bars().len() == 2);
    let mut client = TestClient::connect(session.compositor());
    pump(&mut session, &mut client, |_, client| {
        client.seen.wm_base.is_some() && client.seen.shm.is_some()
    });
    client.open("full");
    pump(&mut session, &mut client, |_, client| {
        client.seen.configured
    });
    let (w, h) = client
        .seen
        .size
        .filter(|(w, h)| *w > 0 && *h > 0)
        .unwrap_or((300, 200));
    client.show(w, h);
    pump(&mut session, &mut client, |session, _| {
        window(session.compositor()).is_some()
    });
    assert!(
        session.shell_bars().iter().all(|bar| !bar.aside),
        "a window that is not fullscreen leaves every bar standing: {:?}",
        session.shell_bars()
    );

    // The window asks to cover its display, as a video player's full screen does.
    client.toplevel().set_fullscreen(None);
    client.redraw_when_resized(&mut session, (w, h));
    pump(&mut session, &mut client, |session, _| {
        !session.compositor().fullscreen_outputs().is_empty()
    });
    let owned = session.compositor().fullscreen_outputs();
    assert_eq!(owned.len(), 1, "the window covers one display: {owned:?}");
    let owned = &owned[0];
    pump(&mut session, &mut client, |session, _| {
        bar(session, owned).is_some_and(|(_, aside)| aside)
    });
    for place in session.shell_bars() {
        assert_eq!(
            place.aside,
            &place.output == owned,
            "only the bar on the display the window owns steps aside: {place:?}"
        );
    }

    // It leaves fullscreen: the bar is back on that display.
    let full = client.seen.size.unwrap_or((WIDTH, HEIGHT));
    client.toplevel().unset_fullscreen();
    client.redraw_when_resized(&mut session, full);
    pump(&mut session, &mut client, |session, _| {
        session.shell_bars().iter().all(|bar| !bar.aside)
    });
}

/// What the test's one toplevel was told.
#[derive(Default)]
struct Seen {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    configured: bool,
    size: Option<(i32, i32)>,
}

/// One client with one toplevel of a solid colour.
struct TestClient {
    connection: Connection,
    queue: EventQueue<Seen>,
    seen: Seen,
    window: Option<(WlSurface, XdgToplevel)>,
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
            window: None,
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

    /// Open the toplevel of `app_id` and make its first commit.
    fn open(&mut self, app_id: &str) {
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
        self.window = Some((surface, toplevel));
    }

    fn toplevel(&self) -> &XdgToplevel {
        &self.window.as_ref().expect("opened").1
    }

    /// Wait for a configure to a size other than `was`, then draw at it: the commit that
    /// makes the state the configure carried the window's own.
    fn redraw_when_resized(&mut self, session: &mut Session, was: (i32, i32)) {
        pump(session, self, |_, client| {
            client
                .seen
                .size
                .is_some_and(|size| size != was && size.0 > 0 && size.1 > 0)
        });
        let (w, h) = self.seen.size.expect("configured");
        self.show(w, h);
    }

    /// Attach a buffer of one colour, `width` x `height`, and commit it.
    fn show(&self, width: i32, height: i32) {
        let qh = self.queue.handle();
        let stride = width * 4;
        let len = usize::try_from(stride * height).unwrap();
        let fd = rustix::fs::memfd_create("tilo-shell-bar-test", rustix::fs::MemfdFlags::CLOEXEC)
            .unwrap();
        let mut file = std::fs::File::from(fd);
        file.write_all(&[40, 40, 200, 255].repeat(len / 4)).unwrap();
        let pool = self.seen.shm.as_ref().expect("wl_shm").create_pool(
            file.as_fd(),
            stride * height,
            &qh,
            (),
        );
        let buffer =
            pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
        let surface = &self.window.as_ref().expect("opened").0;
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.commit();
        pool.destroy();
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
    panic!(
        "condition not reached; the bars say {:?}, the windows {:?}, fullscreen on {:?}",
        session.shell_bars(),
        session.compositor().windows(),
        session.compositor().fullscreen_outputs()
    );
}

/// The test window, by id and logical rectangle, once it is mapped.
fn window(compositor: &Compositor) -> Option<(u64, (i32, i32, i32, i32))> {
    compositor
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some("full"))
        .map(|(info, rect)| (info.id, rect))
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
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            seen.size = Some((width, height));
        }
    }
}

delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore WlShm);
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore WlSurface);
