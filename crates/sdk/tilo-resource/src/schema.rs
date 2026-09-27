//! JSON Schemas for the values actions carry, so a caller with no screen can
//! read what an operation takes instead of guessing it.
//!
//! [`JsonSchema`] describes a type's serde JSON form. `#[derive(JsonSchema)]`
//! implements it from the type's definition and its serde attributes, so the
//! schema is the type: a new variant or field is described the moment it is
//! written. [`Resource::action_schema`](crate::Resource::action_schema) is
//! what the generic [`APPLY`](crate::APPLY) and [`GROUP`](crate::GROUP)
//! operations publish as their parameters' schema.
//!
//! Schemas stay within the vocabulary `tilo-actions` validates and publishes
//! (`type`, `const`, `enum`, `properties`, `required`,
//! `additionalProperties: false`, `items`, `oneOf`, bounds, `description`):
//! everything is inline, with no references, so every surface that lists an
//! operation shows the whole shape and binding enforces it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::{Map, Value, json};
use tilo_actions::{ActionParam, ParamType};

/// A type whose serde JSON form has a JSON Schema.
///
/// Derive it with `#[derive(JsonSchema)]` next to `Serialize` and
/// `Deserialize`. The derive follows the serde attributes that change the
/// JSON form (`rename`, `rename_all`, `tag`, `content`, `transparent`,
/// `default`, `skip`) and refuses the ones it cannot describe (`flatten`,
/// `untagged`, custom `with` functions), so a schema never silently differs
/// from what serde reads. Doc comments become descriptions.
pub trait JsonSchema {
    /// Whether serde reads an absent field of this type as its default, as it
    /// does for [`Option`]: such a field is not required.
    const OPTIONAL: bool = false;

    /// The schema of this type's JSON form.
    fn json_schema() -> Value;
}

macro_rules! primitive {
    ($($ty:ty => $schema:expr),* $(,)?) => {
        $(impl JsonSchema for $ty {
            fn json_schema() -> Value {
                $schema
            }
        })*
    };
}

primitive! {
    bool => json!({"type": "boolean"}),
    String => json!({"type": "string"}),
    str => json!({"type": "string"}),
    char => json!({"type": "string", "minLength": 1, "maxLength": 1}),
    f32 => json!({"type": "number"}),
    f64 => json!({"type": "number"}),
    i8 => json!({"type": "integer", "minimum": i8::MIN, "maximum": i8::MAX}),
    i16 => json!({"type": "integer", "minimum": i16::MIN, "maximum": i16::MAX}),
    i32 => json!({"type": "integer", "minimum": i32::MIN, "maximum": i32::MAX}),
    i64 => json!({"type": "integer"}),
    isize => json!({"type": "integer"}),
    u8 => json!({"type": "integer", "minimum": 0, "maximum": u8::MAX}),
    u16 => json!({"type": "integer", "minimum": 0, "maximum": u16::MAX}),
    u32 => json!({"type": "integer", "minimum": 0, "maximum": u32::MAX}),
    u64 => json!({"type": "integer", "minimum": 0}),
    usize => json!({"type": "integer", "minimum": 0}),
    serde_json::Value => json!({}),
    crate::Id => json!({"type": "string"}),
    uuid::Uuid => json!({"type": "string", "format": "uuid"}),
}

impl<T: JsonSchema> JsonSchema for Option<T> {
    const OPTIONAL: bool = true;

    fn json_schema() -> Value {
        nullable(T::json_schema())
    }
}

impl<T: JsonSchema + ?Sized> JsonSchema for Box<T> {
    const OPTIONAL: bool = T::OPTIONAL;

    fn json_schema() -> Value {
        T::json_schema()
    }
}

impl<T: JsonSchema + ?Sized> JsonSchema for std::sync::Arc<T> {
    const OPTIONAL: bool = T::OPTIONAL;

    fn json_schema() -> Value {
        T::json_schema()
    }
}

impl<T: JsonSchema + ?Sized> JsonSchema for &T {
    const OPTIONAL: bool = T::OPTIONAL;

    fn json_schema() -> Value {
        T::json_schema()
    }
}

impl<T: JsonSchema> JsonSchema for [T] {
    fn json_schema() -> Value {
        array(T::json_schema())
    }
}

impl<T: JsonSchema> JsonSchema for Vec<T> {
    fn json_schema() -> Value {
        array(T::json_schema())
    }
}

impl<T: JsonSchema, const N: usize> JsonSchema for [T; N] {
    fn json_schema() -> Value {
        let mut schema = array(T::json_schema());
        if let Some(object) = schema.as_object_mut() {
            object.insert("minItems".into(), N.into());
            object.insert("maxItems".into(), N.into());
        }
        schema
    }
}

macro_rules! tuples {
    ($($len:literal => ($($name:ident),+)),* $(,)?) => {
        $(impl<$($name: JsonSchema),+> JsonSchema for ($($name,)+) {
            fn json_schema() -> Value {
                tuple($len)
            }
        })*
    };
}

tuples! {
    1 => (A),
    2 => (A, B),
    3 => (A, B, C),
    4 => (A, B, C, D),
}

// Serde reads a set from any array and keeps one of each value, so a
// repeated value is not an error the schema should assert.
impl<T: JsonSchema, S> JsonSchema for HashSet<T, S> {
    fn json_schema() -> Value {
        array(T::json_schema())
    }
}

impl<T: JsonSchema> JsonSchema for BTreeSet<T> {
    fn json_schema() -> Value {
        array(T::json_schema())
    }
}

// The published vocabulary has no schema for a map's values, so a map is
// described as an object and its values are checked by the action itself.
impl<K, V: JsonSchema, S> JsonSchema for HashMap<K, V, S> {
    fn json_schema() -> Value {
        json!({"type": "object"})
    }
}

impl<K, V: JsonSchema> JsonSchema for BTreeMap<K, V> {
    fn json_schema() -> Value {
        json!({"type": "object"})
    }
}

/// An array of `items`.
pub fn array(items: Value) -> Value {
    let mut schema = Map::new();
    schema.insert("type".into(), "array".into());
    schema.insert("items".into(), items);
    Value::Object(schema)
}

/// An array of exactly `len` values. The published vocabulary has no
/// per-position item schema, so the positions are described in words.
pub fn tuple(len: usize) -> Value {
    json!({"type": "array", "minItems": len, "maxItems": len})
}

/// `schema`, or JSON `null`.
pub fn nullable(schema: Value) -> Value {
    let Value::Object(mut object) = schema else {
        return schema;
    };
    if object.is_empty() {
        return Value::Object(object);
    }
    let simple = !object.contains_key("oneOf")
        && !object.contains_key("const")
        && !object.contains_key("enum");
    match object.get_mut("type") {
        Some(Value::String(name)) if simple => {
            if name != "null" {
                let name = std::mem::take(name);
                object.insert("type".into(), json!([name, "null"]));
            }
            Value::Object(object)
        }
        Some(Value::Array(types)) if simple => {
            if !types.iter().any(|name| name == "null") {
                types.push("null".into());
            }
            Value::Object(object)
        }
        _ => {
            let description = object.remove("description");
            let mut wrapped = Map::new();
            wrapped.insert(
                "oneOf".into(),
                json!([{"type": "null"}, Value::Object(object)]),
            );
            if let Some(description) = description {
                wrapped.insert("description".into(), description);
            }
            Value::Object(wrapped)
        }
    }
}

/// `schema` with `description`, when there is one.
pub fn describe(schema: Value, description: &str) -> Value {
    if description.is_empty() {
        return schema;
    }
    match schema {
        Value::Object(mut object) => {
            object.insert("description".into(), description.into());
            Value::Object(object)
        }
        other => other,
    }
}

/// A closed object: `properties` as `(name, schema, required)`, nothing else.
pub fn object(properties: Vec<(&str, Value, bool)>) -> Value {
    let required: Vec<Value> = properties
        .iter()
        .filter(|(_, _, required)| *required)
        .map(|(name, _, _)| Value::from(*name))
        .collect();
    let properties: Map<String, Value> = properties
        .into_iter()
        .map(|(name, schema, _)| (name.to_owned(), schema))
        .collect();
    let mut schema = Map::new();
    schema.insert("type".into(), "object".into());
    if !required.is_empty() {
        schema.insert("required".into(), Value::Array(required));
    }
    schema.insert("properties".into(), Value::Object(properties));
    schema.insert("additionalProperties".into(), false.into());
    Value::Object(schema)
}

/// Exactly one of `alternatives`.
pub fn one_of(alternatives: Vec<Value>) -> Value {
    let mut schema = Map::new();
    schema.insert("oneOf".into(), Value::Array(alternatives));
    Value::Object(schema)
}

/// The string `value`.
pub fn constant(value: &str) -> Value {
    json!({"type": "string", "const": value})
}

/// One of `values`.
pub fn string_enum(values: &[&str]) -> Value {
    json!({"type": "string", "enum": values})
}

/// An externally tagged variant: `{ name: content }`.
pub fn external(name: &str, content: Value) -> Value {
    object(vec![(name, content, true)])
}

/// An adjacently tagged variant: `{ tag: name, content_key: content }`, or
/// only the tag for a unit variant.
pub fn adjacent(tag: &str, content_key: &str, name: &str, content: Option<Value>) -> Value {
    let mut properties = vec![(tag, constant(name), true)];
    if let Some(content) = content {
        properties.push((content_key, content, true));
    }
    object(properties)
}

/// An internally tagged variant: `content`'s fields with `tag` naming the
/// variant among them.
pub fn internal(tag: &str, name: &str, content: Value) -> Value {
    let Value::Object(mut schema) = content else {
        return object(vec![(tag, constant(name), true)]);
    };
    if let Some(Value::Object(properties)) = schema.get_mut("properties") {
        properties.insert(tag.to_owned(), constant(name));
    } else {
        schema.insert("type".into(), "object".into());
        schema.insert("properties".into(), json!({ tag: constant(name) }));
    }
    match schema.get_mut("required") {
        Some(Value::Array(required)) => required.insert(0, tag.into()),
        _ => {
            schema.insert("required".into(), json!([tag]));
        }
    }
    Value::Object(schema)
}

/// `schema` without its descriptions: the same shape, smaller, for a copy
/// nested where the documented original is published beside it.
pub fn undescribed(schema: &Value) -> Value {
    match schema {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter(|(key, _)| key.as_str() != "description")
                .map(|(key, value)| {
                    let value = if key == "properties" {
                        match value {
                            Value::Object(properties) => Value::Object(
                                properties
                                    .iter()
                                    .map(|(name, child)| (name.clone(), undescribed(child)))
                                    .collect(),
                            ),
                            other => other.clone(),
                        }
                    } else {
                        undescribed(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(undescribed).collect()),
        other => other.clone(),
    }
}

/// How a parameter's value is read: by [`tilo_actions::ActionArgs::bind`],
/// which refuses `null` unless the parameter is nullable, or by serde, which
/// reads `null` into an optional field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reading {
    Bound,
    Serde,
}

/// The schema of one declared parameter's value.
pub(crate) fn param_schema(param: &ActionParam, reading: Reading) -> Value {
    let base = match param.ty {
        ParamType::Text | ParamType::Secret | ParamType::Resource => json!({"type": "string"}),
        ParamType::DateTime => json!({"type": "string", "format": "date-time"}),
        ParamType::Integer => json!({"type": "integer"}),
        ParamType::Number => json!({"type": "number"}),
        ParamType::Boolean => json!({"type": "boolean"}),
        ParamType::Array => json!({"type": "array"}),
        // Binding checks the kind; serde reads whatever the field's type is,
        // which a declaration does not record.
        ParamType::Object => match reading {
            Reading::Bound => json!({"type": "object"}),
            Reading::Serde => json!({}),
        },
    };
    let mut schema = match (&param.schema, base) {
        (Some(Value::Object(declared)), Value::Object(base)) => {
            let mut merged = declared.clone();
            for (key, value) in base {
                merged.entry(key).or_insert(value);
            }
            merged
        }
        (_, Value::Object(base)) => base,
        (_, _) => Map::new(),
    };
    if !param.choices.is_empty() && !schema.contains_key("enum") {
        let choices = param
            .choices
            .iter()
            .map(|choice| match param.ty {
                ParamType::Text | ParamType::Secret | ParamType::Resource | ParamType::DateTime => {
                    Value::String(choice.to_string())
                }
                _ => serde_json::from_str(choice).unwrap_or_else(|_| choice.to_string().into()),
            })
            .collect();
        schema.insert("enum".into(), Value::Array(choices));
    }
    let schema = describe(Value::Object(schema), &param.description);
    let null = match reading {
        Reading::Bound => param.nullable,
        Reading::Serde => param.nullable || !param.required,
    };
    if null { nullable(schema) } else { schema }
}

/// The arguments object of a declaration.
pub(crate) fn args_schema(params: &[ActionParam], reading: Reading) -> Value {
    object(
        params
            .iter()
            .map(|param| {
                (
                    param.name.as_ref(),
                    param_schema(param, reading),
                    param.required,
                )
            })
            .collect(),
    )
}

/// The replayable action of a `#[resource]` record, as the generic
/// [`APPLY`](crate::APPLY) takes it: `{"op": {"action": id, "args": {..}},
/// "at": ms}`, one alternative per declaration.
pub fn record_action_schema<R: crate::Resource>() -> Value {
    let operations = R::declarations()
        .iter()
        .map(|decl| {
            let variant = object(vec![
                ("action", constant(decl.id), true),
                ("args", args_schema(&decl.params, Reading::Serde), true),
            ]);
            describe(variant, decl.description.as_deref().unwrap_or(&decl.label))
        })
        .collect();
    object(vec![
        ("op", one_of(operations), true),
        (
            "at",
            describe(
                json!({"type": "integer"}),
                "When the action was authored, in milliseconds since the Unix epoch",
            ),
            false,
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_values_accept_null_and_keep_their_shape() {
        assert_eq!(
            <Option<String>>::json_schema(),
            json!({"type": ["string", "null"]})
        );
        let choice = nullable(one_of(vec![constant("a"), constant("b")]));
        assert!(tilo_actions::validate_json_schema(&Value::Null, &choice).is_ok());
        assert!(tilo_actions::validate_json_schema(&json!("a"), &choice).is_ok());
        assert!(tilo_actions::validate_json_schema(&json!("c"), &choice).is_err());
        const { assert!(<Option<u8>>::OPTIONAL) };
        const { assert!(!<Vec<u8>>::OPTIONAL) };
    }

    #[test]
    fn internal_tags_join_the_variant_fields() {
        let schema = internal(
            "kind",
            "Circle",
            object(vec![("radius", f64::json_schema(), true)]),
        );
        assert!(
            tilo_actions::validate_json_schema(&json!({"kind": "Circle", "radius": 1.5}), &schema)
                .is_ok()
        );
        assert!(tilo_actions::validate_json_schema(&json!({"radius": 1.5}), &schema).is_err());
    }

    #[test]
    fn a_copy_without_descriptions_keeps_fields_named_description() {
        let schema = describe(
            object(vec![(
                "description",
                describe(String::json_schema(), "Text"),
                true,
            )]),
            "A note",
        );
        let copy = undescribed(&schema);
        assert!(copy.get("description").is_none());
        assert_eq!(copy["properties"]["description"], json!({"type": "string"}));
    }
}
