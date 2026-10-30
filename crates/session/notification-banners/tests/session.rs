//! The session starts the banners itself: a process of its own, handed a
//! socket the compositor trusts and the layer surface their window is to be.
//! What they draw is the banners' own frame, shown like any client's; what
//! they offer is actions, published to the session and run when it asks.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "a failed step in a test is the assertion"
)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tilo_compositor::{OutputConfig, Screenshot};
use tilo_session::surfaces::{Anchor, Keyboard, Layer, Surface};
use tilo_session::{Options, Session};

const WIDTH: i32 = 900;
const HEIGHT: i32 = 600;
/// The width the banner stack takes at the output's top right.
const BANNERS: u32 = 420;
/// How long the banners may take to show their window.
const OPENING: Duration = Duration::from_secs(90);
/// One display's refresh: how often the session asks its clients for their
/// next frame.
const REFRESH: Duration = Duration::from_millis(16);

/// The banners, as the session starts them.
fn banners() -> Surface {
    Surface::new(
        "notification-banners",
        env!("CARGO_BIN_EXE_tilo-notification-banners"),
    )
    .layer(Layer::Top)
    .namespace("tilo-banners")
    .anchor(Anchor::TOP | Anchor::RIGHT)
    .size((BANNERS, u32::try_from(HEIGHT).unwrap()))
    .keyboard(Keyboard::OnDemand)
}

#[test]
fn the_session_starts_the_banners_as_a_trusted_client_on_their_layer() {
    let mut session = session_with(&[]);
    // The desk as it stands before the banners appear: what they leave alone
    // has to stay exactly this, pixel for pixel.
    let desk = drawn_desk(&mut session);
    session
        .start_surface(&banners())
        .expect("the banners start");

    assert!(
        session.surfaces().running("notification-banners"),
        "the session started the banners: {:?}",
        session.surfaces().names()
    );

    // What they drew is their own frame, composited like any client's: the
    // stack at the output's top right, and what lies left of it untouched.
    let frame = drawn(&mut session, &desk);
    save(&frame, "session-banners");
    let beside = WIDTH - width() - 20;
    assert_eq!(
        pixel(&frame, beside, 100),
        pixel(&desk, beside, 100),
        "the banners take exactly their width"
    );

    session.stop_surface("notification-banners");
    assert!(
        !session.surfaces().running("notification-banners"),
        "the banners are gone"
    );
}

#[test]
fn the_session_asks_the_banners_to_focus_their_newest() {
    let mut session = session_with(&[]);
    let desk = drawn_desk(&mut session);
    session
        .start_surface(&banners())
        .expect("the banners start");

    assert!(
        session.surfaces().running("notification-banners"),
        "the session started the banners"
    );

    // What they offer reaches the session as actions, like any app's.
    let deadline = Instant::now() + OPENING;
    while session
        .surfaces()
        .published("notification-banners")
        .is_empty()
    {
        assert!(
            Instant::now() < deadline,
            "the banners published nothing: {:?}",
            session.surfaces().names()
        );
        pump(&mut session);
    }
    let published = session.surfaces().published("notification-banners");
    let newest = published
        .iter()
        .find(|action| action.action == "focus-newest")
        .expect("the banners offer moving focus into the newest");
    let (resource, action) = (newest.resource.clone(), newest.action.clone());

    // The session runs it, as a person's palette or an agent would, and the
    // banners answer on their own actions socket.
    let id = session
        .ask_surface("notification-banners", &resource, &action, None)
        .expect("the banners are running");
    let mut ended = None;
    while ended.is_none() {
        assert!(Instant::now() < deadline, "the banners did not answer");
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
        "the banners ran it: {ended:?}"
    );

    let frame = drawn(&mut session, &desk);
    save(&frame, "session-banners-focused");
    session.stop_surface("notification-banners");
}

/// The frame once the banners have drawn into it.
fn drawn(session: &mut Session, desk: &Screenshot) -> Screenshot {
    let deadline = Instant::now() + OPENING;
    let stack_before = pixel(desk, WIDTH - width() / 2, 100);
    loop {
        pump(session);
        let frame = session.compositor().screenshot("HEADLESS-1").expect("shot");
        let stack = pixel(&frame, WIDTH - width() / 2, 100);
        if stack != stack_before {
            return frame;
        }
        assert!(
            Instant::now() < deadline,
            "the banners show nothing here (headless session without a renderer?)"
        );
    }
}

/// The width the banners are placed at.
fn width() -> i32 {
    i32::try_from(BANNERS).unwrap()
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

/// Keep `frame` for a person to look at: written to the directory
/// `TILO_SURFACE_SHOTS` names when it is set, and printed base64 either way,
/// so a run whose files nobody keeps shows its shots when its output is not
/// captured.
fn save(frame: &Screenshot, name: &str) {
    use base64::Engine as _;
    let Some(pixels) = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.clone())
    else {
        panic!("the frame's pixels do not fill it");
    };
    let mut png = Vec::new();
    pixels
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("the shot");
    println!(
        "tilo-shot {name} {}",
        base64::engine::general_purpose::STANDARD.encode(&png)
    );
    let Some(directory) = std::env::var_os("TILO_SURFACE_SHOTS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&directory).expect("the directory for shots");
    std::fs::write(directory.join(format!("{name}.png")), png).expect("the shot");
}
