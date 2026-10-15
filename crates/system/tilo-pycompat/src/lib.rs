//! Python behaviours that signed documents, status files and checks were
//! first defined with, kept exact so the Rust ports judge and write them the
//! same way: what counts as whitespace and as a line break, how `int()` reads
//! a number, how strictly `base64.b64decode(validate=True)` decodes, what
//! `bool()` makes of a JSON value, and the text `json.dumps` writes.

use std::fmt::Write as _;

use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use serde_json::Value;

/// `str.isspace()`: Unicode whitespace plus the four ASCII separators
/// (U+001C..U+001F) that Rust does not count.
#[must_use]
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()`.
#[must_use]
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `str.split()`: runs of whitespace separate, none at either end.
#[must_use]
pub fn split(text: &str) -> Vec<&str> {
    text.split(is_space)
        .filter(|part| !part.is_empty())
        .collect()
}

fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r'
            | '\u{0b}'
            | '\u{0c}'
            | '\u{1c}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// `str.splitlines()`.
#[must_use]
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let Some((at, c)) = rest.char_indices().find(|&(_, c)| is_line_break(c)) else {
            lines.push(rest);
            break;
        };
        let (line, tail) = rest.split_at(at);
        lines.push(line);
        let mut tail = tail.strip_prefix(c).unwrap_or(tail);
        if c == '\r' {
            tail = tail.strip_prefix('\n').unwrap_or(tail);
        }
        rest = tail;
    }
    lines
}

/// `int(text)` for a decimal string: surrounding whitespace, a sign, and `_`
/// between digits. Values beyond `i128` saturate; callers refuse them by
/// range, as Python did. Non-ASCII decimal digits, which Python also reads,
/// are refused.
///
/// # Errors
/// Anything else, worded as Python worded it.
pub fn int(text: &str) -> Result<i128, String> {
    let invalid = || format!("invalid literal for int() with base 10: {text:?}");
    let trimmed = strip(text);
    let (negative, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, trimmed.get(1..).unwrap_or_default()),
        Some(b'+') => (false, trimmed.get(1..).unwrap_or_default()),
        _ => (false, trimmed),
    };
    let bytes = digits.as_bytes();
    if bytes.is_empty() || bytes.first() == Some(&b'_') || bytes.last() == Some(&b'_') {
        return Err(invalid());
    }
    let mut value: i128 = 0;
    let mut previous_underscore = false;
    for byte in bytes {
        if *byte == b'_' {
            if previous_underscore {
                return Err(invalid());
            }
            previous_underscore = true;
            continue;
        }
        previous_underscore = false;
        if !byte.is_ascii_digit() {
            return Err(invalid());
        }
        value = value
            .saturating_mul(10)
            .saturating_add(i128::from(byte.saturating_sub(b'0')));
    }
    Ok(if negative {
        value.saturating_neg()
    } else {
        value
    })
}

/// `base64.b64decode(text, validate=True)`: the standard alphabet with
/// correct padding and nothing else; unused trailing bits are ignored.
///
/// # Errors
/// Any other character, missing or misplaced padding.
pub fn b64decode(text: &str) -> Result<Vec<u8>, base64::DecodeError> {
    const STRICT: GeneralPurpose = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(DecodePaddingMode::RequireCanonical),
    );
    STRICT.decode(text)
}

/// `bool(value)` for a parsed JSON value.
#[must_use]
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|f| f != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// A number as Python's `repr` (and `json.dumps`) writes it.
#[must_use]
pub fn number_repr(number: &serde_json::Number) -> String {
    if number.is_i64() || number.is_u64() {
        return number.to_string();
    }
    let value = number.as_f64().unwrap_or_default();
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e16 {
        return format!("{value:.1}");
    }
    let text = format!("{value:?}");
    match text.split_once('e') {
        Some((mantissa, exponent)) if !exponent.starts_with('-') => {
            format!("{mantissa}e+{exponent:0>2}")
        }
        Some((mantissa, exponent)) => {
            format!("{mantissa}e-{:0>2}", exponent.trim_start_matches('-'))
        }
        None => text,
    }
}

/// How `json.dumps` was called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dumps {
    /// `sort_keys=True`; otherwise insertion order.
    pub sort_keys: bool,
    /// `separators=(",", ":")`; otherwise the default `", "` and `": "`.
    pub compact: bool,
    /// `indent=2`: each item on its own line, two spaces per level.
    pub indent: bool,
}

impl Dumps {
    /// `json.dumps(value, sort_keys=True)`.
    pub const SORTED: Self = Self {
        sort_keys: true,
        compact: false,
        indent: false,
    };
    /// `json.dumps(value, sort_keys=True, separators=(",", ":"))`.
    pub const CANONICAL: Self = Self {
        sort_keys: true,
        compact: true,
        indent: false,
    };
    /// `json.dumps(value, separators=(",", ":"))`.
    pub const COMPACT: Self = Self {
        sort_keys: false,
        compact: true,
        indent: false,
    };
    /// `json.dumps(value, indent=2, sort_keys=True)`.
    pub const INDENTED: Self = Self {
        sort_keys: true,
        compact: false,
        indent: true,
    };

    /// The text `json.dumps` wrote for `value` (with `ensure_ascii`).
    #[must_use]
    pub fn dumps(self, value: &Value) -> String {
        let mut out = String::new();
        self.dump(value, 0, &mut out);
        out
    }

    fn dump(self, value: &Value, level: usize, out: &mut String) {
        let key_separator = if self.compact { ":" } else { ": " };
        match value {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::Number(number) => out.push_str(&number_repr(number)),
            Value::String(text) => string(text, out),
            Value::Array(items) if items.is_empty() => out.push_str("[]"),
            Value::Object(map) if map.is_empty() => out.push_str("{}"),
            Value::Array(items) => {
                out.push('[');
                for (index, value) in items.iter().enumerate() {
                    self.separate(index, level, out);
                    self.dump(value, level.saturating_add(1), out);
                }
                self.close(level, out);
                out.push(']');
            }
            Value::Object(map) => {
                let mut entries: Vec<(&String, &Value)> = map.iter().collect();
                if self.sort_keys {
                    entries.sort_by(|a, b| a.0.cmp(b.0));
                }
                out.push('{');
                for (index, (name, value)) in entries.into_iter().enumerate() {
                    self.separate(index, level, out);
                    string(name, out);
                    out.push_str(key_separator);
                    self.dump(value, level.saturating_add(1), out);
                }
                self.close(level, out);
                out.push('}');
            }
        }
    }

    /// What goes before the item at `index` of a container at `level`.
    fn separate(self, index: usize, level: usize, out: &mut String) {
        if index > 0 {
            out.push_str(if self.compact || self.indent {
                ","
            } else {
                ", "
            });
        }
        if self.indent {
            newline(level.saturating_add(1), out);
        }
    }

    fn close(self, level: usize, out: &mut String) {
        if self.indent {
            newline(level, out);
        }
    }
}

fn newline(level: usize, out: &mut String) {
    out.push('\n');
    for _ in 0..level {
        out.push_str("  ");
    }
}

/// A JSON string with `ensure_ascii`: everything outside printable ASCII is
/// a `\uXXXX` escape (a surrogate pair above the BMP).
fn string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failed fixture call fails the test")]

    use super::*;

    #[test]
    fn dumps_matches_python_json() {
        let value: Value = serde_json::from_str(
            r#"{"b": [1, {"y": null, "x": true}], "a": "é😀\"\n\u007f", "c": {}, "d": [], "e": 1.0}"#,
        )
        .unwrap();
        assert_eq!(
            Dumps::SORTED.dumps(&value),
            r#"{"a": "\u00e9\ud83d\ude00\"\n\u007f", "b": [1, {"x": true, "y": null}], "c": {}, "d": [], "e": 1.0}"#
        );
        assert_eq!(
            Dumps::CANONICAL.dumps(&value),
            r#"{"a":"\u00e9\ud83d\ude00\"\n\u007f","b":[1,{"x":true,"y":null}],"c":{},"d":[],"e":1.0}"#
        );
        assert_eq!(
            Dumps::COMPACT.dumps(&value),
            r#"{"b":[1,{"y":null,"x":true}],"a":"\u00e9\ud83d\ude00\"\n\u007f","c":{},"d":[],"e":1.0}"#
        );
        assert_eq!(
            Dumps::INDENTED.dumps(&value),
            "{\n  \"a\": \"\\u00e9\\ud83d\\ude00\\\"\\n\\u007f\",\n  \"b\": [\n    1,\n    {\n      \
             \"x\": true,\n      \"y\": null\n    }\n  ],\n  \"c\": {},\n  \"d\": [],\n  \"e\": 1.0\n}"
        );
    }

    #[test]
    fn whitespace_and_lines_read_as_python_read_them() {
        assert_eq!(strip("\u{1c} a b\x0b\t"), "a b");
        assert_eq!(strip("\u{1f} x \u{85}"), "x");
        assert_eq!(split(" a\t b\u{1f}c "), ["a", "b", "c"]);
        assert_eq!(
            splitlines("a\r\nb\rc\u{2028}d\n\ne\u{1c}f"),
            ["a", "b", "c", "d", "", "e", "f"]
        );
    }

    #[test]
    fn int_reads_like_python() {
        for (text, value) in [("17", 17), (" 17\n", 17), ("+1_000", 1000), ("-1", -1)] {
            assert_eq!(int(text), Ok(value), "{text}");
        }
        for text in ["", "1.5", "0x1", "1__0", "_1", "x"] {
            assert!(int(text).is_err(), "{text}");
        }
    }

    #[test]
    fn base64_is_strict_about_alphabet_and_padding() {
        assert_eq!(b64decode("AAAAC3Nz").unwrap(), [0, 0, 0, 11, 115, 115]);
        assert_eq!(b64decode("QR==").unwrap(), b"A", "unused bits are ignored");
        for text in ["QQ", "QQ=", "Q Q==", "QQ==\n", "QQ==QQ==", "-_8=", "=QQ="] {
            assert!(b64decode(text).is_err(), "{text}");
        }
    }
}
