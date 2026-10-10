//! A resource's public surface as data: its verbs with their parameters and
//! kinds, and its fields with their schema. `tilo-app surface` writes it to
//! the app's `surface.toml` and `tilo-app check` compares it with the code,
//! so a rename or an incompatible storage change shows up in review, and it
//! is what the OS knows about an app that is not running.

use serde::{Deserialize, Serialize};

use crate::action::ActionKind;
use crate::resource::{ActionDecl, Resource};
use crate::shape::Shape;

/// The public surface of one resource kind. See the module docs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Surface {
    pub kind: String,
    pub label: String,
    /// The storage version (`#[resource(version = N)]`).
    pub version: u32,
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
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub kind: ActionKind,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub destructive: bool,
    pub params: Vec<ParamSurface>,
    /// The units it writes ([`crate::footprint`]), when more than the
    /// record unit.
    #[serde(default = "record_unit", skip_serializing_if = "only_record")]
    pub writes: Vec<String>,
    /// The field it sits under in the tree (`#[action(field = "...")]`): a
    /// comment verb under the comments it writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

fn record_unit() -> Vec<String> {
    vec![crate::footprint::RECORD_UNIT.to_owned()]
}

fn only_record(units: &[String]) -> bool {
    units == [crate::footprint::RECORD_UNIT]
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

/// One stored field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldSurface {
    /// Where it is: stored keys from the root, `*` for every item of a
    /// collection (`snippets.*.title`).
    pub path: String,
    /// The record type it belongs to, kebab case.
    pub owner: String,
    pub title: String,
    /// How it is stored: `value`, `record` or `collection`.
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

impl VerbSurface {
    fn of<R: Resource>(decl: &ActionDecl<R>) -> Self {
        Self {
            id: decl.id.to_owned(),
            label: decl.label.clone(),
            description: decl.description.clone(),
            kind: decl.kind,
            destructive: decl.destructive,
            params: decl
                .params
                .iter()
                .map(|param| ParamSurface {
                    name: param.name.to_string(),
                    required: param.required,
                    schema: param
                        .schema
                        .as_deref()
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                })
                .collect(),
            writes: decl.writes.clone(),
            field: decl.field.map(str::to_owned),
        }
    }
}

fn stored(shape: Shape) -> &'static str {
    match shape {
        // A map is published as the value it is written as.
        Shape::Value | Shape::Map(_) => "value",
        Shape::Record(_) => "record",
        Shape::Collection(_) => "collection",
        Shape::Keyed(_) => "keyed",
    }
}

fn fields_of(shape: Shape, prefix: &str, out: &mut Vec<FieldSurface>) {
    let (fields, item) = match shape {
        Shape::Record(fields) => (fields, false),
        Shape::Collection(item) | Shape::Keyed(item) => {
            fields_of(*item, &format!("{prefix}*."), out);
            return;
        }
        Shape::Value | Shape::Map(_) => return,
    };
    let _ = item;
    for field in fields {
        let path = format!("{prefix}{}", field.key);
        out.push(FieldSurface {
            path: path.clone(),
            owner: field.owner.to_owned(),
            title: field.title.to_owned(),
            stored: stored(field.shape).to_owned(),
            ty: field.ty.to_owned(),
            readonly: field.readonly,
            unit: field.unit,
            label: field.label,
            ops: None,
        });
        fields_of(field.shape, &format!("{path}."), out);
    }
}

/// The surface of `R`, from its declarations and stored shape.
#[must_use]
pub fn surface_of<R: Resource>() -> Surface {
    let mut fields = Vec::new();
    if let Some(shape) = R::shape() {
        fields_of(shape, "", &mut fields);
    }
    for (path, schema) in R::op_schemas() {
        if let Some(field) = fields.iter_mut().find(|field| field.path == path) {
            field.ops = Some(schema);
        }
    }
    Surface {
        kind: R::KIND.to_owned(),
        label: R::LABEL.to_owned(),
        version: R::VERSION,
        verbs: R::declarations().iter().map(VerbSurface::of).collect(),
        fields,
    }
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
    fn field_at(&self, path: &[tilo_actions::PathStep]) -> Option<&FieldSurface> {
        let mut dotted = String::new();
        let mut found = None;
        for step in path {
            let part = match step {
                tilo_actions::PathStep::Field(key) => key.as_str(),
                tilo_actions::PathStep::Item(_)
                | tilo_actions::PathStep::Key(_)
                | tilo_actions::PathStep::Cell(..) => "*",
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
    /// a field above it read-only.
    #[must_use]
    pub fn editable(&self, path: &[tilo_actions::PathStep]) -> bool {
        let Some(field) = self.field_at(path) else {
            return false;
        };
        !self.fields.iter().any(|candidate| {
            candidate.readonly
                && (field.path == candidate.path
                    || field.path.starts_with(&format!("{}.", candidate.path)))
        })
    }

    /// The storage unit a field edit at `path` writes: the field's unit, an
    /// item of a unit collection, else the record (the empty path).
    #[must_use]
    pub fn edit_unit(&self, path: &[tilo_actions::PathStep]) -> Vec<tilo_actions::PathStep> {
        let mut dotted = String::new();
        let mut unit = Vec::new();
        for (index, step) in path.iter().enumerate() {
            let tilo_actions::PathStep::Field(key) = step else {
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
    pub fn footprint(&self, action: &str, item: Option<&str>) -> Vec<Vec<tilo_actions::PathStep>> {
        let Some(verb) = self.verbs.iter().find(|verb| verb.id == action) else {
            return Vec::new();
        };
        verb.writes
            .iter()
            .map(|unit| {
                if unit == crate::footprint::RECORD_UNIT {
                    return Vec::new();
                }
                match unit.strip_suffix(".*") {
                    Some(field) => vec![
                        tilo_actions::PathStep::Field(field.to_owned()),
                        tilo_actions::PathStep::Item(item.unwrap_or("*").to_owned()),
                    ],
                    None => vec![tilo_actions::PathStep::Field(unit.clone())],
                }
            })
            .collect()
    }
}
