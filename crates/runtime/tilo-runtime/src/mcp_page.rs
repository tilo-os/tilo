//! The compact directory page `tilo_query` answers by default.
//!
//! A model pays for every byte of every page it reads, and again on every
//! later turn that still carries it. A full row repeats its resource id three
//! times and its contract twice; the compact page names each resource once
//! and each action by a one-line signature. Everything it leaves out stays one
//! `tilo_resolve` (or `detail: "full"`) away.
use base64::Engine as _;
use serde_json::{Map, Value, json};
use tilo_actions::remote::{CataloguePage, CatalogueRow};
use tilo_actions::{ActionDescriptor, ActionParam, Availability, ParamType, RiskLevel};

/// Marks a cursor that is not printable text and so travels as base64.
const BASE64_CURSOR: &str = "b64:";
/// Longest action description a compact signature carries.
const DESCRIPTION_CHARS: usize = 120;

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

/// Whether every word begins a word of the row's names: action id, label
/// and description, resource label and kind, app id.
pub fn matches_words(row: &CatalogueRow, words: &[String]) -> bool {
    let mut names = vec![
        row.resource.label.as_str(),
        row.resource.kind.as_str(),
        row.app_id.as_str(),
    ];
    if let Some(action) = &row.action {
        names.extend([action.address.action.as_str(), action.action_label.as_str()]);
        names.extend(action.description.as_deref());
    }
    let tokens = names
        .iter()
        .flat_map(|name| name.split(|c: char| !c.is_alphanumeric()))
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    words
        .iter()
        .all(|word| tokens.iter().any(|token| token.starts_with(word.as_str())))
}

/// `page` with its rows grouped by resource and each action reduced to a
/// signature: `{"save-as": "Save document as (path?)"}`.
pub fn compact_page(page: &CataloguePage) -> Value {
    /// One resource of the page: its head and its actions, in page order.
    struct Group {
        id: String,
        head: Map<String, Value>,
        actions: Map<String, Value>,
    }
    let mut groups: Vec<Group> = Vec::new();
    for row in &page.rows {
        if !groups.iter().any(|group| group.id == row.resource.id) {
            groups.push(Group {
                id: row.resource.id.clone(),
                head: resource_head(row),
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
            group
                .actions
                .insert(action.address.action.clone(), signature(action).into());
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

fn resource_head(row: &CatalogueRow) -> Map<String, Value> {
    let mut head = Map::new();
    head.insert("resource".into(), row.resource.id.clone().into());
    head.insert("kind".into(), row.resource.kind.clone().into());
    head.insert("label".into(), row.resource.label.clone().into());
    head
}

/// One line an agent can act on: label, parameters, and only the contract
/// facts that change what it should do.
fn signature(action: &ActionDescriptor) -> String {
    let mut line = action.action_label.clone();
    if let Some(description) = action
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
    if param.schema.is_some() && matches!(param.ty, ParamType::Object | ParamType::Array) {
        text.push_str(" (schema: tilo_resolve)");
    }
    text
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

/// The first sentence of `text`, cut at a word boundary if it is long.
fn first_sentence(text: &str) -> String {
    let sentence = text
        .split_once(". ")
        .map_or(text, |(sentence, _)| sentence)
        .trim_end_matches('.');
    if sentence.chars().count() <= DESCRIPTION_CHARS {
        return sentence.to_owned();
    }
    let cut: String = sentence.chars().take(DESCRIPTION_CHARS).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    format!("{cut}…")
}
