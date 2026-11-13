use vello::kurbo::Point;
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

pub fn cursor_at(p: Point) -> WindowEvent {
    ui::synthetic_input::cursor_moved(p.x, p.y)
}

/// Widgets read the last cursor position, so send a `cursor_at` first.
pub fn left_press() -> WindowEvent {
    ui::synthetic_input::mouse_press()
}

pub fn wheel(lines_y: f32) -> WindowEvent {
    ui::synthetic_input::mouse_wheel(0.0, lines_y)
}

pub fn named_press(named: NamedKey) -> WindowEvent {
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        Key::Named(named),
        None,
        ElementState::Pressed,
    ))
}

pub fn no_mods() -> Modifiers {
    Modifiers::default()
}
