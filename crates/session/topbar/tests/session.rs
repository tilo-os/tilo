//! The session starts the bar itself: a process of its own, handed a socket
//! the compositor trusts and the layer surface its window is to be. What it
//! draws is the bar's own frame, shown like any client's; what it offers is
//! actions, published to the session and run when the session asks.

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

const WIDTH: i32 = 640;
const HEIGHT: i32 = 480;
/// The height the bar takes along the top of the output.
const BAR: u32 = 32;
/// How long the bar may take to show its window.
const OPENING: Duration = Duration::from_secs(90);
/// One display's refresh: how often the session asks its clients for their
/// next frame.
const REFRESH: Duration = Duration::from_millis(16);

/// The bar, as the session starts it.
fn bar() -> Surface {
    Surface::new("topbar", env!("CARGO_BIN_EXE_tilo-topbar"))
        .layer(Layer::Top)
        .namespace("tilo-bar")
        .anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT)
        .size((0, BAR))
        .exclusive(i32::try_from(BAR).unwrap())
        .keyboard(Keyboard::None)
}

#[test]
fn the_session_starts_the_bar_as_a_trusted_client_on_its_layer() {
    let mut session = session_with(&[bar()]);

    assert!(
        session.surfaces().running("topbar"),
        "the session started the bar: {:?}",
        session.surfaces().names()
    );

    // Its window is a layer surface of the session's: placed across the top
    // of the output, reserving the band the desktop keeps free for it.
    let deadline = Instant::now() + OPENING;
    while session.compositor().reserved("HEADLESS-1").is_empty() {
        assert!(
            Instant::now() < deadline,
            "the bar shows nothing here (headless session without a renderer?)"
        );
        pump(&mut session);
    }
    assert_eq!(
        session.compositor().reserved("HEADLESS-1"),
        vec![(0, 0, WIDTH, i32::try_from(BAR).unwrap())],
        "the bar reserves the top of the output for itself"
    );

    // What it drew is its own frame, composited like any client's: the bar
    // across the band it took, and what lies below untouched by it.
    let frame = session.compositor().screenshot("HEADLESS-1").expect("shot");
    let bar_pixel = frame
        .pixel(u32::try_from(WIDTH / 2).unwrap(), BAR / 2)
        .expect("a pixel of the bar");
    let below = frame
        .pixel(
            u32::try_from(WIDTH / 2).unwrap(),
            u32::try_from(HEIGHT / 2).unwrap(),
        )
        .expect("a pixel below the bar");
    assert_ne!(bar_pixel, below, "the bar draws over what is below it");
    assert_eq!(
        frame
            .pixel(u32::try_from(WIDTH / 2).unwrap(), BAR)
            .expect("the pixel under the bar"),
        below,
        "the bar takes exactly the band it reserved"
    );
    save(&frame, "session-bar");

    session.stop_surface("topbar");
    assert!(!session.surfaces().running("topbar"), "the bar is gone");
}

#[test]
fn the_session_asks_the_bar_to_open_its_workspace_menu() {
    let mut session = session_with(&[bar()]);

    // What the bar offers reaches the session as actions, like any app's.
    let deadline = Instant::now() + OPENING;
    while session.surfaces().published("topbar").is_empty() {
        assert!(
            Instant::now() < deadline,
            "the bar published nothing: {:?}",
            session.surfaces().names()
        );
        pump(&mut session);
    }
    let published = session.surfaces().published("topbar");
    let menu = published
        .iter()
        .find(|action| action.action == "workspace-menu")
        .expect("the bar offers its workspace menu");
    let (resource, action) = (menu.resource.clone(), menu.action.clone());

    // The session runs it, as a person's palette or an agent would, and the
    // bar answers on its own actions socket.
    let id = session
        .ask_surface("topbar", &resource, &action, None, "the person")
        .expect("the bar is running");
    let mut ended = None;
    while ended.is_none() {
        assert!(Instant::now() < deadline, "the bar did not answer");
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
        "the bar ran it: {ended:?}"
    );

    let frame = session.compositor().screenshot("HEADLESS-1").expect("shot");
    save(&frame, "session-bar-workspace-menu");
    session.stop_surface("topbar");
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
