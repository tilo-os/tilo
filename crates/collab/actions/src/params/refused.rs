//! What a refused argument is told: the kind of thing it was given, the kind
//! it must be and the shape that works, written from the action's own
//! declaration, so that no app writes such a sentence by hand. Never the
//! value it was given: a refusal is read by whoever called, and it is logged.

use serde_json::{Map, Value};

use super::{ActionParam, ArgError, ArgValue, ParamType};

/// What stands in a shape where any value may.
const ANY: &str = "<any value>";
/// How text is written in a shape.
const TEXT: &str = "\"<text>\"";
/// How a date and time is written in a shape.
const DATE_TIME: &str = "\"<date and time such as 2026-01-31T09:00:00Z>\"";

/// The kind of thing `value` is, in a caller's words. Its kind alone: what it
/// holds is never said.
pub(super) fn given(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "an integer",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// The kind of thing a parameter of type `ty` takes, in the same words.
pub(super) fn kind(ty: ParamType) -> &'static str {
    match ty {
        ParamType::Text => "text",
        ParamType::Integer => "an integer",
        ParamType::Number => "a number",
        ParamType::Boolean => "a boolean",
        ParamType::Object => "an object",
        ParamType::Array => "a list",
        ParamType::Resource => "a resource's address",
        ParamType::DateTime => "a date and time",
        ParamType::Secret => "a secret",
    }
}

/// The JSON type a parameter of type `ty` is written as.
fn written_as(ty: ParamType) -> &'static str {
    match ty {
        ParamType::Text | ParamType::Resource | ParamType::DateTime | ParamType::Secret => "string",
        ParamType::Integer => "integer",
        ParamType::Number => "number",
        ParamType::Boolean => "boolean",
        ParamType::Object => "object",
        ParamType::Array => "array",
    }
}

/// The JSON types `schema` takes: the ones its `type` names, else `own` (the
/// type of the parameter it belongs to, empty for none), else the one its
/// other keywords are about.
fn types<'a>(schema: &'a Map<String, Value>, own: &'a str) -> Vec<&'a str> {
    let about = |keys: &[&str]| keys.iter().any(|key| schema.contains_key(*key));
    match schema.get("type") {
        Some(Value::String(name)) => vec![name.as_str()],
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).collect(),
        _ if !own.is_empty() => vec![own],
        _ if about(&["properties", "required", "additionalProperties"]) => vec!["object"],
        _ if about(&[
            "items",
            "prefixItems",
            "minItems",
            "maxItems",
            "uniqueItems",
        ]) =>
        {
            vec!["array"]
        }
        _ if about(&["minLength", "maxLength", "pattern", "format"]) => vec!["string"],
        _ if about(&["minimum", "maximum"]) => vec!["number"],
        _ => Vec::new(),
    }
}

/// The kinds `types` name, in a caller's words: `string` is text, `array` a
/// list.
fn kinds(types: &[&str]) -> String {
    types
        .iter()
        .map(|name| match *name {
            "string" => "text",
            "integer" => "an integer",
            "number" => "a number",
            "boolean" => "a boolean",
            "object" => "an object",
            "array" => "a list",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(" or ")
}

/// The shape of a value `schema` takes, written as JSON with a word in angle
/// brackets where a value goes and `?` after a key that may be left out.
pub(super) fn shape(schema: &Value) -> String {
    schema
        .as_object()
        .map_or_else(|| ANY.to_owned(), |schema| shape_under(schema, ""))
}

/// [`shape`] of a schema, taken as of the JSON type `own` where it names
/// none.
fn shape_under(schema: &Map<String, Value>, own: &str) -> String {
    if let Some(constant) = schema.get("const") {
        return constant.to_string();
    }
    if let Some(values) = schema
        .get("enum")
        .and_then(Value::as_array)
        .filter(|values| !values.is_empty())
    {
        return values
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" | ");
    }
    let mut shapes: Vec<String> = Vec::new();
    for alternative in schema
        .get("oneOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let alternative = shape(alternative);
        if alternative != "null" && !shapes.contains(&alternative) {
            shapes.push(alternative);
        }
    }
    if shapes.is_empty() {
        shapes = types(schema, own)
            .into_iter()
            .filter(|name| *name != "null")
            .map(|name| shape_of_type(name, schema))
            .collect();
    }
    if shapes.is_empty() {
        ANY.to_owned()
    } else {
        shapes.join(" or ")
    }
}

/// The shape of a value of the JSON type `name` under `schema`.
fn shape_of_type(name: &str, schema: &Map<String, Value>) -> String {
    match name {
        "string" => text(schema).to_owned(),
        "integer" => number("integer", schema),
        "number" => number("number", schema),
        "boolean" => "<true or false>".to_owned(),
        "array" => list(schema),
        "object" => object(schema),
        other => other.to_owned(),
    }
}

/// How text under `schema` is written: by its format, where it has one.
fn text(schema: &Map<String, Value>) -> &'static str {
    match schema.get("format").and_then(Value::as_str) {
        Some("date-time") => DATE_TIME,
        Some("date") => "\"<date such as 2026-01-31>\"",
        Some("uuid") => "\"<uuid>\"",
        Some("uri") => "\"<address such as https://example.com>\"",
        _ => TEXT,
    }
}

/// How a number called `word` under `schema` is written: with its bounds.
fn number(word: &str, schema: &Map<String, Value>) -> String {
    let bound = |key: &str| schema.get(key).filter(|bound| bound.is_number());
    match (bound("minimum"), bound("maximum")) {
        (Some(least), Some(most)) => format!("<{word} from {least} to {most}>"),
        (Some(least), None) => format!("<{word}, at least {least}>"),
        (None, Some(most)) => format!("<{word}, at most {most}>"),
        (None, None) => format!("<{word}>"),
    }
}

/// How a list under `schema` is written: one of each item it takes.
fn list(schema: &Map<String, Value>) -> String {
    let mut items: Vec<String> = schema
        .get("prefixItems")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(shape)
        .collect();
    match schema.get("items") {
        Some(rest) => items.push(shape(rest)),
        None if items.is_empty() => items.push(ANY.to_owned()),
        None => {}
    }
    format!("[{}]", items.join(", "))
}

/// How an object under `schema` is written: the keys it requires, then the
/// ones that may be left out, each with the shape of its value, then `<key>`
/// for the keys of the caller's own, with the shape the object declares for
/// what stands under them.
fn object(schema: &Map<String, Value>) -> String {
    let properties = schema.get("properties").and_then(Value::as_object);
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut fields: Vec<String> = required
        .iter()
        .map(|key| {
            let value = properties
                .and_then(|properties| properties.get(*key))
                .map_or_else(|| ANY.to_owned(), shape);
            format!("{}: {value}", quoted(key))
        })
        .collect();
    for (key, property) in properties.into_iter().flatten() {
        if !required.contains(&key.as_str()) {
            fields.push(format!("{}?: {}", quoted(key), shape(property)));
        }
    }
    match schema.get("additionalProperties") {
        Some(of_its_own @ Value::Object(_)) => {
            fields.push(format!("<key>: {}", shape(of_its_own)));
        }
        Some(Value::Bool(false)) => {}
        _ if fields.is_empty() => fields.push(format!("<key>: {ANY}")),
        _ => {}
    }
    format!("{{{}}}", fields.join(", "))
}

/// `key` as JSON writes it.
fn quoted(key: &str) -> String {
    Value::String(key.to_owned()).to_string()
}

/// The shape that works for `param`: its choices, what its schema takes, or
/// how its type is written.
pub(super) fn shape_of_param(param: &ActionParam) -> String {
    let own = written_as(param.ty);
    if !param.choices.is_empty() {
        return param
            .choices
            .iter()
            .map(|choice| {
                if own == "string" {
                    quoted(choice)
                } else {
                    choice.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" | ");
    }
    let nothing = Map::new();
    let schema = param
        .schema
        .as_deref()
        .and_then(Value::as_object)
        .unwrap_or(&nothing);
    let own_shape = shape_under(schema, own);
    let written = match param.ty {
        ParamType::Resource if own_shape == TEXT => "\"<kind:id>\"".to_owned(),
        ParamType::DateTime if own_shape == TEXT => DATE_TIME.to_owned(),
        _ => own_shape,
    };
    match by_another(param) {
        Some(decided) => format!("{written} ({decided})"),
        None => written,
    }
}

/// What another argument's value decides of `param`'s shape, where its
/// declaration says so: each value that names a shape, with that shape.
fn by_another(param: &ActionParam) -> Option<String> {
    let (argument, shapes) = param.shapes_by()?;
    let decided: Vec<String> = shapes
        .iter()
        .map(|(value, schema)| {
            format!(
                "when {} is {}: {}",
                quoted(argument),
                quoted(value),
                shape(schema)
            )
        })
        .collect();
    (!decided.is_empty()).then(|| decided.join("; "))
}

/// The arguments an action with `params` takes, as the one object a call
/// gives: the shape of the call that works.
pub(super) fn arguments(params: &[ActionParam]) -> String {
    let fields: Vec<String> = params
        .iter()
        .map(|param| {
            let optional = if param.required { "" } else { "?" };
            format!(
                "{}{optional}: {}",
                quoted(&param.name),
                shape_of_param(param)
            )
        })
        .collect();
    format!("{{{}}}", fields.join(", "))
}

/// `param` was given `value`, which is of another kind than it takes.
pub(super) fn wrong_type(param: &ActionParam, value: &ArgValue) -> ArgError {
    ArgError::WrongType {
        name: param.name.to_string(),
        expected: param.ty,
        got: value.ty(),
        shape: shape_of_param(param),
    }
}

/// `param` was given null, and takes none.
pub(super) fn null(param: &ActionParam) -> ArgError {
    ArgError::Constraint {
        name: param.name.to_string(),
        reason: format!(
            "was given null and must be {}: {}",
            kind(param.ty),
            shape_of_param(param)
        ),
    }
}

/// `param` was given `value`, which is none of its choices.
pub(super) fn not_a_choice(param: &ActionParam, value: &ArgValue) -> ArgError {
    ArgError::NotAllowed {
        name: param.name.to_string(),
        given: given(&value.to_json()),
        allowed: param.choices.iter().map(ToString::to_string).collect(),
    }
}

/// The required `param` was left out.
pub(super) fn missing(param: &ActionParam) -> ArgError {
    ArgError::Missing {
        name: param.name.to_string(),
        takes: format!("{}: {}", kind(param.ty), shape_of_param(param)),
    }
}

/// `param` was left out where the shape another argument's value names for
/// it, `named`, requires it.
pub(super) fn missing_shaped(param: &ActionParam, named: &Value) -> ArgError {
    let nothing = Map::new();
    let fields = named.as_object().unwrap_or(&nothing);
    ArgError::Missing {
        name: param.name.to_string(),
        takes: format!(
            "{}: {}",
            kinds(&types(fields, written_as(param.ty))),
            shape(named)
        ),
    }
}

/// What text that is not written as the `format` its `schema` asks is told:
/// the format, and how such text is written where this crate can say so.
pub(super) fn off_format(format: &str, schema: &Map<String, Value>) -> String {
    let written = text(schema);
    if written == TEXT {
        format!("must match format `{format}`")
    } else {
        format!("must match format `{format}`: {written}")
    }
}

/// `name` is no parameter among `params`.
pub(super) fn unknown(name: String, params: &[ActionParam]) -> ArgError {
    ArgError::Unknown {
        name,
        takes: arguments(params),
    }
}

/// What a value of another JSON type than `schema` takes is told.
pub(super) fn wrong_kind(value: &Value, schema: &Map<String, Value>) -> String {
    format!(
        "was given {} and must be {}: {}",
        given(value),
        kinds(&types(schema, "")),
        shape_under(schema, "")
    )
}

/// What an object without the property `key` its `schema` requires is told.
pub(super) fn lacks(key: &str, schema: &Map<String, Value>) -> String {
    let property = schema
        .get("properties")
        .and_then(|properties| properties.get(key))
        .map_or_else(|| ANY.to_owned(), shape);
    if property == ANY {
        format!("must contain property `{key}`")
    } else {
        format!("must contain property `{key}`: {property}")
    }
}

/// What an object with the property `key`, which its `schema` does not take,
/// is told.
pub(super) fn takes_no(key: &str, schema: &Map<String, Value>) -> String {
    format!(
        "contains unknown property `{key}`; it takes {}",
        object(schema)
    )
}

/// What a value that is none of `values` is told.
pub(super) fn none_of(values: &[Value]) -> String {
    let values: Vec<String> = values.iter().map(ToString::to_string).collect();
    format!("must be one of {}", values.join(", "))
}

/// What a value that is none of `alternatives`, which have no names, is told.
pub(super) fn no_alternative(alternatives: &[Value]) -> String {
    let shapes: Vec<String> = alternatives.iter().map(shape).collect();
    format!(
        "must match exactly one `oneOf` alternative: {}",
        shapes.join(" or ")
    )
}

/// How a value of `param` is written, where its declaration says more of it
/// than its type does: the keys of an object, the items of a list, choices,
/// bounds.
pub(super) fn declared_shape(param: &ActionParam) -> Option<String> {
    let shape = shape_of_param(param);
    let bare = shape_of_type(written_as(param.ty), &Map::new());
    (shape != bare).then_some(shape)
}
