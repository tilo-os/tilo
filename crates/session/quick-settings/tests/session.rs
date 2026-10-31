//! The session starts Quick Settings itself: a process of its own, handed a
//! socket the compositor trusts and the layer surface its window is to be.
//! What it draws is the panel's own frame, shown like any client's; what it
//! offers is actions, published to the session and run when the session asks.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "a failed step in a test is the assertion, and the session outlives every call"
)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tilo_compositor::{OutputConfig, Screenshot};
use tilo_session::surfaces::{Anchor, Keyboard, Layer, Surface};
use tilo_session::{Options, Session};

const WIDTH: i32 = 800;
const HEIGHT: i32 = 600;
/// How long the panel may take to show its window.
const OPENING: Duration = Duration::from_secs(90);
/// One display's refresh: how often the session asks its clients for their
/// next frame.
const REFRESH: Duration = Duration::from_millis(16);

/// The panel, as the session starts it: below the bar's right end.
fn panel() -> Surface {
    Surface::new("quick-settings", env!("CARGO_BIN_EXE_tilo-quick-settings"))
        .layer(Layer::Top)
        .namespace("tilo-quick-settings")
        .anchor(Anchor::TOP | Anchor::RIGHT)
        .size((
            tilo_quick_settings::resource::PANEL_WIDTH,
            u32::try_from(HEIGHT).unwrap(),
        ))
        .keyboard(Keyboard::OnDemand)
}

#[test]
fn the_session_starts_the_panel_as_a_trusted_client_on_its_layer() {
    let mut session = session_with(&[]);
    // The desk as it stands before the panel appears: what the panel leaves
    // alone has to stay exactly this, pixel for pixel.
    let desk = drawn_desk(&mut session);
    session.start_surface(&panel()).expect("the panel starts");

    assert!(
        session.surfaces().running("quick-settings"),
        "the session started the panel: {:?}",
        session.surfaces().names()
    );

    // What it drew is its own frame, composited like any client's: the panel
    // along the output's right edge, and what lies left of it untouched.
    let frame = drawn(&mut session, &desk);
    save(&frame, "session-quick-settings");
    let inside = pixel(&frame, WIDTH - width() / 2, 200);
    assert_ne!(
        inside,
        pixel(&desk, WIDTH - width() / 2, 200),
        "the panel draws its own frame"
    );
    let beside = WIDTH - width() - 20;
    assert_eq!(
        pixel(&frame, beside, 200),
        pixel(&desk, beside, 200),
        "the panel takes exactly its width"
    );

    session.stop_surface("quick-settings");
    assert!(
        !session.surfaces().running("quick-settings"),
        "the panel is gone"
    );
}

#[test]
fn the_session_asks_the_panel_to_show_the_notifications() {
    let mut session = session_with(&[]);
    let desk = drawn_desk(&mut session);
    session.start_surface(&panel()).expect("the panel starts");

    assert!(
        session.surfaces().running("quick-settings"),
        "the session started the panel"
    );

    // What the panel offers reaches the session as actions, like any app's.
    let deadline = Instant::now() + OPENING;
    while session.surfaces().published("quick-settings").is_empty() {
        assert!(
            Instant::now() < deadline,
            "the panel published nothing: {:?}",
            session.surfaces().names()
        );
        pump(&mut session);
    }
    let published = session.surfaces().published("quick-settings");
    let notifications = published
        .iter()
        .find(|action| action.action == "show-notifications")
        .expect("the panel offers its notifications page");
    let (resource, action) = (notifications.resource.clone(), notifications.action.clone());

    // The session runs it, as a person's palette or an agent would, and the
    // panel answers on its own actions socket.
    let id = session
        .ask_surface("quick-settings", &resource, &action, None)
        .expect("the panel is running");
    let mut ended = None;
    while ended.is_none() {
        assert!(Instant::now() < deadline, "the panel did not answer");
        pump(&mut session);
        if let Some(state) = session.surfaces().outcome(id)
            && !matches!(
                state.update,
                ui::tilo_actions::InvocationUpdate::Accepted { .. }
            )
        {
            ended = Some(state.update.clone());
        }
    }
    assert!(
        matches!(
            ended,
            Some(ui::tilo_actions::InvocationUpdate::Succeeded { .. })
        ),
        "the panel ran it: {ended:?}"
    );

    let frame = drawn(&mut session, &desk);
    save(&frame, "session-quick-settings-notifications");
    session.stop_surface("quick-settings");
}

/// The frame once the panel has drawn into it.
fn drawn(session: &mut Session, desk: &Screenshot) -> Screenshot {
    let deadline = Instant::now() + OPENING;
    let panel = pixel(desk, WIDTH - width() / 2, 200);
    loop {
        pump(session);
        let frame = session.compositor().screenshot("HEADLESS-1").expect("shot");
        let inside = pixel(&frame, WIDTH - width() / 2, 200);
        if inside != panel {
            return frame;
        }
        assert!(
            Instant::now() < deadline,
            "the panel shows nothing here (headless session without a renderer?)"
        );
    }
}

/// The width the panel is placed at.
fn width() -> i32 {
    i32::try_from(tilo_quick_settings::resource::PANEL_WIDTH).unwrap()
}

/// A pixel of `frame`, as the colour it shows.
fn pixel(frame: &Screenshot, x: i32, y: i32) -> [u8; 4] {
    let x = u32::try_from(x).unwrap();
    let y = u32::try_from(y).unwrap();
    frame.pixel(x, y).expect("a pixel of the frame")
}

/// A headless session that starts `surfaces` with it.
fn session_with(surfaces: &[Surface]) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            surfaces: surfaces.to_vec(),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// The desk as it stands before the surface appears: pumped until it has
/// drawn, so what the surface leaves alone can be compared with it.
fn drawn_desk(session: &mut Session) -> Screenshot {
    let deadline = Instant::now() + OPENING;
    let mut last = session.compositor().screenshot("HEADLESS-1").expect("shot");
    loop {
        pump(session);
        let frame = session.compositor().screenshot("HEADLESS-1").expect("shot");
        // Settled: the same frame twice running, with something on it.
        if frame.rgba == last.rgba && frame.rgba.iter().any(|byte| *byte != 0) {
            return frame;
        }
        last = frame;
        assert!(Instant::now() < deadline, "the desk never drew");
    }
}

/// One turn of the session's loop.
fn pump(session: &mut Session) {
    session.step(REFRESH).expect("a turn of the session");
}

/// Write `frame` where a person can look at it, when the run asked for
/// shots: `TILO_SURFACE_SHOTS` names the directory.
fn save(frame: &Screenshot, name: &str) {
    let Some(pixels) = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.clone())
    else {
        panic!("the frame's pixels do not fill it");
    };
    let mut png = Vec::new();
    pixels
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("the shot");
    let Some(directory) = std::env::var_os("TILO_SURFACE_SHOTS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&directory).expect("the directory for shots");
    std::fs::write(directory.join(format!("{name}.png")), png).expect("the shot");
}
