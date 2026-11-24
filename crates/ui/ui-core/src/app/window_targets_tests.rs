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
        after_input: None,
    })
    .expect("a shell-ui frame");
    frame.push(b'\n');
    shell.write_all(&frame).unwrap();
}

/// The window sends the session its accessibility tree when it changes, and an
/// action a client of the session's export takes on one of its nodes reaches
/// the window as its own assistive client's action does.
#[test]
fn a_window_sends_its_tree_and_acts_on_what_a_client_of_the_export_asks() {
    use crate::accesskit::{Action, ActionRequest, NodeId, Role, TreeId};

    let (mut app, mut shell, pressed) = window();
    app.window.shown_title = Some("Tasks".to_owned());

    app.pump_floating_surface_service();
    let tree = sent(&mut shell)
        .into_iter()
        .find_map(|message| match message {
            FloatingSurfaceClientMessage::AccessibilityTree { tree, name, .. } => {
                assert_eq!(name, "Tasks", "the tree is named as the window is");
                Some(tree)
            }
            _ => None,
        })
        .expect("the window sends its tree");
    assert!(
        tree.nodes
            .iter()
            .any(|(_, node)| node.label() == Some("Go") && node.role() == Role::Button),
        "the button is in the window's tree"
    );
    let root = tree.tree.as_ref().expect("a window names its root").root;
    assert_eq!(
        tree.nodes
            .iter()
            .find(|(id, _)| *id == root)
            .map(|(_, node)| node.role()),
        Some(Role::Window),
        "the tree is the window's"
    );

    // Nothing changed since: the window has nothing more to say.
    app.pump_floating_surface_service();
    assert!(
        !sent(&mut shell).iter().any(|message| matches!(
            message,
            FloatingSurfaceClientMessage::AccessibilityTree { .. }
        )),
        "a pass that changed nothing sends no tree"
    );

    // The frame that lays out the window, as the runner draws one before a person can act in it,
    // and the node the laid-out tree names the button by.
    layout(&mut app);
    let go = app
        .build_a11y_tree_update(1.0)
        .nodes
        .into_iter()
        .find(|(_, node)| node.label() == Some("Go") && node.role() == Role::Button)
        .map(|(id, _)| id)
        .expect("the button is in the window's tree");

    // A client of the session's export activates the button: the window acts.
    let mut frame = serde_json::to_vec(&FloatingSurfaceHostMessage::AccessibilityAction {
        request: ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: go,
            data: None,
        },
        after_input: None,
    })
    .expect("a shell-ui frame");
    frame.push(b'\n');
    shell.write_all(&frame).unwrap();
    app.pump_floating_surface_service();
    assert_eq!(
        pressed.get(),
        1,
        "the action reached the window over its shell-ui connection and acted in it"
    );
    assert_ne!(go, NodeId(0), "the action names the button's node");
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

/// A composer row: a field that sends what it holds on Enter and an icon-only Send button beside
/// it, both running the one send action the app published.
fn composer_window() -> App {
    let send = format!("chat.Channel{}send", '\u{1f}');
    let field = crate::testing::OwnedText::new(
        "draft",
        crate::widget::TextInput::new(vec![crate::presence::PathSegment::Field("draft".into())])
            .with_id("draft")
            .with_accessible_name("Message #general"),
    );
    let button = crate::widget::Button::icon_only("send", crate::icons::Icon::Send, "Send");
    let row = crate::widget::Row::new()
        .push(Box::new(crate::widget::Identified::new(
            Box::new(field),
            send.clone(),
        )))
        .push(Box::new(crate::widget::Identified::new(
            Box::new(button),
            send,
        )));
    let mut app = app_with_root(crate::State::new(), Box::new(row));
    layout(&mut app);
    app
}

/// The field and the Send button beside it run the same action and are still two controls: the
/// icon-only button is listed by its label, where a person reaches it, beside the field.
#[test]
fn an_icon_only_button_in_a_composer_row_is_listed_by_its_label() {
    let mut app = composer_window();
    let controls = app.window_controls();
    let names: Vec<&str> = controls
        .iter()
        .map(|control| control.name.as_str())
        .collect();
    let send = controls
        .iter()
        .find(|control| control.name == "Send")
        .unwrap_or_else(|| panic!("the Send button is one of the window's controls: {names:?}"));
    assert!(
        send.rect.2 > 0.0 && send.rect.3 > 0.0,
        "drawn where a person reaches it: {:?}",
        send.rect
    );
    assert!(
        names.contains(&"Message #general"),
        "and so is the field that sends on Enter: {names:?}"
    );
}

/// What a window shows that has no menu of its own, counting the keys that reach it.
struct Keys(Rc<Cell<u32>>);

impl crate::widget::Widget for Keys {
    crate::testing::inert_widget_methods!(leaf_layout);
    fn name(&self) -> &'static str {
        "keys"
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        _ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        if matches!(event, winit::event::WindowEvent::KeyboardInput { .. }) {
            self.0.set(self.0.get() + 1);
        }
    }
}

/// A window with no menu of its own, on a shell-ui connection whose shell end the test holds, and
/// the keys its content heard.
fn box_window() -> (App, UnixStream, Rc<Cell<u32>>) {
    let keys = Rc::new(Cell::new(0));
    let column = crate::widget::Column::new()
        .push(Box::new(crate::widget::Button::new("go", "Go")))
        .push(Box::new(Keys(keys.clone())));
    let mut app = App::new_with_controlled_inputs(crate::State::new(), Box::new(column));
    let (app_side, shell_side) = UnixStream::pair().unwrap();
    app.floating.client = Some(FloatingSurfaceServiceClient::from_stream(app_side).unwrap());
    (app, shell_side, keys)
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
    let (mut app, mut shell, _keys) = shells_box();
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
}

/// Send is pressed through the target the shell was told of, and the window sends what the box
/// holds: the path an assistive technology's activation takes, not a point aimed at.
#[test]
fn the_sends_send_is_pressed_through_the_target_and_sends_what_the_box_holds() {
    let (mut app, mut shell, _keys) = box_window();
    drawn(&mut app);
    let _window = told(&mut app, &mut shell);
    let adalovelace = Recipient::found(RecipientKind::Person, "ada", "Ada");
    app.open_send_box(
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
    let mut delivered = None;
    for _ in 0..4 {
        drawn(&mut app);
        app.pump_floating_surface_service();
        delivered = crate::shared_selection::take_delivered().into_iter().next();
        if delivered.is_some() {
            break;
        }
    }
    let arguments = delivered.expect("the window sent what the box held");
    assert_eq!(arguments["text"], "Ship the launch notes");
    assert_eq!(
        arguments["to"][0]["name"], "Ada",
        "to whom the box was addressed"
    );
}

/// [`box_window`] with the box the shell asks for, drawn and described once.
fn shells_box() -> (App, UnixStream, Rc<Cell<u32>>) {
    let (mut app, mut shell, keys) = box_window();
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
    let _ = crate::shared_selection::take_send_to();
    (app, shell, keys)
}

/// The box the shell asks a window for stands though nothing the app shows has a menu: the
/// window's own carries it and gives it the pointer and the keys: a key typed next is the box's,
/// not the content's.
#[test]
fn a_window_with_no_menu_of_its_own_stands_the_box_and_it_takes_the_keys() {
    let (mut app, _shell, keys) = shells_box();
    assert!(
        app.window_box.is_open(),
        "the box hears the pointer while it stands"
    );
    app.surface_input(&Window, crate::synthetic_input::char_key("w"), false);
    assert_eq!(keys.get(), 0, "the key typed next went to the box");
    // The key went to the message, so Enter sends it to the new agent the box opened with.
    app.surface_input(
        &Window,
        crate::synthetic_input::named_key(winit::keyboard::NamedKey::Enter),
        false,
    );
    let sent = crate::shared_selection::take_send_to();
    assert_eq!(sent.len(), 1, "Enter in the message sends: {sent:?}");
    assert_eq!(sent[0].text, "w");
    assert_eq!(
        sent[0].to.iter().map(|to| to.kind).collect::<Vec<_>>(),
        [RecipientKind::NewAgent],
        "to a new agent, as nobody else was named"
    );
    assert!(!app.window_box.is_open(), "the box went");
    assert_eq!(keys.get(), 0);
}

/// Escape puts away the box the shell asked for, and the next key is the content's again.
#[test]
fn escape_puts_the_shells_box_away() {
    let (mut app, _shell, keys) = shells_box();
    assert!(app.window_box.is_open(), "the box stands");
    app.surface_input(
        &Window,
        crate::synthetic_input::named_key(winit::keyboard::NamedKey::Escape),
        false,
    );
    assert!(!app.window_box.is_open(), "Escape put it away");
    app.surface_input(&Window, crate::synthetic_input::char_key("w"), false);
    assert_eq!(keys.get(), 1, "the content hears the keys again");
}

/// An agent reaches the box the shell asked for through the window's described controls, as a
/// person's keys do: it writes the message and presses Send, and the window sends what it wrote.
#[test]
fn an_agent_writes_in_the_shells_box_and_sends_it() {
    let (mut app, _shell, keys) = shells_box();
    let control = |app: &mut App, name: &str| {
        app.refresh_semantic_tree();
        let wanted = crate::i18n::I18nManager::lookup_plain(name);
        app.semantic_tree()
            .iter()
            .find(|node| node.name.as_deref() == Some(wanted.as_str()))
            .map(|node| node.id.clone())
            .unwrap_or_else(|| panic!("the box describes {wanted:?}"))
    };
    let message = control(&mut app, "send-to-message-name");
    assert!(
        app.invoke_semantic_action(&message, "set-value", Some("Ship the launch notes"))
            .expect("the message takes a value"),
        "the agent wrote the message"
    );
    let send = control(&mut app, "send-to-send");
    assert!(
        app.invoke_semantic_action(&send, "activate", None)
            .expect("Send takes a press"),
        "the agent pressed Send"
    );
    let sent = crate::shared_selection::take_send_to();
    assert_eq!(sent.len(), 1, "Send sent the box: {sent:?}");
    assert_eq!(sent[0].text, "Ship the launch notes");
    assert_eq!(
        sent[0].to.iter().map(|to| to.kind).collect::<Vec<_>>(),
        [RecipientKind::NewAgent],
    );
    assert!(!app.window_box.is_open(), "the box went");
    assert_eq!(keys.get(), 0, "nothing reached the content");
}
