//! A resource kind's public surface as data: its verbs with their
//! parameters and kinds, and its fields with their schema. An app's
//! `surface.toml` holds it (written by `tilo-app surface` from the code), and
//! it is what a receiver without the app knows about the app's changes.
//!
//! The file keeps what receipt and the storage check need. The labels,
//! descriptions, field titles and owners the code reports are for people and
//! history; `tilo-app surface` leaves them out of the file, so they read as
//! empty here.

use serde::{Deserialize, Serialize};

use crate::PathStep;
use tilo_action_policy::ActionKind;

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes the field by reference"
)]
const fn is_false(value: &bool) -> bool {
    !*value
}

/// The record unit, as footprints and unit indexes name it.
pub const RECORD_UNIT: &str = "record";

/// The public surface of one resource kind. See the module docs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Surface {
    pub kind: String,
    pub label: String,
    /// The storage version (`#[resource(version = N)]`).
    pub version: u32,
    /// A record of the person's own, kept in their room rather than the
    /// room of the window showing it: one the app opens beside its own
    /// record (its `ResourceApp::records`). Only the kinds an app's surface
    /// marks so may it keep in, or read from, the person's room.
    #[serde(default, skip_serializing_if = "is_false")]
    pub person: bool,
    /// Absent from a surface file that lists none.
    #[serde(default)]
    pub verbs: Vec<VerbSurface>,
    /// Every stored field, depth first; empty for a resource without a
    /// model shape.
    #[serde(default)]
    pub fields: Vec<FieldSurface>,
}

/// One verb.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerbSurface {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub kind: ActionKind,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub destructive: bool,
    /// It records a message someone received, so an incoming-message
    /// trigger fires on its changes
    /// ([`INCOMING_MESSAGE_TAG`](crate::INCOMING_MESSAGE_TAG)).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub incoming_message: bool,
    /// Left out of a surface file when it takes none.
    #[serde(default)]
    pub params: Vec<ParamSurface>,
    /// The units it writes (`tilo_resource::footprint`), when more than the
    /// record unit.
    #[serde(default = "record_unit", skip_serializing_if = "only_record")]
    pub writes: Vec<String>,
    /// The field it sits under in the tree (`#[action(field = "...")]`): a
    /// comment verb under the comments it writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

fn stored_value() -> String {
    STORED_VALUE.to_owned()
}

/// How a field holding a plain value is stored.
pub const STORED_VALUE: &str = "value";

fn record_unit() -> Vec<String> {
    vec![RECORD_UNIT.to_owned()]
}

fn only_record(units: &[String]) -> bool {
    units == [RECORD_UNIT]
}

/// One parameter of a verb.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamSurface {
    pub name: String,
    pub required: bool,
    /// Its JSON Schema, from its type.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub schema: serde_json::Value,
}

/// Whether `ty`, a field's type as written (`Outbox<Upload>`,
/// `tilo_resource::Outbox<Removal>`), is an outbox: what a change queues
/// there reaches outside the record once delivered.
#[must_use]
pub fn is_outbox_type(ty: &str) -> bool {
    let head = ty.split('<').next().unwrap_or_default().trim();
    head.rsplit("::").next() == Some("Outbox") && ty.contains('<')
}

/// Whether `ty`, a field's type as written (`Reactions`,
/// `ui::tilo_resource::Reactions`), is the framework's reactions: changed
/// only by the react and unreact field edits, which store their invoker
/// as the author, so a read-only item still takes them.
#[must_use]
pub fn is_reactions_type(ty: &str) -> bool {
    ty.trim().rsplit("::").next() == Some("Reactions")
}

/// One stored field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldSurface {
    /// Where it is: stored keys from the root, `*` for every item of a
    /// collection (`snippets.*.title`).
    pub path: String,
    /// The record type it belongs to, kebab case.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub owner: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// How it is stored: `value` (left out of a surface file), `record` or
    /// `collection`.
    #[serde(default = "stored_value")]
    pub stored: String,
    /// Its Rust type as written.
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub readonly: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unit: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub label: bool,
    /// The schema of the field's own operations, for a type with its own
    /// vocabulary (rich text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ops: Option<serde_json::Value>,
}

/// What a receiver without the app reads from its surface: each verb's kind
/// and tree field, which paths offer field edits, and which storage units
/// an action may write (`*` for any item).
impl Surface {
    /// The kind of the verb `action` and the field it sits under, or `None`
    /// when the app publishes no such verb.
    #[must_use]
    pub fn verb(&self, action: &str) -> Option<(ActionKind, Option<String>)> {
        self.verbs
            .iter()
            .find(|verb| verb.id == action)
            .map(|verb| (verb.kind, verb.field.clone()))
    }

    /// The stored field a path (from the record's root) reaches: the
    /// deepest one its steps name, items and keys as `*`.
    fn field_at(&self, path: &[PathStep]) -> Option<&FieldSurface> {
        let mut dotted = String::new();
        let mut found = None;
        for step in path {
            let part = match step {
                PathStep::Field(key) => key.as_str(),
                PathStep::Item(_) | PathStep::Key(_) | PathStep::Cell(..) => "*",
            };
            if !dotted.is_empty() {
                dotted.push('.');
            }
            dotted.push_str(part);
            if let Some(field) = self.fields.iter().find(|field| field.path == dotted) {
                found = Some(field);
            }
        }
        found
    }

    /// Whether the field at `path` offers edits: stored, and neither it nor
    /// a field above it read-only. Reactions are the exception: their only
    /// edits record who invoked them, so they forge nothing a read-only
    /// field protects, and the record refuses any other edit of them.
    #[must_use]
    pub fn editable(&self, path: &[PathStep]) -> bool {
        let Some(field) = self.field_at(path) else {
            return false;
        };
        if is_reactions_type(&field.ty) {
            return true;
        }
        !self.fields.iter().any(|candidate| {
            candidate.readonly
                && (field.path == candidate.path
                    || field.path.starts_with(&format!("{}.", candidate.path)))
        })
    }

    /// The storage unit a field edit at `path` writes: the field's unit, an
    /// item of a unit collection, else the record (the empty path).
    #[must_use]
    pub fn edit_unit(&self, path: &[PathStep]) -> Vec<PathStep> {
        let mut dotted = String::new();
        let mut unit = Vec::new();
        for (index, step) in path.iter().enumerate() {
            let PathStep::Field(key) = step else {
                if !dotted.is_empty() {
                    dotted.push('.');
                }
                dotted.push('*');
                continue;
            };
            if !dotted.is_empty() {
                dotted.push('.');
            }
            dotted.push_str(key);
            let Some(field) = self.fields.iter().find(|field| field.path == dotted) else {
                continue;
            };
            if field.unit {
                let through = if matches!(field.stored.as_str(), "collection" | "keyed") {
                    index.saturating_add(2)
                } else {
                    index.saturating_add(1)
                };
                unit = path.iter().take(through).cloned().collect();
            }
        }
        unit
    }

    /// The units the verb `action` may write, on the item `item` when it
    /// acts on one: the record (the empty path), its item's unit, and the
    /// units of the fields it declares, `*` standing for any item.
    #[must_use]
    pub fn footprint(&self, action: &str, item: Option<&str>) -> Vec<Vec<PathStep>> {
        let Some(verb) = self.verbs.iter().find(|verb| verb.id == action) else {
            return Vec::new();
        };
        verb.writes
            .iter()
            .map(|unit| {
                if unit == RECORD_UNIT {
                    return Vec::new();
                }
                match unit.strip_suffix(".*") {
                    Some(field) => vec![
                        PathStep::Field(field.to_owned()),
                        PathStep::Item(item.unwrap_or("*").to_owned()),
                    ],
                    None => vec![PathStep::Field(unit.clone())],
                }
            })
            .collect()
    }
}
