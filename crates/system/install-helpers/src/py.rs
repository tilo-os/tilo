//! The Python behaviours the helpers' checks were defined with: what counts
//! as whitespace, how a text file's line endings read, and how strictly
//! `base64.b64decode(validate=True)` decodes.

pub use tilo_pycompat::{Dumps, b64decode, int, is_space, split, strip, truthy};

/// What `Path.read_text()` returns for `bytes`: universal newlines, so
/// `\r\n` and a lone `\r` both read as `\n`.
#[must_use]
pub fn universal_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
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
    fn text_reads_with_universal_newlines() {
        assert_eq!(universal_newlines("a\r\nb\rc\n"), "a\nb\nc\n");
    }
}
