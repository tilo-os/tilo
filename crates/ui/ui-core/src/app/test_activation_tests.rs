//! A development session's test control activates a control of the focused
//! window by its test id, over the window's shell-ui connection: the focused
//! window presses it, any other window of the app leaves it alone.

#![cfg(all(feature = "native", unix))]

use std::cell::Cell;
use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::rc::Rc;

use crate::floating_surface::{FloatingSurfaceHostMessage, FloatingSurfaceServiceClient};

use super::super::App;
use super::super::framework_history_tests::{app_with_root, layout};

/// A window showing one button, `go`, counting its presses, on a shell-ui
/// connection whose shell end the test holds.
fn window() -> (App, UnixStream, Rc<Cell<u32>>) {
    let pressed = Rc::new(Cell::new(0));
    let count = Rc::clone(&pressed);
    let button =
        crate::widget::Button::new("go", "Go").on_effect(move || count.set(count.get() + 1));
    let column = crate::widget::Column::new().push(Box::new(button));
    let mut app = app_with_root(crate::State::new(), Box::new(column));
    let (app_side, shell_side) = UnixStream::pair().unwrap();
    app.floating.client = Some(FloatingSurfaceServiceClient::from_stream(app_side).unwrap());
    layout(&mut app);
    (app, shell_side, pressed)
}

fn activate(shell: &mut UnixStream, test_id: &str) {
    let mut frame = serde_json::to_vec(&FloatingSurfaceHostMessage::ActivateTestId {
        test_id: test_id.to_owned(),
    })
    .unwrap();
    frame.push(b'\n');
    shell.write_all(&frame).unwrap();
}

#[test]
fn the_focused_window_presses_the_control_its_test_id_names() {
    let (mut app, mut shell, pressed) = window();
    app.input.window_focused = true;
    activate(&mut shell, "go");
    app.pump_floating_surface_service();
    assert_eq!(pressed.get(), 1, "the focused window pressed `go`");

    activate(&mut shell, "nothing-here");
    app.pump_floating_surface_service();
    assert_eq!(
        pressed.get(),
        1,
        "a test id the window does not show presses nothing"
    );
}

#[test]
fn a_window_without_the_keyboard_leaves_its_controls_alone() {
    let (mut app, mut shell, pressed) = window();
    app.input.window_focused = false;
    activate(&mut shell, "go");
    app.pump_floating_surface_service();
    assert_eq!(pressed.get(), 0, "only the focused window of the app acts");
}
