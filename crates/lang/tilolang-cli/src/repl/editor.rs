//! A small line editor for the REPL: the keys a shell's line takes (moving by character and
//! word, deleting, Ctrl-A/E/K/U/W/L), history kept between runs, and completion on Tab.
//!
//! On a terminal it reads keys in raw mode and redraws the line itself. On anything else (a
//! pipe, a script) it reads plain lines and echoes each after the prompt, so a recorded run
//! reads as the session looked.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::os::fd::AsFd as _;
use std::path::PathBuf;

/// What completes a word: given the line and the cursor (in characters), where the word being
/// completed starts and the candidates for it.
pub trait Complete {
    fn complete(&self, line: &str, cursor: usize) -> (usize, Vec<String>);
}

/// What one read gave.
pub enum Read {
    Line(String),
    /// Ctrl-C: the line was dropped.
    Interrupted,
    /// Ctrl-D on an empty line, or the input ended.
    Eof,
}

/// How many entries the history keeps.
const HISTORY_KEPT: usize = 1000;

pub struct Editor {
    history: Vec<String>,
    path: Option<PathBuf>,
    terminal: bool,
}

/// The terminal in raw mode until this is dropped.
struct Raw {
    saved: rustix::termios::Termios,
}

impl Raw {
    fn enter() -> io::Result<Self> {
        let stdin = io::stdin();
        let saved = rustix::termios::tcgetattr(stdin.as_fd())?;
        let mut raw = saved.clone();
        raw.make_raw();
        // Keep output processing, so `\n` still returns the carriage.
        raw.output_modes
            .insert(rustix::termios::OutputModes::OPOST | rustix::termios::OutputModes::ONLCR);
        rustix::termios::tcsetattr(stdin.as_fd(), rustix::termios::OptionalActions::Now, &raw)?;
        Ok(Self { saved })
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        let _ = rustix::termios::tcsetattr(
            io::stdin().as_fd(),
            rustix::termios::OptionalActions::Now,
            &self.saved,
        );
    }
}

fn columns() -> usize {
    rustix::termios::tcgetwinsize(io::stdout().as_fd())
        .map(|size| usize::from(size.ws_col))
        .ok()
        .filter(|&cols| cols > 0)
        .unwrap_or(80)
}

/// A key as the terminal sends it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Char(char),
    Enter,
    Interrupt,
    EndOrDelete,
    Home,
    End,
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    KillToEnd,
    KillToStart,
    KillWord,
    Clear,
    Backspace,
    Delete,
    Tab,
    Other,
}

/// The key a control byte is.
const fn control(byte: u8) -> Key {
    match byte {
        b'\r' | b'\n' => Key::Enter,
        3 => Key::Interrupt,
        4 => Key::EndOrDelete,
        1 => Key::Home,
        5 => Key::End,
        2 => Key::Left,
        6 => Key::Right,
        11 => Key::KillToEnd,
        21 => Key::KillToStart,
        23 => Key::KillWord,
        12 => Key::Clear,
        127 | 8 => Key::Backspace,
        b'\t' => Key::Tab,
        _ => Key::Other,
    }
}

/// The key an escape sequence (after its `ESC [` or `ESC O`) is.
fn sequence(code: &[u8]) -> Key {
    match code {
        b"A" => Key::Up,
        b"B" => Key::Down,
        b"C" => Key::Right,
        b"D" => Key::Left,
        b"H" | b"1~" | b"7~" => Key::Home,
        b"F" | b"4~" | b"8~" => Key::End,
        b"3~" => Key::Delete,
        b"1;5C" | b"1;3C" => Key::WordRight,
        b"1;5D" | b"1;3D" => Key::WordLeft,
        _ => Key::Other,
    }
}

/// Reads keys from the terminal.
struct Keys<R> {
    input: R,
    pending: Vec<u8>,
}

impl<R: io::Read> Keys<R> {
    fn byte(&mut self) -> io::Result<Option<u8>> {
        let mut one = [0_u8; 1];
        Ok((self.input.read(&mut one)? == 1)
            .then_some(one)
            .map(|[byte]| byte))
    }

    /// The next key, `None` when the input ended.
    fn next(&mut self) -> io::Result<Option<Key>> {
        loop {
            let Some(byte) = self.byte()? else {
                return Ok(None);
            };
            if !self.pending.is_empty() || byte >= 0x80 {
                self.pending.push(byte);
                match std::str::from_utf8(&self.pending) {
                    Ok(text) => {
                        let character = text.chars().next();
                        self.pending.clear();
                        if let Some(character) = character {
                            return Ok(Some(Key::Char(character)));
                        }
                    }
                    Err(error) if error.error_len().is_some() => self.pending.clear(),
                    Err(_) => {}
                }
                continue;
            }
            if byte == 0x1b {
                return Ok(Some(match self.byte()? {
                    Some(b'[' | b'O') => {
                        let mut code = Vec::new();
                        while let Some(next) = self.byte()? {
                            code.push(next);
                            if next.is_ascii_alphabetic() || next == b'~' {
                                break;
                            }
                        }
                        sequence(&code)
                    }
                    Some(b'f') => Key::WordRight,
                    Some(b'b') => Key::WordLeft,
                    _ => Key::Other,
                }));
            }
            return Ok(Some(if byte >= 0x20 {
                Key::Char(char::from(byte))
            } else {
                control(byte)
            }));
        }
    }
}

/// The line being written and the cursor in it, in characters.
#[derive(Default)]
struct Line {
    chars: Vec<char>,
    cursor: usize,
}

fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

impl Line {
    fn text(&self) -> String {
        self.chars.iter().collect()
    }

    fn set(&mut self, text: &str) {
        self.chars = text.chars().collect();
        self.cursor = self.chars.len();
    }

    fn insert(&mut self, character: char) {
        let at = self.cursor.min(self.chars.len());
        self.chars.insert(at, character);
        self.cursor = at.saturating_add(1);
    }

    fn is_word_at(&self, at: usize) -> Option<bool> {
        self.chars.get(at).copied().map(is_word)
    }

    /// Where the word before the cursor starts.
    fn word_start(&self) -> usize {
        let mut at = self.cursor;
        while at > 0 && self.is_word_at(at.saturating_sub(1)) == Some(false) {
            at = at.saturating_sub(1);
        }
        while at > 0 && self.is_word_at(at.saturating_sub(1)) == Some(true) {
            at = at.saturating_sub(1);
        }
        at
    }

    /// Where the word after the cursor ends.
    fn word_end(&self) -> usize {
        let mut at = self.cursor;
        while self.is_word_at(at) == Some(false) {
            at = at.saturating_add(1);
        }
        while self.is_word_at(at) == Some(true) {
            at = at.saturating_add(1);
        }
        at
    }

    fn remove_at_cursor(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.cursor.saturating_sub(1);
            self.chars.remove(self.cursor);
        }
    }

    fn kill_word(&mut self) {
        let start = self.word_start();
        self.chars.drain(start..self.cursor);
        self.cursor = start;
    }
}

impl Editor {
    /// An editor whose history lives in `path`.
    #[must_use]
    pub fn new(path: Option<PathBuf>) -> Self {
        let history = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| text.lines().map(unescape).collect())
            .unwrap_or_default();
        Self {
            history,
            path,
            terminal: rustix::termios::isatty(io::stdin().as_fd())
                && rustix::termios::isatty(io::stdout().as_fd()),
        }
    }

    /// Whether keys are read from a terminal.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        self.terminal
    }

    /// Keep `entry` in the history, and in its file.
    pub fn remember(&mut self, entry: &str) {
        if entry.trim().is_empty() || self.history.last().is_some_and(|last| last == entry) {
            return;
        }
        self.history.push(entry.to_owned());
        if self.history.len() > HISTORY_KEPT {
            self.history.remove(0);
        }
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let text: String = self
                .history
                .iter()
                .map(|entry| escape(entry) + "\n")
                .collect();
            let _ = std::fs::write(path, text);
        }
    }

    /// Read a line after `prompt`, which may hold colour.
    ///
    /// # Errors
    /// The terminal could not be read or written.
    pub fn read(&self, prompt: &str, completer: &dyn Complete) -> io::Result<Read> {
        if !self.terminal {
            return Self::read_plain(prompt);
        }
        let raw = Raw::enter()?;
        let read = self.read_raw(prompt, completer);
        drop(raw);
        read
    }

    fn read_plain(prompt: &str) -> io::Result<Read> {
        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            return Ok(Read::Eof);
        }
        let line = line.trim_end_matches(['\n', '\r']).to_owned();
        let mut out = io::stdout();
        writeln!(out, "{prompt}{line}")?;
        out.flush()?;
        Ok(Read::Line(line))
    }

    /// Complete the word before the cursor: as far as every candidate agrees, and list them
    /// when that is no further.
    fn complete(
        line: &mut Line,
        completer: &dyn Complete,
        out: &mut impl io::Write,
    ) -> io::Result<()> {
        let (start, candidates) = completer.complete(&line.text(), line.cursor);
        let typed = line.cursor.saturating_sub(start);
        let common = common_prefix(&candidates);
        if common.chars().count() > typed {
            for character in common.chars().skip(typed) {
                line.insert(character);
            }
        } else if candidates.len() > 1 {
            write!(out, "\r\n{}", in_columns(&candidates, columns()))?;
        }
        Ok(())
    }

    fn read_raw(&self, prompt: &str, completer: &dyn Complete) -> io::Result<Read> {
        let mut out = io::stdout();
        let mut line = Line::default();
        // Where in the history Up and Down are, and the line being written before them.
        let mut place = self.history.len();
        let mut draft = String::new();
        let mut keys = Keys {
            input: io::stdin().lock(),
            pending: Vec::new(),
        };
        redraw(&mut out, prompt, &line)?;
        while let Some(key) = keys.next()? {
            match key {
                Key::Enter => {
                    write!(out, "\r\n")?;
                    out.flush()?;
                    return Ok(Read::Line(line.text()));
                }
                Key::Interrupt => {
                    write!(out, "^C\r\n")?;
                    out.flush()?;
                    return Ok(Read::Interrupted);
                }
                Key::EndOrDelete if line.chars.is_empty() => {
                    write!(out, "\r\n")?;
                    out.flush()?;
                    return Ok(Read::Eof);
                }
                Key::EndOrDelete | Key::Delete => line.remove_at_cursor(),
                Key::Home => line.cursor = 0,
                Key::End => line.cursor = line.chars.len(),
                Key::Left => line.cursor = line.cursor.saturating_sub(1),
                Key::Right => line.cursor = line.cursor.saturating_add(1).min(line.chars.len()),
                Key::WordLeft => line.cursor = line.word_start(),
                Key::WordRight => line.cursor = line.word_end(),
                Key::KillToEnd => line.chars.truncate(line.cursor),
                Key::KillToStart => {
                    line.chars.drain(..line.cursor);
                    line.cursor = 0;
                }
                Key::KillWord => line.kill_word(),
                Key::Clear => write!(out, "\x1b[H\x1b[2J")?,
                Key::Backspace => line.backspace(),
                Key::Tab => Self::complete(&mut line, completer, &mut out)?,
                Key::Up if place > 0 => {
                    if place == self.history.len() {
                        draft = line.text();
                    }
                    place = place.saturating_sub(1);
                    line.set(self.history.get(place).map_or("", String::as_str));
                }
                Key::Down if place < self.history.len() => {
                    place = place.saturating_add(1);
                    let next = self
                        .history
                        .get(place)
                        .map_or(draft.as_str(), String::as_str);
                    line.set(next);
                }
                Key::Char(character) => line.insert(character),
                Key::Up | Key::Down | Key::Other => {}
            }
            redraw(&mut out, prompt, &line)?;
        }
        Ok(Read::Eof)
    }
}

fn redraw(out: &mut impl io::Write, prompt: &str, line: &Line) -> io::Result<()> {
    write!(
        out,
        "\r{prompt}{}\x1b[K",
        super::style::highlight(&line.text())
    )?;
    let back = line.chars.len().saturating_sub(line.cursor);
    if back > 0 {
        write!(out, "\x1b[{back}D")?;
    }
    out.flush()
}

/// The longest start every candidate shares.
#[must_use]
pub fn common_prefix(candidates: &[String]) -> String {
    let mut all = candidates.iter();
    let Some(first) = all.next() else {
        return String::new();
    };
    let mut prefix: Vec<char> = first.chars().collect();
    for candidate in all {
        let shared = prefix
            .iter()
            .zip(candidate.chars())
            .take_while(|(a, b)| **a == *b)
            .count();
        prefix.truncate(shared);
    }
    prefix.into_iter().collect()
}

/// Candidates laid out in columns across `width`.
#[must_use]
pub fn in_columns(candidates: &[String], width: usize) -> String {
    let widest = candidates
        .iter()
        .map(|c| c.chars().count())
        .max()
        .unwrap_or(0)
        .saturating_add(2);
    let per_row = width.checked_div(widest).unwrap_or(1).max(1);
    let mut out = String::new();
    for row in candidates.chunks(per_row) {
        let mut line = String::new();
        for candidate in row {
            let _ = write!(line, "{candidate:<widest$}");
        }
        out.push_str(line.trim_end());
        out.push_str("\r\n");
    }
    out
}

fn escape(entry: &str) -> String {
    entry.replace('\\', "\\\\").replace('\n', "\\n")
}

fn unescape(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(character);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_entries_survive_their_file() {
        let entry = "let a = {\n  1 \\ 2\n}";
        assert_eq!(unescape(&escape(entry)), entry);
    }

    #[test]
    fn completion_extends_to_what_every_candidate_shares() {
        let candidates = vec!["notes.pin".to_owned(), "notes.pinned".to_owned()];
        assert_eq!(common_prefix(&candidates), "notes.pin");
    }

    #[test]
    fn words_end_at_dots() {
        let mut line = Line::default();
        line.set("notes.pin(n1)");
        line.cursor = 9;
        assert_eq!(line.word_start(), 6);
        line.cursor = 0;
        assert_eq!(line.word_end(), 5);
        line.cursor = 9;
        line.kill_word();
        assert_eq!(line.text(), "notes.(n1)");
    }

    #[test]
    fn keys_read_as_a_terminal_sends_them() {
        let mut keys = Keys {
            input: "a\u{1b}[D\u{1b}[3~é\t\r".as_bytes(),
            pending: Vec::new(),
        };
        let mut read = Vec::new();
        while let Some(key) = keys.next().unwrap() {
            read.push(key);
        }
        assert_eq!(
            read,
            [
                Key::Char('a'),
                Key::Left,
                Key::Delete,
                Key::Char('é'),
                Key::Tab,
                Key::Enter
            ]
        );
    }
}
