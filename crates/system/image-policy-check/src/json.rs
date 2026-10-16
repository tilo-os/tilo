//! JSON as the check reads it: objects keep their key order, a duplicate key
//! anywhere is refused (the effective rule would be ambiguous), and values
//! appear in messages as Python's `repr()` shows them.

use std::fmt::{self, Write as _};

use serde::Deserialize;
use serde::de::{self, MapAccess, SeqAccess, Visitor};

/// A parsed JSON value; objects are ordered key-value lists.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

impl Json {
    /// `value.get(key)` on an object.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }
}

/// Marks the duplicate-key refusal inside serde's error, so it is reported as
/// the original reported it rather than as a syntax error.
const DUPLICATE: &str = "\u{0}duplicate:";

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Json, E> {
        Ok(Json::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Json, E> {
        Ok(Json::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Json, E> {
        Ok(Json::Number(value.into()))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Json, E> {
        serde_json::Number::from_f64(value)
            .map(Json::Number)
            .ok_or_else(|| E::custom("number out of range"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Json, E> {
        Ok(Json::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Json, E> {
        Ok(Json::String(value))
    }

    fn visit_unit<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_none<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element::<Json>()? {
            items.push(item);
        }
        Ok(Json::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut fields: Vec<(String, Json)> = Vec::new();
        while let Some((key, value)) = map.next_entry::<String, Json>()? {
            fields.push((key, value));
        }
        // Like json.loads' object_pairs_hook: judged once the object closes.
        for (index, (key, _)) in fields.iter().enumerate() {
            if fields.iter().take(index).any(|(earlier, _)| earlier == key) {
                return Err(de::Error::custom(format!("{DUPLICATE}{key}")));
            }
        }
        Ok(Json::Object(fields))
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

/// Parses policy text.
///
/// # Errors
/// `duplicate key '…'; the effective rule is ambiguous`, or
/// `policy is not valid JSON: …`.
pub fn parse(text: &str) -> Result<Json, String> {
    serde_json::from_str::<Json>(text).map_err(|error| {
        let message = error.to_string();
        message.find(DUPLICATE).map_or_else(
            || format!("policy is not valid JSON: {message}"),
            |at| {
                let key = message
                    .get(at.saturating_add(DUPLICATE.len())..)
                    .unwrap_or_default();
                // serde appends " at line L column C"; the key is everything before it.
                let key = key.rsplit_once(" at line ").map_or(key, |(key, _)| key);
                format!(
                    "duplicate key {}; the effective rule is ambiguous",
                    repr_str(key)
                )
            },
        )
    })
}

/// `repr()` of a string: single-quoted unless it holds a `'` and no `"`.
#[must_use]
pub fn repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len().saturating_add(2));
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if u32::from(c) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\x{:02x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn repr_number(number: &serde_json::Number) -> String {
    if number.is_f64() {
        let float = number.as_f64().unwrap_or_default();
        let text = format!("{float:?}");
        // Python writes the exponent with its sign: 1e+16, 1e-07.
        match text.split_once('e') {
            Some((mantissa, exponent)) if !exponent.starts_with('-') => {
                format!("{mantissa}e+{exponent}")
            }
            _ => text,
        }
    } else {
        number.to_string()
    }
}

/// `repr()` of an optional JSON value, `None` for a missing one.
#[must_use]
pub fn repr(value: Option<&Json>) -> String {
    match value {
        None | Some(Json::Null) => "None".to_owned(),
        Some(Json::Bool(true)) => "True".to_owned(),
        Some(Json::Bool(false)) => "False".to_owned(),
        Some(Json::Number(number)) => repr_number(number),
        Some(Json::String(text)) => repr_str(text),
        Some(Json::Array(items)) => {
            let shown: Vec<String> = items.iter().map(|item| repr(Some(item))).collect();
            format!("[{}]", shown.join(", "))
        }
        Some(Json::Object(fields)) => {
            let shown: Vec<String> = fields
                .iter()
                .map(|(key, value)| format!("{}: {}", repr_str(key), repr(Some(value))))
                .collect();
            format!("{{{}}}", shown.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failed fixture call fails the test")]

    use super::*;

    #[test]
    fn duplicates_are_refused_at_any_depth_with_the_key_named() {
        assert_eq!(
            parse(r#"{"a": {"b": 1, "b": 2}}"#).unwrap_err(),
            "duplicate key 'b'; the effective rule is ambiguous"
        );
        assert_eq!(
            parse(r#"{"it's": 1, "it's": 2}"#).unwrap_err(),
            "duplicate key \"it's\"; the effective rule is ambiguous"
        );
        assert!(parse(r#"{"a": 1, "b": {"a": 2}}"#).is_ok());
        assert!(
            parse("{")
                .unwrap_err()
                .starts_with("policy is not valid JSON: ")
        );
    }

    #[test]
    fn objects_keep_their_order() {
        let Json::Object(fields) = parse(r#"{"z": 1, "a": 2, "m": 3}"#).unwrap() else {
            panic!("not an object");
        };
        let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["z", "a", "m"]);
    }

    #[test]
    fn values_read_like_python_repr() {
        let value =
            parse(r#"[null, true, false, 1, 1.5, 1e16, "a'b", "x\ny", {"k": []}]"#).unwrap();
        assert_eq!(
            repr(Some(&value)),
            r#"[None, True, False, 1, 1.5, 1e+16, "a'b", 'x\ny', {'k': []}]"#
        );
        assert_eq!(repr(None), "None");
    }
}
