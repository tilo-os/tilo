//! Whether a resource's published actions say everything they need.
//!
//! An agent builds a call from the schema an action publishes and nothing
//! else. A field the action reads but its schema never names (an edit's `id`,
//! a rich-text body's `rich`) is one the agent cannot know to send, and the
//! call fails with `missing field` however carefully it was written. This
//! check builds every published action from only what its schema requires,
//! with a value of each required argument's own shape, and reports each one
//! the action still finds missing.

use crate::Resource;

/// Every published action of `R`, as `state` publishes it, that needs an
/// argument its schema does not require: one line each, naming the action,
/// the shape of the call and what the action said was missing. Empty when
/// every schema says what its action reads.
#[must_use]
pub fn unstated_arguments<R: Resource>(state: &R::State) -> Vec<String> {
    let tree = crate::resource::publish::<R>(R::KIND, R::LABEL, state);
    let mut found = Vec::new();
    let mut specs: Vec<&tilo_actions::ActionSpec> = Vec::new();
    collect(&tree, &mut specs);
    let mut seen = std::collections::BTreeSet::new();
    for spec in specs {
        // `apply` and a group carry other actions, each checked here on its
        // own; their schemas leave each member's arguments to its action's.
        if [crate::APPLY, crate::GROUP].contains(&&*spec.id) || !seen.insert(spec.id.clone()) {
            continue;
        }
        for arguments in calls(spec) {
            if let Err(error) = crate::resource::build_action::<R>(&spec.id, &arguments, 0)
                && unstated(&error)
            {
                found.push(format!(
                    "{}: {} with {}",
                    spec.id,
                    error,
                    serde_json::Value::Object(arguments)
                ));
            }
        }
    }
    found
}

fn collect<'a>(spec: &'a tilo_actions::ResourceSpec, out: &mut Vec<&'a tilo_actions::ActionSpec>) {
    out.extend(spec.actions.iter());
    for child in &spec.children {
        collect(child, out);
    }
}

/// Whether a build error says the call lacked something, or had something
/// the action does not read.
fn unstated(error: &str) -> bool {
    error.contains("missing field") || error.contains("unknown field")
}

/// The calls of `spec` its schema describes: its required parameters, each
/// a value of its shape, and one call per alternative a parameter's schema
/// offers (each path and operation of a field edit).
fn calls(spec: &tilo_actions::ActionSpec) -> Vec<serde_json::Map<String, serde_json::Value>> {
    let mut calls = vec![serde_json::Map::new()];
    for param in spec.params().iter().filter(|param| param.required) {
        let values = match param.schema.as_deref() {
            Some(schema) => samples(schema),
            None => vec![sample_of_type(param.ty)],
        };
        let mut next = Vec::new();
        for call in &calls {
            for value in &values {
                let mut call = call.clone();
                call.insert(param.name.to_string(), value.clone());
                next.push(call);
            }
        }
        calls = next;
    }
    calls
}

fn sample_of_type(ty: tilo_actions::ParamType) -> serde_json::Value {
    use tilo_actions::ParamType;
    match ty {
        ParamType::Integer | ParamType::Number => 0.into(),
        ParamType::Boolean => false.into(),
        ParamType::Object => serde_json::json!({}),
        ParamType::Array => serde_json::json!([]),
        ParamType::DateTime => "2025-03-03T10:00:00Z".into(),
        ParamType::Text | ParamType::Resource | ParamType::Secret => "x".into(),
    }
}

/// Values `schema` describes: one for each of its alternatives, and for an
/// object whose `op` names operations, one per operation with what that
/// operation requires.
fn samples(schema: &serde_json::Value) -> Vec<serde_json::Value> {
    for key in ["anyOf", "oneOf"] {
        if let Some(alternatives) = schema.get(key).and_then(serde_json::Value::as_array)
            && !alternatives.is_empty()
            && schema.get("properties").is_none()
        {
            return alternatives.iter().flat_map(samples).collect();
        }
    }
    let ops: Vec<String> = schema
        .pointer("/properties/op/enum")
        .and_then(serde_json::Value::as_array)
        .map(|ops| {
            ops.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if ops.is_empty() {
        return vec![sample(schema, 0)];
    }
    // Each example the schema gives (a field edit's one per shape of path),
    // with each operation.
    let bases: Vec<serde_json::Value> = schema
        .get("examples")
        .and_then(serde_json::Value::as_array)
        .filter(|examples| !examples.is_empty())
        .cloned()
        .unwrap_or_else(|| vec![sample(schema, 0)]);
    let mut out = Vec::new();
    for base in &bases {
        for op in &ops {
            let mut value = base.clone();
            if let Some(object) = value.as_object_mut() {
                object.insert("op".into(), op.as_str().into());
                for key in required_by(schema, op) {
                    if !object.contains_key(&key) {
                        let property = schema
                            .pointer(&format!("/properties/{key}"))
                            .map_or_else(|| serde_json::json!({}), |property| sample(property, 1));
                        object.insert(key, property);
                    }
                }
            }
            out.push(value);
        }
    }
    out
}

/// What `schema` requires of a value whose `op` is `op`: its own required
/// properties and those of its `oneOf` alternative for `op`.
fn required_by(schema: &serde_json::Value, op: &str) -> Vec<String> {
    let mut required = names(schema.get("required"));
    for alternative in schema
        .get("oneOf")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        if alternative
            .pointer("/properties/op/const")
            .and_then(serde_json::Value::as_str)
            == Some(op)
        {
            required.extend(names(alternative.get("required")));
        }
    }
    required
}

fn names(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// One value `schema` describes, with only its required properties.
fn sample(schema: &serde_json::Value, depth: usize) -> serde_json::Value {
    if depth > 8 {
        return serde_json::Value::Null;
    }
    if let Some(example) = schema
        .get("examples")
        .and_then(serde_json::Value::as_array)
        .and_then(|examples| examples.first())
    {
        return example.clone();
    }
    if let Some(value) = schema.get("const").or_else(|| schema.get("default")) {
        return value.clone();
    }
    if let Some(first) = schema
        .get("enum")
        .and_then(serde_json::Value::as_array)
        .and_then(|options| options.first())
    {
        return first.clone();
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if schema.get("properties").is_none()
            && let Some(first) = schema
                .get(key)
                .and_then(serde_json::Value::as_array)
                .and_then(|alternatives| alternatives.first())
        {
            return sample(first, depth.saturating_add(1));
        }
    }
    let ty = match schema.get("type") {
        Some(serde_json::Value::String(ty)) => ty.as_str(),
        Some(serde_json::Value::Array(types)) => types
            .iter()
            .filter_map(serde_json::Value::as_str)
            .find(|ty| *ty != "null")
            .unwrap_or("null"),
        _ if schema.get("properties").is_some() => "object",
        _ => "null",
    };
    match ty {
        "string" => match schema.get("format").and_then(serde_json::Value::as_str) {
            Some("date-time") => "2025-03-03T10:00:00Z".into(),
            _ => "x".into(),
        },
        "integer" | "number" => schema.get("minimum").cloned().unwrap_or_else(|| 0.into()),
        "boolean" => false.into(),
        "array" => {
            let prefix: Vec<serde_json::Value> = schema
                .get("prefixItems")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .map(|item| sample(item, depth.saturating_add(1)))
                .collect();
            serde_json::Value::Array(prefix)
        }
        "object" => {
            let mut object = serde_json::Map::new();
            for key in names(schema.get("required")) {
                let property = schema
                    .pointer(&format!("/properties/{key}"))
                    .map_or(serde_json::Value::Null, |property| {
                        sample(property, depth.saturating_add(1))
                    });
                object.insert(key, property);
            }
            serde_json::Value::Object(object)
        }
        _ => serde_json::Value::Null,
    }
}
