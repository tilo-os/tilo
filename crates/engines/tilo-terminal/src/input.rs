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

/// The modifiers held beside a key or a button.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub sup: bool,
}

impl Held {
    #[must_use]
    pub const fn new(shift: bool, alt: bool, ctrl: bool, sup: bool) -> Self {
        Self {
            shift,
            alt,
            ctrl,
            sup,
        }
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

    if let Key::Named(Named::Backspace) = key {
        if held.ctrl && !held.alt {
            return Some(vec![0x17]);
        }
        if held.alt && !held.ctrl {
            return Some(vec![0x1b, 0x7f]);
        }
    }

    let bytes = match key {
        Key::Char(character) if held.ctrl => ctrl_char(character, held.alt),
        Key::Char(character) if held.alt && !held.ctrl && !held.sup => alt_char(character),
        Key::Char(character) if !held.ctrl && !held.alt && !held.sup => {
            Some(character.to_string().into_bytes())
        }
        Key::Named(named) => named_key(named, held.shift, modifier, modes.app_cursor_keys),
        _ => None,
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
    if held.shift {
        pressed |= 1;
    }
    if held.alt {
        pressed |= 2;
    }
    if held.ctrl {
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

fn named_key(named: Named, shift: bool, modifier: Option<u8>, app_cursor_keys: bool) -> Option<Vec<u8>> {
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
    if held.shift {
        code |= 4;
    }
    if held.alt {
        code |= 8;
    }
    if held.ctrl {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> TermModes {
        TermModes {
            cursor_visible: true,
            ..TermModes::default()
        }
    }

    fn ctrl_arrow(key: Named, expected: &[u8], what: &str) {
        assert_eq!(
            encode_key(
                Key::Named(key),
                None,
                Held::new(false, false, true, false),
                plain()
            )
            .as_deref(),
            Some(expected),
            "{what}"
        );
    }

    /// Cursor keys carry the xterm modifier parameter, F1–F4 keep their SS3 form, and a program
    /// that asked for application cursor keys gets `ESC O` for an unmodified arrow.
    #[test]
    fn keys_send_the_sequences_a_shell_decodes() {
        ctrl_arrow(Named::ArrowLeft, b"\x1b[1;5D", "Ctrl+Left is word-left");
        ctrl_arrow(Named::ArrowRight, b"\x1b[1;5C", "Ctrl+Right is word-right");
        ctrl_arrow(Named::ArrowUp, b"\x1b[1;5A", "Ctrl+Up");
        ctrl_arrow(Named::ArrowDown, b"\x1b[1;5B", "Ctrl+Down");

        assert_eq!(
            encode_key(Key::Named(Named::ArrowLeft), None, Held::default(), plain()).as_deref(),
            Some(&b"\x1b[D"[..]),
            "an unmodified arrow is CSI when the program did not ask for application keys"
        );
        let app = TermModes {
            app_cursor_keys: true,
            ..plain()
        };
        assert_eq!(
            encode_key(Key::Named(Named::ArrowLeft), None, Held::default(), app).as_deref(),
            Some(&b"\x1bOD"[..]),
            "DECCKM sends SS3"
        );

        assert_eq!(
            encode_key(Key::Named(Named::Tab), None, Held::new(true, false, false, false), plain())
                .as_deref(),
            Some(&b"\x1b[Z"[..]),
            "Shift+Tab is back-tab"
        );
        assert_eq!(
            encode_key(Key::Named(Named::Function(1)), None, Held::default(), plain()).as_deref(),
            Some(&b"\x1bOP"[..]),
            "F1 keeps its SS3 form"
        );
        assert_eq!(
            encode_key(
                Key::Named(Named::Function(1)),
                None,
                Held::new(false, false, true, false),
                plain()
            )
            .as_deref(),
            Some(&b"\x1b[1;5P"[..]),
            "a held modifier takes the CSI form"
        );
        assert_eq!(
            encode_key(Key::Named(Named::Function(5)), None, Held::default(), plain()).as_deref(),
            Some(&b"\x1b[15~"[..])
        );
        assert_eq!(
            encode_key(Key::Named(Named::Delete), None, Held::default(), plain()).as_deref(),
            Some(&b"\x1b[3~"[..])
        );
        assert_eq!(
            encode_key(Key::Named(Named::Enter), None, Held::default(), plain()).as_deref(),
            Some(&b"\r"[..])
        );
        assert_eq!(
            encode_key(Key::Named(Named::Escape), None, Held::default(), plain()).as_deref(),
            Some(&b"\x1b"[..])
        );
    }

    /// Ctrl+letter is its C0 code, Alt is Meta, and a composed character sends its text.
    #[test]
    fn control_alt_and_composed_text_send_what_the_shell_expects() {
        let ctrl = Held::new(false, false, true, false);
        assert_eq!(
            encode_key(Key::Char('c'), None, ctrl, plain()).as_deref(),
            Some(&b"\x03"[..]),
            "Ctrl+C interrupts"
        );
        assert_eq!(
            encode_key(Key::Char(' '), None, ctrl, plain()).as_deref(),
            Some(&b"\x00"[..]),
            "Ctrl+Space is NUL"
        );
        assert_eq!(
            encode_key(Key::Char('a'), None, Held::new(false, true, false, false), plain()).as_deref(),
            Some(&b"\x1ba"[..]),
            "Alt+a is Meta-a"
        );
        assert_eq!(
            encode_key(Key::Char('ä'), Some("ä"), Held::new(false, true, false, false), plain())
                .as_deref(),
            Some("ä".as_bytes()),
            "AltGr keeps the character it composed"
        );
        assert_eq!(
            encode_key(
                Key::Named(Named::Backspace),
                None,
                Held::new(false, false, true, false),
                plain()
            )
            .as_deref(),
            Some(&b"\x17"[..]),
            "Ctrl+Backspace deletes a word"
        );
        assert_eq!(
            encode_key(Key::Char('中'), Some("中"), Held::default(), plain()).as_deref(),
            Some("中".as_bytes()),
            "a character with no ASCII form sends its bytes"
        );
    }

    /// A paste's line breaks are Enters, wrapped only when the program asked for bracketed paste.
    #[test]
    fn paste_carries_its_lines_and_the_programs_brackets() {
        assert_eq!(encode_paste("a\nb\r\nc", plain()), b"a\rb\rc");
        let bracketed = TermModes {
            bracketed_paste: true,
            ..plain()
        };
        assert_eq!(
            encode_paste("a\nb", bracketed),
            b"\x1b[200~a\rb\x1b[201~"
        );
    }

    /// The pointer is reported only when the program asked, in the mode it asked for, and a
    /// program that asked for nothing gets nothing so the view selects instead.
    #[test]
    fn mice_are_reported_in_the_mode_the_program_asked_for() {
        assert_eq!(
            encode_mouse(plain(), Button::Left, true, Held::default(), 3, 4),
            None,
            "no report without a request"
        );
        let sgr = TermModes {
            mouse_report: true,
            mouse_sgr: true,
            mouse_tracking: 1000,
            ..plain()
        };
        assert_eq!(
            encode_mouse(sgr, Button::Left, true, Held::default(), 3, 4).as_deref(),
            Some(&b"\x1b[<0;3;4M"[..])
        );
        assert_eq!(
            encode_mouse(sgr, Button::Left, false, Held::default(), 3, 4).as_deref(),
            Some(&b"\x1b[<0;3;4m"[..]),
            "a release is its own final byte in SGR"
        );
        assert_eq!(
            encode_mouse(sgr, Button::Right, true, Held::new(true, false, false, false), 1, 1)
                .as_deref(),
            Some(&b"\x1b[<6;1;1M"[..]),
            "Shift is reported for a program that ignores it"
        );

        let x10 = TermModes {
            mouse_report: true,
            mouse_sgr: false,
            mouse_tracking: 1000,
            ..plain()
        };
        let bytes = encode_mouse(x10, Button::Left, true, Held::default(), 1, 1).expect("a report");
        assert_eq!(bytes, vec![0x1b, b'[', b'M', 32, 33, 33]);
        let release = encode_mouse(x10, Button::Left, false, Held::default(), 1, 1).expect("a report");
        assert_eq!(release, vec![0x1b, b'[', b'M', 35, 33, 33], "X10 releases as button 3");
    }
}
