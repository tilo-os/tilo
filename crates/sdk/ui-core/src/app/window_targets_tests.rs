//! A window tells the shell what its own content offers by name, and acts on
//! what the shell presses there: one description per change of its tree, and
//! every press through the path an assistive technology's activation takes.

#![cfg(all(feature = "native", unix))]

use std::cell::Cell;
use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::rc::Rc;

use crate::floating_surface::{
    FloatingSurfaceClientMessage, FloatingSurfaceHostMessage, FloatingSurfaceServiceClient,
};
use crate::window_targets::{TargetAction, WindowTarget};

use super::App;
use super::framework_history_tests::{app_with_root, layout};

/// A window showing one button, `go`, counting its presses, on a shell-ui
/// connection whose shell end the test holds.
fn window() -> (App, UnixStream, Rc<Cell<u32>>) {
    let pressed = Rc::new(Cell::new(0));
    let count = Rc::clone(&pressed);
    let button =
        crate::widget::Button::new("go", "Go").on_click(move || count.set(count.get() + 1));
    let column = crate::widget::Column::new().push(Box::new(button));
    let mut app = app_with_root(crate::State::new(), Box::new(column));
    let (app_side, shell_side) = UnixStream::pair().unwrap();
    app.floating.client = Some(FloatingSurfaceServiceClient::from_stream(app_side).unwrap());
    layout(&mut app);
    (app, shell_side, pressed)
}

/// What the window wrote on its connection since the last call, whole frames
/// only.
fn sent(shell: &mut UnixStream) -> Vec<FloatingSurfaceClientMessage> {
    shell.set_nonblocking(true).unwrap();
    let mut text = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match shell.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => text.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("the connection broke: {error}"),
        }
    }
    let whole = text
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(&[][..], |end| &text[..end]);
    String::from_utf8(whole.to_vec())
        .expect("frames are text")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line.trim()).expect("a shell-ui frame"))
        .collect()
}

/// The controls the window described on this pass.
fn described(app: &mut App, shell: &mut UnixStream) -> Option<Vec<WindowTarget>> {
    app.pump_floating_surface_service();
    sent(shell).into_iter().find_map(|message| match message {
        FloatingSurfaceClientMessage::WindowTargets { targets, .. } => Some(targets),
        _ => None,
    })
}

/// What the shell presses on the window: the control `name` of `role`, as an
/// assistive technology acts on it.
fn press(shell: &mut UnixStream, name: &str, role: &str, action: TargetAction) {
    let mut frame = serde_json::to_vec(&FloatingSurfaceHostMessage::PressTarget {
        name: name.to_owned(),
        role: role.to_owned(),
        identity: None,
        action,
    })
    .expect("a shell-ui frame");
    frame.push(b'\n');
    shell.write_all(&frame).unwrap();
}

#[test]
fn a_window_describes_its_controls_when_they_change_and_acts_on_what_is_pressed() {
    let (mut app, mut shell, pressed) = window();

    let targets = described(&mut app, &mut shell).expect("the window describes its controls");
    let go = targets
        .iter()
        .find(|target| target.name == "Go")
        .expect("the button is one of the window's controls");
    assert_eq!(go.role, "Button");
    assert!(go.actions.contains(&TargetAction::Activate));
    assert!(
        go.rect[2] > go.rect[0] && go.rect[3] > go.rect[1],
        "a control is described where it is drawn: {:?}",
        go.rect
    );

    // Nothing changed since: the window has nothing more to say.
    app.pump_floating_surface_service();
    assert!(
        !sent(&mut shell)
            .iter()
            .any(|message| matches!(message, FloatingSurfaceClientMessage::WindowTargets { .. })),
        "a pass that changed nothing sends no controls"
    );

    // The frame that lays out what the window described, as the runner draws
    // one before a person can press anything in it.
    layout(&mut app);

    press(&mut shell, "Go", "Button", TargetAction::Activate);
    app.pump_floating_surface_service();
    assert_eq!(pressed.get(), 1, "the pressed control acted");
}

#[test]
fn a_control_the_window_no_longer_shows_presses_nothing() {
    let (mut app, mut shell, pressed) = window();
    let _targets = described(&mut app, &mut shell).expect("the window describes its controls");
    layout(&mut app);

    press(&mut shell, "Gone", "Button", TargetAction::Activate);
    app.pump_floating_surface_service();
    assert_eq!(
        pressed.get(),
        0,
        "a control the window does not show acts nothing"
    );

    // The name alone is not the control: a role the window does not show it
    // as finds nothing either.
    press(&mut shell, "Go", "ListItem", TargetAction::Activate);
    app.pump_floating_surface_service();
    assert_eq!(pressed.get(), 0, "the role says which control is meant");
}

/// A window whose one control does `tube.video`'s `play`, as a hosted window wraps the control
/// its app published an action for.
fn doing_window() -> (App, Rc<Cell<u32>>) {
    let pressed = Rc::new(Cell::new(0));
    let count = Rc::clone(&pressed);
    let button =
        crate::widget::Button::new("go", "Play").on_click(move || count.set(count.get() + 1));
    let identified = crate::widget::Identified::new(Box::new(button), doing());
    let column = crate::widget::Column::new().push(Box::new(identified));
    let mut app = app_with_root(crate::State::new(), Box::new(column));
    layout(&mut app);
    (app, pressed)
}

/// What the window's control does: the record kind, and the operation on it.
const DOING: (&str, &str) = ("tube.video", "play");

/// [`DOING`] as a control publishes it.
fn doing() -> String {
    format!("{}{}{}", DOING.0, '\u{1f}', DOING.1)
}

#[test]
fn a_control_that_runs_an_app_action_is_listed_and_pressed_by_what_it_does() {
    let identity = doing();
    let (mut app, pressed) = doing_window();
    let controls = app.window_controls();
    let control = controls
        .iter()
        .find(|control| control.identity.as_deref() == Some(identity.as_str()))
        .unwrap_or_else(|| {
            panic!(
                "the window lists the control by what it does: {:?}",
                controls
                    .iter()
                    .map(|control| &control.name)
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(
        control.name, "Play",
        "and names it in the person's language"
    );
    assert!(
        control.rect.2 > 0.0 && control.rect.3 > 0.0,
        "where a person reaches it: {:?}",
        control.rect
    );
    assert!(
        app.press_window_control(&identity, TargetAction::Activate),
        "the window acts on the control the identity names"
    );
    assert_eq!(pressed.get(), 1, "and the press reached it");
}
