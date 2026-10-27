//! Checking an answer against a declared shape, in the one schema vocabulary
//! Tilo's actions already publish.
//!
//! A suite must not be able to declare a shape the feature it describes would
//! never accept: an unsupported keyword, a malformed pattern, a `oneOf` of
//! nothing. [`check_declaration`] refuses those before a run starts, so a suite
//! that passes is one whose refusal at run time is about the model's answer
//! and not about the case's own typo.

use serde_json::Value;

/// The keywords that assert something about a value. A suite may use these.
///
/// This list is `tilo-actions`' own, which this tool does not link; the pin
/// test below holds it word for word against that crate's, so a suite cannot
/// declare a shape the action model would refuse to check.
pub(crate) const ASSERTIONS: &[&str] = &[
    "type",
    "enum",
    "const",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "minItems",
    "maxItems",
    "uniqueItems",
    "items",
    "prefixItems",
    "required",
    "properties",
    "additionalProperties",
    "oneOf",
];

/// Keywords that say something about a shape without constraining a value.
/// Pinned against `tilo-actions` like [`ASSERTIONS`].
pub(crate) const ANNOTATIONS: &[&str] = &[
    "$schema",
    "$id",
    "$comment",
    "title",
    "description",
    "default",
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
    "resource_kind",
    "place",
    "content",
    "choice_labels",
    "people",
    "pick",
];

/// The formats a shape may ask for.
const FORMATS: &[&str] = &["date-time", "uuid", "uri"];

/// The JSON types a shape may name.
const TYPES: &[&str] = &[
    "null", "boolean", "object", "array", "number", "integer", "string",
];

/// Whether `shape` belongs to the schema vocabulary an action's result is
/// checked against.
///
/// # Errors
///
/// The first thing the shape would let an answer get away with: an unknown
/// keyword, a type or format nothing checks, a malformed pattern, or a
/// `oneOf` with no alternative.
pub fn check_declaration(shape: &Value) -> Result<(), String> {
    let object = shape
        .as_object()
        .ok_or_else(|| "a shape must be an object".to_string())?;
    for keyword in object.keys() {
        if !ASSERTIONS.contains(&keyword.as_str()) && !ANNOTATIONS.contains(&keyword.as_str()) {
            return Err(format!("`{keyword}` is not a keyword Tilo checks"));
        }
    }
    check_type(object)?;
    check_strings(object)?;
    check_collections(object)?;
    check_numbers(object)
}

fn check_type(object: &serde_json::Map<String, Value>) -> Result<(), String> {
    let Some(types) = object.get("type") else {
        return Ok(());
    };
    let known = |name: &str| TYPES.contains(&name);
    let valid = types.as_str().is_some_and(known)
        || types.as_array().is_some_and(|types| {
            !types.is_empty() && types.iter().all(|one| one.as_str().is_some_and(known))
        });
    if valid {
        Ok(())
    } else {
        Err(format!("`{types}` is not a JSON type"))
    }
}

fn check_strings(object: &serde_json::Map<String, Value>) -> Result<(), String> {
    if let Some(pattern) = object.get("pattern") {
        let pattern = pattern
            .as_str()
            .ok_or_else(|| "`pattern` must be a string".to_string())?;
        regex_lite::Regex::new(pattern)
            .map_err(|error| format!("`pattern` is not a regular expression: {error}"))?;
    }
    if let Some(format) = object.get("format") {
        let format = format
            .as_str()
            .ok_or_else(|| "`format` must be a string".to_string())?;
        if !FORMATS.contains(&format) {
            return Err(format!("`format` {format} is not one Tilo checks"));
        }
    }
    Ok(())
}

fn check_collections(object: &serde_json::Map<String, Value>) -> Result<(), String> {
    if let Some(required) = object.get("required")
        && !required
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string))
    {
        return Err("`required` must be a list of property names".into());
    }
    if object.get("enum").is_some_and(|values| !values.is_array()) {
        return Err("`enum` must be a list".into());
    }
    for flag in ["additionalProperties", "uniqueItems"] {
        if object.get(flag).is_some_and(|value| !value.is_boolean()) {
            return Err(format!("`{flag}` must be true or false"));
        }
    }
    if let Some(properties) = object.get("properties") {
        let properties = properties
            .as_object()
            .ok_or_else(|| "`properties` must be an object".to_string())?;
        for (name, child) in properties {
            check_declaration(child).map_err(|reason| format!("property `{name}` {reason}"))?;
        }
    }
    if let Some(items) = object.get("items") {
        check_declaration(items).map_err(|reason| format!("items {reason}"))?;
    }
    if let Some(prefix) = object.get("prefixItems") {
        let prefix = prefix
            .as_array()
            .ok_or_else(|| "`prefixItems` must be a list".to_string())?;
        for (index, item) in prefix.iter().enumerate() {
            check_declaration(item).map_err(|reason| format!("prefixItems[{index}] {reason}"))?;
        }
    }
    if let Some(alternatives) = object.get("oneOf") {
        let alternatives = alternatives
            .as_array()
            .filter(|alternatives| !alternatives.is_empty())
            .ok_or_else(|| "`oneOf` must be a non-empty list".to_string())?;
        for (index, alternative) in alternatives.iter().enumerate() {
            check_declaration(alternative).map_err(|reason| format!("oneOf[{index}] {reason}"))?;
        }
    }
    Ok(())
}

fn check_numbers(object: &serde_json::Map<String, Value>) -> Result<(), String> {
    for keyword in ["minimum", "maximum"] {
        if object.get(keyword).is_some_and(|value| !value.is_number()) {
            return Err(format!("`{keyword}` must be a number"));
        }
    }
    for keyword in ["minLength", "maxLength", "minItems", "maxItems"] {
        if object
            .get(keyword)
            .is_some_and(|value| value.as_u64().is_none())
        {
            return Err(format!(
                "`{keyword}` must be a whole number of at least zero"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_shape_the_feature_would_accept_is_accepted() {
        for shape in [
            json!({"type": "string", "minLength": 1, "maxLength": 40}),
            json!({"type": "string", "enum": ["answer", "work"]}),
            json!({"type": "object", "properties": {"done": {"type": "boolean"}},
                   "required": ["done"], "additionalProperties": false}),
            json!({"oneOf": [{"type": "string"}, {"type": "array",
                   "items": {"type": "string"}, "uniqueItems": true}]}),
        ] {
            assert_eq!(check_declaration(&shape), Ok(()), "{shape}");
        }
    }

    #[test]
    fn a_shape_that_would_weaken_itself_is_refused_with_its_reason() {
        let refused = |shape: Value| check_declaration(&shape).unwrap_err();
        assert!(refused(json!({"type": "string", "x-anything": 1})).contains("x-anything"));
        assert!(refused(json!({"type": "text"})).contains("not a JSON type"));
        assert!(refused(json!({"pattern": "("})).contains("regular expression"));
        assert!(refused(json!({"format": "email"})).contains("email"));
        assert!(refused(json!({"oneOf": []})).contains("non-empty"));
        assert!(refused(json!({"properties": {"a": {"type": "nope"}}})).contains("property `a`"));
        assert!(refused(json!({"maxLength": -1})).contains("maxLength"));
        assert!(refused(json!("every string")).contains("object"));
    }

    /// `tilo-actions`' `schema_vocabulary_pin_tests` holds the same text. A
    /// keyword added to one crate and not the other fails one of the two.
    const PINNED: &str = "\
type
enum
const
minimum
maximum
minLength
maxLength
pattern
format
minItems
maxItems
uniqueItems
items
prefixItems
required
properties
additionalProperties
oneOf
---
$schema
$id
$comment
title
description
default
examples
deprecated
readOnly
writeOnly
resource_kind
place
content
choice_labels
people
pick";

    #[test]
    fn the_vocabulary_is_the_one_the_action_model_checks() {
        let listed: Vec<&str> = ASSERTIONS
            .iter()
            .chain(std::iter::once(&"---"))
            .chain(ANNOTATIONS)
            .copied()
            .collect();
        assert_eq!(PINNED, listed.join("\n"));
    }
}
