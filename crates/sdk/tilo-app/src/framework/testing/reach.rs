//! Whether every action an app publishes is reachable through the API, with
//! arguments sampled from each action's own schema. A test tool only: it is
//! built with `test-support`, so no release binary carries it.

use super::{HeadlessApp, Host};
use crate::framework::app::ResourceApp;

/// Drive every `shares`th scenario `A` stages from the `share`th, and each
/// of the `long` ones, and ask each drive for the actions it could not
/// invoke ([`unreachable_actions`]) and those an agent is never shown
/// ([`tilo_actions::hidden_from_agents`]). A `long` scenario is walked by every
/// share, each asking its own part of what it publishes
/// ([`unreachable_actions_in_share`]); the shares together ask everything.
///
/// # Errors
/// Each scenario that did not open, or whose drive left published actions
/// unreachable, with those actions.
pub fn check_scenarios_reach_every_action<A: ResourceApp, H: Host>(
    share: usize,
    shares: usize,
    long: &[&str],
) -> Result<(), String> {
    let mut failures = Vec::new();
    for (index, scenario) in A::scenarios().iter().enumerate() {
        let long = long.contains(scenario);
        if !long && index.checked_rem(shares) != Some(share) {
            continue;
        }
        let mut app = match HeadlessApp::<A, H>::drive(scenario) {
            Ok(app) => app,
            Err(error) => {
                failures.push(format!("{scenario}: {error}"));
                continue;
            }
        };
        // An action on a resource disclosed only through a private view is
        // never offered to an agent: asked once per scenario, by the first
        // share of a long one.
        if !long || share == 0 {
            let hidden = tilo_actions::hidden_from_agents(&app.harness().resources());
            if !hidden.is_empty() {
                failures.push(format!(
                    "{scenario}: published actions an agent is never shown: {hidden:#?}"
                ));
            }
        }
        let unreachable = if long {
            unreachable_actions_in_share(&mut app, share, shares)
        } else {
            unreachable_actions(&mut app)
        };
        if !unreachable.is_empty() {
            failures.push(format!(
                "{scenario}: published actions a drive cannot invoke, whose call by an \
                 agent opens a dialog on the person's screen, or that an agent could press: \
                 {unreachable:#?}"
            ));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

/// Invoke every action `app` publishes, one after another, as a headless
/// drive's `invoke` steps do (`Driver::invoke_action`: the API's own
/// resolution, availability, confirmation and strict binding), each with
/// `sample_arguments` and confirmed: an app's own action first as an agent,
/// then, when the agent was refused, as the person at the screen. Answers the ones the API
/// could not address, as `resource action: why`: published yet reachable
/// only by clicking; the ones whose call by the agent opened a dialog or
/// sheet over the person's window, which an agent never does; and the drawn
/// controls an agent could press, which only the person at the screen does. An action an earlier one withdrew (its row was removed) is not
/// asked; one that is unavailable now, refuses the sample as arguments (a
/// payload only another answer can build, such as a review to restore), or
/// ran and failed at its job, was reached.
pub fn unreachable_actions<A: ResourceApp, H: Host>(app: &mut HeadlessApp<A, H>) -> Vec<String> {
    unreachable_actions_in_share(app, 0, 1)
}

/// [`unreachable_actions`] for one share of what `app` publishes: the
/// actions of every `shares`th resource in the order the API surveys them,
/// from the `share`th (below `shares`). A window publishing thousands of
/// actions (one set per row of a long list) is asked in shares, each its
/// own test running beside the others; the shares together ask every
/// action. A resource's actions stay in one share, so one that withdraws
/// its row spares the walk the rest of the row's, as in a whole walk.
pub fn unreachable_actions_in_share<A: ResourceApp, H: Host>(
    app: &mut HeadlessApp<A, H>,
    share: usize,
    shares: usize,
) -> Vec<String> {
    // The published tree costs a walk of the whole window, and a long list
    // publishes thousands of actions: the walk resolves each against a tree
    // it read earlier, and reads it again only where that tree decides
    // something. The API resolves every invocation afresh itself, so an
    // action the kept tree still names is asked whatever became of it: one
    // that answered with an error changed nothing to resolve against, and
    // the answers that depend on the tree (no such action, a confirmation
    // phrase) are checked against a fresh one. Only an action the kept tree
    // no longer names is left unasked, and that is decided on a tree read
    // since the last invocation ran (`ran`): what ran may have withdrawn
    // rows the walk has yet to ask, or published them again.
    let mut resources = app.harness().resources();
    let mut ran = false;
    let surveyed = tilo_actions::api::survey(&resources);
    let mut unreachable = Vec::new();
    let agent = tilo_actions::Actor::Member(tilo_actions::MemberId::agent("reach-agent", "ada"));
    // The dialogs on the screen since anything last ran, counted when asked.
    let mut shown = None;
    let mut resource_ordinals = std::collections::HashMap::new();
    let ours = surveyed.into_iter().filter(|descriptor| {
        let next = resource_ordinals.len();
        let ordinal = *resource_ordinals
            .entry(descriptor.address.resource.clone())
            .or_insert(next);
        ordinal.checked_rem(shares) == Some(share)
    });
    for descriptor in ours {
        let address = descriptor.address.clone();
        let arguments = sample_arguments(&descriptor);
        if ran && tilo_actions::api::resolve(&resources, &address).is_none() {
            resources = app.harness().resources();
            ran = false;
        }
        let Some(spec) = tilo_actions::api::resolve(&resources, &address) else {
            continue;
        };
        let phrase = confirmation_phrase(spec, &arguments);
        // A control the window draws is the person's pointer and keyboard:
        // an agent pressing it is refused, and the person's press is asked
        // below. The platform's own operations are asked of the person
        // alone.
        if tilo_actions::kind_is_a_drawn_control(&descriptor.resource_kind) {
            let approval = app.harness().approval_typing(phrase.as_deref());
            let pressed =
                app.harness()
                    .invoke_action_as(&agent, &address, &arguments, Some(&approval));
            // Refused as the person's own; or withdrawn by what ran before it.
            let refused = match &pressed {
                Err(tilo_actions::ApiError::Unavailable { code, .. }) => {
                    code == tilo_actions::Availability::PRIVATE_VIEW
                }
                Err(error) => error.code() == "unknown_action",
                Ok(_) => false,
            };
            if !refused {
                unreachable.push(format!(
                    "{} {}: an agent pressed a control the person's window draws: {pressed:?}",
                    address.resource, address.action
                ));
            }
        } else if !tilo_actions::kind_mirrors_a_control(&descriptor.resource_kind) {
            let before = *shown.get_or_insert_with(|| dialogs(app));
            let approval = app.harness().approval_typing(phrase.as_deref());
            if app
                .harness()
                .invoke_action_as(&agent, &address, &arguments, Some(&approval))
                .is_ok()
            {
                ran = true;
                let after = dialogs(app);
                shown = Some(after);
                if after > before {
                    unreachable.push(format!(
                        "{} {}: an agent's call opened a dialog on the person's screen",
                        address.resource, address.action
                    ));
                }
                continue;
            }
        }
        let approval = app.harness().approval_typing(phrase.as_deref());
        let answer = app
            .harness()
            .invoke_action(&address, &arguments, Some(&approval));
        match answer {
            // What it ran may have withdrawn rows the walk has yet to ask:
            // the tree is read again before it leaves one unasked.
            Ok(_) => {
                ran = true;
                shown = None;
            }
            // The invocation first takes the work still pending in the
            // window, as a real input event does: an earlier action's
            // withdrawal of this row (a file it closed) may land only then,
            // and so may the withdrawal by an action that ran since the tree
            // was read. A row no longer published was withdrawn, not left
            // without a handler.
            Err(error) if error.code() == "unknown_action" => {
                resources = app.harness().resources();
                ran = false;
                if tilo_actions::api::resolve(&resources, &address).is_some() {
                    unreachable.push(format!("{} {}: {error}", address.resource, address.action));
                }
            }
            Err(error) => {
                // Only an action the API no longer resolves continues the
                // walk; any other refusal is worth saying.
                tracing::warn!(%error, resource = %address.resource, action = %address.action, "an action could not be invoked");
            }
        }
    }
    unreachable
}

/// How many dialogs and sheets `app` shows now.
fn dialogs<A: ResourceApp, H: Host>(app: &mut HeadlessApp<A, H>) -> usize {
    app.harness()
        .a11y_nodes()
        .iter()
        .filter(|(_, node)| node.role() == ui_core::accesskit::Role::Dialog && !node.is_hidden())
        .count()
}

/// The confirmation phrase `spec` asks for `arguments`, if any.
fn confirmation_phrase(
    spec: &tilo_actions::ActionSpec,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    match spec.confirmation_for_json(arguments) {
        tilo_actions::ConfirmationPolicy::Phrase { phrase } => Some(phrase),
        tilo_actions::ConfirmationPolicy::None | tilo_actions::ConfirmationPolicy::Confirm => None,
    }
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
        // A choice the schema writes as JSON text is that number; any other
        // choice is the text itself.
        let number = numeric
            .then(|| {
                tilo_failure::ignored!(
                    "a choice that is not a JSON number is the text itself",
                    serde_json::from_str(choice).ok()
                )
            })
            .flatten();
        return number.unwrap_or_else(|| serde_json::Value::String(choice.to_string()));
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
