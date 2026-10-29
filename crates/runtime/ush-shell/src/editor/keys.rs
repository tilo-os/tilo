//! Keys, as a terminal sends them: bytes in, [`Key`]s out.
//!
//! The emacs keys readline binds by default, in the spellings terminals
//! send them (`ESC [ A`, `ESC O A`, `ESC [ 1 ; 5 D`), bracketed paste, and
//! the keyed marks Terminal sends the shell on its input (`ESC ] 133 ; …
//! BEL`: a history page, a command line `run` asked for). A lone `ESC` is a
//! meta prefix, as in readline: `ESC f` is `Alt+F`.

/// One key, or one thing the terminal sent that is not a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// A character to insert.
    Char(char),
    /// `Ctrl` with a letter (`'a'` for `Ctrl+A`), or `Ctrl+_` as `'_'`.
    Ctrl(char),
    /// `Alt` (or `ESC` first) with a character.
    Alt(char),
    Enter,
    Tab,
    /// Shift+Tab.
    BackTab,
    Backspace,
    /// `Alt+Backspace`.
    AltBackspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    /// `Ctrl+Left` (or `Alt+Left`): a word back.
    WordLeft,
    /// `Ctrl+Right`: a word on.
    WordRight,
    Home,
    End,
    /// Text pasted between `ESC [ 200 ~` and `ESC [ 201 ~`: inserted as it
    /// is, newlines included, never run by its own newlines.
    Paste(String),
    /// A mark of Terminal's on the input (`ESC ] 133 ; <body> BEL`), as its
    /// body: the shell checks its key before acting on it.
    Mark(String),
    /// A sequence this editor does not bind.
    Unknown,
}

/// Turns bytes into keys, across reads.
#[derive(Debug, Default)]
pub struct Decoder {
    bytes: Vec<u8>,
}

/// The longest mark or paste taken from the input. A paste is bounded the
/// way the command line is: by what the person can see and edit.
const MAX_SEQUENCE: usize = 1 << 20;

impl Decoder {
    /// Add one byte; a key it completes comes back.
    pub fn push(&mut self, byte: u8) -> Option<Key> {
        self.bytes.push(byte);
        match decode(&self.bytes) {
            Step::More => {
                if self.bytes.len() > MAX_SEQUENCE {
                    self.bytes.clear();
                    return Some(Key::Unknown);
                }
                None
            }
            Step::Key(key) => {
                self.bytes.clear();
                Some(key)
            }
        }
    }
}

enum Step {
    More,
    Key(Key),
}

fn decode(bytes: &[u8]) -> Step {
    let Some((&first, rest)) = bytes.split_first() else {
        return Step::More;
    };
    match first {
        0x1b => escape(rest),
        b'\r' | b'\n' => Step::Key(Key::Enter),
        b'\t' => Step::Key(Key::Tab),
        0x7f | 0x08 => Step::Key(Key::Backspace),
        0x1f => Step::Key(Key::Ctrl('_')),
        0x00 => Step::Key(Key::Ctrl(' ')),
        0x01..=0x1a => Step::Key(Key::Ctrl(char::from(first.saturating_add(b'a' - 1)))),
        0x1c..=0x1e => Step::Key(Key::Unknown),
        _ => utf8(bytes),
    }
}

fn utf8(bytes: &[u8]) -> Step {
    match std::str::from_utf8(bytes) {
        Ok(text) => text
            .chars()
            .next()
            .map_or(Step::More, |c| Step::Key(Key::Char(c))),
        Err(e) if e.error_len().is_none() => Step::More,
        Err(_) => Step::Key(Key::Unknown),
    }
}

/// What follows an `ESC`.
fn escape(rest: &[u8]) -> Step {
    let Some((&second, tail)) = rest.split_first() else {
        return Step::More;
    };
    match second {
        b'[' => csi(tail),
        b'O' => match tail.first() {
            None => Step::More,
            Some(b'A') => Step::Key(Key::Up),
            Some(b'B') => Step::Key(Key::Down),
            Some(b'C') => Step::Key(Key::Right),
            Some(b'D') => Step::Key(Key::Left),
            Some(b'H') => Step::Key(Key::Home),
            Some(b'F') => Step::Key(Key::End),
            Some(_) => Step::Key(Key::Unknown),
        },
        b']' => osc(tail),
        0x7f | 0x08 => Step::Key(Key::AltBackspace),
        b'\r' | b'\n' | 0x1b => Step::Key(Key::Unknown),
        _ => match utf8(rest) {
            Step::Key(Key::Char(c)) => Step::Key(Key::Alt(c.to_ascii_lowercase())),
            Step::Key(_) => Step::Key(Key::Unknown),
            Step::More => Step::More,
        },
    }
}

/// `ESC [ params final`: the cursor keys, Home, End, Delete, and pastes.
fn csi(tail: &[u8]) -> Step {
    let Some(end) = tail.iter().position(|b| (0x40..=0x7e).contains(b)) else {
        return if tail.len() > 32 {
            Step::Key(Key::Unknown)
        } else {
            Step::More
        };
    };
    let params = std::str::from_utf8(tail.get(..end).unwrap_or_default()).unwrap_or("");
    let modifier = params
        .split(';')
        .nth(1)
        .and_then(|m| m.parse::<u8>().ok())
        .unwrap_or(1);
    // Ctrl (5) or Alt (3) on the horizontal arrows moves by words.
    let by_word = matches!(modifier, 3 | 5);
    let key = match tail.get(end) {
        Some(b'A') => Key::Up,
        Some(b'B') => Key::Down,
        Some(b'C') if by_word => Key::WordRight,
        Some(b'D') if by_word => Key::WordLeft,
        Some(b'C') => Key::Right,
        Some(b'D') => Key::Left,
        Some(b'H') => Key::Home,
        Some(b'F') => Key::End,
        Some(b'Z') => Key::BackTab,
        Some(b'~') => match params.split(';').next().unwrap_or("") {
            "1" | "7" => Key::Home,
            "4" | "8" => Key::End,
            "3" => Key::Delete,
            "200" => return paste(tail.get(end.saturating_add(1)..).unwrap_or_default()),
            _ => Key::Unknown,
        },
        _ => Key::Unknown,
    };
    Step::Key(key)
}

/// The pasted text up to `ESC [ 201 ~`.
fn paste(text: &[u8]) -> Step {
    const END: &[u8] = b"\x1b[201~";
    if !text.ends_with(END) {
        return Step::More;
    }
    let body = text
        .get(..text.len().saturating_sub(END.len()))
        .unwrap_or_default();
    // A pasted `\r\n` or `\r` is one newline in the line.
    let body = String::from_utf8_lossy(body)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    Step::Key(Key::Paste(body))
}

/// `ESC ] body BEL` (or `ESC \`): only Terminal's `133;` marks mean
/// anything here.
fn osc(tail: &[u8]) -> Step {
    let end = tail
        .iter()
        .position(|b| *b == 0x07)
        .map(|at| (at, 1))
        .or_else(|| {
            tail.windows(2)
                .position(|w| w == b"\x1b\\")
                .map(|at| (at, 2))
        });
    let Some((at, _)) = end else {
        return Step::More;
    };
    let body = String::from_utf8_lossy(tail.get(..at).unwrap_or_default());
    match body.strip_prefix("133;") {
        Some(mark) => Step::Key(Key::Mark(mark.to_owned())),
        None => Step::Key(Key::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(bytes: &[u8]) -> Vec<Key> {
        let mut decoder = Decoder::default();
        bytes.iter().filter_map(|b| decoder.push(*b)).collect()
    }

    #[test]
    fn the_keys_terminals_send_read_as_the_keys_they_are() {
        assert_eq!(
            keys(b"a\x01\x1b[A\x1bOB\x1b[1;5D\x1b[3~\x1bf\x1b\x7f\x1b[H\x1b[4~\r"),
            [
                Key::Char('a'),
                Key::Ctrl('a'),
                Key::Up,
                Key::Down,
                Key::WordLeft,
                Key::Delete,
                Key::Alt('f'),
                Key::AltBackspace,
                Key::Home,
                Key::End,
                Key::Enter,
            ]
        );
        assert_eq!(keys("é".as_bytes()), [Key::Char('é')]);
    }

    #[test]
    fn a_paste_and_a_mark_arrive_whole() {
        assert_eq!(
            keys(b"\x1b[200~echo one\r\necho two\x1b[201~\x1b]133;H;k=x\x07"),
            [
                Key::Paste("echo one\necho two".into()),
                Key::Mark("H;k=x".into())
            ]
        );
    }
}
