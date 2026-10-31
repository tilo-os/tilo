//! The rotation lock: the built-in panel turns with the device until the
//! person pins it, and the desk lays its windows out on the turned screen.

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
use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_compositor::{Compositor, OutputConfig};
use tilo_session::{Accelerometer, Options, Session};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry, wl_registry::WlRegistry, wl_shm,
    wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_surface::XdgSurface, xdg_toplevel, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};

/// The built-in panel's size, as the connector reports it.
const WIDTH: i32 = 800;
const HEIGHT: i32 = 600;
/// The rectangle the panel shows once the machine is turned on its side: the
/// same pixels, the other way round.
const TURNED: (i32, i32, i32, i32) = (0, 0, HEIGHT, WIDTH);
/// The rectangle it shows upright.
const UPRIGHT: (i32, i32, i32, i32) = (0, 0, WIDTH, HEIGHT);

/// What the compositor paints every window of the test client with, in
/// `ARGB8888` bytes.
const WINDOW_BYTES: [u8; 4] = [200, 40, 40, 255];

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
    toplevels: Vec<Toplevel>,
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

    /// Open toplevel number `self.windows.len()` of `app_id`, committed once.
    /// Returns its index.
    fn open(&mut self, app_id: &str) -> usize {
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
        let fd = rustix::fs::memfd_create("tilo-rotation-test", rustix::fs::MemfdFlags::CLOEXEC)
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

/// Let the session run until `done` holds. The accelerometer is read a few
/// times a second and its orientation is debounced over consecutive
/// readings, so a turn a person makes takes a moment of real time: the loop
/// runs until a deadline rather than a number of passes, because a pass can
/// return as soon as anything else in the session is due.
#[track_caller]
fn until(
    session: &mut Session,
    client: &mut TestClient,
    mut done: impl FnMut(&mut Session, &TestClient) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        client.exchange();
        session.step(Duration::from_millis(20)).expect("step");
        client.exchange();
        if done(session, client) {
            return;
        }
    }
    panic!("the session never got there");
}

/// A stand-in for the kernel's accelerometer: the three axes a test writes
/// its readings to.
fn sensor() -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("a sensor root");
    let device = root.path().join("iio:device0");
    std::fs::create_dir(&device).expect("a sensor");
    hold(&root, (0, -9, 0));
    root
}

/// Hold the device so that the sensor reads `raw` on each axis.
fn hold(root: &tempfile::TempDir, raw: (i32, i32, i32)) {
    let device = root.path().join("iio:device0");
    for (axis, value) in [("x", raw.0), ("y", raw.1), ("z", raw.2)] {
        std::fs::write(
            device.join(format!("in_accel_{axis}_raw")),
            value.to_string(),
        )
        .expect("a reading");
    }
}

/// A session on one built-in panel, reading `sensor`.
fn session(root: &std::path::Path) -> Session {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            accelerometer: Accelerometer::At(root.to_owned()),
            ..Options::default()
        },
        &[OutputConfig::new("eDP-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.step(Duration::from_millis(1)).unwrap();
    session
}

/// The person's settings, as the session reads them: the rotation lock off.
fn settings() {
    let mut document = serde_json::Map::new();
    document.insert(
        "display".into(),
        serde_json::json!({ "rotation_lock": false }),
    );
    settings_store::install_source(Some(Arc::new(settings_store::HostDocument::new(
        document,
        |_| Ok(()),
    ))));
}

fn client(session: &mut Session) -> TestClient {
    let mut client = TestClient::connect(session.compositor());
    pump(session, &mut client, |_, client| {
        client.seen.wm_base.is_some() && client.seen.shm.is_some()
    });
    client
}

/// Open a toplevel of `app_id` at the size it was configured to, and wait
/// until it is mapped.
fn map(session: &mut Session, client: &mut TestClient, app_id: &str) -> usize {
    let index = client.open(app_id);
    pump(session, client, |_, client| {
        client.seen.toplevels[index].configured
    });
    let (width, height) = client.seen.toplevels[index]
        .size
        .filter(|(width, height)| *width > 0 && *height > 0)
        .unwrap_or((200, 150));
    client.show(index, width, height);
    let app = app_id.to_owned();
    pump(session, client, |session, _| {
        window(session.compositor(), &app).is_some()
    });
    index
}

/// Draw toplevel `index` at every size the session hands it, until the window
/// `app` reaches `want`, its left edge, width and bottom edge.
///
/// A maximised window is laid out over the screen it is on, so the screen
/// turning is a new size for it: the client draws at the new size and the
/// window then fills the screen the person is looking at.
#[track_caller]
fn fill(
    session: &mut Session,
    client: &mut TestClient,
    index: usize,
    app: &str,
    want: (i32, i32, i32),
) {
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut drawn = None;
    while Instant::now() < deadline {
        client.exchange();
        session.step(Duration::from_millis(20)).expect("step");
        client.exchange();
        let size = client.seen.toplevels[index]
            .size
            .filter(|(width, height)| *width > 0 && *height > 0);
        if let Some((width, height)) = size
            && drawn != size
        {
            client.show(index, width, height);
            drawn = size;
        }
        if let Some((x, y, width, height)) = window(session.compositor(), app).map(|(_, rect)| rect)
            && (x, width, y + height) == want
        {
            return;
        }
    }
    panic!("the window never filled the screen");
}

/// The window of `app` and its rectangle `(x, y, w, h)`.
fn window(compositor: &Compositor, app: &str) -> Option<(u64, (i32, i32, i32, i32))> {
    compositor
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some(app))
        .map(|(info, rect)| (info.id, rect))
}

/// Where the window of `app` reaches to: its left and top edges, its width
/// and its bottom edge, in the desk's coordinates. A window filling the
/// screen fills it edge to edge and may start below the top inset.
fn edges(session: &mut Session, app: &str) -> (i32, i32, i32, i32) {
    let (x, y, width, height) = window(session.compositor(), app).expect("mapped").1;
    (x, y, width, y + height)
}

/// What the person sees of the built-in panel: its rectangle in the desk's
/// coordinates, the way the compositor holds it.
fn panel(session: &mut Session) -> (i32, i32, i32, i32) {
    session
        .compositor()
        .outputs()
        .first()
        .and_then(|output| output.rect)
        .expect("the panel is on")
}

#[test]
fn the_lock_turns_the_outputs_and_the_desk_follows() {
    let _settled = ui::motion::testing::settled();
    let sensor = sensor();
    settings();
    let mut session = session(sensor.path());
    let mut client = client(&mut session);
    assert_eq!(panel(&mut session), UPRIGHT, "the panel starts upright");

    let editor = map(&mut session, &mut client, "org.example.Editor");
    client.windows[editor].1.set_maximized();
    fill(
        &mut session,
        &mut client,
        editor,
        "org.example.Editor",
        (0, WIDTH, HEIGHT),
    );
    let upright = edges(&mut session, "org.example.Editor");

    // The lock is off and the machine is turned on its side: the panel turns
    // with it, and the window that filled the screen fills the turned one,
    // still from the top.
    hold(&sensor, (9, 0, 0));
    until(&mut session, &mut client, |session, _| {
        panel(session) == TURNED
    });
    fill(
        &mut session,
        &mut client,
        editor,
        "org.example.Editor",
        (0, HEIGHT, WIDTH),
    );
    let turned = edges(&mut session, "org.example.Editor");
    assert_eq!(
        turned.1, upright.1,
        "the window is still at the top of what the person sees"
    );

    // The person locks the rotation and turns the machine back upright: the
    // screen they are reading stays where it is.
    settings_store::hardware::ROTATION_LOCK
        .set(true)
        .expect("the person's setting");
    hold(&sensor, (0, -9, 0));
    let settle = Instant::now() + Duration::from_secs(2);
    while Instant::now() < settle {
        client.exchange();
        session.step(Duration::from_millis(20)).expect("step");
        client.exchange();
    }
    assert_eq!(panel(&mut session), TURNED, "the locked panel stays put");
    assert_eq!(
        edges(&mut session, "org.example.Editor"),
        turned,
        "and the desk keeps its windows on the screen the person reads"
    );
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

delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore WlShm);
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore WlSurface);
