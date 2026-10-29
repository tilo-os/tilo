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

#[test]
fn the_bar_shows_the_pointer_shape_its_controls_ask_for() {
    let _settled = ui::motion::testing::settled();
    let size = fixture::viewport(Some("bar"));
    let mut host = bar(size);
    host.prepare_frame();
    assert_eq!(host.cursor(), tilo_compositor::CursorIcon::Default);
    let shapes: Vec<_> = (0..size.0)
        .step_by(4)
        .map(|x| {
            host.pointer_motion(f64::from(x), 12.0);
            host.prepare_frame();
            host.cursor()
        })
        .collect();
    assert!(
        shapes
            .iter()
            .any(|shape| *shape != tilo_compositor::CursorIcon::Default),
        "some control along the bar asks for its own pointer shape"
    );
}

#[test]
fn files_dragged_over_the_bar_are_taken_and_anything_else_refused() {
    let _settled = ui::motion::testing::settled();
    let size = fixture::viewport(Some("bar"));
    let mut host = bar(size);
    host.prepare_frame();
    let text = vec!["text/plain;charset=utf-8".to_owned()];
    assert_eq!(host.drop_offered(&text, 10.0, 12.0), None);
    let files = vec!["text/plain".to_owned(), "text/uri-list".to_owned()];
    assert_eq!(
        host.drop_offered(&files, 10.0, 12.0).as_deref(),
        Some("text/uri-list")
    );
    host.drop_data("text/uri-list", b"file:///home/ada/notes.txt\r\n".to_vec());
    host.drag_ended(true);
}
