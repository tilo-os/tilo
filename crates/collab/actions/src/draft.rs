//! Every invocation as a draft a person can read, change and confirm.
//!
//! Where a delegate's action asks first, the person is shown exactly what
//! will happen before it does. [`Draft::render`] derives that from the
//! action's inert descriptor and the arguments it would run with, so every
//! application's actions are draftable without code of their own: a field
//! per parameter (an object parameter's fields one level down), resource
//! references and attachments by label, secrets withheld. An action that
//! wants a richer preview names its layout with a contract tag
//! ([`MESSAGE_PREVIEW_TAG`]); a parameter shaped like a message (recipients
//! and a body) is previewed as one without it.
//!
//! What the person confirms is bound to the exact arguments
//! ([`arguments_digest`]): changing a field ([`Draft::apply_edits`]) makes
//! new arguments, and those are what run.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{ActionDescriptor, ParamType};

/// The error code an invocation is answered with while the person it is
/// made for has not yet confirmed it. The caller waits and asks again with
/// the same arguments; nothing has run.
pub const AWAITING_CONFIRMATION: &str = "action_awaiting_confirmation";

/// The error code an invocation is answered with when the person declined
/// it. What follows the code is the reason they gave, if any.
pub const DECLINED: &str = "action_declined";

/// Contract tag of an action whose draft is previewed as a message: who it
/// goes to, its subject, its body and what it carries.
pub const MESSAGE_PREVIEW_TAG: &str = "draft:message";

/// Longest text a draft keeps for one field; a longer value is shown cut
/// and cannot be edited in place.
pub const MAX_FIELD_CHARS: usize = 64 * 1024;

/// Most entries a list field names before saying how many more there are.
const MAX_LISTED: usize = 12;

/// How a draft is laid out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftLayout {
    /// One labelled field per parameter.
    #[default]
    Fields,
    /// A message: recipients, subject, body and attachments.
    Message,
}

/// The role a field plays in a [`DraftLayout::Message`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    To,
    Cc,
    Bcc,
    Subject,
    Body,
    Attachments,
}

impl MessageRole {
    fn of(name: &str) -> Option<Self> {
        Some(match name {
            "to" | "recipients" => Self::To,
            "cc" => Self::Cc,
            "bcc" => Self::Bcc,
            "subject" | "title" => Self::Subject,
            "body" | "text" | "message" => Self::Body,
            "attachments" | "files" => Self::Attachments,
            _ => return None,
        })
    }
}

/// What one field holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DraftValue {
    Text {
        text: String,
    },
    Number {
        text: String,
    },
    Flag {
        on: bool,
    },
    /// Several entries, each by its label: `3 attachments: a.pdf, b.md, c.png`.
    List {
        count: usize,
        labels: Vec<String>,
    },
    /// Another resource, by its label.
    Resource {
        label: String,
    },
    /// A secret: shown as withheld, never copied into the draft.
    Withheld,
}

impl DraftValue {
    /// The value in one line, as a person reads it (a list's labels joined,
    /// a flag as `yes`/`no`). Interfaces with their own wording for counts
    /// and flags build the line themselves.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Self::Text { text } | Self::Number { text } => text.clone(),
            Self::Flag { on } => if *on { "yes" } else { "no" }.to_owned(),
            Self::List { count, labels } => {
                let line = labels.join(", ");
                match count.checked_sub(labels.len()) {
                    Some(more) if more > 0 => format!("{line} +{more}"),
                    _ => line,
                }
            }
            Self::Resource { label } => label.clone(),
            Self::Withheld => "•••".to_owned(),
        }
    }
}

/// One field of a draft.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftField {
    /// Where the value sits in the arguments: the parameter, then the
    /// field of an object parameter.
    pub path: Vec<String>,
    /// The field as a person reads it.
    pub label: String,
    pub value: DraftValue,
    /// Whether a person may change it before confirming.
    #[serde(default)]
    pub editable: bool,
    /// Whether it is prose that wants several lines.
    #[serde(default)]
    pub multiline: bool,
    /// Its part in a message preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<MessageRole>,
}

/// An invocation as a person reviews it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub action_label: String,
    pub resource_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub layout: DraftLayout,
    pub fields: Vec<DraftField>,
}

/// One change a person made to a draft's field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftEdit {
    pub path: Vec<String>,
    pub text: String,
}

impl Draft {
    /// The draft of invoking `descriptor` with `arguments`.
    #[must_use]
    pub fn render(descriptor: &ActionDescriptor, arguments: &Map<String, Value>) -> Self {
        let mut fields = Vec::new();
        for param in &descriptor.params {
            let Some(value) = arguments.get(param.name.as_ref()) else {
                continue;
            };
            let path = vec![param.name.to_string()];
            match (param.ty, value) {
                (ParamType::Secret, _) => {
                    fields.push(field(path, &param.name, DraftValue::Withheld));
                }
                (ParamType::Object, Value::Object(object)) => {
                    let properties = param
                        .schema
                        .as_deref()
                        .and_then(|schema| schema.get("properties"))
                        .and_then(Value::as_object);
                    for (name, inner) in object {
                        let schema = properties.and_then(|properties| properties.get(name));
                        let mut path = path.clone();
                        path.push(name.clone());
                        if let Some(field) = value_field(path, name, inner, schema) {
                            fields.push(field);
                        }
                    }
                }
                (ty, value) => {
                    let schema = param.schema.as_deref();
                    let mut made = value_field(path, &param.name, value, schema);
                    if let Some(made) = made.as_mut()
                        && ty == ParamType::Resource
                    {
                        made.value = DraftValue::Resource {
                            label: made.value.line(),
                        };
                        made.editable = false;
                    }
                    fields.extend(made);
                }
            }
        }
        let message = descriptor
            .contract
            .tags
            .iter()
            .any(|tag| tag == MESSAGE_PREVIEW_TAG)
            || (fields
                .iter()
                .any(|field| field.role == Some(MessageRole::To))
                && fields
                    .iter()
                    .any(|field| field.role == Some(MessageRole::Body)));
        if !message {
            for field in &mut fields {
                field.role = None;
            }
        }
        Self {
            action_label: descriptor.action_label.clone(),
            resource_label: descriptor.resource_label.clone(),
            description: descriptor.description.clone(),
            layout: if message {
                DraftLayout::Message
            } else {
                DraftLayout::Fields
            },
            fields,
        }
    }

    /// The field with `role`, in a message preview.
    #[must_use]
    pub fn role(&self, role: MessageRole) -> Option<&DraftField> {
        self.fields.iter().find(|field| field.role == Some(role))
    }

    /// What the draft is about, in one line of its own values: a message's
    /// recipients and subject, otherwise its first short values. `None` for a
    /// draft with nothing to show.
    #[must_use]
    pub fn gist(&self) -> Option<String> {
        if self.layout == DraftLayout::Message {
            let parts: Vec<String> = [MessageRole::To, MessageRole::Subject]
                .into_iter()
                .filter_map(|role| self.role(role))
                .map(|field| field.value.line())
                .filter(|line| !line.is_empty())
                .collect();
            if !parts.is_empty() {
                return Some(parts.join(" · "));
            }
        }
        let parts: Vec<String> = self
            .fields
            .iter()
            .filter(|field| !field.multiline && field.value != DraftValue::Withheld)
            .map(|field| field.value.line())
            .filter(|line| !line.is_empty())
            .take(2)
            .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    /// `arguments` with the person's `edits` applied. Only fields this draft
    /// offers for editing change, each keeping its JSON type.
    ///
    /// # Errors
    /// An edit names a field the draft does not offer for editing, or its
    /// text is not a value of the field's type.
    pub fn apply_edits(
        &self,
        arguments: &Map<String, Value>,
        edits: &[DraftEdit],
    ) -> Result<Map<String, Value>, String> {
        let mut edited = arguments.clone();
        for edit in edits {
            set_path(&mut edited, &edit.path, self.edit_value(edit)?)?;
        }
        Ok(edited)
    }

    /// Whether every one of `edits` changes a field this draft offers for
    /// editing to a value of its type.
    ///
    /// # Errors
    /// The first edit that does not.
    pub fn check_edits(&self, edits: &[DraftEdit]) -> Result<(), String> {
        edits
            .iter()
            .try_for_each(|edit| self.edit_value(edit).map(drop))
    }

    fn edit_value(&self, edit: &DraftEdit) -> Result<Value, String> {
        let field = self
            .fields
            .iter()
            .find(|field| field.path == edit.path && field.editable)
            .ok_or_else(|| format!("{} cannot be changed here", edit.path.join(".")))?;
        if edit.text.chars().count() > MAX_FIELD_CHARS {
            return Err(format!("{} is too long", field.label));
        }
        Ok(match &field.value {
            DraftValue::Text { .. } => Value::String(edit.text.clone()),
            DraftValue::Number { .. } => {
                let number: serde_json::Number = edit
                    .text
                    .trim()
                    .parse()
                    .map_err(|_| format!("{} needs a number", field.label))?;
                Value::Number(number)
            }
            DraftValue::Flag { .. } => match edit.text.trim() {
                "true" | "yes" => Value::Bool(true),
                "false" | "no" => Value::Bool(false),
                _ => return Err(format!("{} needs yes or no", field.label)),
            },
            _ => return Err(format!("{} cannot be changed here", field.label)),
        })
    }

    /// The arguments this draft shows: each text, number and flag field set
    /// at its path. `None` when a field shows its value only in part (a
    /// secret, a list, a resource, a text cut short). A draft also leaves
    /// out nulls and empty texts, so what this returns is only a candidate:
    /// act on it only when it digests ([`arguments_digest`]) to the digest
    /// the draft was confirmed for.
    #[must_use]
    pub fn arguments(&self) -> Option<Map<String, Value>> {
        let mut arguments = Map::new();
        for field in &self.fields {
            let value = match &field.value {
                DraftValue::Text { text } => Value::String(text.clone()),
                DraftValue::Number { text } => Value::Number(text.parse().ok()?),
                DraftValue::Flag { on } => Value::Bool(*on),
                DraftValue::List { .. } | DraftValue::Resource { .. } | DraftValue::Withheld => {
                    return None;
                }
            };
            let (last, parents) = field.path.split_last()?;
            let mut object = &mut arguments;
            for parent in parents {
                object = object
                    .entry(parent.clone())
                    .or_insert_with(|| Value::Object(Map::new()))
                    .as_object_mut()?;
            }
            object.insert(last.clone(), value);
        }
        Some(arguments)
    }

    /// The draft as `edits` change it: what the person confirmed, shown
    /// back to them. Edits this draft does not offer are left out.
    #[must_use]
    pub fn edited(&self, edits: &[DraftEdit]) -> Self {
        let mut draft = self.clone();
        for edit in edits {
            let Ok(value) = self.edit_value(edit) else {
                continue;
            };
            if let Some(field) = draft
                .fields
                .iter_mut()
                .find(|field| field.path == edit.path)
            {
                field.value = match value {
                    Value::Bool(on) => DraftValue::Flag { on },
                    Value::Number(number) => DraftValue::Number {
                        text: number.to_string(),
                    },
                    _ => DraftValue::Text {
                        text: edit.text.clone(),
                    },
                };
            }
        }
        draft
    }
}

/// The digest binding a confirmation to the exact arguments it was given
/// for. Keys are ordered, so the same arguments always digest the same.
#[must_use]
pub fn arguments_digest(arguments: &Map<String, Value>) -> [u8; 32] {
    let canonical = canonical(&Value::Object(arguments.clone()));
    *blake3::Hasher::new_derive_key("Tilo draft arguments v1")
        .update(canonical.to_string().as_bytes())
        .finalize()
        .as_bytes()
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let sorted: std::collections::BTreeMap<&String, Value> = object
                .iter()
                .map(|(key, value)| (key, canonical(value)))
                .collect();
            Value::Object(
                sorted
                    .into_iter()
                    .map(|(key, value)| (key.clone(), value))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

fn set_path(
    arguments: &mut Map<String, Value>,
    path: &[String],
    value: Value,
) -> Result<(), String> {
    let Some((last, parents)) = path.split_last() else {
        return Err("an edit names no field".into());
    };
    let mut object = arguments;
    for parent in parents {
        object = object
            .get_mut(parent)
            .and_then(Value::as_object_mut)
            .ok_or_else(|| format!("{} is not in the draft", path.join(".")))?;
    }
    object.insert(last.clone(), value);
    Ok(())
}

fn field(path: Vec<String>, name: &str, value: DraftValue) -> DraftField {
    let role = path.last().and_then(|last| MessageRole::of(last));
    DraftField {
        path,
        label: humanize(name),
        value,
        editable: false,
        multiline: false,
        role,
    }
}

/// The field for one value; `None` for nothing worth showing (null, an
/// empty text or list).
fn value_field(
    path: Vec<String>,
    name: &str,
    value: &Value,
    schema: Option<&Value>,
) -> Option<DraftField> {
    let label = schema
        .and_then(|schema| schema.get("title"))
        .and_then(Value::as_str)
        .map_or_else(|| humanize(name), str::to_owned);
    let mut made = field(path, name, DraftValue::Withheld);
    made.label = label;
    if schema
        .and_then(|schema| schema.get("format"))
        .is_some_and(|format| format == "password")
    {
        return Some(made);
    }
    match value {
        Value::Null => return None,
        Value::String(text) if text.trim().is_empty() => return None,
        Value::String(text) => {
            let long = text.chars().count() > MAX_FIELD_CHARS;
            let shown: String = text.chars().take(MAX_FIELD_CHARS).collect();
            made.multiline = text.contains('\n')
                || text.chars().count() > 120
                || made.role == Some(MessageRole::Body);
            made.editable = !long;
            made.value = DraftValue::Text { text: shown };
        }
        Value::Number(number) => {
            made.editable = true;
            made.value = DraftValue::Number {
                text: number.to_string(),
            };
        }
        Value::Bool(on) => {
            made.editable = true;
            made.value = DraftValue::Flag { on: *on };
        }
        Value::Array(items) if items.is_empty() => return None,
        Value::Array(items) => {
            made.value = DraftValue::List {
                count: items.len(),
                labels: items.iter().take(MAX_LISTED).map(entry_label).collect(),
            };
        }
        Value::Object(object) => {
            made.value = DraftValue::Resource {
                label: object_label(object).unwrap_or_else(|| humanize(name)),
            };
        }
    }
    Some(made)
}

/// The keys an entry or referenced resource is named by, best first.
const LABEL_KEYS: [&str; 8] = [
    "label",
    "name",
    "title",
    "filename",
    "file_name",
    "path",
    "address",
    "id",
];

fn object_label(object: &Map<String, Value>) -> Option<String> {
    LABEL_KEYS.iter().find_map(|key| {
        let text = object.get(*key)?.as_str()?;
        let text = if *key == "path" {
            text.rsplit('/').next().unwrap_or(text)
        } else {
            text
        };
        (!text.is_empty()).then(|| text.to_owned())
    })
}

fn entry_label(value: &Value) -> String {
    match value {
        Value::String(text) => text.rsplit('/').next().unwrap_or(text).to_owned(),
        Value::Object(object) => object_label(object).unwrap_or_else(|| "…".to_owned()),
        other => other.to_string(),
    }
}

/// `in_reply_to` as `In reply to`.
fn humanize(name: &str) -> String {
    let spaced = name.replace(['_', '-'], " ");
    let mut chars = spaced.trim().chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
#[path = "draft_tests.rs"]
mod tests;
