//! What the editor shows about a draft, derived from the draft and the live
//! catalogue: where the checker says it is wrong, which values each
//! parameter may be bound to, and how steps and bindings read.
//!
//! Everything here is pure, so the editor, its tests and the save effect
//! agree by construction: the save effect runs the same checker the editor
//! shows, and the binding choices are exactly what the checker accepts.

use std::collections::BTreeMap;

use serde_json::Value;
use tilo_automation::{
    ActionCatalog, AiTransform, Binding, BindingOption, CheckError, Condition, ResourceTarget,
    Selector, Shortcut, Step, StepKind, StepPath, binding_options, check,
};
use ui::tilo_actions::remote::CatalogueRow;
use ui::tilo_actions::{ActionParam, ParamType};

use crate::grant;
use crate::model::Draft;

/// The shortcut `draft` would save as, with the least grant it needs, as the
/// checker and the save effect see it.
#[must_use]
pub fn candidate(draft: &Draft, catalog: &ActionCatalog) -> Shortcut {
    let permissions = grant::least_privilege(&draft.program.0.steps, catalog);
    draft.shortcut(grant::rules(&permissions))
}

/// Every problem the checker finds in `draft`, keyed by the step path it
/// concerns (`""` for the shortcut itself, `output` for its result).
#[must_use]
pub fn problems(
    draft: &Draft,
    catalog: &ActionCatalog,
    saved: &[Shortcut],
) -> BTreeMap<String, Vec<String>> {
    let shortcut = candidate(draft, catalog);
    let resolver = |id: &str| saved.iter().find(|other| other.id == id).cloned();
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Err(errors) = check(&shortcut, catalog, &resolver) {
        for CheckError { path, kind } in errors {
            out.entry(path.as_str().to_owned())
                .or_default()
                .push(kind.to_string());
        }
    }
    out
}

/// One value a parameter may be bound to, as the editor offers it.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub binding: Binding,
    pub label: String,
}

/// The inputs, variables and earlier step outputs the checker would accept
/// for `param` of the step at `path`.
#[must_use]
pub fn choices(
    draft: &Draft,
    catalog: &ActionCatalog,
    path: &str,
    param: &ActionParam,
) -> Vec<Choice> {
    let shortcut = candidate(draft, catalog);
    binding_options(&shortcut, catalog, &step_path(path), param)
        .into_iter()
        .map(|BindingOption { binding, describes }| Choice {
            label: format!("{} · {describes}", describe(&binding)),
            binding,
        })
        .collect()
}

/// The checker's step path for an editor path such as `2.then.1`.
#[must_use]
pub fn step_path(path: &str) -> StepPath {
    let mut out = StepPath::root();
    for segment in path.split('.').filter(|segment| !segment.is_empty()) {
        out = match segment {
            "then" => out.then_branch(),
            "else" => out.else_branch(),
            "each" => out.each(None),
            index => index
                .parse()
                .map_or_else(|_| out.clone(), |index| out.child(index)),
        };
    }
    out
}

/// How a binding reads in the editor.
#[must_use]
pub fn describe(binding: &Binding) -> String {
    match binding {
        Binding::Literal(Value::String(text)) => format!("“{text}”"),
        Binding::Literal(value) => value.to_string(),
        Binding::Input(name) => format!("input {name}"),
        Binding::Var(name) => format!("variable {name}"),
        Binding::StepOutput { step, pointer } if pointer.is_empty() => {
            format!("result of {step}")
        }
        Binding::StepOutput { step, pointer } => {
            format!("{} of {step}", pointer.trim_start_matches('/'))
        }
        Binding::Template(text) => format!("text “{text}”"),
    }
}

/// The literal a typed text becomes for `param`: a number for a numeric
/// parameter, a boolean for a boolean one, parsed JSON for structured ones,
/// and the text itself otherwise.
#[must_use]
pub fn literal(text: &str, param: &ActionParam) -> Value {
    let trimmed = text.trim();
    match param.ty {
        ParamType::Integer => trimmed
            .parse::<i64>()
            .map_or_else(|_| Value::String(text.to_owned()), Value::from),
        ParamType::Number => trimmed
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| Value::String(text.to_owned()), Value::Number),
        ParamType::Boolean => match trimmed {
            "true" | "yes" | "on" => Value::Bool(true),
            "false" | "no" | "off" => Value::Bool(false),
            _ => Value::String(text.to_owned()),
        },
        ParamType::Object | ParamType::Array => {
            serde_json::from_str(trimmed).unwrap_or_else(|_| Value::String(text.to_owned()))
        }
        ParamType::Text | ParamType::DateTime | ParamType::Secret | ParamType::Resource => {
            Value::String(text.to_owned())
        }
    }
}

/// The text a literal is edited as.
#[must_use]
pub fn literal_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The catalogue row a step invokes, when it names one the person is
/// offered.
#[must_use]
pub fn row_for<'c>(step: &Step, catalog: &'c ActionCatalog) -> Option<&'c CatalogueRow> {
    let StepKind::Invoke {
        app_id,
        resource,
        action,
        ..
    } = &step.kind
    else {
        return None;
    };
    let target = match resource {
        Selector::Fixed(id) => ResourceTarget::Id(id),
        Selector::FirstOfKind(kind) => ResourceTarget::Kind(kind),
        Selector::FromBinding(_) => ResourceTarget::Any,
    };
    catalog.find(app_id, target, action)
}

/// The parameters of the action a step invokes; empty for other steps or
/// an action the catalogue no longer offers.
#[must_use]
pub fn params_of(step: &Step, catalog: &ActionCatalog) -> Vec<ActionParam> {
    row_for(step, catalog)
        .and_then(|row| row.action.as_ref())
        .map(|descriptor| descriptor.params.clone())
        .unwrap_or_default()
}

/// How a step reads as a heading.
#[must_use]
pub fn title(step: &Step, catalog: &ActionCatalog) -> String {
    match &step.kind {
        StepKind::Invoke { app_id, action, .. } => row_for(step, catalog)
            .and_then(|row| row.action.as_ref())
            .map_or_else(
                || format!("{} · {action}", app_label(app_id)),
                |descriptor| {
                    format!(
                        "{} · {} · {}",
                        app_label(app_id),
                        descriptor.resource_label,
                        descriptor.action_label
                    )
                },
            ),
        StepKind::If { cond, .. } => format!("If {}", describe_condition(cond)),
        StepKind::ForEach { items, item, .. } => {
            format!("Repeat with each {item} in {}", describe(items))
        }
        StepKind::SetVar { name, value } => format!("Set {name} to {}", describe(value)),
        StepKind::Ask { param } => format!("Ask for {}", param.description),
        StepKind::Ai { transform, input } => {
            let what = match transform {
                AiTransform::Summarise => "Summarise".to_owned(),
                AiTransform::Extract { .. } => "Extract from".to_owned(),
                AiTransform::Classify { choices } => {
                    format!("Classify as {} ·", choices.join(" / "))
                }
                AiTransform::Freeform { prompt } => format!("Ask the agent “{prompt}” about"),
            };
            format!("{what} {}", describe(input))
        }
    }
}

fn describe_condition(condition: &Condition) -> String {
    match condition {
        Condition::Truthy(binding) => format!("{} is set", describe(binding)),
        Condition::Equals { left, right } => {
            format!("{} is {}", describe(left), describe(right))
        }
        Condition::Not(inner) => format!("not ({})", describe_condition(inner)),
        Condition::All(all) => all
            .iter()
            .map(describe_condition)
            .collect::<Vec<_>>()
            .join(" and "),
        Condition::Any(any) => any
            .iter()
            .map(describe_condition)
            .collect::<Vec<_>>()
            .join(" or "),
    }
}

/// A readable name for an application id: `so.tilo.notes` → `Notes`.
#[must_use]
pub fn app_label(app_id: &str) -> String {
    let last = app_id.rsplit('.').next().unwrap_or(app_id);
    let mut words = last.split(['-', '_']).filter(|word| !word.is_empty());
    let mut out = String::new();
    for (index, word) in words.by_ref().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if out.is_empty() {
        app_id.to_owned()
    } else {
        out
    }
}

/// The catalogue the "Add action" picker offers, grouped by application
/// and then by resource kind. Only rows carrying an action are offered.
#[must_use]
pub fn picker(catalog: &ActionCatalog) -> Vec<AppGroup> {
    let mut apps: BTreeMap<String, BTreeMap<String, Vec<Offer>>> = BTreeMap::new();
    for row in catalog.rows() {
        let Some(descriptor) = &row.action else {
            continue;
        };
        apps.entry(row.app_id.clone())
            .or_default()
            .entry(row.resource.kind.clone())
            .or_default()
            .push(Offer {
                app_id: row.app_id.clone(),
                resource: row.resource.id.clone(),
                resource_label: row.resource.label.clone(),
                action: descriptor.address.action.clone(),
                label: descriptor.action_label.clone(),
                params: descriptor
                    .params
                    .iter()
                    .map(|param| {
                        format!(
                            "{}{}: {}",
                            param.name,
                            if param.required { "" } else { "?" },
                            param_type_name(param.ty)
                        )
                    })
                    .collect(),
            });
    }
    apps.into_iter()
        .map(|(app_id, kinds)| AppGroup {
            label: app_label(&app_id),
            app_id,
            kinds: kinds
                .into_iter()
                .map(|(kind, offers)| KindGroup { kind, offers })
                .collect(),
        })
        .collect()
}

/// One application's actions in the picker.
#[derive(Clone, Debug, PartialEq)]
pub struct AppGroup {
    pub app_id: String,
    pub label: String,
    pub kinds: Vec<KindGroup>,
}

/// One resource kind's actions in the picker.
#[derive(Clone, Debug, PartialEq)]
pub struct KindGroup {
    pub kind: String,
    pub offers: Vec<Offer>,
}

/// One action the picker offers, with its typed parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub app_id: String,
    pub resource: String,
    pub resource_label: String,
    pub action: String,
    pub label: String,
    /// `name: type` per parameter, `name?` when optional.
    pub params: Vec<String>,
}

impl Offer {
    /// The step that invokes this action on exactly this resource.
    #[must_use]
    pub fn step(&self) -> StepKind {
        StepKind::Invoke {
            app_id: self.app_id.clone(),
            resource: Selector::Fixed(self.resource.clone()),
            action: self.action.clone(),
            args: BTreeMap::new(),
        }
    }
}

/// The name a parameter type is shown under.
#[must_use]
pub const fn param_type_name(ty: ParamType) -> &'static str {
    match ty {
        ParamType::Text => "text",
        ParamType::Integer => "integer",
        ParamType::Number => "number",
        ParamType::Boolean => "boolean",
        ParamType::DateTime => "date-time",
        ParamType::Secret => "secret",
        ParamType::Resource => "resource",
        ParamType::Object => "object",
        ParamType::Array => "list",
    }
}

#[cfg(test)]
#[path = "editor_tests.rs"]
mod tests;
