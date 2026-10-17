//! The report's values, and the two ways the report prints them: JSON as
//! `json.dumps(indent=2, sort_keys=True)` writes it (the format the report has
//! always had, which scripts and tests read), and the `repr` form the render
//! gate's log lines use.

use std::fmt::Write as _;

/// A report value. Maps keep the order their entries were added in.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `null` / `None`
    Null,
    /// `true` / `True`
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A float.
    Float(f64),
    /// A string.
    Str(String),
    /// A list.
    List(Vec<Value>),
    /// A map, in insertion order.
    Map(Vec<(String, Value)>),
}

impl Value {
    /// An empty map.
    #[must_use]
    pub fn map() -> Self {
        Self::Map(Vec::new())
    }

    /// Sets `key` (replacing an earlier entry), for a map.
    pub fn set(&mut self, key: &str, value: impl Into<Value>) {
        if let Self::Map(entries) = self {
            let value = value.into();
            match entries.iter_mut().find(|(existing, _)| existing == key) {
                Some((_, slot)) => *slot = value,
                None => entries.push((key.to_owned(), value)),
            }
        }
    }

    /// The entry at `key`, for a map.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Map(entries) => entries
                .iter()
                .find(|(existing, _)| existing == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// The integer, if this is one.
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// The string, if this is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(value) => Some(value),
            _ => None,
        }
    }

    /// Whether this is `true`.
    #[must_use]
    pub fn is_true(&self) -> bool {
        matches!(self, Self::Bool(true))
    }

    /// The JSON text, indented by two spaces with keys sorted and non-ASCII
    /// escaped.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out, 0);
        out
    }

    fn write_json(&self, out: &mut String, depth: usize) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Self::Int(value) => {
                let _ = write!(out, "{value}");
            }
            Self::Float(value) => out.push_str(&float_repr(*value)),
            Self::Str(value) => json_string(out, value),
            Self::List(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    newline(out, depth.saturating_add(1));
                    item.write_json(out, depth.saturating_add(1));
                }
                newline(out, depth);
                out.push(']');
            }
            Self::Map(entries) => {
                if entries.is_empty() {
                    out.push_str("{}");
                    return;
                }
                let mut sorted: Vec<&(String, Value)> = entries.iter().collect();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                out.push('{');
                for (index, (key, value)) in sorted.into_iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    newline(out, depth.saturating_add(1));
                    json_string(out, key);
                    out.push_str(": ");
                    value.write_json(out, depth.saturating_add(1));
                }
                newline(out, depth);
                out.push('}');
            }
        }
    }

    /// The form Python's `repr` gives the same value: `{'frames': 120}`,
    /// `None`, `'enabled'`.
    #[must_use]
    pub fn repr(&self) -> String {
        let mut out = String::new();
        self.write_repr(&mut out);
        out
    }

    fn write_repr(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("None"),
            Self::Bool(value) => out.push_str(if *value { "True" } else { "False" }),
            Self::Int(value) => {
                let _ = write!(out, "{value}");
            }
            Self::Float(value) => out.push_str(&float_repr(*value)),
            Self::Str(value) => repr_string(out, value),
            Self::List(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    item.write_repr(out);
                }
                out.push(']');
            }
            Self::Map(entries) => {
                out.push('{');
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    repr_string(out, key);
                    out.push_str(": ");
                    value.write_repr(out);
                }
                out.push('}');
            }
        }
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<usize> for Value {
    fn from(value: usize) -> Self {
        Self::Int(i64::try_from(value).unwrap_or(i64::MAX))
    }
}

impl From<u64> for Value {
    fn from(value: u64) -> Self {
        Self::Int(i64::try_from(value).unwrap_or(i64::MAX))
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::Str(value.to_owned())
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Null, Into::into)
    }
}

impl From<Vec<Value>> for Value {
    fn from(value: Vec<Value>) -> Self {
        Self::List(value)
    }
}

fn newline(out: &mut String, depth: usize) {
    out.push('\n');
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn json_string(out: &mut String, value: &str) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(character),
            other => {
                // Everything else as \uXXXX, astral characters as a
                // surrogate pair, as `ensure_ascii` writes them.
                let mut units = [0_u16; 2];
                for unit in other.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

fn repr_string(out: &mut String, value: &str) {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    out.push(quote);
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
}

/// A float as Python's `repr` writes it: the shortest digits that read back
/// the same, in positional notation between 1e-4 and 1e16, otherwise in
/// exponent form.
#[must_use]
pub fn float_repr(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .to_owned();
    }
    // `{:e}` gives the shortest round-trip digits: "5.994e1".
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .unwrap_or((scientific.as_str(), "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let sign = if negative { "-" } else { "" };
    if (-4..16).contains(&exponent) {
        // The decimal point sits after `exponent + 1` digits.
        let point = exponent.saturating_add(1);
        let body = if point <= 0 {
            let zeros = usize::try_from(point.unsigned_abs()).unwrap_or(0);
            format!("0.{}{digits}", "0".repeat(zeros))
        } else {
            let point = usize::try_from(point).unwrap_or(0);
            match (digits.get(..point), digits.get(point..)) {
                (Some(whole), Some(fraction)) if !fraction.is_empty() => {
                    format!("{whole}.{fraction}")
                }
                _ => format!(
                    "{digits}{}.0",
                    "0".repeat(point.saturating_sub(digits.len()))
                ),
            }
        };
        format!("{sign}{body}")
    } else {
        let (first, rest) = digits.split_at(1.min(digits.len()));
        let fraction = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        format!(
            "{sign}{first}{fraction}e{exponent_sign}{:02}",
            exponent.abs()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_print_as_python_repr_does() {
        for (value, expected) in [
            (0.0, "0.0"),
            (100.0, "100.0"),
            (1000.0 / 16.683, "59.94125756758377"),
            (0.5, "0.5"),
            (0.000_123, "0.000123"),
            (0.000_012_3, "1.23e-05"),
            (1e16, "1e+16"),
            (123_456_789_012_345.6, "123456789012345.6"),
            (-2.5, "-2.5"),
        ] {
            assert_eq!(float_repr(value), expected, "{value}");
        }
    }

    #[test]
    fn json_is_sorted_indented_and_ascii() {
        let mut inner = Value::map();
        inner.set("b", 1_i64);
        inner.set("a", Value::List(vec![]));
        let mut value = Value::map();
        value.set("z", "é\u{1f600}\"\x1b");
        value.set("m", inner);
        value.set("e", Value::map());
        value.set("n", Value::Null);
        assert_eq!(
            value.to_json(),
            "{\n  \"e\": {},\n  \"m\": {\n    \"a\": [],\n    \"b\": 1\n  },\n  \"n\": null,\n  \"z\": \"\\u00e9\\ud83d\\ude00\\\"\\u001b\"\n}"
        );
    }

    #[test]
    fn repr_matches_python() {
        let mut value = Value::map();
        value.set("status", "enabled");
        value.set("reason", Value::Null);
        value.set("quoted", "\"not-initialized\"");
        value.set("count", 3_i64);
        assert_eq!(
            value.repr(),
            "{'status': 'enabled', 'reason': None, 'quoted': '\"not-initialized\"', 'count': 3}"
        );
        assert_eq!(Value::from("it's").repr(), "\"it's\"");
    }
}
