//! Copies of the display never show what the session holds back.

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use tilo_app_launch::Connects as _;

use super::*;

/// What a copy of the whole display shows: what a screenshot, a recording,
/// a share or a remote viewer gets.
#[track_caller]
fn copied(session: &mut Session) -> tilo_compositor::Screenshot {
    session
        .compositor()
        .read_back_region("HEADLESS-1", (0, 0, WIDTH, HEIGHT))
        .unwrap_or_else(|error| panic!("a copy of HEADLESS-1: {error}"))
}

/// What the person's own display shows.
#[track_caller]
fn displayed(session: &mut Session) -> tilo_compositor::Screenshot {
    session
        .compositor()
        .screenshot("HEADLESS-1")
        .unwrap_or_else(|error| panic!("a frame of HEADLESS-1: {error}"))
}

/// The colours a frame is drawn in.
fn colours(frame: &tilo_compositor::Screenshot) -> std::collections::BTreeSet<[u8; 4]> {
    frame
        .rgba
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect()
}

/// While the session is locked, whatever the person types into the lock
/// screen's password field never leaves the display: a copy of it is the one
/// placeholder, and the person still sees their lock screen.
#[test]
fn a_capture_of_the_lock_screen_withholds_the_password_field() {
    let _settled = ui::motion::testing::settled();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            user: "ada".to_owned(),
            verifier: std::sync::Arc::new(Sesame),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.step(Duration::from_millis(1)).unwrap();

    // Unlocked, a copy is the desktop with everything on it.
    let open = copied(&mut session);
    assert!(colours(&open).len() > 8, "the desktop, not a flat frame");

    invoke(&mut session, tilo_session::SESSION_RESOURCE, "lock");
    assert!(session.compositor().locked());
    let screen = publisher(&session, LockEffect::UNLOCK.id).expect("the lock screen is up");
    invoke_with(
        &mut session,
        &screen,
        LockEffect::TYPE_PASSWORD.id,
        &serde_json::json!({"password": "sesame"}),
    );

    // The copy shows nothing of the lock screen: one flat placeholder over
    // the whole display, the same wherever it is read.
    let copy = copied(&mut session);
    let withheld = colours(&copy);
    assert_eq!(
        withheld.len(),
        1,
        "one placeholder, no password: {withheld:?}"
    );
    assert!(!colours(&open).contains(&withheld.iter().next().copied().unwrap()));

    // The person's own display still shows their lock screen, field and all.
    let display = displayed(&mut session);
    assert!(
        colours(&display).len() > 8,
        "the lock screen is on the display: {:?}",
        colours(&display).len()
    );
    assert!(
        publisher(&session, LockEffect::UNLOCK.id).is_some(),
        "and it is still there to unlock"
    );
}

/// The shell-ui connection of a window of `app` the launch `pid` runs, with
/// `messages` written on it as the app's own frames.
fn shell_ui_connection(
    launches: &Arc<tilo_session::Launches>,
    app: &str,
    room: &str,
    pid: u32,
    messages: &[ui::floating_surface::FloatingSurfaceClientMessage],
) -> UnixStream {
    let mut stream: UnixStream =
        tilo_session::mount::app_tabs::ServiceLinks::new(Arc::clone(launches))
            .window(app, room, 7, Some(pid))
            .shell_ui
            .expect("the app's shell-ui connection")
            .into();
    stream
        .set_read_timeout(Some(Duration::from_millis(20)))
        .expect("a timeout on reading");
    for message in messages {
        let mut frame = serde_json::to_vec(message).expect("a frame");
        frame.push(b'\n');
        stream.write_all(&frame).expect("writing the frame");
    }
    stream.flush().expect("the frames");
    stream
}

/// Read the next message the session writes on `stream`, stepping it until
/// one arrives.
#[track_caller]
fn answer(
    session: &mut Session,
    stream: &UnixStream,
) -> ui::floating_surface::FloatingSurfaceHostMessage {
    let mut reader = std::io::BufReader::new(stream.try_clone().expect("a reading end"));
    let mut line = String::new();
    for _ in 0..400 {
        session.step(Duration::from_millis(2)).expect("step");
        match std::io::BufRead::read_line(&mut reader, &mut line) {
            Ok(0) => panic!("the session closed the connection"),
            Ok(_) => {
                return serde_json::from_str(line.trim_end()).expect("a message");
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("reading the session's answer: {error}"),
        }
    }
    panic!("the session did not answer");
}

/// An app that says over its own shell-ui connection that its windows show a
/// secret has them withheld from captures — the session answers once the
/// compositor installs it, and a copy of the display is the placeholder while
/// the person's own display still shows the window. The mark goes when the
/// app's connection does.
#[test]
fn an_app_that_shows_a_secret_has_its_window_withheld_in_copies() {
    use ui::floating_surface::{FLOATING_SURFACE_PROTOCOL_VERSION, FloatingSurfaceClientMessage};

    let _settled = ui::motion::testing::settled();
    let launches = Arc::new(tilo_session::Launches::default());
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            launches: Arc::clone(&launches),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.step(Duration::from_millis(1)).unwrap();

    // The app this session started, in the room it runs in, and its window.
    let mut app_process = std::process::Command::new("sleep")
        .arg("120")
        .spawn()
        .expect("a launched app's process");
    let app = "org.example.Vault";
    launches.started(app_process.id(), app, "private", false);
    let _client = mapped(&mut session, app);
    let (_, (x, y, w, h)) = window(session.compositor(), app).expect("the app's window");
    let centre = (
        u32::try_from(x + w / 2).expect("on the display"),
        u32::try_from(y + h / 2).expect("on the display"),
    );
    let content = [200, 40, 40, 255];

    // Nothing is asked yet: a copy of the display carries the window.
    let copy = copied(&mut session);
    assert_eq!(copy.pixel(centre.0, centre.1), Some(content));

    let connection = shell_ui_connection(
        &launches,
        app,
        "private",
        app_process.id(),
        &[
            FloatingSurfaceClientMessage::Hello {
                version: FLOATING_SURFACE_PROTOCOL_VERSION,
            },
            FloatingSurfaceClientMessage::SetCaptureProtection {
                protected: true,
                generation: Some(3),
            },
        ],
    );
    assert_eq!(
        answer(&mut session, &connection),
        ui::floating_surface::FloatingSurfaceHostMessage::CaptureProtectionApplied {
            generation: 3,
            protected: true,
        },
        "the app is told the compositor has it"
    );

    // The window left the copy, and only it: one flat placeholder where it
    // stands, still on the person's own display.
    let copy = copied(&mut session);
    let placeholder = copy.pixel(centre.0, centre.1).expect("inside the copy");
    assert_ne!(placeholder, content, "the window's pixels are gone");
    for row in y..(y + h) {
        for column in x..(x + w) {
            assert_eq!(
                copy.pixel(
                    u32::try_from(column).expect("on the display"),
                    u32::try_from(row).expect("on the display")
                ),
                Some(placeholder),
                "the whole window is the placeholder at {column},{row}"
            );
        }
    }
    let display = displayed(&mut session);
    assert_eq!(
        display.pixel(centre.0, centre.1),
        Some(content),
        "the person's own display shows the window"
    );

    // The app's connection closes: its windows are its own business again,
    // and a copy shows them.
    drop(connection);
    for _ in 0..40 {
        session.step(Duration::from_millis(2)).expect("step");
    }
    let copy = copied(&mut session);
    assert_eq!(copy.pixel(centre.0, centre.1), Some(content));

    let _ = app_process.kill();
    let _ = app_process.wait();
}
