//! Synthetic `winit` event constructors shared by the in-crate widget tests.
//!
//! These let the tests drive pointer + keyboard flows through the real
//! `Widget::handle_event` production paths (hit-testing, focus routing,
//! shortcut dispatch) rather than reaching past them into private helpers.
//!
//! winit's per-platform `KeyEventExtra` is `pub(crate)`, so a `KeyEvent`
//! can't be built field-by-field off-platform; we zero the struct and set
//! only the public fields — the same trick the terminal / `modal_dialog` /
//! pdf-viewer tests use.

use vello::kurbo::Point;
use winit::event::{
    DeviceId, ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, TouchPhase,
    WindowEvent,
};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, NativeKeyCode, PhysicalKey};

fn device() -> DeviceId {
    unsafe { std::mem::zeroed() }
}

/// A pointer move to `p` (logical coords; scale factor 1.0 in the tests).
pub fn cursor_at(p: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: device(),
        position: winit::dpi::PhysicalPosition::new(p.x, p.y),
    }
}

/// A left-button press. Widgets read the last cursor position, so send a
/// `cursor_at` first.
pub fn left_press() -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: device(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    }
}

/// A vertical wheel scroll of `lines_y` notches (positive = up/away).
pub fn wheel(lines_y: f32) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: device(),
        delta: MouseScrollDelta::LineDelta(0.0, lines_y),
        phase: TouchPhase::Moved,
    }
}

/// A character keypress (logical key + text), e.g. `char_press("n")`.
pub fn char_press(c: &str) -> WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Character(c.into());
    ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = Some(c.into());
    WindowEvent::KeyboardInput {
        device_id: device(),
        event: ev,
        is_synthetic: false,
    }
}

/// A named keypress (Enter, Escape, Tab, …).
pub fn named_press(named: NamedKey) -> WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Named(named);
    ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = None;
    WindowEvent::KeyboardInput {
        device_id: device(),
        event: ev,
        is_synthetic: false,
    }
}

/// Modifiers with the Control key held (for Ctrl+N / Ctrl+F).
pub fn ctrl() -> Modifiers {
    Modifiers::from(ModifiersState::CONTROL)
}

/// No modifiers held.
pub fn no_mods() -> Modifiers {
    Modifiers::default()
}
