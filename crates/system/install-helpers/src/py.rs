//! The Python behaviours the helpers' checks were defined with: what counts
//! as whitespace, how a text file's line endings read, and how strictly
//! `base64.b64decode(validate=True)` decodes.

use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

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

/// What `Path.read_text()` returns for `bytes`: universal newlines, so
/// `\r\n` and a lone `\r` both read as `\n`.
#[must_use]
pub fn universal_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
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

/// How `json.dumps` was called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dumps {
    /// `sort_keys=True`; otherwise insertion order.
    pub sort_keys: bool,
    /// `separators=(",", ":")`; otherwise the default `", "` and `": "`.
    pub compact: bool,
}

impl Dumps {
    /// `json.dumps(value, sort_keys=True)`.
    pub const SORTED: Self = Self {
        sort_keys: true,
        compact: false,
    };
    /// `json.dumps(value, sort_keys=True, separators=(",", ":"))`.
    pub const CANONICAL: Self = Self {
        sort_keys: true,
        compact: true,
    };
    /// `json.dumps(value, separators=(",", ":"))`.
    pub const COMPACT: Self = Self {
        sort_keys: false,
        compact: true,
    };

    /// The text `json.dumps` wrote for `value` (with `ensure_ascii`).
    #[must_use]
    pub fn dumps(self, value: &serde_json::Value) -> String {
        let mut out = String::new();
        self.dump(value, &mut out);
        out
    }

    fn dump(self, value: &serde_json::Value, out: &mut String) {
        use serde_json::Value;
        let (item, key) = if self.compact {
            (",", ":")
        } else {
            (", ", ": ")
        };
        match value {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::Number(number) => out.push_str(&number_text(number)),
            Value::String(text) => string(text, out),
            Value::Array(items) => {
                out.push('[');
                for (index, value) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(item);
                    }
                    self.dump(value, out);
                }
                out.push(']');
            }
            Value::Object(map) => {
                let mut entries: Vec<(&String, &Value)> = map.iter().collect();
                if self.sort_keys {
                    entries.sort_by(|a, b| a.0.cmp(b.0));
                }
                out.push('{');
                for (index, (name, value)) in entries.into_iter().enumerate() {
                    if index > 0 {
                        out.push_str(item);
                    }
                    string(name, out);
                    out.push_str(key);
                    self.dump(value, out);
                }
                out.push('}');
            }
        }
    }
}

/// A number as Python's `repr` writes it.
fn number_text(number: &serde_json::Number) -> String {
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

/// A JSON string with `ensure_ascii`: everything outside printable ASCII is
/// a `\uXXXX` escape (a surrogate pair above the BMP).
fn string(text: &str, out: &mut String) {
    use std::fmt::Write as _;
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

/// `bool(value)` for a parsed JSON value.
#[must_use]
pub fn truthy(value: &serde_json::Value) -> bool {
    use serde_json::Value;
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|f| f != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// A JSON number or boolean as the number Python's `==` compares.
fn numeric(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
        serde_json::Value::Number(number) => number.as_f64(),
        _ => None,
    }
}

/// `a == b` between parsed JSON values in Python: `True == 1 == 1.0`, and
/// objects equal regardless of key order.
#[must_use]
pub fn equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (a, b) {
        (Value::Number(x), Value::Number(y))
            if (x.is_i64() || x.is_u64()) && (y.is_i64() || y.is_u64()) =>
        {
            x == y
        }
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Bool(_) | Value::Number(_), Value::Bool(_) | Value::Number(_)) => {
            #[allow(clippy::float_cmp, reason = "Python compares the numbers exactly")]
            let same = numeric(a) == numeric(b);
            same
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| equal(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(key, x)| y.get(key).is_some_and(|y| equal(x, y)))
        }
        _ => a == b,
    }
}

/// The lowercase hex SHA-256 of `bytes` (`hashlib.sha256(bytes).hexdigest()`).
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failed fixture call fails the test")]

    use super::*;

    #[test]
    fn dumps_matches_python_json() {
        let value: serde_json::Value = serde_json::from_str(
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
    }

    #[test]
    fn whitespace_and_lines_read_as_python_read_them() {
        assert_eq!(strip("\u{1c} a b\x0b\t"), "a b");
        assert_eq!(split(" a\t b\u{1f}c "), ["a", "b", "c"]);
        assert_eq!(universal_newlines("a\r\nb\rc\n"), "a\nb\nc\n");
    }

    #[test]
    fn base64_is_strict() {
        assert_eq!(b64decode("AAAAC3Nz").unwrap(), [0, 0, 0, 11, 115, 115]);
        assert_eq!(b64decode("YQ==").unwrap(), b"a");
        for bad in ["YQ", "YQ=", "Y Q==", "YQ==YQ==", "=YQ=", "Y-Q="] {
            assert!(b64decode(bad).is_err(), "{bad}");
        }
    }
}
