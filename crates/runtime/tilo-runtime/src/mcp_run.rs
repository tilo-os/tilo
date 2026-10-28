//! The run this session took, as a Shortcut can replay it.
//!
//! An agent that got something done through this bridge knows the steps it
//! took; nothing else does. History keeps a digest of each invocation's
//! arguments ([`tilo_actions`]'s invocation journal), so a run cannot be
//! rebuilt from it afterwards: the steps are recorded here, where they are
//! invoked.
//!
//! Two pieces, both in memory and neither persisted until the person asks
//! for it:
//!
//! - [`Recorder`] — an ordered, bounded log of the invocations of this
//!   session that succeeded: what was invoked, on which resource, with
//!   which arguments and what it answered. A secret parameter's value is
//!   never kept, only the fact that it was given.
//! - [`export`] — the recorded run as the draft of a Shortcut: page element
//!   refs become find-page-element steps whose answer binds the action that
//!   follows, argument values equal to a named input become [`Binding::Input`]
//!   bindings, what was only looked at is dropped, and the run ends on a
//!   check step that fails when the page no longer says what it said.
//!
//! The draft is opened through the Shortcuts app's own published action
//! ([`export::DRAFT_ACTION`]) and nothing else: this bridge builds data, and
//! Shortcuts decides what a draft may be.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tilo_actions::ActionKey;
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::remote::CatalogueRow;

#[path = "mcp_run/export.rs"]
pub mod export;

/// How many invocations one session keeps: an agent that took more than this
/// to finish is doing more than a Shortcut replays, and the oldest steps are
/// the exploration it no longer needs.
pub const MAX_RECORDED: usize = 256;
/// Longest text kept of any one recorded answer's fields. A reading of the
/// page can be pages long; what a replayed step needs is what changed.
pub const MAX_RECORDED_TEXT: usize = 2 * 1024;
/// How deep into an answer the compact form follows: objects and lists.
const MAX_RECORDED_DEPTH: usize = 4;
/// How many members a kept object or list holds.
const MAX_RECORDED_ITEMS: usize = 32;

/// One action that ran and succeeded, as the log keeps it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recorded {
    /// The app that published the action, when the session knew it.
    pub app: Option<String>,
    /// The provider (peer or workspace token) the action was invoked on: a
    /// replay addresses the same instance when it is still there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The resource's stable id, as invoked.
    pub resource: String,
    /// The resource's kind, from the catalogue row that offered it.
    pub kind: String,
    /// The action's own name.
    pub action: String,
    /// What the action is called, when the catalogue said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// What the action is able to do, as the host decides its need:
    /// `read`, `undoable` or `outside`. An export keeps what changes
    /// something and drops what only reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub need: Option<String>,
    /// The arguments, with every secret parameter's value replaced by
    /// [`SECRET_HELD`].
    pub arguments: BTreeMap<String, Value>,
    /// The parameters declared secret, which held a value this log does not.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
    /// The answer's own value, compacted: what the step produced, for a
    /// later step or the check at the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// The structured findings of the answer (`element`, `role`, `name`,
    /// `nth`, `frame`, `acted`, …), which a page action names what it
    /// reached in.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub data: BTreeMap<String, Value>,
    /// What the answer said it did, as the person reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The page's address after the action, when the Browser said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// What a secret parameter's value reads as in the log, and in a draft
/// before the person names the input that supplies it. Never a value.
pub const SECRET_HELD: &str = "\u{1}secret";

impl Recorded {
    /// The action, as a step names it: the app, the resource's kind and the
    /// action's own name.
    #[must_use]
    pub fn key(&self) -> Option<ActionKey> {
        Some(ActionKey::new(
            self.app.as_deref()?,
            self.kind.as_str(),
            self.action.as_str(),
        ))
    }

    /// Whether the action only reads: a step that changes nothing is not
    /// work a Shortcut replays, unless it is the run's last look.
    #[must_use]
    pub fn reads(&self) -> bool {
        self.need.as_deref() == Some("read")
    }

    /// What the action did, in words: the catalogue's label, else the
    /// message its answer gave, else its name.
    #[must_use]
    pub fn described(&self) -> String {
        self.label
            .clone()
            .or_else(|| self.message.clone())
            .unwrap_or_else(|| self.action.clone())
    }

    /// The element the Browser acted on, when the answer named one: the
    /// `acted` description a find accepts, else a find's own answer.
    #[must_use]
    pub fn element(&self) -> Option<Value> {
        if let Some(acted) = self.data.get("acted").and_then(Value::as_object) {
            // Exactly what `find` names an element by, so a step can find it
            // again after a reload, when no ref of the old page is left.
            return Some(Value::Object(
                ["role", "name", "nth", "frame"]
                    .into_iter()
                    .filter_map(|key| {
                        let field = acted.get(key)?;
                        (!field.is_null()).then(|| (key.to_owned(), field.clone()))
                    })
                    .collect(),
            ));
        }
        // A find answers the element itself, as `role`, `name`, `nth` and
        // the ref it found.
        let described: serde_json::Map<String, Value> = ["role", "name", "nth", "frame"]
            .into_iter()
            .filter_map(|key| Some((key.to_owned(), self.data.get(key)?.clone())))
            .collect();
        described
            .contains_key("role")
            .then_some(Value::Object(described))
    }
}

/// One session's log of what it did.
#[derive(Debug, Default)]
pub struct Recorder {
    entries: std::sync::Mutex<Vec<Recorded>>,
}

impl Recorder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Keep one invocation that succeeded, dropping the oldest when the log
    /// is full.
    pub fn record(&self, entry: Recorded) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.len() >= MAX_RECORDED {
            entries.remove(0);
        }
        entries.push(entry);
    }

    /// The invocations recorded so far, oldest first.
    #[must_use]
    pub fn entries(&self) -> Vec<Recorded> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// How many invocations are recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Whether nothing has been recorded: a run that invoked nothing has no
    /// Shortcut in it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What an invocation's arguments were, as the log keeps them: each secret
/// parameter's value replaced, everything else as it was finally sent.
#[must_use]
pub fn recorded_arguments(
    params: &[ActionParam],
    arguments: &serde_json::Map<String, Value>,
) -> (BTreeMap<String, Value>, Vec<String>) {
    let secret = |name: &str| {
        params
            .iter()
            .find(|param| param.name.as_ref() == name)
            .is_some_and(|param| param.ty == ParamType::Secret)
    };
    let mut kept = BTreeMap::new();
    let mut secrets = Vec::new();
    for (name, value) in arguments {
        if value.is_null() {
            continue;
        }
        if secret(name) {
            kept.insert(name.clone(), Value::String(SECRET_HELD.into()));
            secrets.push(name.clone());
        } else {
            kept.insert(name.clone(), compact(value));
        }
    }
    (kept, secrets)
}

/// A value as the log keeps it: strings cut to [`MAX_RECORDED_TEXT`], and
/// objects and lists to their first [`MAX_RECORDED_ITEMS`] members, a few
/// levels deep. Long text is kept with its length in a note, rather than
/// silently cut.
#[must_use]
pub fn compact(value: &Value) -> Value {
    compact_at(value, 0)
}

fn compact_at(value: &Value, depth: usize) -> Value {
    match value {
        Value::String(text) if text.chars().count() > MAX_RECORDED_TEXT => {
            let cut: String = text.chars().take(MAX_RECORDED_TEXT).collect();
            json!({"text": cut, "characters": text.chars().count(), "cut": true})
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .take(MAX_RECORDED_ITEMS)
                .map(|item| {
                    if depth >= MAX_RECORDED_DEPTH {
                        json!(null)
                    } else {
                        compact_at(item, depth.saturating_add(1))
                    }
                })
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(_, field)| !field.is_null())
                .take(MAX_RECORDED_ITEMS)
                .map(|(name, field)| {
                    let field = if depth >= MAX_RECORDED_DEPTH {
                        json!(null)
                    } else {
                        compact_at(field, depth.saturating_add(1))
                    };
                    (name.clone(), field)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The answer of one invocation, as the log reads it: the value it
/// produced, the fields that say what it reached, and the page's address.
#[derive(Debug, Default)]
pub struct Answer {
    pub value: Option<Value>,
    pub data: BTreeMap<String, Value>,
    pub message: Option<String>,
    pub url: Option<String>,
}

impl Answer {
    /// What an answer says, read from the update an invocation answers
    /// with. Only what it states is kept: an action that says nothing adds
    /// nothing.
    #[must_use]
    pub fn of(value: &Value) -> Self {
        let update = value.get("update").unwrap_or(&Value::Null);
        let output = update.get("output").unwrap_or(&Value::Null);
        let data: BTreeMap<String, Value> = output
            .get("data")
            .and_then(Value::as_object)
            .map(|data| {
                data.iter()
                    .map(|(name, field)| (name.clone(), compact(field)))
                    .collect()
            })
            .unwrap_or_default();
        let produced = output
            .get("value")
            .filter(|produced| produced.get("data").is_none())
            .cloned();
        let message = ["summary", "message"]
            .into_iter()
            .find_map(|key| {
                update
                    .get(key)
                    .or_else(|| output.get(key))
                    .and_then(Value::as_str)
            })
            .map(|message| message.chars().take(MAX_RECORDED_TEXT).collect());
        let url = data
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                output
                    .get("value")
                    .and_then(|produced| produced.get("url"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        Self {
            value: produced.map(|value| compact(&value)),
            data,
            message,
            url,
        }
    }
}

/// The invocation a session is recording, as the catalogue describes the
/// action that ran.
#[derive(Debug)]
pub struct Invoked {
    app: Option<String>,
    provider: Option<String>,
    resource: String,
    kind: String,
    action: String,
    label: Option<String>,
    /// What the action is able to do, as the host decides its need:
    /// `read`, `undoable` or `outside`.
    need: Option<String>,
    params: Vec<ActionParam>,
}

impl Invoked {
    /// The invocation of `action` on `resource`, before its answer. The app
    /// and the resource's kind come from the catalogue row the session finds
    /// for it ([`Self::described`]), else from what the invocation named.
    #[must_use]
    pub fn new(app: Option<&str>, provider: Option<&str>, resource: &str, action: &str) -> Self {
        Self {
            app: app.map(str::to_owned),
            provider: provider.map(str::to_owned),
            resource: resource.to_owned(),
            kind: String::new(),
            action: action.to_owned(),
            label: None,
            need: None,
            params: Vec::new(),
        }
    }

    /// The same invocation, described by the catalogue row `row` when the
    /// session found one: the kind of its resource, the action's label, what
    /// it does and the parameters it declared (whose secret ones keep no
    /// value). `fallback_kind` is the resource's kind when the row has none.
    #[must_use]
    pub fn described(mut self, row: Option<&CatalogueRow>, fallback_kind: &str) -> Self {
        if let Some(action) = row.and_then(|row| row.action.as_ref()) {
            let kind = if action.resource_kind.is_empty() {
                row.map(|row| row.resource.kind.as_str())
                    .unwrap_or_default()
            } else {
                action.resource_kind.as_str()
            };
            kind.clone_into(&mut self.kind);
            if self.label.is_none() {
                self.label = Some(action.action_label.clone()).filter(|label| !label.is_empty());
            }
            self.need = Some(action.contract.need().kind.to_string());
            self.params.clone_from(&action.params);
        }
        if self.kind.is_empty() {
            self.kind = match row.map(|row| row.resource.kind.as_str()) {
                Some(kind) if !kind.is_empty() => kind.to_owned(),
                _ => fallback_kind.to_owned(),
            };
        }
        self
    }

    /// The entry a succeeded invocation is recorded as: what ran, with its
    /// arguments as the log keeps them and the answer it gave.
    #[must_use]
    pub fn succeeded(
        self,
        arguments: &serde_json::Map<String, Value>,
        answer: &Answer,
    ) -> Recorded {
        let (kept, secrets) = recorded_arguments(&self.params, arguments);
        Recorded {
            app: self.app,
            provider: self.provider,
            resource: self.resource,
            kind: self.kind,
            action: self.action,
            label: self.label,
            need: self.need,
            arguments: kept,
            secrets,
            value: answer.value.clone(),
            data: answer.data.clone(),
            message: answer.message.clone(),
            url: answer.url.clone(),
        }
    }

    /// The resource's kind as the invocation itself names it: `<kind>:<id>`
    /// or `<kind>/<field>/<item>`, as every published resource is
    /// addressed.
    #[must_use]
    pub fn kind_named(resource: &str) -> String {
        let mut parts = resource.splitn(3, '/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(kind), Some(field), Some(_)) => format!("{kind}.{field}"),
            _ => resource.split(':').next().unwrap_or(resource).to_owned(),
        }
    }
}

#[cfg(test)]
#[path = "mcp_run/tests.rs"]
mod tests;
