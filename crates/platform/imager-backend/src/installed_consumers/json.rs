//! Closed JSON wire handling shared by the installed consumer producers.
//!
//! Receipts are compared byte-for-byte and hashed, so decoding refuses
//! duplicate keys (`serde_json` would silently keep the last one) and encoding
//! is the fixed sorted, compact form every reader of these receipts expects.
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::{fmt, io};

/// Parse JSON, refusing any object that repeats a key at any depth.
pub fn strict(bytes: &[u8]) -> io::Result<Value> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = Strict
        .deserialize(&mut deserializer)
        .map_err(|error| super::invalid(&format!("invalid consumer JSON: {error}")))?;
    deserializer
        .end()
        .map_err(|_| super::invalid("trailing data after consumer JSON"))?;
    Ok(value)
}

struct Strict;

impl<'de> DeserializeSeed<'de> for Strict {
    type Value = Value;
    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Strict {
    type Value = Value;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON without duplicate object keys")
    }
    fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
        Ok(Value::from(value))
    }
    fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
        Ok(Value::from(value))
    }
    fn visit_f64<E>(self, value: f64) -> Result<Value, E> {
        Ok(serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number))
    }
    fn visit_str<E>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }
    fn visit_string<E>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }
    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(Strict)? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut fields = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if fields.contains_key(&key) {
                return Err(de::Error::custom("duplicate consumer proof field"));
            }
            let value = map.next_value_seed(Strict)?;
            fields.insert(key, value);
        }
        Ok(Value::Object(fields))
    }
}

/// Sorted keys, no whitespace, non-ASCII text kept as UTF-8.
pub fn canonical(value: &Value) -> Vec<u8> {
    let mut output = Vec::new();
    encode(value, false, &mut output);
    output
}

/// Sorted keys, no whitespace, non-ASCII text escaped as `\uXXXX`.
pub fn canonical_ascii(value: &Value) -> Vec<u8> {
    let mut output = Vec::new();
    encode(value, true, &mut output);
    output
}

fn encode(value: &Value, ascii: bool, output: &mut Vec<u8>) {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(true) => output.extend_from_slice(b"true"),
        Value::Bool(false) => output.extend_from_slice(b"false"),
        Value::Number(number) => output.extend_from_slice(number.to_string().as_bytes()),
        Value::String(text) => string(text, ascii, output),
        Value::Array(items) => {
            output.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                encode(item, ascii, output);
            }
            output.push(b']');
        }
        Value::Object(fields) => {
            // The workspace may enable serde_json's preserve_order, so sort here.
            let mut keys = fields.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            output.push(b'{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                string(key, ascii, output);
                output.push(b':');
                if let Some(item) = fields.get(key.as_str()) {
                    encode(item, ascii, output);
                }
            }
            output.push(b'}');
        }
    }
}

fn string(text: &str, ascii: bool, output: &mut Vec<u8>) {
    output.push(b'"');
    for character in text.chars() {
        match character {
            '"' => output.extend_from_slice(b"\\\""),
            '\\' => output.extend_from_slice(b"\\\\"),
            '\n' => output.extend_from_slice(b"\\n"),
            '\r' => output.extend_from_slice(b"\\r"),
            '\t' => output.extend_from_slice(b"\\t"),
            '\u{8}' => output.extend_from_slice(b"\\b"),
            '\u{c}' => output.extend_from_slice(b"\\f"),
            c if (c as u32) < 0x20 || (ascii && !(' '..='~').contains(&c)) => {
                let mut units = [0_u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    output.extend_from_slice(format!("\\u{unit:04x}").as_bytes());
                }
            }
            c => {
                let mut buffer = [0_u8; 4];
                output.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
            }
        }
    }
    output.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicates_refuse_at_any_depth_and_encoding_is_sorted_compact() {
        assert!(strict(br#"{"schema":1,"schema":1}"#).is_err());
        assert!(strict(br#"{"a":[{"b":1,"b":2}]}"#).is_err());
        assert!(strict(b"{} {}").is_err());
        let value = strict("{\"b\":\"\u{e9}\\n\",\"a\":[1,true,null]}".as_bytes()).unwrap();
        assert_eq!(
            canonical(&value),
            "{\"a\":[1,true,null],\"b\":\"\u{e9}\\n\"}".as_bytes()
        );
        assert_eq!(
            canonical_ascii(&value),
            br#"{"a":[1,true,null],"b":"\u00e9\n"}"#
        );
        assert_eq!(
            canonical_ascii(&Value::String("\u{1f600}\u{7f}\u{1}".into())),
            br#""\ud83d\ude00\u007f\u0001""#
        );
    }
}
