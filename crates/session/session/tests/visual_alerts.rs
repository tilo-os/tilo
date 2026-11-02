//! The visual alert for what the person is told: when they asked for one, a
//! notice the session raises, or a notification an app sends, flashes the
//! screen — the way someone who cannot hear the alert sound is told of it.
//! With the setting off, nothing flashes.

#![cfg(target_os = "linux")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call"
)]

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use tilo_compositor::{A11y, OutputConfig, Screenshot};
use tilo_session::desktop_services::{Bus, Buses, DesktopServices};
use tilo_session::mount::drives::Medium;
use tilo_session::{Options, Session, SessionConfig};

const DISPLAY: &str = "HEADLESS-1";
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// A headless session at its desk on this device's own desktop services,
/// drawn once, with the person's visual alerts set as `visual_alerts` says.
fn session(visual_alerts: bool) -> (Session, Rc<DesktopServices>) {
    let wake = tilo_service::LoopWake::default();
    let (side, _link) = tilo_session::portal_link::channel(wake.handle());
    let desktop = Rc::new(
        DesktopServices::start(
            &Buses {
                session: Bus::None,
                system: Bus::None,
            },
            &wake.handle(),
            std::sync::Arc::new(tilo_portal::NoCapture),
            &side,
            None,
        )
        .expect("the desktop services"),
    );
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            services: desktop.services(),
            wake,
            desktop: Some(Rc::clone(&desktop)),
            collab: false,
            config: SessionConfig {
                a11y: A11y {
                    visual_alerts,
                    ..A11y::default()
                },
                ..SessionConfig::default()
            },
            ..Options::default()
        },
        &[OutputConfig::new(DISPLAY, WIDTH, HEIGHT)],
    )
    .expect("session");
    session.step(Duration::from_millis(1)).expect("a frame");
    (session, desktop)
}

/// The drive the person just plugged in, whose notice the session raises.
fn plugged_in() -> Medium {
    Medium {
        device: "/org/freedesktop/UDisks2/block_devices/sdb1".to_owned(),
        label: Some("Backup".to_owned()),
        size: 64 * 1024 * 1024 * 1024,
        mount: PathBuf::from("/run/media/ada/Backup"),
    }
}

/// How bright `frame` is on average, in 0..=255: the visual alert covers the
/// whole screen, and a banner does not.
fn brightness(frame: &Screenshot) -> f64 {
    let lit: u32 = frame
        .rgba
        .chunks(4)
        .map(|pixel| {
            pixel
                .iter()
                .take(3)
                .map(|channel| u32::from(*channel))
                .sum::<u32>()
        })
        .sum();
    f64::from(lit) / (frame.rgba.len() / 4 * 3) as f64
}

/// The frame on screen now.
fn frame(session: &mut Session) -> Screenshot {
    session.compositor().screenshot(DISPLAY).expect("a frame")
}

#[test]
fn a_notice_flashes_the_screen_when_visual_alerts_are_on() {
    let _settled = ui::motion::testing::settled();
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let (mut session, desktop) = session(true);
    let plain = brightness(&frame(&mut session));

    let _sent = tilo_session::notices::announce_medium(desktop.notifications(), &plugged_in());
    session.step(Duration::from_millis(1)).expect("a frame");

    let up = session.notices().log().active().to_vec();
    assert_eq!(up.len(), 1, "the notice is up as a banner");
    assert!(
        up[0].title.contains("Backup"),
        "the banner is the drive's notice: {}",
        up[0].title
    );
    let flashed = brightness(&frame(&mut session));
    assert!(
        flashed > plain + 60.0,
        "the whole screen flashes for it: {plain} then {flashed}"
    );
}

#[test]
fn nothing_flashes_when_visual_alerts_are_off() {
    let _settled = ui::motion::testing::settled();
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let (mut session, desktop) = session(false);
    let plain = brightness(&frame(&mut session));

    let _sent = tilo_session::notices::announce_medium(desktop.notifications(), &plugged_in());
    session.step(Duration::from_millis(1)).expect("a frame");

    let up = session.notices().log().active().to_vec();
    assert_eq!(up.len(), 1, "the notice is up as a banner");
    assert!(
        up[0].title.contains("Backup"),
        "the banner is the drive's notice: {}",
        up[0].title
    );
    let told = brightness(&frame(&mut session));
    assert!(
        (told - plain).abs() < 5.0,
        "the desk is unchanged by it: {plain} then {told}"
    );
}
