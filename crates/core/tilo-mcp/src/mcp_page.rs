//! The session's short names for long resource ids (handles), and the
//! readings of values and arguments an agent's calls are answered with.
use serde_json::Value;
use std::collections::HashMap;
use tilo_actions::remote::CatalogueRow;
use tilo_actions::{ActionParam, Availability, ParamType, RiskLevel};

/// Resource ids up to this long are shown as they are: `sheets.workbook`
/// reads better than a handle and costs about as much.
const SHORT_ID_BYTES: usize = 24;
/// Handles one session keeps; later ids are shown in full.
const MAX_HANDLES: usize = 4096;

/// Short names for one session's long resource ids. A bound UI resource's
/// id is about 130 characters, and an agent repeats it in every call it
/// makes and every page it reads; `r3` says the same.
#[derive(Clone, Default)]
pub struct Handles {
    by_id: HashMap<String, String>,
    by_handle: HashMap<String, String>,
}

impl Handles {
    /// The name a compact page shows for `id`: `id` itself when it is short
    /// or the session holds as many handles as it keeps.
    pub fn short(&mut self, id: &str) -> String {
        if id.len() <= SHORT_ID_BYTES {
            return id.to_owned();
        }
        if let Some(handle) = self.by_id.get(id) {
            return handle.clone();
        }
        if self.by_handle.len() >= MAX_HANDLES {
            return id.to_owned();
        }
        let handle = format!("r{}", self.by_handle.len().saturating_add(1));
        self.by_id.insert(id.to_owned(), handle.clone());
        self.by_handle.insert(handle.clone(), id.to_owned());
        handle
    }

    /// The resource id `name` stands for: a handle this session issued, or
    /// a full id as given.
    pub fn full(&self, name: &str) -> String {
        self.by_handle
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_owned())
    }

    /// The resource id `handle` stands for, when this session issued it.
    pub fn issued(&self, handle: &str) -> Option<&str> {
        self.by_handle.get(handle).map(String::as_str)
    }
}

pub fn param(param: &ActionParam) -> String {
    let mut text = param.name.to_string();
    if !param.required {
        text.push('?');
    }
    if !param.choices.is_empty() {
        let choices = param
            .choices
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        text.push(':');
        text.push_str(&choices.join("|"));
    } else if param.ty != ParamType::Text {
        text.push(':');
        text.push_str(param.ty.label());
    }
    if let Some(range) = param.schema.as_deref().and_then(range) {
        text.push(' ');
        text.push_str(&range);
    }
    if param.schema.is_some() && matches!(param.ty, ParamType::Object | ParamType::Array) {
        text.push('*');
    }
    text
}

/// A numeric schema's bounds as `1..64`, `1..` or `..64`: a caller that
/// guesses past one fails a whole call. Bounds that only restate an integer
/// type's limits (`0..4294967295`) say nothing a caller would reach.
fn range(schema: &Value) -> Option<String> {
    let bound = |key: &str| {
        schema.get(key).filter(|value| {
            value
                .as_f64()
                .is_some_and(|value| value.abs() < f64::from(i32::MAX))
        })
    };
    let (mut minimum, maximum) = (bound("minimum"), bound("maximum"));
    if maximum.is_none() && minimum.and_then(Value::as_u64) == Some(0) {
        minimum = None;
    }
    if minimum.is_none() && maximum.is_none() {
        return None;
    }
    let show = |value: Option<&Value>| value.map(ToString::to_string).unwrap_or_default();
    Some(format!("{}..{}", show(minimum), show(maximum)))
}

/// What an action's contract says an agent should know before calling it:
/// its risk when it is more than low, and why it is unavailable.
pub fn risk_note(contract: &tilo_actions::ActionContract) -> Option<String> {
    let mut notes = Vec::new();
    match contract.risk {
        RiskLevel::None | RiskLevel::Low => {}
        risk => notes.push(risk_label(risk).to_owned()),
    }
    if let Availability::Unavailable { reason, .. } = &contract.availability {
        notes.push(format!("unavailable: {reason}"));
    }
    (!notes.is_empty()).then(|| notes.join(", "))
}

const fn risk_label(risk: RiskLevel) -> &'static str {
    match risk {
        RiskLevel::None => "none",
        RiskLevel::Low => "low",
        RiskLevel::Destructive => "destructive",
        RiskLevel::Privileged => "privileged",
        RiskLevel::Sensitive => "sensitive",
    }
}

/// The resource `name` means among `rows` (a provider's resources): one
/// whose kind or label is `name`, when exactly one is. `None` when `name`
/// is no kind or label there, so it is taken as a resource id.
pub fn resource_named(name: &str, rows: &[CatalogueRow]) -> Option<Result<String, String>> {
    if rows.iter().any(|row| row.resource.id == name) {
        return None;
    }
    let mut found = Vec::<&CatalogueRow>::new();
    for row in rows
        .iter()
        .filter(|row| row.resource.kind == name || row.resource.label.eq_ignore_ascii_case(name))
    {
        if !found.iter().any(|seen| seen.resource.id == row.resource.id) {
            found.push(row);
        }
    }
    match found.as_slice() {
        [] => None,
        [one] => Some(Ok(one.resource.id.clone())),
        many => Some(Err(format!(
            "{name} names {} resources ({}); name one by its label",
            many.len(),
            many.iter()
                .map(|row| row.resource.label.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Whether `name` has the shape of a session handle (`r12`).
pub fn looks_like_handle(name: &str) -> bool {
    name.strip_prefix('r')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}
