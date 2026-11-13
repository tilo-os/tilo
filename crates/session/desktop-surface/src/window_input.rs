//! A person's pointer and keys on the picture of another device's window, as the input the device
//! hosting the window replays ([`InputEvent`]).
//!
//! Every desk that shows such a window, natively or in a browser, translates with these: the
//! picture is drawn whole in its tile, centred and keeping its shape, so a point is read against the
//! picture and not the tile, and lands in the window's own pixels however large the tile is. A key
//! is the keyboard's physical key as a Linux evdev code, which the host turns into its own
//! keyboard's symbol; text an input method composed travels as text.

use neighbour_wire::input::InputEvent;
use ui::kurbo::{Point, Rect};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};

/// Window pixels one turn of a wheel scrolls: a mouse wheel's detent, as Linux counts it.
pub const LINE_PIXELS: f64 = 15.0;

/// Where a picture of a `size`-pixel window stands in `rect`: whole, centred, keeping its shape.
/// `None` while either has no area.
#[must_use]
pub fn picture_rect(rect: Rect, size: (u32, u32)) -> Option<Rect> {
    let (width, height) = (f64::from(size.0), f64::from(size.1));
    if width <= 0.0 || height <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    let scale = (rect.width() / width).min(rect.height() / height);
    let (shown_width, shown_height) = (width * scale, height * scale);
    let left = rect.x0 + (rect.width() - shown_width) / 2.0;
    let top = rect.y0 + (rect.height() - shown_height) / 2.0;
    Some(Rect::new(left, top, left + shown_width, top + shown_height))
}

/// Where `point` of a tile `rect` lands in the window's own pixels, its picture drawn as
/// [`picture_rect`] places it; `None` off the picture.
#[must_use]
pub fn window_point(rect: Rect, size: (u32, u32), point: Point) -> Option<(f64, f64)> {
    let shown = picture_rect(rect, size)?;
    if !shown.contains(point) {
        return None;
    }
    Some((
        (point.x - shown.x0) / shown.width() * f64::from(size.0),
        (point.y - shown.y0) / shown.height() * f64::from(size.1),
    ))
}

/// What `event`, on the picture of a `size`-pixel window drawn in `rect`, asks of the window:
/// `None` for an event that asks nothing of it (a pointer off the picture, a key the wire has no
/// code for, a key held down repeating, which the host repeats itself). `scale_factor` is the
/// device pixels per point the event's positions are in, `modifiers` the keys held.
#[must_use]
pub fn translate(
    event: &WindowEvent,
    rect: Rect,
    size: (u32, u32),
    scale_factor: f64,
    modifiers: ModifiersState,
) -> Option<InputEvent> {
    let scale_factor = if scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    match event {
        WindowEvent::CursorMoved { position, .. } => {
            let point = Point::new(position.x / scale_factor, position.y / scale_factor);
            let (x, y) = window_point(rect, size, point)?;
            Some(InputEvent::PointerMotion { x, y })
        }
        WindowEvent::MouseInput { state, button, .. } => Some(InputEvent::PointerButton {
            button: evdev_button(*button),
            pressed: *state == ElementState::Pressed,
        }),
        WindowEvent::MouseWheel { delta, .. } => {
            // A wheel turned away from the person scrolls up; the host counts down as positive.
            let (dx, dy) = match delta {
                MouseScrollDelta::LineDelta(x, y) => {
                    (-f64::from(*x) * LINE_PIXELS, -f64::from(*y) * LINE_PIXELS)
                }
                MouseScrollDelta::PixelDelta(moved) => {
                    let shown = picture_rect(rect, size)?;
                    let window_per_point = f64::from(size.0) / shown.width();
                    (
                        -moved.x / scale_factor * window_per_point,
                        -moved.y / scale_factor * window_per_point,
                    )
                }
            };
            Some(InputEvent::PointerAxis { dx, dy })
        }
        WindowEvent::KeyboardInput { event, .. } if !event.repeat => {
            let PhysicalKey::Code(code) = event.physical_key else {
                return None;
            };
            Some(InputEvent::Key {
                keysym: evdev_keycode(code)?,
                pressed: event.state == ElementState::Pressed,
                modifiers: modifier_bits(modifiers),
            })
        }
        WindowEvent::Ime(Ime::Commit(text)) if !text.is_empty() => {
            Some(InputEvent::Text { text: text.clone() })
        }
        _ => None,
    }
}

/// One key of the keyboard as the wire names it: a Linux evdev key code, which the host translates
/// into its own keyboard.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per key: the wire's code for each key of the keyboard"
)]
#[must_use]
pub fn evdev_keycode(code: KeyCode) -> Option<u32> {
    use KeyCode as K;
    let code = match code {
        K::KeyA => 30,
        K::KeyB => 48,
        K::KeyC => 46,
        K::KeyD => 32,
        K::KeyE => 18,
        K::KeyF => 33,
        K::KeyG => 34,
        K::KeyH => 35,
        K::KeyI => 23,
        K::KeyJ => 36,
        K::KeyK => 37,
        K::KeyL => 38,
        K::KeyM => 50,
        K::KeyN => 49,
        K::KeyO => 24,
        K::KeyP => 25,
        K::KeyQ => 16,
        K::KeyR => 19,
        K::KeyS => 31,
        K::KeyT => 20,
        K::KeyU => 22,
        K::KeyV => 47,
        K::KeyW => 17,
        K::KeyX => 45,
        K::KeyY => 21,
        K::KeyZ => 44,
        K::Digit0 => 11,
        K::Digit1 => 2,
        K::Digit2 => 3,
        K::Digit3 => 4,
        K::Digit4 => 5,
        K::Digit5 => 6,
        K::Digit6 => 7,
        K::Digit7 => 8,
        K::Digit8 => 9,
        K::Digit9 => 10,
        K::Escape => 1,
        K::Minus => 12,
        K::Equal => 13,
        K::Backspace => 14,
        K::Tab => 15,
        K::BracketLeft => 26,
        K::BracketRight => 27,
        K::Enter => 28,
        K::ControlLeft => 29,
        K::Semicolon => 39,
        K::Quote => 40,
        K::Backquote => 41,
        K::ShiftLeft => 42,
        K::Backslash => 43,
        K::Comma => 51,
        K::Period => 52,
        K::Slash => 53,
        K::ShiftRight => 54,
        K::NumpadMultiply => 55,
        K::AltLeft => 56,
        K::Space => 57,
        K::CapsLock => 58,
        K::F1 => 59,
        K::F2 => 60,
        K::F3 => 61,
        K::F4 => 62,
        K::F5 => 63,
        K::F6 => 64,
        K::F7 => 65,
        K::F8 => 66,
        K::F9 => 67,
        K::F10 => 68,
        K::NumLock => 69,
        K::ScrollLock => 70,
        K::Numpad7 => 71,
        K::Numpad8 => 72,
        K::Numpad9 => 73,
        K::NumpadSubtract => 74,
        K::Numpad4 => 75,
        K::Numpad5 => 76,
        K::Numpad6 => 77,
        K::NumpadAdd => 78,
        K::Numpad1 => 79,
        K::Numpad2 => 80,
        K::Numpad3 => 81,
        K::Numpad0 => 82,
        K::NumpadDecimal => 83,
        K::F11 => 87,
        K::F12 => 88,
        K::NumpadEnter => 96,
        K::ControlRight => 97,
        K::NumpadDivide => 98,
        K::PrintScreen => 99,
        K::AltRight => 100,
        K::Home => 102,
        K::ArrowUp => 103,
        K::PageUp => 104,
        K::ArrowLeft => 105,
        K::ArrowRight => 106,
        K::End => 107,
        K::ArrowDown => 108,
        K::PageDown => 109,
        K::Insert => 110,
        K::Delete => 111,
        K::SuperLeft => 125,
        K::SuperRight => 126,
        _ => return None,
    };
    Some(code)
}

/// The wire's name for a pointer button: the libinput code the host's compositor replays.
#[must_use]
pub fn evdev_button(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 272,
        MouseButton::Right => 273,
        MouseButton::Middle => 274,
        MouseButton::Back => 275,
        MouseButton::Forward => 276,
        MouseButton::Other(other) => u32::from(other),
    }
}

/// What the held modifiers are, in the xkb bitfield the wire carries: shift, control, alt and
/// super, at the bits the host reads.
#[must_use]
pub fn modifier_bits(modifiers: ModifiersState) -> u32 {
    u32::from(modifiers.shift_key())
        | (u32::from(modifiers.control_key()) << 2)
        | (u32::from(modifiers.alt_key()) << 3)
        | (u32::from(modifiers.super_key()) << 6)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::synthetic_input::{cursor_moved, key_event, keyboard_input, mouse_input, mouse_wheel};
    use winit::keyboard::{Key, NamedKey};

    /// A 1280 x 720 window drawn in a 640 x 640 tile at (100, 50): half size, 140 points above and
    /// below it empty.
    const TILE: Rect = Rect::new(100.0, 50.0, 740.0, 690.0);
    const WINDOW: (u32, u32) = (1280, 720);

    fn of(event: &WindowEvent, scale: f64) -> Option<InputEvent> {
        translate(event, TILE, WINDOW, scale, ModifiersState::empty())
    }

    #[test]
    fn a_point_on_the_picture_lands_in_the_windows_own_pixels() {
        let shown = picture_rect(TILE, WINDOW).expect("a picture");
        assert_eq!(shown, Rect::new(100.0, 190.0, 740.0, 550.0));
        assert_eq!(
            of(&cursor_moved(420.0, 370.0), 1.0),
            Some(InputEvent::PointerMotion { x: 640.0, y: 360.0 })
        );
        // The device's pixels are halved into points first.
        assert_eq!(
            of(&cursor_moved(200.0, 380.0), 2.0),
            Some(InputEvent::PointerMotion { x: 0.0, y: 0.0 })
        );
        // The band the picture leaves empty is not the window.
        assert_eq!(of(&cursor_moved(420.0, 100.0), 1.0), None);
    }

    #[test]
    fn buttons_and_the_wheel_are_the_hosts_own_codes() {
        assert_eq!(
            of(&mouse_input(ElementState::Pressed, MouseButton::Left), 1.0),
            Some(InputEvent::PointerButton {
                button: 272,
                pressed: true
            })
        );
        assert_eq!(
            of(
                &mouse_input(ElementState::Released, MouseButton::Right),
                1.0
            ),
            Some(InputEvent::PointerButton {
                button: 273,
                pressed: false
            })
        );
        // Two lines away from the person scroll up by two detents.
        assert_eq!(
            of(&mouse_wheel(0.0, 2.0), 1.0),
            Some(InputEvent::PointerAxis { dx: 0.0, dy: -30.0 })
        );
        // A trackpad's pixels are the tile's: at half size each is two of the window's.
        let pixels = WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, -10.0)),
            phase: winit::event::TouchPhase::Moved,
        };
        assert_eq!(
            of(&pixels, 1.0),
            Some(InputEvent::PointerAxis { dx: 0.0, dy: 20.0 })
        );
    }

    #[test]
    fn a_key_is_its_evdev_code_with_the_held_modifiers_and_repeats_stay_here() {
        let mut press = key_event(Key::Character("a".into()), None, ElementState::Pressed);
        press.physical_key = PhysicalKey::Code(KeyCode::KeyA);
        let event = keyboard_input(press.clone());
        assert_eq!(
            translate(&event, TILE, WINDOW, 1.0, ModifiersState::CONTROL),
            Some(InputEvent::Key {
                keysym: 30,
                pressed: true,
                modifiers: 4
            })
        );
        press.repeat = true;
        assert_eq!(of(&keyboard_input(press), 1.0), None);
        let mut enter = key_event(Key::Named(NamedKey::Enter), None, ElementState::Released);
        enter.physical_key = PhysicalKey::Code(KeyCode::Enter);
        assert_eq!(
            of(&keyboard_input(enter), 1.0),
            Some(InputEvent::Key {
                keysym: 28,
                pressed: false,
                modifiers: 0
            })
        );
    }

    #[test]
    fn composed_text_travels_as_text() {
        let commit = WindowEvent::Ime(Ime::Commit("é".into()));
        assert_eq!(
            of(&commit, 1.0),
            Some(InputEvent::Text { text: "é".into() })
        );
        assert_eq!(of(&WindowEvent::Ime(Ime::Commit(String::new())), 1.0), None);
    }
}
