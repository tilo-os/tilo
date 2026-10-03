//! The compact directory page `tilo_query` answers by default.
//!
//! A model pays for every byte of every page it reads, and again on every
//! later turn that still carries it. A full row repeats its resource id three
//! times and its contract twice; the compact page names each resource once
//! and each action by a one-line signature. Everything it leaves out stays one
//! `tilo_resolve` (or `detail: "full"`) away.
use base64::Engine as _;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use tilo_actions::remote::{CataloguePage, CatalogueRow};
use tilo_actions::{ActionDescriptor, ActionParam, Availability, ParamType, RiskLevel};

/// Marks a cursor that is not printable text and so travels as base64.
const BASE64_CURSOR: &str = "b64:";
/// Longest first sentence a compact signature carries whole. Apps keep
/// first sentences short; one past this is cut at a clause boundary.
const DESCRIPTION_CHARS: usize = 240;
/// Resource ids up to this long are shown as they are: `sheets.workbook`
/// reads better than a handle and costs about as much.
const SHORT_ID_BYTES: usize = 24;
/// Handles one session keeps; later ids are shown in full.
const MAX_HANDLES: usize = 4096;

/// An opaque continuation as a JSON string: printable text verbatim (an
/// app's cursor usually is), anything else as prefixed base64.
pub fn cursor_text(cursor: &[u8]) -> String {
    match std::str::from_utf8(cursor) {
        Ok(text)
            if !text.starts_with(BASE64_CURSOR)
                && text.bytes().all(|byte| byte.is_ascii_graphic()) =>
        {
            text.to_owned()
        }
        _ => format!(
            "{BASE64_CURSOR}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(cursor)
        ),
    }
}

/// The bytes [`cursor_text`] encoded.
pub fn cursor_bytes(text: &str) -> Result<Vec<u8>, String> {
    text.strip_prefix(BASE64_CURSOR).map_or_else(
        || Ok(text.as_bytes().to_vec()),
        |encoded| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| "cursor is not one this server issued".to_string())
        },
    )
}

/// The lowercase words of a needle.
pub fn needle_words(needle: &str) -> Vec<String> {
    needle
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Whether every word begins a word of the row's names: action id, label,
/// description and parameters, resource label and kind, app id.
pub fn matches_words(row: &CatalogueRow, words: &[String]) -> bool {
    word_score(row, words) == words.len()
}

/// How many of `words` begin a word of the row's names.
pub fn word_score(row: &CatalogueRow, words: &[String]) -> usize {
    let mut names = vec![
        row.resource.label.as_str(),
        row.resource.kind.as_str(),
        row.app_id.as_str(),
    ];
    if let Some(action) = &row.action {
        names.extend([action.address.action.as_str(), action.action_label.as_str()]);
        names.extend(action.description.as_deref());
        for param in &action.params {
            names.extend([param.name.as_ref(), param.description.as_ref()]);
        }
    }
    let tokens = names
        .iter()
        .flat_map(|name| name.split(|c: char| !c.is_alphanumeric()))
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    words
        .iter()
        .filter(|word| tokens.iter().any(|token| token.starts_with(word.as_str())))
        .count()
}

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

/// `page` with its rows grouped by resource and each action reduced to a
/// signature: `{"save-as": "Save document as (path?)"}`. With `handles`,
/// long resource ids are named by their session handles. With `describe`
/// false each action's description is left out (an `open` reply, read again
/// on every later turn); the label, parameters and risk stay: that is what a
/// call needs.
pub fn compact_described(
    page: &CataloguePage,
    handles: Option<&mut Handles>,
    describe: bool,
) -> Value {
    compact_first_described(page, handles, if describe { usize::MAX } else { 0 })
}

/// [`compact_described`] describing only the first `described` rows: the ones a
/// needle matched. The rest of a matched resource's actions follow as bare
/// signatures, so finding one action shows the others without paying for
/// every description again.
pub fn compact_first_described(
    page: &CataloguePage,
    mut handles: Option<&mut Handles>,
    described: usize,
) -> Value {
    /// One resource of the page: its head and its actions, in page order.
    struct Group {
        id: String,
        head: Map<String, Value>,
        actions: Map<String, Value>,
    }
    let mut groups: Vec<Group> = Vec::new();
    for (index, row) in page.rows.iter().enumerate() {
        if !groups.iter().any(|group| group.id == row.resource.id) {
            groups.push(Group {
                id: row.resource.id.clone(),
                head: resource_head(row, handles.as_deref_mut()),
                actions: Map::new(),
            });
        }
        let Some(group) = groups.iter_mut().find(|group| group.id == row.resource.id) else {
            continue;
        };
        if row.has_children {
            group.head.insert("children".into(), Value::Bool(true));
        }
        if let Some(action) = &row.action {
            group.actions.insert(
                action.address.action.clone(),
                signature(action, index < described).into(),
            );
        }
    }
    let resources = groups
        .into_iter()
        .map(|mut group| {
            if !group.actions.is_empty() {
                group
                    .head
                    .insert("actions".into(), Value::Object(group.actions));
            }
            Value::Object(group.head)
        })
        .collect::<Vec<_>>();
    let mut value =
        json!({"revision": page.revision, "resources": resources, "exhausted": page.exhausted});
    if let Some(fields) = value.as_object_mut() {
        if !page.next_cursor.is_empty() {
            fields.insert("next_cursor".into(), cursor_text(&page.next_cursor).into());
        }
        for (flag, set) in [
            ("stale_cursor", page.stale_cursor),
            ("budget_exhausted", page.budget_exhausted),
        ] {
            if set {
                fields.insert(flag.into(), Value::Bool(true));
            }
        }
    }
    value
}

fn resource_head(row: &CatalogueRow, handles: Option<&mut Handles>) -> Map<String, Value> {
    let mut head = Map::new();
    let name = handles.map_or_else(
        || row.resource.id.clone(),
        |handles| handles.short(&row.resource.id),
    );
    head.insert("resource".into(), name.into());
    head.insert("kind".into(), row.resource.kind.clone().into());
    head.insert("label".into(), row.resource.label.clone().into());
    head
}

/// One line an agent can act on: label, parameters, and only the contract
/// facts that change what it should do.
fn signature(action: &ActionDescriptor, describe: bool) -> String {
    let mut line = action.action_label.clone();
    if describe
        && let Some(description) = action
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
    {
        line.push_str(" — ");
        line.push_str(&first_sentence(description));
    }
    if !action.params.is_empty() {
        let params = action.params.iter().map(param).collect::<Vec<_>>();
        line.push_str(" (");
        line.push_str(&params.join(", "));
        line.push(')');
    }
    match action.contract.risk {
        RiskLevel::None | RiskLevel::Low => {}
        risk => {
            line.push_str(" [");
            line.push_str(risk_label(risk));
            line.push(']');
        }
    }
    if let Availability::Unavailable { reason, .. } = &action.contract.availability {
        line.push_str(" [unavailable: ");
        line.push_str(reason);
        line.push(']');
    }
    line
}

fn param(param: &ActionParam) -> String {
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
    if let Some(range) = param.schema.as_ref().and_then(range) {
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

const fn risk_label(risk: RiskLevel) -> &'static str {
    match risk {
        RiskLevel::None => "none",
        RiskLevel::Low => "low",
        RiskLevel::Destructive => "destructive",
        RiskLevel::Privileged => "privileged",
        RiskLevel::Sensitive => "sensitive",
    }
}

/// The first sentence of `text`, whole unless it is pathologically long;
/// then cut at the last clause boundary (`;`, `:`, `,`) or word before the
/// cap.
fn first_sentence(text: &str) -> String {
    let sentence = text
        .split_once(". ")
        .map_or(text, |(sentence, _)| sentence)
        .trim_end_matches('.');
    if sentence.chars().count() <= DESCRIPTION_CHARS {
        return sentence.to_owned();
    }
    let cut: String = sentence.chars().take(DESCRIPTION_CHARS).collect();
    let clause = cut
        .rfind([';', ':', ','])
        .filter(|at| *at >= DESCRIPTION_CHARS / 2);
    let head = clause.map_or_else(
        || cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head),
        |at| cut.get(..at).unwrap_or(cut.as_str()),
    );
    format!("{}…", head.trim_end())
}

/// The running app `name` means: its exact id, else the one id among
/// `running` whose last dotted part is `name` (`canvas` for
/// `so.tilo.canvas`), so an agent may use the name it sees in the window.
pub fn app_named(name: &str, running: &[&str]) -> Result<String, String> {
    if running.contains(&name) {
        return Ok(name.to_owned());
    }
    let mut by_tail = running
        .iter()
        .filter(|id| {
            id.rsplit('.')
                .next()
                .is_some_and(|tail| tail.eq_ignore_ascii_case(name))
        })
        .collect::<Vec<_>>();
    by_tail.dedup();
    match by_tail.as_slice() {
        [one] => Ok((**one).to_owned()),
        [] => Err(not_running(name, running)),
        many => Err(format!(
            "{name} names several running apps: {}; name one",
            many.iter().map(|id| **id).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Why `app_id` cannot be addressed: what is running instead, and how to
/// start it.
pub fn not_running(app_id: &str, running: &[&str]) -> String {
    let mut ids = running.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let listed = if ids.is_empty() {
        "no app is running".to_owned()
    } else {
        format!("running: {}", ids.join(", "))
    };
    format!(
        "{app_id} is not running ({listed}); start it with tilo_invoke {{workspace: true, resource: \"app.<app_id>\", action: \"open\"}}"
    )
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
