//! The rotation lock: the built-in panel turns with the device until the person pins it, and the
//! desk lays its windows out on the turned screen.

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

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::window::{Client, Seen, window_of};
use tilo_compositor::{Compositor, OutputConfig};
use tilo_session::{Accelerometer, Options, Session};

/// The built-in panel's size, as the connector reports it.
const WIDTH: i32 = 800;
const HEIGHT: i32 = 600;
/// The rectangle the panel shows once the machine is turned on its side: the same pixels, the other
/// way round.
const TURNED: (i32, i32, i32, i32) = (0, 0, HEIGHT, WIDTH);
/// The rectangle it shows upright.
const UPRIGHT: (i32, i32, i32, i32) = (0, 0, WIDTH, HEIGHT);

/// Let the session run until `done` holds. The accelerometer is read a few times a second and its
/// orientation is debounced over consecutive readings, so a turn a person makes takes a moment of
/// real time: the loop runs until a deadline rather than a number of passes, because a pass can
/// return as soon as anything else in the session is due.
#[track_caller]
fn until(
    session: &mut Session,
    client: &mut Client,
    mut done: impl FnMut(&mut Session, &Seen) -> bool,
) {
    let deadline = tilo_test_env::hang_deadline();
    while Instant::now() < deadline {
        client.exchange();
        session.step(Duration::from_millis(20)).expect("step");
        client.exchange();
        if done(session, &client.seen) {
            return;
        }
    }
    panic!("the session never got there");
}

/// A stand-in for the kernel's accelerometer: the three axes a test writes its readings to.
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

/// A session on one built-in panel, reading `sensor` .
fn session(root: &std::path::Path) -> Session {
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("eDP-1", WIDTH, HEIGHT)],
        Options {
            collab: false,
            accelerometer: Accelerometer::At(root.to_owned()),
            ..Options::default()
        },
    );
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

fn client(session: &mut Session) -> Client {
    let mut client = Client::connect(session.compositor());
    client.pump(session, |_, seen| {
        seen.wm_base.is_some() && seen.shm.is_some()
    });
    client
}

/// Open a toplevel of `app_id` at the size it was configured to, and wait until it is mapped.
fn map(session: &mut Session, client: &mut Client, app_id: &str) -> usize {
    client.map_plain(session, app_id, (200, 150))
}

/// Draw toplevel `index` at every size the session hands it, until the window `app` reaches `want`
/// , its left edge, width and bottom edge.
/// A maximised window is laid out over the screen it is on, so the screen turning is a new size for
/// it: the client draws at the new size and the window then fills the screen the person is looking
/// at.
#[track_caller]
fn fill(
    session: &mut Session,
    client: &mut Client,
    index: usize,
    app: &str,
    want: (i32, i32, i32),
) {
    let deadline = tilo_test_env::hang_deadline();
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
            client.show(index, width, height, common::window::WINDOW_BYTES);
            drawn = size;
        }
        if let Some((x, y, width, height)) =
            window_of(session.compositor(), app).map(|(_, rect)| rect)
            && (x, width, y + height) == want
        {
            return;
        }
    }
    panic!("the window never filled the screen");
}

/// The window of `app` and its rectangle `(x, y, w, h)` .
fn window(compositor: &Compositor, app: &str) -> Option<(u64, (i32, i32, i32, i32))> {
    window_of(compositor, app)
}

/// Where the window of `app` reaches to: its left and top edges, its width and its bottom edge, in
/// the desk's coordinates. A window filling the screen fills it edge to edge and may start below
/// the top inset.
fn edges(session: &mut Session, app: &str) -> (i32, i32, i32, i32) {
    let (x, y, width, height) = window(session.compositor(), app).expect("mapped").1;
    (x, y, width, y + height)
}

/// What the person sees of the built-in panel: its rectangle in the desk's coordinates, the way the
/// compositor holds it.
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
    client.toplevel(editor).set_maximized();
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

    // The person locks the rotation and turns the machine back upright: the screen they are reading
    // stays where it is.
    settings_store::hardware::ROTATION_LOCK
        .set(true)
        .expect("the person's setting");
    hold(&sensor, (0, -9, 0));
    // Two seconds of the desk's own time with the machine on its side: the turn has long been read
    // and debounced by now, so anything that were to follow the sensor would have happened.
    for _ in 0..100 {
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
