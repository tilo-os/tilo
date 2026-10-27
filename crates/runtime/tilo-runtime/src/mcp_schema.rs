//! Parameter schemas as `tilo_resolve` answers them: the same constraints,
//! said once. A rich action's schema is kilobytes an agent pays for on every
//! later turn that still carries it, and much of it is repetition: the same
//! point or shadow shape under several properties, `null` beside every
//! optional property, `"type"` beside a `const` that already has one.
//!
//! Nothing a caller can send changes meaning. Serde reads an absent optional
//! field and `null` alike, so an optional property need not also list
//! `null`; a bound no value reaches (`4294967295`) restates the integer type;
//! a repeated subschema becomes one `$defs` entry its places `$ref`.
use serde_json::{Map, Value};

/// Bounds at or beyond this magnitude only restate an integer type's range.
const TYPE_LIMIT: f64 = 2_147_483_647.0;

/// `schema`, the schema of a parameter that may be left out (`optional`),
/// said once.
pub(super) fn compact(schema: &mut Value, optional: bool) {
    if optional {
        drop_null(schema);
    }
    tidy(schema);
    share(schema);
}

/// `schema` without accepting `null`: what an optional property's absence
/// already says.
fn drop_null(schema: &mut Value) {
    let Some(fields) = schema.as_object_mut() else {
        return;
    };
    if let Some(Value::Array(types)) = fields.get_mut("type") {
        if types.len() > 1 {
            types.retain(|name| name != "null");
        }
        if let [one] = types.as_slice() {
            let one = one.clone();
            fields.insert("type".into(), one);
        }
        return;
    }
    // `nullable` wraps what a type array cannot say: `oneOf: [null, X]`.
    let inner = match fields.get("oneOf") {
        Some(Value::Array(alternatives)) if alternatives.len() == 2 => {
            let null = Value::Object(Map::from_iter([("type".into(), "null".into())]));
            match alternatives.as_slice() {
                [first, other] if *first == null => Some(other.clone()),
                [other, second] if *second == null => Some(other.clone()),
                _ => None,
            }
        }
        _ => None,
    };
    let Some(Value::Object(mut inner)) = inner else {
        return;
    };
    if fields.len() > 2
        || fields
            .keys()
            .any(|key| key != "oneOf" && key != "description")
    {
        return;
    }
    if let Some(description) = fields.remove("description") {
        inner.entry("description").or_insert(description);
    }
    *fields = inner;
}

/// Each subschema of `schema` with what it says twice left out: a `const`'s
/// type, a type's own limits, `null` on optional properties.
fn tidy(schema: &mut Value) {
    let Some(fields) = schema.as_object_mut() else {
        return;
    };
    if fields.get("const").is_some_and(Value::is_string)
        && fields.get("type") == Some(&"string".into())
    {
        fields.remove("type");
    }
    for key in ["minimum", "maximum"] {
        if fields
            .get(key)
            .and_then(Value::as_f64)
            .is_some_and(|bound| bound.abs() >= TYPE_LIMIT)
        {
            fields.remove(key);
        }
    }
    let required: Vec<String> = fields
        .get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if let Some(Value::Object(properties)) = fields.get_mut("properties") {
        for (name, property) in properties.iter_mut() {
            if !required.contains(name) {
                drop_null(property);
            }
            tidy(property);
        }
    }
    each_subschema(fields, tidy);
}

/// Apply `visit` to the subschemas under `fields` other than its
/// `properties`: `items`, alternatives, `additionalProperties`, `$defs`.
fn each_subschema(fields: &mut Map<String, Value>, mut visit: impl FnMut(&mut Value)) {
    for (key, value) in fields.iter_mut() {
        match (key.as_str(), value) {
            ("items" | "additionalProperties" | "not", value) if value.is_object() => visit(value),
            ("items" | "oneOf" | "anyOf" | "allOf", Value::Array(alternatives)) => {
                alternatives.iter_mut().for_each(&mut visit);
            }
            ("$defs", Value::Object(defs)) => defs.values_mut().for_each(&mut visit),
            _ => {}
        }
    }
}

/// Move the subschema `schema` repeats that saves the most into its
/// `$defs`, again while one is repeated and worth a reference: the same
/// factoring every publisher applies, in the validator's own vocabulary
/// ([`tilo_actions::params::share_repeated_subschemas`]).
fn share(schema: &mut Value) {
    tilo_actions::params::share_repeated_subschemas(schema);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shadow(description: &str) -> Value {
        json!({
            "type": ["object", "null"],
            "properties": {
                "offset": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2, "description": "`[dx, dy]` in units."},
                "blur": {"type": "number", "description": "Softness in units; 0 is hard."},
                "colour": {"type": ["string", "null"]}
            },
            "additionalProperties": false,
            "description": description
        })
    }

    #[test]
    fn a_schema_says_each_shape_once_and_keeps_every_constraint() {
        let mut schema = json!({
            "type": "object",
            "required": ["kind", "count"],
            "properties": {
                "kind": {"type": "string", "const": "stroke"},
                "count": {"type": ["integer", "null"], "minimum": 0, "maximum": 4_294_967_295_u64},
                "quality": {"type": ["integer", "null"], "minimum": 1, "maximum": 100},
                "drop_shadow": shadow("Behind the mark."),
                "inner_shadow": shadow("Inside the mark."),
                "tint": {"oneOf": [{"type": "null"}, {"type": "string", "enum": ["warm", "cool"]}], "description": "A tint."}
            },
            "additionalProperties": false
        });
        compact(&mut schema, true);
        let properties = &schema["properties"];
        assert_eq!(properties["kind"], json!({"const": "stroke"}));
        assert_eq!(
            properties["count"],
            json!({"type": ["integer", "null"], "minimum": 0}),
            "a required property keeps null; a type's own limit goes"
        );
        assert_eq!(
            properties["quality"],
            json!({"type": "integer", "minimum": 1, "maximum": 100}),
            "an optional property's absence says null; its range stays"
        );
        assert_eq!(
            properties["tint"],
            json!({"type": "string", "enum": ["warm", "cool"], "description": "A tint."})
        );
        assert_eq!(
            properties["drop_shadow"],
            json!({"$ref": "#/$defs/drop_shadow", "description": "Behind the mark."})
        );
        assert_eq!(
            properties["inner_shadow"],
            json!({"$ref": "#/$defs/drop_shadow", "description": "Inside the mark."})
        );
        let shared = &schema["$defs"]["drop_shadow"];
        assert_eq!(shared["type"], "object");
        assert_eq!(shared["properties"]["offset"]["maxItems"], 2);
        assert_eq!(shared["properties"]["colour"]["type"], "string");
        assert!(shared.get("description").is_none(), "{schema}");
    }

    #[test]
    fn a_small_or_single_shape_stays_inline() {
        let point = json!({"type": "array", "items": {"type": "number"}});
        let mut schema = json!({
            "type": "object",
            "properties": {"from": point, "to": point, "only": shadow("Once.")},
            "additionalProperties": false
        });
        compact(&mut schema, false);
        assert!(schema.get("$defs").is_none(), "{schema}");
        assert_eq!(schema["properties"]["from"], point);
    }
}
