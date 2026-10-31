//! The display the person makes primary in Settings: across two displays
//! their new windows and the sheets they open go to that one rather than to
//! the one on the left, and the choice is theirs again after a restart.

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

use settings_store::hardware::{
    DisplayConfig, DisplaySet, DisplayTransaction, OutputConfig as StoredOutput,
};
use tilo_compositor::OutputConfig;
use tilo_session::{Options, Session};
use tilo_sheets::Seat as _;
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
    fn connect(compositor: &mut tilo_compositor::Compositor) -> Self {
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

    /// Open toplevel number `self.windows.len()` of `app_id`. Returns its
    /// index.
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
        let fd =
            rustix::fs::memfd_create("tilo-session-test", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
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

/// Let the session run until `done` holds.
#[track_caller]
fn until(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) {
    for _ in 0..500 {
        session.step(Duration::from_millis(2)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!("condition not reached");
}

/// A session whose two displays sit side by side, `HEADLESS-1` on the left
/// and `HEADLESS-2` on the right.
fn docked() -> Session {
    let outputs: Vec<OutputConfig> = (1..=2)
        .map(|index| {
            OutputConfig::new(format!("HEADLESS-{index}"), WIDTH, HEIGHT)
                .at(WIDTH * (index - 1), 0)
        })
        .collect();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            ..Options::default()
        },
        &outputs,
    )
    .expect("session");
    session.step(Duration::from_millis(1)).expect("step");
    session
}

fn client(session: &mut Session) -> TestClient {
    let mut client = TestClient::connect(session.compositor());
    pump(session, &mut client, |_, client| {
        client.seen.wm_base.is_some() && client.seen.shm.is_some()
    });
    client
}

/// Open a window of `app_id` and wait until it is mapped.
fn map(session: &mut Session, client: &mut TestClient, app_id: &str) {
    let index = client.open(app_id);
    pump(session, client, |_, client| {
        client.seen.toplevels[index].configured
    });
    let (w, h) = client.seen.toplevels[index]
        .size
        .filter(|(w, h)| *w > 0 && *h > 0)
        .unwrap_or((300, 200));
    client.show(index, w, h);
    let app = app_id.to_owned();
    pump(session, client, |session, _| {
        session
            .compositor()
            .windows()
            .iter()
            .any(|(info, _)| info.app_id.as_deref() == Some(app.as_str()))
    });
}

/// Which display (0 left, 1 right) the window of `app` opened on; `None`
/// while it is out of sight.
fn display_of(session: &mut Session, app: &str) -> Option<i32> {
    let (_, (x, y, _, _)) = session
        .compositor()
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some(app))?;
    (x >= 0 && y >= 0).then(|| x.div_euclid(WIDTH))
}

/// The rectangle of the right display, `HEADLESS-2`.
fn right() -> (i32, i32, u32, u32) {
    (
        WIDTH,
        0,
        u32::try_from(WIDTH).unwrap(),
        u32::try_from(HEIGHT).unwrap(),
    )
}

/// The displays attached now, as Settings sees them, and the set they make.
fn attached(session: &mut Session) -> (Vec<String>, DisplaySet) {
    let snapshot = session
        .compositor()
        .display_snapshot()
        .cloned()
        .expect("the displays are published");
    let descriptors: Vec<_> = snapshot
        .outputs
        .iter()
        .map(|output| output.descriptor.clone())
        .collect();
    let connectors = descriptors
        .iter()
        .map(|descriptor| descriptor.connector.clone())
        .collect();
    (connectors, DisplaySet::of_descriptors(&descriptors))
}

/// Make `primary` the display the person chose, the way Settings does: the
/// arrangement the displays attached now already have, with that one made
/// primary, asked for as the display change the person made. Changing only
/// the primary leaves no screen dark, so the session keeps it at once
/// instead of waiting for an answer.
fn make_primary(session: &mut Session, primary: &str) {
    let (connectors, set) = attached(session);
    let mut arrangement = DisplayConfig::default();
    for (index, connector) in connectors.iter().enumerate() {
        arrangement.outputs.insert(
            connector.clone(),
            StoredOutput {
                x: WIDTH * i32::try_from(index).expect("a handful of displays"),
                ..StoredOutput::default()
            },
        );
    }
    arrangement.save_for(&set).expect("the arrangement so far");
    let mut candidate = arrangement.clone();
    for (connector, output) in &mut candidate.outputs {
        output.primary = connector == primary;
    }
    DisplayTransaction::request(set, candidate).expect("a display change staged");
}

/// The display on the right of the two, `HEADLESS-2`, as the desk numbers
/// its displays: the one the person makes primary here.
fn right_display(session: &Session) -> Option<u32> {
    session.handle().displays().get(1).copied()
}

/// Whether the desk has been told that the right display is the primary
/// one; until it has, the first display stands in and the primary it names
/// is the left one.
fn primary_reached(session: &Session) -> bool {
    let right = right_display(session);
    right.is_some() && session.handle().primary_display() == right
}

/// The display the sheet would open on right now.
fn sheet_place(session: &Session) -> Option<(i32, i32, u32, u32)> {
    session
        .power_menu()
        .expect("power sheet")
        .presentation()
        .place()
}

#[test]
fn new_windows_and_sheets_open_on_the_primary_the_person_chose() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);

    make_primary(&mut session, "HEADLESS-2");
    until(&mut session, |session| primary_reached(session));

    // The sheet opens on the display the person chose, before anything is
    // open on either of them.
    let power = session.power_menu().expect("power sheet");
    power.presentation().open(false);
    pump(&mut session, &mut client, |session, _| {
        sheet_place(session) == Some(right())
    });
    assert_eq!(
        sheet_place(&session),
        Some(right()),
        "the sheet opens on the primary display"
    );
    session
        .power_menu()
        .expect("power sheet")
        .presentation()
        .close();
    pump(&mut session, &mut client, |session, _| {
        !session
            .power_menu()
            .expect("power sheet")
            .presentation()
            .is_open()
    });

    // So does a window the person opens.
    map(&mut session, &mut client, "org.example.Primary");
    assert_eq!(
        display_of(&mut session, "org.example.Primary"),
        Some(1),
        "a new window opens on the primary display, not the one on the left"
    );
}

#[test]
fn the_display_the_person_made_primary_is_theirs_again_after_a_restart() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    {
        let mut session = docked();
        make_primary(&mut session, "HEADLESS-2");
        until(&mut session, |session| primary_reached(session));
        let (_, set) = attached(&mut session);
        assert_eq!(
            DisplayConfig::load_for(&set)
                .outputs
                .get("HEADLESS-2")
                .map(|output| output.primary),
            Some(true),
            "the choice is kept for the next session"
        );
    }

    let mut session = docked();
    let mut client = client(&mut session);
    until(&mut session, |session| primary_reached(session));
    let power = session.power_menu().expect("power sheet");
    power.presentation().open(false);
    pump(&mut session, &mut client, |session, _| {
        sheet_place(session) == Some(right())
    });
    assert_eq!(
        sheet_place(&session),
        Some(right()),
        "the sheet opens on the display the person chose, in a new session"
    );
    map(&mut session, &mut client, "org.example.Again");
    assert_eq!(
        display_of(&mut session, "org.example.Again"),
        Some(1),
        "and so does a window in a new session"
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
