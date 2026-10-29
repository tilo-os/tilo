//! Colour for the REPL: one small palette, on only where a terminal shows it.

use std::sync::atomic::{AtomicBool, Ordering};

static ON: AtomicBool = AtomicBool::new(false);

/// Colour on or off for everything the REPL prints from now on.
pub fn set(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

#[must_use]
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// What a piece of output is, which picks its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// A reference (`n3`).
    Ref,
    /// A resource's title.
    Title,
    /// A field name, a column heading.
    Label,
    Text,
    Number,
    /// `true`, `false`, enum cases, `none`.
    Literal,
    /// Counts, notes on what a result covers.
    Note,
    Error,
    Done,
    Prompt,
    Keyword,
}

const fn code(role: Role) -> &'static str {
    match role {
        Role::Ref => "36",
        Role::Title => "1",
        Role::Label | Role::Note => "2",
        Role::Text => "32",
        Role::Number => "33",
        Role::Literal => "35",
        Role::Error => "31",
        Role::Done => "32;1",
        Role::Prompt => "34;1",
        Role::Keyword => "34",
    }
}

/// `text` in the colour of `role`, when colour is on.
#[must_use]
pub fn paint(role: Role, text: &str) -> String {
    if on() && !text.is_empty() {
        format!("\x1b[{}m{text}\x1b[0m", code(role))
    } else {
        text.to_owned()
    }
}

const KEYWORDS: &[&str] = &[
    "let", "if", "else", "match", "for", "in", "fn", "true", "false", "not", "and", "or", "none",
    "undo", "confirm",
];

/// A line of input with its keywords, strings and numbers coloured, as it is typed.
#[must_use]
pub fn highlight(line: &str) -> String {
    if !on() {
        return line.to_owned();
    }
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '"' {
            let mut text = String::from(character);
            let mut escaped = false;
            for next in chars.by_ref() {
                text.push(next);
                match next {
                    _ if escaped => escaped = false,
                    '\\' => escaped = true,
                    '"' => break,
                    _ => {}
                }
            }
            out.push_str(&paint(Role::Text, &text));
        } else if character.is_ascii_digit() {
            let mut text = String::from(character);
            while let Some(next) = chars.next_if(|c| c.is_ascii_digit() || *c == '.') {
                text.push(next);
            }
            out.push_str(&paint(Role::Number, &text));
        } else if character.is_alphabetic() || character == '_' {
            let mut word = String::from(character);
            while let Some(next) = chars.next_if(|c| c.is_alphanumeric() || *c == '_') {
                word.push(next);
            }
            if KEYWORDS.contains(&word.as_str()) {
                out.push_str(&paint(Role::Keyword, &word));
            } else {
                out.push_str(&word);
            }
        } else {
            out.push(character);
        }
    }
    out
}

/// How many columns `text` takes, colour codes aside.
#[must_use]
pub fn width(text: &str) -> usize {
    let mut count: usize = 0;
    let mut escaped = false;
    for character in text.chars() {
        if escaped {
            if character.is_ascii_alphabetic() {
                escaped = false;
            }
        } else if character == '\x1b' {
            escaped = true;
        } else {
            count = count.saturating_add(1);
        }
    }
    count
}

/// `text` padded with spaces to `columns`, colour codes aside.
#[must_use]
pub fn pad(text: &str, columns: usize) -> String {
    let short = columns.saturating_sub(width(text));
    format!("{text}{}", " ".repeat(short))
}
