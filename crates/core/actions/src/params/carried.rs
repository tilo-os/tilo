//! What an action that carries a resource's other actions says it carries, cut to what one
//! caller may make.
//!
//! A typed edit and a group of edits publish, in the schema of their parameter, the ids of
//! the actions they carry: an `action` property that is one id (`const`) or one of several
//! (`enum`). Whoever drops an action for a caller, or lists it to them as not allowed,
//! takes that id out of what a carrier beside it names ([`without_named`]), so the caller
//! reads there only what their own call is not refused for. The rule that refuses is
//! theirs; this only writes its answer where the carrier names the action.

use serde_json::Value;

/// The ids the schema of an `action` property names: its constant, and the members of its
/// enum.
fn named(action: &Value) -> impl Iterator<Item = &str> {
    let listed = action.get("enum").and_then(Value::as_array);
    action
        .get("const")
        .into_iter()
        .chain(listed.into_iter().flatten())
        .filter_map(Value::as_str)
}

/// The schema of the `action` property of the object `schema` describes, where it has one.
fn action_of(schema: &Value) -> Option<&Value> {
    schema.get("properties")?.get("action")
}

/// Whether `schema` names one of `refused` anywhere in it.
fn names_any(schema: &Value, refused: &[&str]) -> bool {
    match schema {
        Value::Object(object) => {
            action_of(schema).is_some_and(|action| named(action).any(|id| refused.contains(&id)))
                || object.values().any(|value| names_any(value, refused))
        }
        Value::Array(values) => values.iter().any(|value| names_any(value, refused)),
        _ => false,
    }
}

/// Whether `schema` describes an object that names actions and has none left to name.
fn names_none(schema: &Value) -> bool {
    action_of(schema)
        .and_then(|action| action.get("enum"))
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
}

/// Take `refused` out of every `action` property in `schema`, and every alternative that
/// then names no action out of the alternatives it stood among.
fn take_named(schema: &mut Value, refused: &[&str]) {
    match schema {
        Value::Object(object) => {
            if let Some(action) = object
                .get_mut("properties")
                .and_then(|properties| properties.get_mut("action"))
                .and_then(Value::as_object_mut)
            {
                let is_refused = |id: &Value| id.as_str().is_some_and(|id| refused.contains(&id));
                if action.get("const").is_some_and(is_refused) {
                    action.remove("const");
                    action.insert("enum".to_owned(), Value::Array(Vec::new()));
                } else if let Some(Value::Array(ids)) = action.get_mut("enum") {
                    ids.retain(|id| !is_refused(id));
                }
            }
            for (key, value) in object.iter_mut() {
                take_named(value, refused);
                if let ("oneOf" | "anyOf", Value::Array(alternatives)) = (key.as_str(), value) {
                    alternatives.retain(|alternative| !names_none(alternative));
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                take_named(value, refused);
            }
        }
        _ => {}
    }
}

/// `schema`, the schema of a carrier's parameter, without the actions `refused` names: each
/// id out of the `action` enum naming it, and an alternative that names only such actions
/// out of the alternatives it stood among. Everything else is as it was. `None` when the
/// schema names none of them, so its caller keeps the one it shares.
pub(crate) fn without_named(schema: &Value, refused: &[&str]) -> Option<Value> {
    if !names_any(schema, refused) {
        return None;
    }
    let mut cut = schema.clone();
    take_named(&mut cut, refused);
    Some(cut)
}

#[cfg(test)]
mod tests {
    use super::without_named;
    use serde_json::json;

    /// A typed edit's parameter as a resource publishes it: a typed value, and its named
    /// actions folded into one alternative.
    fn typed_edit() -> serde_json::Value {
        json!({"type": "object", "required": ["op"], "properties": {"op": {"oneOf": [
            {
                "type": "object",
                "required": ["action"],
                "properties": {"action": {"const": "retitle"}, "title": {"type": "string"}},
            },
            {
                "type": "object",
                "required": ["action", "args"],
                "properties": {
                    "action": {"type": "string", "enum": ["post", "pin", "unpin"]},
                    "args": {"type": "object"},
                },
                "additionalProperties": false,
            },
        ]}}})
    }

    #[test]
    fn a_carrier_names_no_action_its_caller_is_refused_and_everything_else_as_before() {
        let schema = typed_edit();
        assert_eq!(without_named(&schema, &[]), None);
        assert_eq!(
            without_named(&schema, &["archive"]),
            None,
            "it names none of them: the schema it shares stays"
        );
        let cut = without_named(&schema, &["pin", "unpin"]).expect("it names both");
        let mut expected = typed_edit();
        expected["properties"]["op"]["oneOf"][1]["properties"]["action"]["enum"] = json!(["post"]);
        assert_eq!(
            cut, expected,
            "the two ids are gone and nothing else changed"
        );
        // An alternative that names only refused actions goes with them.
        let cut = without_named(&schema, &["retitle", "pin"]).expect("it names both");
        assert_eq!(
            cut["properties"]["op"]["oneOf"],
            json!([{
                "type": "object",
                "required": ["action", "args"],
                "properties": {
                    "action": {"type": "string", "enum": ["post", "unpin"]},
                    "args": {"type": "object"},
                },
                "additionalProperties": false,
            }])
        );
        // A group names its members the same way, in the items of an array.
        let group = json!({"type": "array", "items": {"oneOf": [
            {"type": "object", "properties": {
                "action": {"type": "string", "enum": ["post", "pin"]},
                "args": {"type": "object"},
            }},
            {"type": "object", "properties": {
                "action": {"const": "apply"},
                "args": {"type": "object"},
            }},
        ]}});
        let cut = without_named(&group, &["pin"]).expect("it names it");
        assert_eq!(
            cut["items"]["oneOf"][0]["properties"]["action"]["enum"],
            json!(["post"])
        );
        assert_eq!(cut["items"]["oneOf"][1], group["items"]["oneOf"][1]);
    }
}
