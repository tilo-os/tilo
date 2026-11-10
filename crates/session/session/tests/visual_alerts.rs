//! The visual alert for what the person is told: when they asked for one, a notice the session
//! raises, or a notification an app sends, lights the edge of the screen — the way someone who
//! cannot hear the alert sound is told of it. With the setting off, nothing changes.

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

mod common;

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use tilo_compositor::{A11y, OutputConfig, Screenshot};
use tilo_session::desktop_services::{Bus, Buses, DesktopServices};
use tilo_session::drives::Medium;
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
    let mut session = crate::common::headless_on(
        &[OutputConfig::new(DISPLAY, WIDTH, HEIGHT)],
        Options {
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
    );
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

/// How wide the alert's edge is on screen, physical pixels: the band it lights along each side of a
/// display.
const EDGE: u32 = 4;

/// How bright the pixels of `frame` inside `area` are on average, in
/// 0..=255.
fn brightness(frame: &Screenshot, area: (u32, u32, u32, u32)) -> f64 {
    let (x0, y0, x1, y1) = area;
    let mut lit = 0u32;
    let mut pixels = 0u32;
    for y in y0..y1 {
        for x in x0..x1 {
            let Some(pixel) = frame.pixel(x, y) else {
                continue;
            };
            lit += pixel
                .iter()
                .take(3)
                .map(|channel| u32::from(*channel))
                .sum::<u32>();
            pixels += 1;
        }
    }
    assert!(pixels > 0, "the area is on screen");
    f64::from(lit) / f64::from(pixels * 3)
}

/// How bright the display's own edge is: the band the visual alert lights.
fn edge_brightness(frame: &Screenshot) -> f64 {
    let (w, h) = (frame.width, frame.height);
    let bands = [
        (0, 0, w, EDGE),
        (0, h - EDGE, w, h),
        (0, 0, EDGE, h),
        (w - EDGE, 0, w, h),
    ];
    bands
        .into_iter()
        .map(|band| brightness(frame, band))
        .sum::<f64>()
        / 4.0
}

/// How bright the middle of the screen is: what an alert along the edge leaves as it was.
fn centre_brightness(frame: &Screenshot) -> f64 {
    let (w, h) = (frame.width, frame.height);
    brightness(frame, (w / 3, h / 3, w * 2 / 3, h * 2 / 3))
}

/// The frame on screen now.
fn frame(session: &mut Session) -> Screenshot {
    session.compositor().screenshot(DISPLAY).expect("a frame")
}

#[test]
fn a_notice_lights_the_screen_edge_when_visual_alerts_are_on() {
    let _settled = ui::motion::testing::settled();
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let (mut session, desktop) = session(true);
    let plain = frame(&mut session);
    let (edge, middle) = (edge_brightness(&plain), centre_brightness(&plain));

    let _sent = tilo_session::notices::announce_medium(desktop.notifications(), &plugged_in());
    session.step(Duration::from_millis(1)).expect("a frame");

    let up = session.notices().log().active().to_vec();
    assert_eq!(up.len(), 1, "the notice is up as a banner");
    assert!(
        up[0].title.contains("Backup"),
        "the banner is the drive's notice: {}",
        up[0].title
    );
    let lit = frame(&mut session);
    assert!(
        edge_brightness(&lit) > edge + 20.0,
        "the display's edge lights up for it: {edge} then {}",
        edge_brightness(&lit)
    );
    assert!(
        (centre_brightness(&lit) - middle).abs() < 5.0,
        "and the middle of the screen is left alone: {middle} then {}",
        centre_brightness(&lit)
    );
}

#[test]
fn nothing_lights_when_visual_alerts_are_off() {
    let _settled = ui::motion::testing::settled();
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let (mut session, desktop) = session(false);
    let plain = frame(&mut session);
    let (edge, middle) = (edge_brightness(&plain), centre_brightness(&plain));

    let _sent = tilo_session::notices::announce_medium(desktop.notifications(), &plugged_in());
    session.step(Duration::from_millis(1)).expect("a frame");

    let up = session.notices().log().active().to_vec();
    assert_eq!(up.len(), 1, "the notice is up as a banner");
    assert!(
        up[0].title.contains("Backup"),
        "the banner is the drive's notice: {}",
        up[0].title
    );
    let told = frame(&mut session);
    assert!(
        (edge_brightness(&told) - edge).abs() < 5.0,
        "the display's edge does not light up: {edge} then {}",
        edge_brightness(&told)
    );
    assert!(
        (centre_brightness(&told) - middle).abs() < 5.0,
        "and the desk is unchanged by it: {middle} then {}",
        centre_brightness(&told)
    );
}
