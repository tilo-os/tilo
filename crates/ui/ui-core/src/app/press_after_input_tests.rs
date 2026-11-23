//! A control the shell presses reaches the window after the input the shell sent before it: a
//! save pressed as the person's typing is still on its way reads every character typed.

#![cfg(all(feature = "native", unix))]

use std::cell::RefCell;
use std::rc::Rc;

use vello::kurbo::{Point, Rect};

use crate::floating_surface::FloatingSurfaceHostMessage;
use crate::synthetic_input::{char_key, cursor_moved, mouse_press, mouse_release};
use crate::window_targets::TargetAction;

use super::App;
use super::framework_history_tests::{app_with_root, layout};

struct Window;

impl crate::app::AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(900, 650)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: winit::window::CursorIcon) {}
}

/// A window showing a field and the button that saves what it holds; `secret` makes it a
/// password field, whose state a form's save reads as it reads a plain field's.
fn window(secret: bool) -> (App, Rc<RefCell<Vec<String>>>) {
    let saved = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&saved);
    let name = crate::LocalState::new(String::new());
    let read = name.clone();
    let field = crate::widget::TextInput::from_state(&name)
        .with_id("name")
        .with_accessible_name("Display name")
        .with_secret(secret);
    let save = crate::widget::Button::new("save", "Save").on_click(move || {
        sink.borrow_mut().push(read.get());
    });
    let column = crate::widget::Column::new()
        .push(Box::new(field))
        .push(Box::new(save));
    let mut app = app_with_root(crate::State::new(), Box::new(column));
    layout(&mut app);
    (app, saved)
}

/// Tell the window the shell pressed the control `name` of its content, after the input with
/// the serial `after_input` (the shell's).
fn press_from_shell(app: &mut App, name: &str, role: &str, after_input: Option<u32>) {
    app.test_deliver_host_message(FloatingSurfaceHostMessage::PressTarget {
        name: name.to_owned(),
        role: role.to_owned(),
        identity: None,
        action: TargetAction::Activate,
        after_input,
    });
}

/// Where the window draws the control `test_id`.
fn find_rect(root: &dyn crate::widget::Widget, test_id: &str) -> Option<Rect> {
    if root.test_id() == Some(test_id) {
        return Some(root.get_rect());
    }
    root.children()
        .into_iter()
        .find_map(|child| find_rect(child, test_id))
}

/// Point at `at` and press the left button, as a person does.
fn click(app: &mut App, at: Point) {
    app.surface_input(&Window, cursor_moved(at.x, at.y), false);
    app.surface_input(&Window, mouse_press(), false);
    app.surface_input(&Window, mouse_release(), false);
}

/// A save the shell presses while the typing before it is still queued for the window runs
/// after that typing: the field holds every character typed before the press.
#[test]
fn a_save_pressed_while_the_typing_before_it_is_queued_runs_after_it() {
    let (mut app, saved) = window(false);
    let field = find_rect(app.root_widget.as_ref(), "name")
        .unwrap_or_else(|| panic!("the window draws the name field"));
    click(&mut app, field.center());
    // The keys the person typed are queued for the window but not handed it yet: the shell
    // wrote the press after that input, before the window read it.
    press_from_shell(&mut app, "Save", "Button", Some(7));
    for ch in "Ada".chars() {
        app.surface_input(&Window, char_key(&ch.to_string()), false);
    }
    assert!(
        saved.borrow().is_empty(),
        "the save waits for the typing before it: {:?}",
        saved.borrow()
    );
    // The window takes the typing its event loop held: the save reads it, in order.
    app.input_handled(7);
    assert_eq!(
        saved.borrow().as_slice(),
        ["Ada"],
        "the save reads every character typed before the press"
    );
}

/// A save the shell presses with no input queued for the window runs at once: the ordering
/// holds no press back that has nothing before it.
#[test]
fn a_save_pressed_with_nothing_queued_runs_at_once() {
    let (mut app, saved) = window(false);
    press_from_shell(&mut app, "Save", "Button", None);
    assert_eq!(
        saved.borrow().as_slice(),
        [""],
        "the save runs with what the field holds now"
    );
}

/// A password field writes what was typed into it where its form's save reads it, as a plain
/// field does: a save pressed after the person typed their password receives it, never an
/// empty draft.
#[test]
fn a_password_typed_before_the_press_is_what_the_save_reads() {
    let (mut app, saved) = window(true);
    let field = find_rect(app.root_widget.as_ref(), "name")
        .unwrap_or_else(|| panic!("the window draws the password field"));
    click(&mut app, field.center());
    press_from_shell(&mut app, "Save", "Button", Some(9));
    for ch in "s3cret".chars() {
        app.surface_input(&Window, char_key(&ch.to_string()), false);
    }
    assert!(
        saved.borrow().is_empty(),
        "the save waits for the password typed before it"
    );
    app.input_handled(9);
    assert_eq!(
        saved.borrow().as_slice(),
        ["s3cret"],
        "the save reads the password the person typed"
    );
}
