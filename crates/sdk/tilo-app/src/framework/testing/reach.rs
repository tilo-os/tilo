//! Whether every action an app publishes is reachable through the door, with
//! arguments sampled from each action's own schema. A test tool only: it is
//! built with `test-support`, so no release binary carries it.

use super::HeadlessApp;
use crate::framework::app::ResourceApp;

/// Invoke every action `app` publishes, one after another, as a headless
/// drive's `invoke` steps do (`FrameHarness::invoke_action`: the door's own
/// resolution, availability, confirmation and strict binding), each with
/// [`sample_arguments`] and confirmed. Answers the ones the door could not
/// address, as `resource action: why`: published yet reachable only by
/// clicking. An action an earlier one withdrew (its row was removed) is not
/// asked; one that is unavailable now, refuses the sample as arguments (a
/// payload only another answer can build, such as a review to restore), or
/// ran and failed at its job, was reached.
pub fn unreachable_actions<A: ResourceApp>(app: &mut HeadlessApp<A>) -> Vec<String> {
    let surveyed = tilo_actions::door::survey(&app.harness().resources());
    let mut unreachable = Vec::new();
    for descriptor in surveyed {
        let address = descriptor.address.clone();
        let arguments = sample_arguments(&descriptor);
        let resources = app.harness().resources();
        let Some(spec) = tilo_actions::door::resolve(&resources, &address) else {
            continue;
        };
        let phrase = match spec.confirmation_for_json(&arguments) {
            tilo_actions::ConfirmationPolicy::Phrase { phrase } => Some(phrase),
            tilo_actions::ConfirmationPolicy::None | tilo_actions::ConfirmationPolicy::Confirm => {
                None
            }
        };
        let answer = app
            .harness()
            .invoke_action(&address, &arguments, true, phrase.as_deref());
        if let Err(error) = answer
            && error.code() == "unknown_action"
        {
            unreachable.push(format!("{} {}: {error}", address.resource, address.action));
        }
    }
    unreachable
}

/// Arguments `descriptor`'s parameters accept, for a test that invokes an
/// action without knowing it: each required parameter gets its first
/// choice, else what its schema names (a default, a constant, the first
/// enumerated value) or the least value of its type the schema admits, and
/// a resource parameter the action's own resource. Optional parameters are
/// left out, as a caller who knows no better leaves them.
fn sample_arguments(
    descriptor: &tilo_actions::ActionDescriptor,
) -> serde_json::Map<String, serde_json::Value> {
    descriptor
        .params
        .iter()
        .filter(|param| param.required)
        .map(|param| (param.name.to_string(), sample_param(descriptor, param)))
        .collect()
}

fn sample_param(
    descriptor: &tilo_actions::ActionDescriptor,
    param: &tilo_actions::ActionParam,
) -> serde_json::Value {
    use tilo_actions::ParamType;
    let numeric = matches!(param.ty, ParamType::Integer | ParamType::Number);
    if let Some(choice) = param.choices.first() {
        return numeric
            .then(|| serde_json::from_str(choice).ok())
            .flatten()
            .unwrap_or_else(|| serde_json::Value::String(choice.to_string()));
    }
    if let Some(value) = param.schema.as_deref().and_then(sample_schema) {
        return value;
    }
    let schema = param.schema.as_deref().unwrap_or(&serde_json::Value::Null);
    match param.ty {
        ParamType::Text | ParamType::Secret => sample_string(schema),
        ParamType::Integer => sample_integer(schema),
        ParamType::Number => sample_number(schema),
        ParamType::Boolean => serde_json::Value::Bool(false),
        ParamType::Object => serde_json::Value::Object(serde_json::Map::new()),
        ParamType::Array => serde_json::Value::Array(Vec::new()),
        ParamType::Resource => serde_json::Value::String(descriptor.address.resource.clone()),
        ParamType::DateTime => serde_json::Value::String(SAMPLE_DATE_TIME.to_owned()),
    }
}

const SAMPLE_DATE_TIME: &str = "2026-01-01T09:00:00Z";

/// A value `schema` admits, when it says enough to make one.
fn sample_schema(schema: &serde_json::Value) -> Option<serde_json::Value> {
    if let Some(value) = schema.get("default").or_else(|| schema.get("const")) {
        return Some(value.clone());
    }
    let first = |key: &str| {
        schema
            .get(key)
            .and_then(serde_json::Value::as_array)
            .and_then(|values| values.first())
    };
    if let Some(value) = first("examples").or_else(|| first("enum")) {
        return Some(value.clone());
    }
    if let Some(variant) = first("oneOf").or_else(|| first("anyOf")) {
        return sample_schema(variant);
    }
    let ty = match schema.get("type")? {
        serde_json::Value::String(ty) => ty.as_str(),
        serde_json::Value::Array(types) => types
            .iter()
            .filter_map(serde_json::Value::as_str)
            .find(|ty| *ty != "null")?,
        _ => return None,
    };
    Some(match ty {
        "string" => sample_string(schema),
        "integer" => sample_integer(schema),
        "number" => sample_number(schema),
        "boolean" => serde_json::Value::Bool(false),
        "array" => {
            let items = schema.get("items").and_then(sample_schema);
            let least = schema
                .get("minItems")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let count = usize::try_from(least).unwrap_or(0);
            serde_json::Value::Array(items.map_or_else(Vec::new, |item| vec![item; count]))
        }
        "object" => {
            let properties = schema.get("properties");
            let required = schema
                .get("required")
                .and_then(serde_json::Value::as_array)
                .map_or(&[][..], Vec::as_slice);
            serde_json::Value::Object(
                required
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(|name| {
                        let value = properties
                            .and_then(|properties| properties.get(name))
                            .and_then(sample_schema)
                            .unwrap_or_else(|| serde_json::Value::String("sample".into()));
                        (name.to_owned(), value)
                    })
                    .collect(),
            )
        }
        "null" => serde_json::Value::Null,
        _ => return None,
    })
}

fn sample_string(schema: &serde_json::Value) -> serde_json::Value {
    let text = match schema.get("format").and_then(serde_json::Value::as_str) {
        Some("date-time") => SAMPLE_DATE_TIME.to_owned(),
        Some("date") => "2026-01-01".to_owned(),
        Some("uri" | "url") => "https://example.org/".to_owned(),
        _ => {
            let pattern = schema
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .and_then(sample_pattern);
            if let Some(text) = pattern {
                return serde_json::Value::String(text);
            }
            let length = |key: &str| {
                schema
                    .get(key)
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|length| usize::try_from(length).ok())
            };
            let least = length("minLength").unwrap_or(0);
            let most = length("maxLength").unwrap_or(usize::MAX);
            "s".repeat(if most < 6 { most } else { least.max(6) })
        }
    };
    serde_json::Value::String(text)
}

/// Text a pattern of one repeated character class admits (`^[0-9a-f]{64}$`,
/// a hash): the class's first character, as many times as it asks.
fn sample_pattern(pattern: &str) -> Option<String> {
    let class = pattern.strip_prefix("^[")?.strip_suffix('$')?;
    let (class, count) = class.split_once("]{")?;
    let count: usize = count.strip_suffix('}')?.parse().ok()?;
    let first = class
        .chars()
        .next()
        .filter(|first| !matches!(first, '^' | '\\'))?;
    Some(first.to_string().repeat(count))
}

fn sample_integer(schema: &serde_json::Value) -> serde_json::Value {
    let bound = |key: &str| schema.get(key).and_then(serde_json::Value::as_i64);
    let least = bound("minimum")
        .or_else(|| bound("exclusiveMinimum").and_then(|bound| bound.checked_add(1)))
        .unwrap_or(0);
    let most = bound("maximum")
        .or_else(|| bound("exclusiveMaximum").and_then(|bound| bound.checked_sub(1)))
        .unwrap_or(i64::MAX);
    // Zero where the bounds admit it, else the bound nearest it.
    serde_json::Value::from(if least > 0 {
        least
    } else if most < 0 {
        most
    } else {
        0
    })
}

fn sample_number(schema: &serde_json::Value) -> serde_json::Value {
    let bound = |key: &str| schema.get(key).and_then(serde_json::Value::as_f64);
    let least = bound("minimum")
        .or_else(|| bound("exclusiveMinimum").map(f64::next_up))
        .unwrap_or(0.0);
    let most = bound("maximum")
        .or_else(|| bound("exclusiveMaximum").map(f64::next_down))
        .unwrap_or(f64::MAX);
    serde_json::Value::from(if least > 0.0 {
        least
    } else if most < 0.0 {
        most
    } else {
        0.0
    })
}
