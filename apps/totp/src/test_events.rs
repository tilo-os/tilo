// the same trick the terminal / `modal_dialog` / ! pdf-viewer tests use.

use vello::kurbo::Point;
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// A pointer move to `p` (logical coords; scale factor 1.0 in the tests).
pub fn cursor_at(p: Point) -> WindowEvent {
    ui::synthetic_input::cursor_moved(p.x, p.y)
}

/// A left-button press. Widgets read the last cursor position, so send a
/// `cursor_at` first.
pub fn left_press() -> WindowEvent {
    ui::synthetic_input::mouse_press()
}

/// A vertical wheel scroll of `lines_y` notches (positive = up/away).
pub fn wheel(lines_y: f32) -> WindowEvent {
    ui::synthetic_input::mouse_wheel(0.0, lines_y)
}

/// A named keypress (Enter, Escape, Tab, …).
pub fn named_press(named: NamedKey) -> WindowEvent {
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        Key::Named(named),
        None,
        ElementState::Pressed,
    ))
}

/// No modifiers held.
pub fn no_mods() -> Modifiers {
    Modifiers::default()
}
