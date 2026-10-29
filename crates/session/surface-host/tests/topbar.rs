//! The top bar app hosted in a headless compositor: drawn along the top of
//! the output above the background, with the pointer reaching it where it
//! drew and passing through where it did not.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures: a failed step is the assertion"
)]

use tilo_compositor::{Compositor, OutputConfig, StackPolicy, SurfaceHost as _};
use tilo_surface_host::{AppHost, Placement};
use tilo_topbar::{Link, fixture};

const BACKGROUND: [u8; 4] = [26, 27, 38, 255];

fn bar(size: (u32, u32)) -> AppHost {
    let link = Link::new();
    link.publish(fixture::feed(Some("bar")));
    link.place(fixture::placement(
        Some("bar"),
        (f64::from(size.0), f64::from(size.1)),
    ));
    let component = tilo_topbar::prepare(link, "desktop", "person").unwrap();
    let app = ui::App::from_component(component, ui::headless::fixture_runner_inputs()).unwrap();
    AppHost::new(
        app,
        Placement {
            origin: (0, 0),
            size,
            scale: 1.0,
        },
    )
}

#[test]
fn the_top_bar_draws_along_the_top_of_the_output() {
    let _settled = ui::motion::testing::settled();
    let size = fixture::viewport(Some("bar"));
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).unwrap();
    compositor
        .add_output(&OutputConfig::new(
            "HEADLESS-1",
            i32::try_from(size.0).unwrap(),
            i32::try_from(size.1).unwrap(),
        ))
        .unwrap();
    let host = bar(size);
    compositor.add_host(Box::new(host));
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();

    let drawn_on_top = (0..frame.width)
        .step_by(8)
        .any(|x| frame.pixel(x, 12) != Some(BACKGROUND));
    assert!(
        drawn_on_top,
        "the bar's row shows something other than the background"
    );
    let middle = frame.pixel(frame.width / 2, frame.height / 2);
    assert_eq!(
        middle,
        Some(BACKGROUND),
        "the bar leaves the desktop below it uncovered"
    );
}

#[test]
fn the_pointer_reaches_the_bar_only_where_it_drew() {
    let _settled = ui::motion::testing::settled();
    let size = fixture::viewport(Some("bar"));
    let mut host = bar(size);
    host.prepare_frame();
    let on_bar = (0..size.0)
        .step_by(8)
        .any(|x| host.accepts_pointer(f64::from(x), 12.0));
    assert!(on_bar, "somewhere along the bar takes the pointer");
    assert!(!host.accepts_pointer(f64::from(size.0) / 2.0, f64::from(size.1) / 2.0));
    assert!(!host.accepts_pointer(-1.0, 12.0));
}
