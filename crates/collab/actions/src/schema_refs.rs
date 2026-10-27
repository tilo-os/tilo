//! Local JSON Schema references: the `$defs` a schema carries and the
//! `$ref`s that name them.
//!
//! A published surface states the same shape under several properties — a
//! point, a rectangle, the operations a field edit takes — and an agent pays
//! for every byte of it on every turn that still carries it. A schema may
//! state such a shape once under `$defs` and name it wherever it applies:
//! the constraints a caller is held to are the ones it read, at the size of
//! one copy.
//!
//! The vocabulary is local on purpose. A `$ref` names a definition of the
//! same schema and nothing else, so publishing a surface never makes a
//! reader fetch anything; a reference to a URL, a file or a bare path is
//! refused rather than ignored, as is one that names nothing, follows itself
//! or grows past what a schema may state. Every other keyword keeps the
//! policy [`validate_schema_definition`](crate::params) already had: a
//! keyword Tilo does not know is refused instead of being read as no
//! constraint at all.

use serde_json::{Map, Value};

/// The definitions a schema carries, by name.
pub(crate) const DEFS: &str = "$defs";
/// The name a schema gives a definition of its own.
pub(crate) const REF: &str = "$ref";

/// Subschemas shorter than this are never worth a `$ref`: the reference
/// alone is `21 + name` bytes, so nothing this small pays for itself however
/// often it is stated. What does pay is decided below, per shape.
const SHARED_BYTES: usize = 32;
/// The most definitions one schema gets from
/// [`share_repeated_subschemas`].
const MAX_DEFS: usize = 32;

/// The schema `reference` names inside `root`: `#/$defs/<name>`, a JSON
/// Pointer of the document's own, with `~0` for a `~` and `~1` for a `/` in
/// a name.
///
/// Anything that could name a resource outside this schema is refused: a
/// URL, a file, a path, or a fragment that is not a pointer. So is one that
/// names nothing.
pub(crate) fn local_reference<'a>(root: &'a Value, reference: &str) -> Result<&'a Value, String> {
    let Some(pointer) = reference.strip_prefix('#') else {
        return Err(format!(
            "schema uses `$ref` `{reference}`, which is not local; only `#/$defs/…` in the same schema is resolved"
        ));
    };
    if !(pointer.is_empty() || pointer.starts_with('/')) {
        return Err(format!(
            "schema uses `$ref` `{reference}`, which is not a JSON Pointer"
        ));
    }
    root.pointer(pointer)
        .ok_or_else(|| format!("schema uses `$ref` `{reference}`, which names nothing in it"))
}

/// `schema`, one of `document`'s, as a schema of its own: the definitions the
/// document states once come with it, so a local `$ref` inside it still names
/// what the document carries. A reader handed a single parameter's schema
/// resolves it with this and then validates, rather than keeping a resolver
/// of its own.
pub fn scoped(document: &Value, schema: &Value) -> Value {
    let Some(definitions) = document.get(DEFS) else {
        return schema.clone();
    };
    let Value::Object(fields) = schema else {
        return schema.clone();
    };
    let mut scoped = fields.clone();
    scoped
        .entry(DEFS.to_owned())
        .or_insert_with(|| definitions.clone());
    Value::Object(scoped)
}

/// `name` as a JSON Pointer segment.
fn pointer_segment(name: &str) -> String {
    name.replace('~', "~0").replace('/', "~1")
}

/// State each shape `schema` repeats once, under its own `$defs`, naming it
/// wherever it applies: the representation an agent reads twice as cheaply,
/// with every constraint, description and example as it was.
///
/// [`share_repeated_subschemas_among`] is the same for a document of several
/// schemas; this is one schema's.
pub fn share_repeated_subschemas(schema: &mut Value) {
    let Value::Object(definitions) = share_repeated_subschemas_among(std::slice::from_mut(schema))
    else {
        return;
    };
    if !definitions.is_empty()
        && let Some(fields) = schema.as_object_mut()
    {
        fields.insert(DEFS.into(), Value::Object(definitions));
    }
}

/// State each shape `schemas` repeat once, under one `$defs` the document
/// carries at its root, and name it wherever it applies. Answers the
/// definitions to put there; a document that repeats nothing gets an empty
/// object and no reference.
///
/// Two subschemas are the same shape when they are the same JSON but for
/// their own `description`, which stays beside the `$ref` at each place, so
/// a shape shared by two properties still tells a reader what each of them
/// is. A shape stands in the `$defs` once and is named from each place only
/// when that is smaller than stating it again.
pub fn share_repeated_subschemas_among(schemas: &mut [Value]) -> Value {
    let mut definitions = Map::new();
    while definitions.len() < MAX_DEFS {
        let Some((shape, name)) = most_repeated(schemas, &definitions) else {
            break;
        };
        let reference = format!("#/{DEFS}/{}", pointer_segment(&name));
        for schema in schemas.iter_mut() {
            replace_inside(schema, &shape, &reference);
        }
        for definition in definitions.values_mut() {
            replace_inside(definition, &shape, &reference);
        }
        definitions.insert(name, shape);
    }
    Value::Object(definitions)
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
            ("properties" | DEFS, Value::Object(children)) => {
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
    (!fields.is_empty() && !fields.contains_key(REF)).then_some(Value::Object(fields))
}

/// `shape` said the same way whatever order its keys are written in: the
/// property a schema lists first is its author's habit, not part of what it
/// states, and two actions state the same rectangle in different habits.
fn canonical(shape: &Value) -> String {
    let mut text = String::new();
    write_canonical(shape, &mut text);
    text
}

fn write_canonical(value: &Value, text: &mut String) {
    match value {
        Value::Object(fields) => {
            let mut keys: Vec<&String> = fields.keys().collect();
            keys.sort_unstable();
            text.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                text.push_str(&Value::String(key.clone()).to_string());
                text.push(':');
                write_canonical(&fields[key], text);
            }
            text.push('}');
        }
        Value::Array(items) => {
            text.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                write_canonical(item, text);
            }
            text.push(']');
        }
        other => text.push_str(&other.to_string()),
    }
}

/// The repeated shape that saves the most bytes once shared, with the name
/// its definition gets: the property it first stands under.
fn most_repeated(schemas: &[Value], definitions: &Map<String, Value>) -> Option<(Value, String)> {
    let mut found = Vec::new();
    for root in schemas.iter().chain(definitions.values()) {
        subschemas(root, None, &mut found);
    }
    let mut counted: Vec<(String, Value, usize, Option<&str>)> = Vec::new();
    for (subschema, name) in found {
        let Some(shape) = shape(subschema) else {
            continue;
        };
        let text = canonical(&shape);
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
            let name = free_name(name.unwrap_or("shared"), definitions);
            // `{"$ref":"#/$defs/<name>"}` at each place, and the
            // definition under its name once.
            let reference = pointer_segment(&name).len().saturating_add(19);
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
fn free_name(name: &str, definitions: &Map<String, Value>) -> String {
    if !definitions.contains_key(name) {
        return name.to_owned();
    }
    // At most `MAX_DEFS` names are taken, so one of these is free.
    (2..=MAX_DEFS.saturating_add(2))
        .map(|index| format!("{name}{index}"))
        .find(|candidate| !definitions.contains_key(candidate))
        .unwrap_or_else(|| name.to_owned())
}

/// `schema`, or each place under it, that has `shape_of` names `reference`
/// instead, keeping its own description.
fn replace_inside(schema: &mut Value, shape_of: &Value, reference: &str) {
    let wanted = canonical(shape_of);
    if shape(schema).is_some_and(|shape| canonical(&shape) == wanted) {
        let mut named = Map::new();
        named.insert(REF.into(), reference.into());
        if let Some(description) = schema.get("description") {
            named.insert("description".into(), description.clone());
        }
        *schema = Value::Object(named);
        return;
    }
    let Some(fields) = schema.as_object_mut() else {
        return;
    };
    let mut place = |child: &mut Value| replace_inside(child, shape_of, reference);
    if let Some(Value::Object(properties)) = fields.get_mut("properties") {
        properties.values_mut().for_each(&mut place);
    }
    for (key, value) in fields.iter_mut() {
        match (key.as_str(), value) {
            ("items" | "additionalProperties" | "not", value) if value.is_object() => place(value),
            ("items" | "oneOf" | "anyOf" | "allOf", Value::Array(alternatives)) => {
                alternatives.iter_mut().for_each(&mut place);
            }
            (DEFS, Value::Object(definitions)) => definitions.values_mut().for_each(&mut place),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn region() -> Value {
        json!({"type": "array", "items": {"type": "number"}, "minItems": 4, "maxItems": 4})
    }

    #[test]
    fn a_shape_stated_twice_is_stated_once_and_named_where_it_applies() {
        let mut schema = json!({
            "type": "object",
            "properties": {"crop": region(), "frame": region(), "name": {"type": "string"}},
            "additionalProperties": false
        });
        share_repeated_subschemas(&mut schema);
        assert_eq!(
            schema["properties"]["crop"],
            json!({"$ref": "#/$defs/crop"}),
            "{schema}"
        );
        assert_eq!(
            schema["properties"]["frame"],
            json!({"$ref": "#/$defs/crop"})
        );
        assert_eq!(schema["$defs"]["crop"], region());
        assert_eq!(schema["properties"]["name"], json!({"type": "string"}));
    }

    #[test]
    fn a_shared_shape_keeps_the_description_it_has_at_each_place() {
        let shape =
            json!({"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2});
        let described = |description: &str| {
            let mut value = shape.clone();
            value["description"] = json!(description);
            value
        };
        let mut schema = json!({
            "type": "object",
            "properties": {"from": described("Where it starts."), "to": described("Where it ends.")},
        });
        share_repeated_subschemas(&mut schema);
        assert_eq!(
            schema["properties"]["from"],
            json!({"$ref": "#/$defs/from", "description": "Where it starts."}),
            "{schema}"
        );
        assert_eq!(
            schema["properties"]["to"],
            json!({"$ref": "#/$defs/from", "description": "Where it ends."})
        );
        assert!(
            schema["$defs"]["from"].get("description").is_none(),
            "the definition states the shape, not one place's words: {schema}"
        );
        assert_eq!(schema["$defs"]["from"], shape);
    }

    #[test]
    fn a_name_that_needs_escaping_is_escaped_in_the_pointer() {
        let point = json!({"type": "object", "properties": {"x": {"type": "number"}}});
        let mut schema = json!({
            "type": "object",
            "properties": {"a/b": point, "a~b": point},
        });
        share_repeated_subschemas(&mut schema);
        assert_eq!(
            schema["properties"]["a/b"]["$ref"].as_str(),
            Some("#/$defs/a~1b"),
            "a `/` in a name is `~1` in the pointer that names it: {schema}"
        );
        assert_eq!(
            schema["properties"]["a~b"]["$ref"].as_str(),
            Some("#/$defs/a~1b"),
            "the same shape names the same definition, in either order: {schema}"
        );
        assert_eq!(
            schema["$defs"]["a/b"], point,
            "the definition keeps its own name: {schema}"
        );
        let root = schema.clone();
        assert_eq!(local_reference(&root, "#/$defs/a~1b").unwrap(), &point);
    }

    #[test]
    fn a_shape_stated_once_or_too_small_stays_where_it_is() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "only": region(),
                "width": {"type": "number"},
                "height": {"type": "number"}
            },
        });
        share_repeated_subschemas(&mut schema);
        assert!(schema.get(DEFS).is_none(), "{schema}");
        assert_eq!(schema["properties"]["only"], region());
        assert_eq!(schema["properties"]["width"], json!({"type": "number"}));
    }

    #[test]
    fn a_place_a_document_states_once_is_named_from_every_schema_that_takes_it() {
        let region = |order: bool| {
            if order {
                json!({"type": ["array", "null"], "items": {"type": "number"}, "minItems": 4, "maxItems": 4})
            } else {
                json!({"items": {"type": "number"}, "minItems": 4, "maxItems": 4, "type": ["array", "null"]})
            }
        };
        let mut schemas = vec![
            json!({"type": "object", "properties": {"crop": region(true)}}),
            json!({"type": "object", "properties": {"frame": region(false)}}),
        ];
        let definitions = share_repeated_subschemas_among(&mut schemas);
        let shared = definitions
            .as_object()
            .expect("the document states the shape once");
        assert_eq!(shared.len(), 1, "{definitions}");
        assert_eq!(
            schemas[0]["properties"]["crop"],
            json!({"$ref": "#/$defs/crop"}),
            "the order a schema lists its keys in is not part of the shape: {schemas:?}"
        );
        assert_eq!(
            schemas[1]["properties"]["frame"],
            json!({"$ref": "#/$defs/crop"})
        );
    }

    #[test]
    fn one_schema_of_a_document_is_read_with_the_definitions_it_names() {
        let document = json!({
            "$defs": {"point": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2}},
            "actions": []
        });
        let mut schema = json!({"$ref": "#/$defs/point"});
        let definitions = share_repeated_subschemas_among(std::slice::from_mut(&mut schema));
        assert!(definitions.as_object().is_some_and(Map::is_empty));
        let scoped = scoped(&document, &json!({"$ref": "#/$defs/point"}));
        assert_eq!(scoped["$defs"], document["$defs"], "{scoped}");
        assert_eq!(
            local_reference(&scoped, "#/$defs/point").unwrap(),
            &document["$defs"]["point"]
        );
    }

    #[test]
    fn a_reference_to_something_else_is_never_resolved() {
        let root = json!({"$defs": {"item": {"type": "string"}}});
        for reference in [
            "https://example.test/schema.json#/$defs/item",
            "file:///etc/passwd#/$defs/item",
            "other.json#/$defs/item",
            "/$defs/item",
            "#definitions/item",
            "#/$defs/missing",
            "#/$defs/item/",
        ] {
            assert!(
                local_reference(&root, reference).is_err(),
                "`{reference}` names nothing in this schema"
            );
        }
        assert_eq!(
            local_reference(&root, "#/$defs/item").unwrap(),
            &json!({"type": "string"})
        );
    }
}
