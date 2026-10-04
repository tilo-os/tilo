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

/// Subschemas shorter than this stay inline: a `$ref` would cost about as
/// much.
const SHARED_BYTES: usize = 48;
/// The most definitions one schema gets.
const MAX_DEFS: usize = 32;
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

/// Every subschema of `schema` with the property name it first stands
/// under, if any.
fn subschemas<'a>(
    schema: &'a Value,
    name: Option<&'a str>,
    out: &mut Vec<(&'a Value, Option<&'a str>)>,
) {
    let Some(fields) = schema.as_object() else {
        return;
    };
    out.push((schema, name));
    for (key, value) in fields {
        match (key.as_str(), value) {
            ("properties" | "$defs", Value::Object(children)) => {
                for (child, child_schema) in children {
                    subschemas(child_schema, Some(child), out);
                }
            }
            ("items" | "additionalProperties" | "not", value) if value.is_object() => {
                subschemas(value, name, out);
            }
            ("items" | "oneOf" | "anyOf" | "allOf", Value::Array(alternatives)) => {
                for alternative in alternatives {
                    subschemas(alternative, None, out);
                }
            }
            _ => {}
        }
    }
}

/// What makes two subschemas the same shape: everything but their own
/// description, which stays beside the `$ref` at each place.
fn shape(schema: &Value) -> Option<Value> {
    let mut fields = schema.as_object()?.clone();
    fields.remove("description");
    (!fields.is_empty() && !fields.contains_key("$ref")).then_some(Value::Object(fields))
}

/// Move the subschema `schema` repeats that saves the most into its
/// `$defs`, again while one is repeated and worth a reference.
fn share(schema: &mut Value) {
    let mut defs = Map::new();
    while defs.len() < MAX_DEFS {
        let Some((shared, name)) = most_repeated(schema, &defs) else {
            break;
        };
        let reference = format!("#/$defs/{name}");
        replace_inside(schema, &shared, &reference);
        for def in defs.values_mut() {
            replace_inside(def, &shared, &reference);
        }
        defs.insert(name, shared);
    }
    if !defs.is_empty()
        && let Some(fields) = schema.as_object_mut()
    {
        fields.insert("$defs".into(), Value::Object(defs));
    }
}

/// The repeated shape that saves the most bytes once shared, with the
/// name its definition gets: the property it first stands under.
fn most_repeated(schema: &Value, defs: &Map<String, Value>) -> Option<(Value, String)> {
    let mut found = Vec::new();
    // Neither the parameter itself nor a definition is a place to share.
    for root in std::iter::once(schema).chain(defs.values()) {
        let at = found.len();
        subschemas(root, None, &mut found);
        found.remove(at);
    }
    let mut counted: Vec<(String, Value, usize, Option<&str>)> = Vec::new();
    for (subschema, name) in found {
        let Some(shape) = shape(subschema) else {
            continue;
        };
        let text = shape.to_string();
        if text.len() < SHARED_BYTES {
            continue;
        }
        match counted.iter_mut().find(|(seen, ..)| *seen == text) {
            Some((_, _, count, first)) => {
                *count = count.saturating_add(1);
                if first.is_none() {
                    *first = name;
                }
            }
            None => counted.push((text, shape, 1, name)),
        }
    }
    counted
        .into_iter()
        .filter(|(_, _, count, _)| *count > 1)
        .map(|(text, shape, count, name)| {
            let name = free_name(name.unwrap_or("shared"), defs);
            // `{"$ref":"#/$defs/<name>"}` at each place, and the
            // definition under its name once.
            let reference = name.len().saturating_add(19);
            let saved = text
                .len()
                .saturating_mul(count)
                .saturating_sub(text.len().saturating_add(name.len()).saturating_add(3))
                .saturating_sub(reference.saturating_mul(count));
            (saved, shape, name)
        })
        .filter(|(saved, ..)| *saved > 0)
        .max_by_key(|(saved, ..)| *saved)
        .map(|(_, shape, name)| (shape, name))
}

/// `name`, or `name2`, `name3`, … when a definition has it already.
fn free_name(name: &str, defs: &Map<String, Value>) -> String {
    if !defs.contains_key(name) {
        return name.to_owned();
    }
    // At most `MAX_DEFS` names are taken, so one of these is free.
    (2..=MAX_DEFS.saturating_add(2))
        .map(|index| format!("{name}{index}"))
        .find(|candidate| !defs.contains_key(candidate))
        .unwrap_or_else(|| name.to_owned())
}

/// Each place under `schema` (not `schema` itself) that has `shape_of`
/// names `reference` instead, keeping its own description.
fn replace_inside(schema: &mut Value, shape_of: &Value, reference: &str) {
    let Some(fields) = schema.as_object_mut() else {
        return;
    };
    let mut place = |child: &mut Value| {
        if shape(child).as_ref() == Some(shape_of) {
            let mut named = Map::new();
            named.insert("$ref".into(), reference.into());
            if let Some(description) = child.get("description") {
                named.insert("description".into(), description.clone());
            }
            *child = Value::Object(named);
        } else {
            replace_inside(child, shape_of, reference);
        }
    };
    if let Some(Value::Object(properties)) = fields.get_mut("properties") {
        properties.values_mut().for_each(&mut place);
    }
    each_subschema(fields, place);
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
