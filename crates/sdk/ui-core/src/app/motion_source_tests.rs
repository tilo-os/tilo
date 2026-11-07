//! What a surface's input says about the motion a change it caused follows:
//! a press moves, a key lands at once. An app shown on a host's surface (the
//! session's desk, its panels and its sheets) reaches the toolkit through
//! the same entry as one in a window of its own.

use super::*;
use crate::motion::{input_source, note_source};
use crate::tokens::MotionSource;
use winit::keyboard::{Key, NamedKey};

struct Window;
impl AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(400, 200)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _cursor: winit::window::CursorIcon) {}
}

fn app() -> App {
    App::new_with_controlled_inputs(
        crate::State::new(),
        Box::new(crate::widget::Text::new("held")),
    )
}

/// A press is the pointer's: what it opens moves.
#[test]
fn a_press_on_a_surfaces_app_is_the_pointers_motion() {
    note_source(MotionSource::Action);
    let mut app = app();
    app.surface_input(&Window, crate::synthetic_input::mouse_press(), false);
    assert_eq!(
        input_source(),
        MotionSource::Pointer,
        "a press on the surface asked for it"
    );
}

/// A key lands at once, as a shortcut's result does.
#[test]
fn a_key_on_a_surfaces_app_lands_at_once() {
    note_source(MotionSource::Pointer);
    let mut app = app();
    let event = crate::synthetic_input::key_press(Key::Named(NamedKey::Enter), None);
    app.surface_input(&Window, event, false);
    assert_eq!(
        input_source(),
        MotionSource::Keyboard,
        "the key asked for it"
    );
}

/// A wheel is the pointer's: a page it opens glides.
#[test]
fn a_wheel_on_a_surfaces_app_is_the_pointers_motion() {
    note_source(MotionSource::Action);
    let mut app = app();
    let event = crate::synthetic_input::mouse_wheel(0.0, 40.0);
    app.surface_input(&Window, event, false);
    assert_eq!(input_source(), MotionSource::Pointer, "the wheel turned");
}
