//! The committed form of a surface: the file an app ships beside its
//! manifest, and any other surface kept in a repository for a reader that
//! cannot ask the code (a view of another language compiling against what the
//! session publishes).
//!
//! The file keeps only what deciding a change, checking storage and compiling
//! against it need ([`filed`]): kinds, verb ids, kinds, footprints, parameters
//! and the value each answers with, field paths, layouts and types, and
//! whether a record is the person's own. Labels, descriptions, field titles
//! and owners stay in the code; a field type's own operation vocabulary is
//! kept as its digest. One line per field keeps the file small and its diffs
//! readable ([`render`]).

use std::fmt::Write as _;

use serde_json::Value;

/// The file's format, written at its top: what the file describes. A file
/// without it was written before surfaces published read actions.
pub const FORMAT: u64 = 2;

/// `value` without JSON nulls, which TOML cannot hold and which mean "not
/// there" in a surface.
#[must_use]
pub fn normalized(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key.clone(), normalized(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .filter(|item| !item.is_null())
                .map(normalized)
                .collect(),
        ),
        other => other.clone(),
    }
}

/// What the surface file keeps of each verb: its id, kind, footprints,
/// parameters and the value it answers with, without the words that are the
/// app's own.
fn filed_verbs(verbs: &mut Value) {
    for verb in verbs
        .as_array_mut()
        .into_iter()
        .flatten()
        .filter_map(Value::as_object_mut)
    {
        verb.remove("label");
        verb.remove("description");
        if verb
            .get("params")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            verb.remove("params");
        }
        for schema in verb
            .get_mut("params")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
            .filter_map(|param| param.get_mut("schema"))
        {
            without_prose(schema);
        }
        if let Some(returns) = verb.get_mut("returns") {
            filed_returns(returns);
        }
    }
}

/// What the surface file keeps of a resource the code reports: see the
/// module docs. Applying it twice changes nothing, so a committed file
/// compares with the code through it too.
#[must_use]
pub fn filed(resource: &Value) -> Value {
    let mut resource = normalized(resource);
    let Some(map) = resource.as_object_mut() else {
        return resource;
    };
    for list in ["verbs", "fields", "types"] {
        if map
            .get(list)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            map.remove(list);
        }
    }
    if let Some(verbs) = map.get_mut("verbs") {
        filed_verbs(verbs);
    }
    for field in map
        .get_mut("fields")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object_mut)
    {
        field.remove("title");
        field.remove("owner");
        if field.get("stored").and_then(Value::as_str) == Some("value") {
            field.remove("stored");
        }
        if let Some(ops) = field.get_mut("ops")
            && !ops.is_string()
        {
            *ops = Value::String(digest(ops));
        }
    }
    // A named type keeps its fields' and cases' names and types, and how a
    // case reads when that is not its name in words; its prose stays in the
    // code.
    for named in map
        .get_mut("types")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object_mut)
    {
        named.remove("doc");
        for case in named
            .get_mut("cases")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
            .filter_map(Value::as_object_mut)
        {
            let name = case.get("name").and_then(Value::as_str).unwrap_or_default();
            let plain = tilo_actions::field_type::label_of(name);
            if case.get("label").and_then(Value::as_str) == Some(plain.as_str()) {
                case.remove("label");
            }
        }
        let mut fields: Vec<&mut Value> = Vec::new();
        for (key, list) in named.iter_mut() {
            let Some(items) = list.as_array_mut() else {
                continue;
            };
            match key.as_str() {
                "fields" => fields.extend(items.iter_mut()),
                "cases" => fields.extend(
                    items
                        .iter_mut()
                        .filter_map(|case| case.get_mut("fields").and_then(Value::as_array_mut))
                        .flatten(),
                ),
                _ => {}
            }
        }
        for field in fields.into_iter().filter_map(Value::as_object_mut) {
            field.remove("title");
        }
    }
    resource
}

/// What the surface file keeps of the value an action answers with: its
/// schema, without the prose the type's doc comments put in it.
fn filed_returns(returns: &mut Value) {
    let Value::Object(returns) = returns else {
        return;
    };
    if let Some(schema) = returns.get_mut("schema") {
        without_prose(schema);
    }
    // An answer of no declared type says only what its schema says.
    if returns.get("schema").is_some_and(Value::is_null) {
        returns.remove("schema");
    }
}

/// `schema` without the prose its types' doc comments put in it
/// (`description`, `examples`): what a parameter accepts stays.
fn without_prose(schema: &mut Value) {
    match schema {
        Value::Object(node) => {
            node.remove("description");
            node.remove("examples");
            for (key, value) in node.iter_mut() {
                match key.as_str() {
                    // Maps from names to schemas: the names are not keywords.
                    "properties" | "$defs" | "definitions" => {
                        for item in value
                            .as_object_mut()
                            .into_iter()
                            .flat_map(|map| map.values_mut())
                        {
                            without_prose(item);
                        }
                    }
                    "const" | "enum" | "default" => {}
                    _ => without_prose(value),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(without_prose),
        _ => {}
    }
}

/// A short, stable digest of `value`'s canonical JSON.
fn digest(value: &Value) -> String {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    tilo_canonical_json::write_bytes(value, |chunk| {
        hasher.update(chunk);
    });
    let hash = hasher.finalize();
    let mut hex = String::from("sha256:");
    for byte in hash.get(..8).unwrap_or_default() {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// `key` as a TOML key: bare when it can be, else quoted.
fn toml_key(key: &str) -> String {
    let bare = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        key.to_owned()
    } else {
        Value::String(key.to_owned()).to_string()
    }
}

/// `value` as one line of TOML: strings in JSON's escapes (which TOML's
/// basic strings share), objects as inline tables.
fn inline(value: &Value, out: &mut String) -> Result<(), String> {
    match value {
        Value::Null => return Err("TOML holds no null".to_owned()),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            out.push_str(&value.to_string());
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                inline(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Object(map) => {
            out.push_str("{ ");
            for (index, (key, item)) in leading(map, &["path", "name", "type"]).enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "{} = ", toml_key(key));
                inline(item, out)?;
            }
            out.push_str(" }");
        }
    }
    Ok(())
}

/// `map`'s entries in the file's order, whatever order they were read in:
/// those named in `first` in that order, then the rest by key.
fn leading<'a>(
    map: &'a serde_json::Map<String, Value>,
    first: &[&str],
) -> impl Iterator<Item = (&'a String, &'a Value)> {
    let mut entries: Vec<(&'a String, &'a Value)> = map.iter().collect();
    entries.sort_by_key(|(key, _)| {
        let rank = first
            .iter()
            .position(|lead| *lead == key.as_str())
            .unwrap_or(first.len());
        (rank, key.as_str())
    });
    entries.into_iter()
}

/// The surface file's text for `resources`, with the record kinds of other apps
/// `reads` names: their list, then each resource's scalars, its fields one per
/// line, then its verbs, their parameters one per line.
pub fn render(header: &str, resources: &[Value], reads: &[String]) -> Result<String, String> {
    let mut text = format!("{header}format = {FORMAT}\n\n");
    if !reads.is_empty() {
        let list: Vec<Value> = reads.iter().cloned().map(Value::String).collect();
        text.push_str("reads = ");
        inline(&Value::Array(list), &mut text)?;
        text.push_str("\n\n");
    }
    for resource in resources.iter().map(filed) {
        let Value::Object(map) = &resource else {
            return Err("a resource of the surface is not a table".to_owned());
        };
        text.push_str("[[resources]]\n");
        for (key, value) in leading(map, &["kind", "label", "version", "person"])
            .filter(|(key, _)| !matches!(key.as_str(), "verbs" | "fields" | "types"))
        {
            let _ = write!(text, "{} = ", toml_key(key));
            inline(value, &mut text)?;
            text.push('\n');
        }
        if let Some(fields) = map.get("fields").and_then(Value::as_array) {
            text.push_str("fields = [\n");
            for field in fields {
                text.push_str("  ");
                inline(field, &mut text)?;
                text.push_str(",\n");
            }
            text.push_str("]\n");
        }
        for verb in map
            .get("verbs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Value::Object(verb) = verb else {
                return Err("a verb of the surface is not a table".to_owned());
            };
            text.push_str("\n[[resources.verbs]]\n");
            let params = verb.iter().filter(|(key, _)| key.as_str() == "params");
            let rest = leading(verb, &["id"]).filter(|(key, _)| key.as_str() != "params");
            for (key, value) in rest.chain(params) {
                let _ = write!(text, "{} = ", toml_key(key));
                match value.as_array() {
                    Some(items) if key == "params" && !items.is_empty() => {
                        text.push_str("[\n");
                        for item in items {
                            text.push_str("  ");
                            inline(item, &mut text)?;
                            text.push_str(",\n");
                        }
                        text.push(']');
                    }
                    _ => inline(value, &mut text)?,
                }
                text.push('\n');
            }
        }
        for named in map
            .get("types")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Value::Object(named) = named else {
                return Err("a type of the surface is not a table".to_owned());
            };
            text.push_str("\n[[resources.types]]\n");
            let lists = ["fields", "cases"];
            let rest = leading(named, &["name"]).filter(|(key, _)| !lists.contains(&key.as_str()));
            let lists = lists.iter().filter_map(|key| named.get_key_value(*key));
            for (key, value) in rest.chain(lists) {
                let _ = write!(text, "{} = ", toml_key(key));
                match value.as_array() {
                    Some(items) if key != "name" && !items.is_empty() => {
                        text.push_str("[\n");
                        for item in items {
                            text.push_str("  ");
                            inline(item, &mut text)?;
                            text.push_str(",\n");
                        }
                        text.push(']');
                    }
                    _ => inline(value, &mut text)?,
                }
                text.push('\n');
            }
        }
        text.push('\n');
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANONICAL_JSON_CORPUS: &[(&str, &str, &[u8])] = &[
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

    #[test]
    fn digest_pins_canonical_bytes_for_shared_corpus() {
        use sha2::Digest as _;

        for &(name, input, expected) in CANONICAL_JSON_CORPUS {
            let value: Value = serde_json::from_str(input).unwrap();
            let hash = sha2::Sha256::digest(expected);
            let expected_digest = format!("sha256:{}", hex::encode(&hash[..8]));
            assert_eq!(digest(&value), expected_digest, "row {name}");
        }
        let nested: Value = serde_json::from_str(CANONICAL_JSON_CORPUS[0].1).unwrap();
        assert_eq!(digest(&nested), "sha256:297e609033c30585");
    }
}
