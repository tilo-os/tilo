//! The Python behaviours the checks were defined with.
//!
//! The selector and the hardware-profile reader were Python, and signed
//! release documents and image files are judged exactly as they judged them:
//! what counts as whitespace and as a line break, how `int()` reads a number,
//! how strictly `base64.b64decode(validate=True)` decodes, and what `==`
//! means between a JSON value and a number.

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

/// `bytes.strip()`: ASCII whitespace only.
#[must_use]
pub fn strip_bytes(bytes: &[u8]) -> &[u8] {
    let space = |b: &u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
    let start = bytes.iter().position(|b| !space(b)).unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !space(b))
        .map_or(start, |i| i.saturating_add(1));
    bytes.get(start..end).unwrap_or_default()
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
/// between digits. Values beyond `i128` saturate; every caller refuses them
/// by range, as Python did. Non-ASCII decimal digits, which Python also
/// reads, are refused.
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

/// `type(value) is int`: a JSON integer, never a boolean or a float.
#[must_use]
pub fn as_int(value: &Value) -> Option<i128> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .map(i128::from)
            .or_else(|| number.as_u64().map(i128::from)),
        _ => None,
    }
}

/// `value == number` in Python, where `True == 1` and `1.0 == 1`.
#[must_use]
pub fn equals_number(value: &Value, number: i64) -> bool {
    match value {
        Value::Bool(flag) => i64::from(*flag) == number,
        Value::Number(n) => {
            if let Some(int) = as_int(value) {
                return int == i128::from(number);
            }
            #[allow(
                clippy::cast_precision_loss,
                reason = "the numbers compared are small constants"
            )]
            let target = number as f64;
            #[allow(
                clippy::float_cmp,
                reason = "Python compares an int and a float exactly"
            )]
            n.as_f64().is_some_and(|float| float == target)
        }
        _ => false,
    }
}

/// `set(value)` compared with a set of names: the keys of an object, the
/// elements of an array, the characters of a string. `None` for a value
/// `set()` refuses (a number, a boolean, null), which raised `TypeError`.
#[must_use]
pub fn set_of(value: &Value) -> Option<std::collections::BTreeSet<String>> {
    match value {
        Value::Object(map) => Some(map.keys().cloned().collect()),
        Value::Array(items) => {
            let mut set = std::collections::BTreeSet::new();
            for item in items {
                // Only strings can equal the names compared against; any other
                // element makes the sets differ.
                set.insert(match item {
                    Value::String(text) => text.clone(),
                    other => format!("\u{0}{other}"),
                });
            }
            Some(set)
        }
        Value::String(text) => Some(text.chars().map(String::from).collect()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failed fixture call fails the test")]

    use super::*;
    use serde_json::json;

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
    fn b64decode_is_strict_about_alphabet_and_padding() {
        assert_eq!(b64decode("QQ==").unwrap(), b"A");
        assert_eq!(b64decode("QR==").unwrap(), b"A", "unused bits are ignored");
        for text in ["QQ", "QQ=", "Q Q==", "QQ==\n", "QQ==QQ==", "-_8=", "=QQ="] {
            assert!(b64decode(text).is_err(), "{text}");
        }
    }

    #[test]
    fn splitlines_and_strip_follow_python() {
        assert_eq!(splitlines("a\r\nb\rc\u{2028}d\n"), ["a", "b", "c", "d"]);
        assert_eq!(strip("\u{1f} x \u{85}"), "x");
        assert_eq!(strip_bytes(b"\x0b ab \n"), b"ab");
    }

    #[test]
    fn numbers_compare_like_python() {
        assert!(equals_number(&json!(1), 1));
        assert!(equals_number(&json!(1.0), 1));
        assert!(equals_number(&json!(true), 1));
        assert!(!equals_number(&json!("1"), 1));
        assert_eq!(as_int(&json!(true)), None);
        assert_eq!(as_int(&json!(2.0)), None);
        assert_eq!(as_int(&json!(u64::MAX)), Some(i128::from(u64::MAX)));
    }
}
