//! What a key or wheel pressed over a session becomes: the bytes an SSH shell
//! takes, the keysym a VNC desktop understands, and the clicks a wheel burst
//! becomes.

use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Translate a key press into the bytes an interactive PTY expects.
pub(super) fn ssh_key_bytes(
    key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
) -> Option<Vec<u8>> {
    match key {
        Key::Named(NamedKey::Enter) => Some(vec![b'\r']),
        Key::Named(NamedKey::Backspace) => Some(vec![0x7f]),
        Key::Named(NamedKey::Tab) => Some(vec![b'\t']),
        Key::Named(NamedKey::Escape) => Some(vec![0x1b]),
        Key::Named(NamedKey::Space) => Some(vec![b' ']),
        Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A".to_vec()),
        Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B".to_vec()),
        Key::Named(NamedKey::ArrowRight) => Some(b"\x1b[C".to_vec()),
        Key::Named(NamedKey::ArrowLeft) => Some(b"\x1b[D".to_vec()),
        Key::Named(NamedKey::Home) => Some(b"\x1b[H".to_vec()),
        Key::Named(NamedKey::End) => Some(b"\x1b[F".to_vec()),
        Key::Named(NamedKey::Delete) => Some(b"\x1b[3~".to_vec()),
        Key::Character(s) => {
            if mods.control_key() {
                // Ctrl+letter → the corresponding control byte (^A=1…^Z=26).
                let c = s.chars().next()?;
                let up = c.to_ascii_uppercase();
                if up.is_ascii_alphabetic() {
                    return Some(vec![(up as u8) - 0x40]);
                }
            }
            text.map(|t| t.as_bytes().to_vec())
                .or_else(|| Some(s.as_bytes().to_vec()))
        }
        _ => text.map(|t| t.as_bytes().to_vec()),
    }
}

/// The most wheel clicks one wheel event sends a VNC desktop, so a fling
/// cannot fill the session's bounded input queue.
pub(super) const VNC_WHEEL_BURST: usize = 10;

/// The wheel buttons to click on a VNC desktop for `delta`, carrying what is
/// short of a whole click in `pending`. RFB's buttons 4 to 7 (mask bits 3 to
/// 6) scroll up, down, left and right; winit's positive deltas move the
/// content right and down, which is scrolling left and up.
///
/// A trackpad's pixels make a click every `wheel_line` logical pixels, the
/// theme's distance for one notch of a line-stepping wheel.
pub(super) fn vnc_wheel_clicks(
    pending: &mut (f64, f64),
    delta: &MouseScrollDelta,
    scale_factor: f64,
    wheel_line: f64,
) -> Vec<u8> {
    let (dx, dy) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (f64::from(*x), f64::from(*y)),
        MouseScrollDelta::PixelDelta(position) => {
            let per_click = wheel_line.max(1.0) * scale_factor.max(f64::MIN_POSITIVE);
            (position.x / per_click, position.y / per_click)
        }
    };
    let mut clicks = Vec::new();
    for (moved, pending, toward, away) in [
        (dy, &mut pending.1, 0x08_u8, 0x10_u8),
        (dx, &mut pending.0, 0x20_u8, 0x40_u8),
    ] {
        if !moved.is_finite() {
            continue;
        }
        *pending += moved;
        let mut whole = pending.trunc();
        *pending -= whole;
        let button = if whole > 0.0 { toward } else { away };
        for _ in 0..VNC_WHEEL_BURST {
            if whole.abs() < 1.0 {
                break;
            }
            clicks.push(button);
            whole -= whole.signum();
        }
    }
    clicks
}

/// The X11 keysym a VNC `KeyEvent` sends for a logical key: a character,
/// or a named key from editing, navigation, the modifiers and F1 to F12.
pub(super) fn vnc_keysym(key: &Key) -> Option<u32> {
    let named = match key {
        Key::Named(named) => named,
        // A character's keysym is its Latin-1 code, or the Unicode keysym
        // for anything past it (RFB 6143 §7.5.4, X11 keysym encoding). The
        // logical key, not the text: Ctrl+C has the text of a control
        // character but is the C key held with Ctrl.
        Key::Character(text) => {
            let mut chars = text.chars();
            let c = u32::from(chars.next()?);
            if chars.next().is_some() {
                return None;
            }
            return Some(if c <= 0xff { c } else { 0x0100_0000 | c });
        }
        _ => return None,
    };
    let sym = match named {
        NamedKey::Enter => 0xff0d,
        NamedKey::Backspace => 0xff08,
        NamedKey::Tab => 0xff09,
        NamedKey::Escape => 0xff1b,
        NamedKey::Space => 0x20,
        NamedKey::ArrowUp => 0xff52,
        NamedKey::ArrowDown => 0xff54,
        NamedKey::ArrowLeft => 0xff51,
        NamedKey::ArrowRight => 0xff53,
        NamedKey::Delete => 0xffff,
        NamedKey::Insert => 0xff63,
        NamedKey::Home => 0xff50,
        NamedKey::End => 0xff57,
        NamedKey::PageUp => 0xff55,
        NamedKey::PageDown => 0xff56,
        NamedKey::Shift => 0xffe1,
        NamedKey::Control => 0xffe3,
        NamedKey::Alt => 0xffe9,
        NamedKey::AltGraph => 0xfe03,
        NamedKey::Super | NamedKey::Meta => 0xffeb,
        NamedKey::CapsLock => 0xffe5,
        NamedKey::F1 => 0xffbe,
        NamedKey::F2 => 0xffbf,
        NamedKey::F3 => 0xffc0,
        NamedKey::F4 => 0xffc1,
        NamedKey::F5 => 0xffc2,
        NamedKey::F6 => 0xffc3,
        NamedKey::F7 => 0xffc4,
        NamedKey::F8 => 0xffc5,
        NamedKey::F9 => 0xffc6,
        NamedKey::F10 => 0xffc7,
        NamedKey::F11 => 0xffc8,
        NamedKey::F12 => 0xffc9,
        _ => return None,
    };
    Some(sym)
}
