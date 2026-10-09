//! Asking the agent to draft a shortcut from a description.
//!
//! The agent's free-form transform (`agent.transform#freeform`) is asked for
//! a value of the shortcut's shape, given the description and the actions
//! the person is offered. Nothing it returns is trusted: the answer is parsed
//! as a shortcut, given a fresh id, stripped of any grant, triggers or
//! consents, and checked against the catalogue. It is only ever offered to
//! the person to open as a draft; saving still goes through the grant review.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use tilo_automation::{
    AGENT_APP_ID, AGENT_FREEFORM_ACTION, AGENT_TRANSFORM_KIND, ActionCatalog, SHORTCUTS_APP_ID,
    Shortcut, check, kind_matches,
};
use ui::tilo_actions::remote::RemoteActionTarget;

use crate::editor;
use crate::grant;

/// Largest description a person may give.
pub const MAX_DESCRIPTION: usize = 2 * 1024;
/// Largest catalogue summary handed to the agent.
const MAX_SUMMARY_BYTES: usize = 96 * 1024;

/// The agent action that drafts, when the person is offered it.
#[must_use]
pub fn target(catalog: &ActionCatalog) -> Option<RemoteActionTarget> {
    catalog
        .offering(AGENT_APP_ID, AGENT_FREEFORM_ACTION)
        .find(|row| kind_matches(AGENT_TRANSFORM_KIND, &row.resource.kind))
        .and_then(ui::tilo_actions::remote::CatalogueRow::target)
}

/// The actions a draft may use, one JSON line each: where the action is,
/// its parameters and what it returns. The shortcut library and the agent's
/// own transforms are left out; AI steps name those.
#[must_use]
pub fn catalogue_summary(catalog: &ActionCatalog) -> String {
    let mut out = String::new();
    for row in catalog.rows() {
        let Some(descriptor) = &row.action else {
            continue;
        };
        if row.app_id == SHORTCUTS_APP_ID || row.app_id == AGENT_APP_ID {
            continue;
        }
        let line = json!({
            "action": {
                "app": row.app_id,
                "kind": row.resource.kind,
                "action": descriptor.address.action,
            },
            "resource": row.resource.id,
            "label": descriptor.action_label,
            "params": descriptor.params,
            "result": descriptor.contract.result,
        })
        .to_string();
        if out.len() + line.len() + 1 > MAX_SUMMARY_BYTES {
            break;
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The shape asked of the agent: a shortcut's name, inputs, steps and
/// result, in the shortcut JSON vocabulary.
#[must_use]
pub fn schema() -> Value {
    json!({
        "type": "object",
        "required": ["name", "steps"],
        "properties": {
            "name": {"type": "string", "minLength": 1},
            "inputs": {"type": "array", "items": {"type": "object"}},
            "steps": {"type": "array", "items": {"type": "object", "required": ["id", "step"]}},
            "output": {"type": "object"}
        }
    })
}

const INSTRUCTIONS: &str = "Write a Tilo shortcut that does what the person describes, using \
only the actions listed in the input (one JSON object per line). Answer with the shortcut as \
JSON: {\"name\", \"inputs\"?, \"steps\", \"output\"?}. Each step has a unique \"id\" (letters, \
digits, - or _) and a \"step\" kind: \"invoke\" with \"action\" (the listed action's \
{\"app\", \"kind\", \"action\"} as given), \"resource\": {\"fixed\": resource id} and \
\"args\" mapping parameter names to bindings; \"if\" with \
\"cond\": {\"truthy\": binding} or {\"equals\": {\"left\", \"right\"}}, \"then\" and \"else\" \
step lists; \"for_each\" with \"items\" binding, \"item\" variable name and \"body\"; \
\"set_var\" with \"name\" and \"value\"; \"ask\" with \"param\": {\"name\", \"ty\", \
\"required\", \"description\"}; \"ai\" with \"transform\" (\"summarise\", {\"classify\": \
{\"choices\"}}, {\"extract\": {\"schema\"}} or {\"freeform\": {\"prompt\"}}) and \"input\" \
binding. A binding is {\"literal\": value}, {\"input\": name}, {\"var\": name}, \
{\"step_output\": {\"step\": id, \"pointer\": \"/field\"}} or {\"template\": \"text with \
{{input.x}}, {{var.x}} or {{step.id/field}}\"}. Bind every required parameter with a value of \
its type. Do not invent actions. The person asked for:\n";

/// The arguments of the drafting invocation for `description`.
#[must_use]
pub fn arguments(description: &str, catalog: &ActionCatalog) -> Map<String, Value> {
    let description: String = description.chars().take(MAX_DESCRIPTION).collect();
    Map::from_iter([
        (
            "prompt".to_owned(),
            Value::String(format!("{INSTRUCTIONS}{description}")),
        ),
        (
            "input".to_owned(),
            Value::String(catalogue_summary(catalog)),
        ),
        ("schema".to_owned(), schema()),
    ])
}

/// A drafted shortcut, as the person is offered it.
#[derive(Clone, Debug, PartialEq)]
pub struct Drafted {
    pub shortcut: Shortcut,
    /// What the checker says is wrong with it, per step; empty when it
    /// checks.
    pub problems: Vec<String>,
}

/// Turn the agent's answer into a draft to offer: parse it, give it an id no
/// saved shortcut has, drop anything that would carry authority, and check
/// it with the least grant its steps need.
///
/// # Errors
///
/// Why the answer is not a shortcut at all.
pub fn adopt(value: Value, saved: &[Shortcut], catalog: &ActionCatalog) -> Result<Drafted, String> {
    let mut object = match value {
        Value::Object(object) => object,
        other => return Err(format!("the agent answered {other}, not a shortcut")),
    };
    object.insert("id".to_owned(), Value::String("drafted".to_owned()));
    for authority in ["grant", "triggers", "consents", "saved_by"] {
        object.remove(authority);
    }
    let mut shortcut: Shortcut = serde_json::from_value(Value::Object(object))
        .map_err(|error| format!("the agent's answer is not a shortcut: {error}"))?;
    shortcut.id = fresh_id(&shortcut.name, saved);
    shortcut.grant = grant::rules(&grant::least_privilege(&shortcut.steps, catalog));
    let resolver = |id: &str| saved.iter().find(|other| other.id == id).cloned();
    let problems = match check(&shortcut, catalog, &resolver) {
        Ok(()) => Vec::new(),
        Err(errors) => {
            let mut by_step: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for error in &errors {
                by_step
                    .entry(error.path.as_str().to_owned())
                    .or_default()
                    .push(editor::explain(error, &shortcut.steps, catalog));
            }
            editor::problem_lines(&shortcut.steps, &by_step)
        }
    };
    Ok(Drafted { shortcut, problems })
}

/// An id from `name` that no saved shortcut uses.
fn fresh_id(name: &str, saved: &[Shortcut]) -> String {
    let mut base: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    base.truncate(40);
    if base.is_empty() {
        "drafted".clone_into(&mut base);
    }
    let taken = |id: &str| saved.iter().any(|other| other.id == id);
    if !taken(&base) {
        return base;
    }
    (2_u32..=u32::MAX)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !taken(candidate))
        .unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_parsed_checked_and_stripped_of_authority() {
        let catalog = crate::samples::catalog();
        let saved: Vec<Shortcut> = vec![
            serde_json::from_value(json!({"id": "tidy-notes", "name": "Tidy", "steps": []}))
                .unwrap(),
        ];
        let answer = json!({
            "id": "tidy",
            "name": "Tidy notes",
            "steps": [
                {"id": "list", "step": "invoke", "action": {"app": "so.tilo.notes", "kind": "notes.folder", "action": "list"},
                 "resource": {"fixed": "notes.folder:inbox"}},
                {"id": "note", "step": "invoke", "action": {"app": "so.tilo.notes", "kind": "notes.folder", "action": "create"},
                 "resource": {"fixed": "notes.folder:inbox"},
                 "args": {"title": {"literal": "Digest"}}}
            ],
            "grant": [{"scope": "Any", "effect": "Grant", "verb": "Admin"}],
            "triggers": [{"trigger": "keyboard", "chord": "Super+X"}]
        });
        let drafted = adopt(answer, &saved, &catalog).unwrap();
        assert_eq!(drafted.shortcut.id, "tidy-notes-2", "never a saved id");
        assert!(
            drafted.shortcut.triggers.is_empty(),
            "no trigger carried in"
        );
        assert_eq!(
            drafted.shortcut.grant.len(),
            2,
            "the least grant, not the agent's"
        );
        assert!(
            drafted
                .problems
                .iter()
                .any(|problem| problem == "Step 2: Pin it needs a value"),
            "the checker's verdict comes with it: {:?}",
            drafted.problems
        );
        assert!(adopt(json!("no"), &saved, &catalog).is_err());
        assert!(adopt(json!({"name": "x"}), &saved, &catalog).is_err());
    }

    #[test]
    fn drafting_is_offered_only_with_the_agent_transform() {
        let catalog = crate::samples::catalog();
        assert!(target(&catalog).is_none(), "the sample offers no freeform");
        let summary = catalogue_summary(&catalog);
        assert!(summary.contains("\"action\":\"create\""));
        assert!(!summary.contains(SHORTCUTS_APP_ID));
        let args = arguments("tidy my notes", &catalog);
        assert!(args["prompt"].as_str().unwrap().ends_with("tidy my notes"));
        assert_eq!(args["schema"], schema());
    }
}
