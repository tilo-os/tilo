//! What a person's keys and pointer become on the wire: the bytes a shell decodes.
//!
//! The encoding follows xterm: cursor and edit keys are DECCKM-aware with a modifier parameter
//! when one is held, F1–F4 keep their SS3 form, and Ctrl+letter is the C0 code it names. The
//! pointer is reported in the mode the program asked for (X10 or SGR), and a program that asked
//! for no reporting gets nothing: the view selects instead.
//!
//! Keys are named in this crate's own vocabulary, so the table is tested without a window; a
//! view translates the window's key events into it once.

use crate::TermModes;

/// The keys a program reads as an escape sequence rather than as text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Named {
    Enter,
    Space,
    Backspace,
    Tab,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// A function key, numbered from 1.
    Function(u8),
}

/// One key press: a character, with `text` as the layout produced it, or a named key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Named(Named),
}

/// The modifiers held beside a key or a button, as the one bit each takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    bits: u8,
}

impl Held {
    /// Shift held.
    pub const SHIFT: Self = Self { bits: 1 };
    /// Alt held.
    pub const ALT: Self = Self { bits: 2 };
    /// Control held.
    pub const CTRL: Self = Self { bits: 4 };
    /// Super held.
    pub const SUP: Self = Self { bits: 8 };

    /// The set, one bit per modifier.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.bits
    }

    #[must_use]
    pub const fn shift(self) -> bool {
        self.bits & Self::SHIFT.bits != 0
    }

    #[must_use]
    pub const fn alt(self) -> bool {
        self.bits & Self::ALT.bits != 0
    }

    #[must_use]
    pub const fn ctrl(self) -> bool {
        self.bits & Self::CTRL.bits != 0
    }

    #[must_use]
    pub const fn sup(self) -> bool {
        self.bits & Self::SUP.bits != 0
    }
}

impl From<[bool; 4]> for Held {
    /// The modifiers held, in the order shift, alt, ctrl, super.
    fn from([shift, alt, ctrl, sup]: [bool; 4]) -> Self {
        let mut held = Self::default();
        for (on, modifier) in [
            (shift, Self::SHIFT),
            (alt, Self::ALT),
            (ctrl, Self::CTRL),
            (sup, Self::SUP),
        ] {
            if on {
                held.bits |= modifier.bits;
            }
        }
        held
    }
}

/// Which button was pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

/// The bytes `key` sends, or `None` when the view acts on it itself.
///
/// `text` is what the layout produced for a character key, so a key that composes a character on
/// this keyboard sends that character; `None` for a named key.
#[must_use]
pub fn encode_key(key: Key, text: Option<&str>, held: Held, modes: TermModes) -> Option<Vec<u8>> {
    let modifier = mod_param(held);

    if key == Key::Named(Named::Backspace) {
        if held.ctrl() && !held.alt() {
            return Some(vec![0x17]);
        }
        if held.alt() && !held.ctrl() {
            return Some(vec![0x1b, 0x7f]);
        }
    }

    let bytes = match key {
        Key::Char(character) if held.ctrl() => ctrl_char(character, held.alt()),
        Key::Char(character) if held.alt() && !held.ctrl() && !held.sup() => alt_char(character),
        Key::Char(character) if !held.ctrl() && !held.alt() && !held.sup() => {
            Some(character.to_string().into_bytes())
        }
        Key::Char(_) => None,
        Key::Named(named) => named_key(named, held.shift(), modifier, modes.app_cursor_keys),
    };
    if bytes.is_some() {
        return bytes;
    }
    // A composed character or a dead key's output arrives as text, never as its key.
    if let Some(text) = text
        && !text.is_empty()
        && !text
            .chars()
            .any(|character| character as u32 == 0x23CE || character as u32 == 0x21B5)
    {
        return Some(text.as_bytes().to_vec());
    }
    None
}

/// The xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl, only when one is held.
fn mod_param(held: Held) -> Option<u8> {
    let mut pressed = 0u8;
    if held.shift() {
        pressed |= 1;
    }
    if held.alt() {
        pressed |= 2;
    }
    if held.ctrl() {
        pressed |= 4;
    }
    (pressed != 0).then_some(pressed + 1)
}

fn csi_final(final_byte: u8, modifier: Option<u8>, app_cursor_keys: bool) -> Vec<u8> {
    if let Some(modifier) = modifier {
        format!("\x1b[1;{modifier}{}", final_byte as char).into_bytes()
    } else {
        let prefix: &[u8] = if app_cursor_keys { b"\x1bO" } else { b"\x1b[" };
        let mut bytes = prefix.to_vec();
        bytes.push(final_byte);
        bytes
    }
}

fn csi_tilde(number: u8, modifier: Option<u8>) -> Vec<u8> {
    match modifier {
        Some(modifier) => format!("\x1b[{number};{modifier}~").into_bytes(),
        None => format!("\x1b[{number}~").into_bytes(),
    }
}

fn fkey_ss3(final_byte: u8, modifier: Option<u8>) -> Vec<u8> {
    match modifier {
        Some(modifier) => format!("\x1b[1;{modifier}{}", final_byte as char).into_bytes(),
        None => format!("\x1bO{}", final_byte as char).into_bytes(),
    }
}

fn named_key(
    named: Named,
    shift: bool,
    modifier: Option<u8>,
    app_cursor_keys: bool,
) -> Option<Vec<u8>> {
    match named {
        Named::Enter => Some(vec![b'\r']),
        Named::Space => Some(vec![b' ']),
        Named::Backspace => Some(vec![127]),
        Named::Tab if shift => Some(b"\x1b[Z".to_vec()),
        Named::Tab => Some(vec![b'\t']),
        Named::Escape => Some(vec![27]),
        Named::ArrowUp => Some(csi_final(b'A', modifier, app_cursor_keys)),
        Named::ArrowDown => Some(csi_final(b'B', modifier, app_cursor_keys)),
        Named::ArrowRight => Some(csi_final(b'C', modifier, app_cursor_keys)),
        Named::ArrowLeft => Some(csi_final(b'D', modifier, app_cursor_keys)),
        Named::Home => Some(csi_final(b'H', modifier, app_cursor_keys)),
        Named::End => Some(csi_final(b'F', modifier, app_cursor_keys)),
        Named::PageUp => Some(csi_tilde(5, modifier)),
        Named::PageDown => Some(csi_tilde(6, modifier)),
        Named::Insert => Some(csi_tilde(2, modifier)),
        Named::Delete => Some(csi_tilde(3, modifier)),
        Named::Function(1) => Some(fkey_ss3(b'P', modifier)),
        Named::Function(2) => Some(fkey_ss3(b'Q', modifier)),
        Named::Function(3) => Some(fkey_ss3(b'R', modifier)),
        Named::Function(4) => Some(fkey_ss3(b'S', modifier)),
        Named::Function(5) => Some(csi_tilde(15, modifier)),
        Named::Function(6) => Some(csi_tilde(17, modifier)),
        Named::Function(7) => Some(csi_tilde(18, modifier)),
        Named::Function(8) => Some(csi_tilde(19, modifier)),
        Named::Function(9) => Some(csi_tilde(20, modifier)),
        Named::Function(10) => Some(csi_tilde(21, modifier)),
        Named::Function(11) => Some(csi_tilde(23, modifier)),
        Named::Function(12) => Some(csi_tilde(24, modifier)),
        Named::Function(_) => None,
    }
}

/// Ctrl+letter is its C0 code; Alt prefixes ESC (Meta).
fn ctrl_char(character: char, alt: bool) -> Option<Vec<u8>> {
    let upper = character.to_ascii_uppercase();
    let code = match upper {
        'A'..='Z' => (upper as u8) - b'A' + 1,
        ' ' | '@' => 0,
        '[' => 27,
        '\\' => 28,
        ']' => 29,
        '^' => 30,
        '_' => 31,
        _ => return None,
    };
    Some(if alt { vec![27, code] } else { vec![code] })
}

/// Alt as Meta for plain ASCII alphanumerics only: AltGr-composed symbols keep their text.
fn alt_char(character: char) -> Option<Vec<u8>> {
    if character.is_ascii_alphanumeric() {
        let mut bytes = vec![27u8];
        bytes.extend_from_slice(character.to_string().as_bytes());
        Some(bytes)
    } else {
        None
    }
}

/// What a paste sends: the text, with its line breaks as Enter, wrapped in the bracketed-paste
/// markers when the program asked for them.
#[must_use]
pub fn encode_paste(text: &str, modes: TermModes) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    let mut bytes = Vec::with_capacity(text.len() + 16);
    if modes.bracketed_paste {
        bytes.extend_from_slice(b"\x1b[200~");
    }
    bytes.extend_from_slice(text.as_bytes());
    if modes.bracketed_paste {
        bytes.extend_from_slice(b"\x1b[201~");
    }
    bytes
}

/// The bytes of one pointer report in the mode the program asked for, at a one-based cell.
#[must_use]
pub fn encode_mouse(
    modes: TermModes,
    button: Button,
    pressed: bool,
    held: Held,
    column: usize,
    row: usize,
) -> Option<Vec<u8>> {
    if !modes.mouse_report {
        return None;
    }
    let code = match button {
        Button::Left => 0u8,
        Button::Middle => 1,
        Button::Right => 2,
    };
    Some(mouse_bytes(modes, code, pressed, held, column, row))
}

/// One mouse report for `code` (the button, or 3 for a release in X10) at a one-based cell.
#[must_use]
pub fn mouse_bytes(
    modes: TermModes,
    mut code: u8,
    pressed: bool,
    held: Held,
    column: usize,
    row: usize,
) -> Vec<u8> {
    // Shift is the conventional override: a program that tracks the mouse still lets a person
    // select, so the view reports the shift and the program ignores it.
    if held.shift() {
        code |= 4;
    }
    if held.alt() {
        code |= 8;
    }
    if held.ctrl() {
        code |= 16;
    }
    let x = column.max(1);
    let y = row.max(1);
    if modes.mouse_sgr {
        let final_byte = if pressed { 'M' } else { 'm' };
        format!("\x1b[<{code};{x};{y}{final_byte}").into_bytes()
    } else {
        let legacy = if pressed { code } else { 3 | (code & 0x1c) };
        let x = x.min(223) as u8 + 32;
        let y = y.min(223) as u8 + 32;
        vec![0x1b, b'[', b'M', legacy + 32, x, y]
    }
}
