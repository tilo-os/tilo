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
    Recipient, RecipientKind,
};
use crate::window_targets::{TargetAction, WindowTarget};

use super::App;
use super::AppSurface;
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

/// A window whose own menu carries the send-to box, on a shell-ui connection whose shell end the
/// test holds.
fn box_window() -> (App, UnixStream) {
    let content = crate::widget::Column::new().push(Box::new(crate::widget::Spacer::fixed(300.0)));
    let area = crate::widget::ContextMenuArea::new(Box::new(content), |_| Vec::new());
    let mut app = App::new_with_controlled_inputs(crate::State::new(), Box::new(area));
    let (app_side, shell_side) = UnixStream::pair().unwrap();
    app.floating.client = Some(FloatingSurfaceServiceClient::from_stream(app_side).unwrap());
    (app, shell_side)
}

struct Window;
impl AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(400, 400)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _cursor: winit::window::CursorIcon) {}
}

/// The frames that draw what the window holds: the box a pass was asked for opens on the first.
fn drawn(app: &mut App) {
    for _ in 0..4 {
        let mut scene = crate::vello::Scene::new();
        if !app.surface_frame(&Window, &mut scene) || !app.unsettled() {
            break;
        }
    }
}

/// The names the window told the shell of, this pass.
fn told(app: &mut App, shell: &mut UnixStream) -> Vec<WindowTarget> {
    described(app, shell).expect("the window describes its controls")
}

/// The send key opens the window's box, and the box's own controls reach the shell as the
/// window's: the field that names whom it goes to, the field that writes the message, what it is
/// about, and Send — the same list, whether a menu opened the box or the key did.
#[test]
fn the_box_a_send_key_opens_tells_the_shell_its_controls() {
    let (mut app, mut shell) = box_window();
    drawn(&mut app);
    let _window = told(&mut app, &mut shell);

    assert!(
        app.apply_floating_host_messages(vec![FloatingSurfaceHostMessage::ShareSelection {
            to: Vec::new(),
            pointing: false,
        }]),
        "the shell asked the window for its box"
    );
    drawn(&mut app);
    let targets = told(&mut app, &mut shell);
    let names: Vec<&str> = targets.iter().map(|target| target.name.as_str()).collect();
    for wanted in [
        crate::i18n::I18nManager::lookup_plain("send-to-recipients-name"),
        crate::i18n::I18nManager::lookup_plain("send-to-message-name"),
        crate::i18n::I18nManager::lookup_plain("send-to-send"),
    ] {
        assert!(
            names.contains(&wanted.as_str()),
            "the box's {wanted:?} is one of its controls: {names:?}"
        );
    }
    let send = targets
        .iter()
        .find(|target| target.name == crate::i18n::I18nManager::lookup_plain("send-to-send"))
        .expect("Send");
    assert_eq!(send.role, "Button");
    assert!(
        send.rect[2] > send.rect[0] && send.rect[3] > send.rect[1],
        "where the person reaches it: {:?}",
        send.rect
    );
    let field = targets
        .iter()
        .find(|target| {
            target.name == crate::i18n::I18nManager::lookup_plain("send-to-recipients-name")
        })
        .expect("the field that names whom it goes to");
    assert_eq!(field.role, "TextInput");
    assert!(
        field.rect[2] > field.rect[0],
        "a field is a control where it is drawn, though a press does not operate it: {:?}",
        field.rect
    );
    assert!(
        crate::shared_selection::take_open_send_box().is_none(),
        "the box stood open, it was not left for the next frame"
    );
}

/// Send is pressed through the target the shell was told of, and the window sends what the box
/// holds: the path an assistive technology's activation takes, not a point aimed at.
#[test]
fn the_sends_send_is_pressed_through_the_target_and_sends_what_the_box_holds() {
    let (mut app, mut shell) = box_window();
    drawn(&mut app);
    let _window = told(&mut app, &mut shell);
    let adalovelace = Recipient::found(RecipientKind::Person, "person:ada", "Ada");
    crate::shared_selection::open_send_box(
        None,
        None,
        Some("Ship the launch notes"),
        std::slice::from_ref(&adalovelace),
    );
    drawn(&mut app);
    let targets = told(&mut app, &mut shell);
    let names: Vec<&str> = targets.iter().map(|target| target.name.as_str()).collect();
    assert!(
        names.contains(&adalovelace.name.as_str()),
        "the chip that takes a recipient off is a control of the box: {names:?}"
    );
    let send = targets
        .iter()
        .find(|target| target.name == crate::i18n::I18nManager::lookup_plain("send-to-send"))
        .expect("Send");
    assert!(
        send.actions.contains(&TargetAction::Activate),
        "with something to send, Send answers the press: {send:?}"
    );

    press(
        &mut shell,
        crate::i18n::I18nManager::lookup_plain("send-to-send").as_str(),
        "Button",
        TargetAction::Activate,
    );
    app.pump_floating_surface_service();
    let mut sent_message = None;
    for _ in 0..4 {
        drawn(&mut app);
        app.pump_floating_surface_service();
        sent_message = sent(&mut shell)
            .into_iter()
            .find_map(|message| match message {
                FloatingSurfaceClientMessage::SendTo { to, text, .. } => Some((to, text)),
                _ => None,
            });
        if sent_message.is_some() {
            break;
        }
    }
    let (to, text) = sent_message.expect("the window sent what the box held");
    assert_eq!(text, "Ship the launch notes");
    assert_eq!(to, [adalovelace], "to whom the box was addressed");
}
