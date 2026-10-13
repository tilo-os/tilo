//! What the checker knows about a value before the run produces it.

use std::fmt;

use serde_json::Value;
use tilo_actions::contract::ResultSpec;
use tilo_actions::params::{ActionArgs, ActionParam, ParamType};

use crate::catalog::kind_matches;

/// The static type of a binding. `Any` is "not knowable at save time"; the
/// run still binds every argument strictly before invoking.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Ty {
    Any,
    Null,
    Text,
    Integer,
    Number,
    Boolean,
    DateTime,
    Secret,
    /// A resource address, with the resource kind when it is declared.
    Resource(Option<String>),
    /// An object, with its schema when one is declared.
    Object(Option<Value>),
    /// An array, with the schema of its items when one is declared.
    Array(Option<Value>),
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => f.write_str("any value"),
            Self::Null => f.write_str("null"),
            Self::Text => f.write_str("text"),
            Self::Integer => f.write_str("an integer"),
            Self::Number => f.write_str("a number"),
            Self::Boolean => f.write_str("a boolean"),
            Self::DateTime => f.write_str("a date-time"),
            Self::Secret => f.write_str("a secret"),
            Self::Resource(Some(kind)) => write!(f, "a `{kind}` resource"),
            Self::Resource(None) => f.write_str("a resource"),
            Self::Object(_) => f.write_str("an object"),
            Self::Array(_) => f.write_str("an array"),
        }
    }
}

impl Ty {
    /// The type of a value supplied for `param`.
    pub(crate) fn of_param(param: &ActionParam) -> Self {
        match param.ty {
            ParamType::Text => Self::Text,
            ParamType::Integer => Self::Integer,
            ParamType::Number => Self::Number,
            ParamType::Boolean => Self::Boolean,
            ParamType::DateTime => Self::DateTime,
            ParamType::Secret => Self::Secret,
            ParamType::Resource => Self::Resource(None),
            ParamType::Object => Self::Object(param.schema.as_deref().cloned()),
            ParamType::Array => Self::Array(
                param
                    .schema
                    .as_ref()
                    .and_then(|schema| schema.get("items"))
                    .cloned(),
            ),
        }
    }

    /// The type a JSON Schema fragment describes, when it names exactly one.
    pub(crate) fn of_schema(schema: &Value) -> Self {
        let Some(name) = schema.get("type").and_then(Value::as_str) else {
            return Self::Any;
        };
        match name {
            "null" => Self::Null,
            "boolean" => Self::Boolean,
            "integer" => Self::Integer,
            "number" => Self::Number,
            "string" if schema.get("format").and_then(Value::as_str) == Some("date-time") => {
                Self::DateTime
            }
            "string" => Self::Text,
            "object" => Self::Object(Some(schema.clone())),
            "array" => Self::Array(schema.get("items").cloned()),
            _ => Self::Any,
        }
    }

    /// The type of a step's successful result, or `None` when it declares
    /// no value to refer to.
    pub(crate) fn of_result(result: &ResultSpec) -> Option<(Self, Option<Value>)> {
        match result {
            ResultSpec::Json { schema } => Some((Self::of_schema(schema), Some(schema.clone()))),
            ResultSpec::Resource { resource_kind } => {
                Some((Self::Resource(Some(resource_kind.clone())), None))
            }
            // A verb answers no value but names the item it created, such
            // as the message a `post` added, for a later step to act on.
            ResultSpec::None => Some((Self::Resource(None), None)),
            ResultSpec::Job => None,
        }
    }

    /// Whether a value of this type may be interpolated into text. A secret
    /// may not: it would end up in whatever the text is shown or stored in.
    pub(crate) const fn renders_as_text(&self) -> bool {
        !matches!(self, Self::Secret)
    }
}

/// Whether a value of type `from` can satisfy `param`, and why not.
pub(crate) fn assignable(from: &Ty, param: &ActionParam) -> Result<(), String> {
    let fits = match (from, param.ty) {
        // An optional parameter takes a null as "not given"; the run omits it.
        (Ty::Null, _) => param.nullable || !param.required,
        // Plain text may be passed where a secret is wanted; the reverse
        // would leak the secret into an ordinary field.
        (Ty::Any, _)
        | (Ty::Text, ParamType::Text | ParamType::Secret)
        | (Ty::Integer, ParamType::Integer | ParamType::Number)
        | (Ty::Number, ParamType::Number)
        | (Ty::Boolean, ParamType::Boolean)
        | (Ty::DateTime, ParamType::DateTime | ParamType::Text)
        | (Ty::Secret, ParamType::Secret)
        | (Ty::Resource(_), ParamType::Resource)
        | (Ty::Object(_), ParamType::Object)
        | (Ty::Array(_), ParamType::Array) => true,
        _ => false,
    };
    if !fits {
        return Err(format!(
            "`{}` takes {}, but is bound to {from}",
            param.name,
            Ty::of_param(param)
        ));
    }
    if let (Ty::Resource(Some(kind)), false) = (from, param.choices.is_empty())
        && !param.choices.iter().any(|choice| {
            choice
                .split_once(':')
                .is_some_and(|(k, _)| kind_matches(kind, k))
        })
    {
        return Err(format!(
            "`{}` accepts none of the `{kind}` resources it is bound to",
            param.name
        ));
    }
    Ok(())
}

/// Check a constant against `param` exactly as the run will bind it.
pub(crate) fn literal_fits(value: &Value, param: &ActionParam) -> Result<(), String> {
    let mut supplied = serde_json::Map::new();
    supplied.insert(param.name.to_string(), value.clone());
    ActionArgs::bind_json(std::slice::from_ref(param), &supplied)
        .map(drop)
        .map_err(|error| error.to_string())
}

/// The schema at `pointer` inside a value described by `schema`.
///
/// `Ok(None)` when the schema does not say (no type, several types, open
/// properties): the pointer may still resolve at run time. `Err` when the
/// schema rules it out.
pub(crate) fn schema_at(schema: &Value, pointer: &str) -> Result<Option<Value>, String> {
    let mut current = schema.clone();
    for token in pointer_tokens(pointer)? {
        let kind = current.get("type").and_then(Value::as_str);
        let next = match kind {
            Some("object") | None if current.get("properties").is_some() || kind.is_some() => {
                let properties = current.get("properties").and_then(Value::as_object);
                match properties.and_then(|properties| properties.get(&token)) {
                    Some(property) => property.clone(),
                    None => match current.get("additionalProperties") {
                        Some(Value::Bool(false)) => {
                            return Err(format!("the result has no field `{token}`"));
                        }
                        Some(extra @ Value::Object(_)) => extra.clone(),
                        _ => return Ok(None),
                    },
                }
            }
            Some("array") => {
                if token.parse::<usize>().is_err() {
                    return Err(format!("`{token}` is not an array index"));
                }
                match current.get("items") {
                    Some(items) => items.clone(),
                    None => return Ok(None),
                }
            }
            Some(scalar) => {
                return Err(format!("`{token}` descends into a {scalar} value"));
            }
            None => return Ok(None),
        };
        current = next;
    }
    Ok(Some(current))
}

/// RFC 6901 tokens of `pointer`.
pub(crate) fn pointer_tokens(pointer: &str) -> Result<Vec<String>, String> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    let rest = pointer
        .strip_prefix('/')
        .ok_or_else(|| format!("`{pointer}` is not a JSON pointer (it must start with `/`)"))?;
    Ok(rest
        .split('/')
        .map(|token| token.replace("~1", "/").replace("~0", "~"))
        .collect())
}
