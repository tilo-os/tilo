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

    const CANONICAL_CORPUS: &[(&str, &str, &[u8])] = &[
        (
            "nested_unsorted",
            r#"{"z":{"b":2,"a":1},"a":{"d":{"y":true,"x":false},"c":[3,2,1]},"m":0}"#,
            br#"{"a":{"c":[3,2,1],"d":{"x":false,"y":true}},"m":0,"z":{"a":1,"b":2}}"#,
        ),
        (
            "utf8_vs_utf16_key_order",
            r#"{"\uD83D\uDE00":4,"\uE000":3,"é":2,"z":1}"#,
            b"{\"z\":1,\"\xc3\xa9\":2,\"\xee\x80\x80\":3,\"\xf0\x9f\x98\x80\":4}",
        ),
        (
            "non_bmp_vs_bmp_high_key_order",
            r#"{"\uD83D\uDE80":1,"\uFFFF":2,"\uE000":3}"#,
            b"{\"\xee\x80\x80\":3,\"\xef\xbf\xbf\":2,\"\xf0\x9f\x9a\x80\":1}",
        ),
        (
            "ascii_empty_prefix_key_order",
            r#"{"b":1,"":2,"a":3,"ab":4,"B":5,"A":6}"#,
            br#"{"":2,"A":6,"B":5,"a":3,"ab":4,"b":1}"#,
        ),
        (
            "nested_ten_keys",
            r#"{"k9":9,"k8":8,"k7":{"j9":9,"j8":8,"j7":7,"j6":6,"j5":5,"j4":4,"j3":3,"j2":2,"j1":1,"j0":0},"k6":6,"k5":5,"k4":4,"k3":3,"k2":2,"k1":1,"k0":0}"#,
            br#"{"k0":0,"k1":1,"k2":2,"k3":3,"k4":4,"k5":5,"k6":6,"k7":{"j0":0,"j1":1,"j2":2,"j3":3,"j4":4,"j5":5,"j6":6,"j7":7,"j8":8,"j9":9},"k8":8,"k9":9}"#,
        ),
        ("empty_object", "{}", b"{}"),
        ("empty_array", "[]", b"[]"),
        ("empty_string", r#""""#, br#""""#),
        (
            "line_seps_fffd_and_solidus",
            r#""\u2028\u2029\ufffd/""#,
            b"\"\xe2\x80\xa8\xe2\x80\xa9\xef\xbf\xbd/\"",
        ),
        (
            "all_32_control_bytes_value",
            r#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f""#,
            br#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f""#,
        ),
        (
            "all_32_control_bytes_key",
            r#"{"a":2,"\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f":1}"#,
            br#"{"\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f":1,"a":2}"#,
        ),
        (
            "escaped_and_unicode_string",
            r#""q\"b\\c\u0000\u0001\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u001f\u007f café 日本語 🚀""#,
            b"\"q\\\"b\\\\c\\u0000\\u0001\\u0007\\b\\t\\n\\u000b\\f\\r\\u000e\\u001f\x7f caf\xc3\xa9 \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e \xf0\x9f\x9a\x80\"",
        ),
        (
            "escaped_keys_object",
            r#"{"q\"b\\c\u0000\b\t\n\f\r\u001f\u007f":2,"a":1}"#,
            b"{\"a\":1,\"q\\\"b\\\\c\\u0000\\b\\t\\n\\f\\r\\u001f\x7f\":2}",
        ),
        ("i64_min", "-9223372036854775808", b"-9223372036854775808"),
        ("i64_max", "9223372036854775807", b"9223372036854775807"),
        (
            "u64_above_i64_max",
            "9223372036854775808",
            b"9223372036854775808",
        ),
        ("u64_max", "18446744073709551615", b"18446744073709551615"),
        ("zero", "0", b"0"),
        ("negative_zero", "-0.0", b"-0.0"),
        ("float_whole", "1.0", b"1.0"),
        ("float_tenth", "0.1", b"0.1"),
        ("float_1e20", "1e20", b"1e+20"),
        ("float_1e21", "1e21", b"1e+21"),
        ("float_1e_neg5", "1e-5", b"0.00001"),
        ("float_1e_neg7", "1e-7", b"1e-7"),
        ("float_min_subnormal", "5e-324", b"5e-324"),
        (
            "float_max_finite",
            "1.7976931348623157e308",
            b"1.7976931348623157e+308",
        ),
        ("float_1e15", "1e15", b"1000000000000000.0"),
        ("float_0_3_ulp", "0.30000000000000004", b"0.30000000000000004"),
        ("bool_true", "true", b"true"),
        ("bool_false", "false", b"false"),
        ("null", "null", b"null"),
        (
            "mixed_array",
            r#"[null,true,false,0,-1,1.0,0.1,-0.0,"a\"b",[],{},{"b":2,"a":1}]"#,
            br#"[null,true,false,0,-1,1.0,0.1,-0.0,"a\"b",[],{},{"a":1,"b":2}]"#,
        ),
    ];

    const CANONICAL_ASCII_CORPUS: &[(&str, &str, &[u8])] = &[
        (
            "nested_unsorted_ascii_keys",
            r#"{"z":{"b":2,"a":1},"a":[true,false,null,0,-1,1.5,"plain"]}"#,
            br#"{"a":[true,false,null,0,-1,1.5,"plain"],"z":{"a":1,"b":2}}"#,
        ),
        (
            "non_ascii_bmp_key_and_value",
            r#"{"é":"日本語 €\u2028\u2029","z":"café"}"#,
            br#"{"z":"caf\u00e9","\u00e9":"\u65e5\u672c\u8a9e \u20ac\u2028\u2029"}"#,
        ),
        (
            "non_bmp_surrogate_pair_key_and_value",
            r#"{"\uD83D\uDE80":"😀🚀","\uFFFF":2,"\uE000":3}"#,
            br#"{"\ue000":3,"\uffff":2,"\ud83d\ude80":"\ud83d\ude00\ud83d\ude80"}"#,
        ),
        (
            "del_and_ascii_bounds",
            r#"{"\u007f":" ~\"\\/\u007f\u0080","a":1}"#,
            br#"{"a":1,"\u007f":" ~\"\\/\u007f\u0080"}"#,
        ),
        (
            "all_32_control_bytes_value",
            r#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f""#,
            br#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f""#,
        ),
        (
            "control_bytes_and_quotes_in_key",
            r#"{"q\"b\\c\u0000\b\t\n\f\r\u001f\u007f":2,"":0,"\u0001é":1}"#,
            br#"{"":0,"\u0001\u00e9":1,"q\"b\\c\u0000\b\t\n\f\r\u001f\u007f":2}"#,
        ),
        (
            "empty_containers_and_numbers",
            r#"{"b":[],"a":{},"c":[0,-0.0,1e21,5e-324,0.30000000000000004]}"#,
            br#"{"a":{},"b":[],"c":[0,-0.0,1e+21,5e-324,0.30000000000000004]}"#,
        ),
    ];

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

    #[test]
    fn canonical_matches_pinned_corpus_bytes() {
        for &(name, input, expected) in CANONICAL_CORPUS {
            let parsed: Value = serde_json::from_str(input).unwrap();
            assert_eq!(canonical(&parsed), expected, "canonical row {name}");
        }
    }

    #[test]
    fn canonical_ascii_matches_pinned_corpus_bytes() {
        for &(name, input, expected) in CANONICAL_ASCII_CORPUS {
            let parsed: Value = serde_json::from_str(input).unwrap();
            assert_eq!(
                canonical_ascii(&parsed),
                expected,
                "canonical_ascii row {name}"
            );
        }
    }
}
