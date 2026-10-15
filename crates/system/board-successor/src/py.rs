//! The Python behaviours the checks were defined with.
//!
//! The selector and the hardware-profile reader were Python, and signed
//! release documents and image files are judged exactly as they judged them:
//! what counts as whitespace and as a line break, how `int()` reads a number,
//! how strictly `base64.b64decode(validate=True)` decodes, and what `==`
//! means between a JSON value and a number.

use serde_json::Value;
pub use tilo_pycompat::{b64decode, int, splitlines, strip};

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
    fn bytes_strip_ascii_whitespace() {
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
