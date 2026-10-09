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
    pub verbs: Vec<VerbSurface>,
    /// Every stored field, depth first; empty for a resource without a
    /// model shape.
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
        }
    }
}

fn stored(shape: Shape) -> &'static str {
    match shape {
        Shape::Value => "value",
        Shape::Record(_) => "record",
        Shape::Collection(_) => "collection",
    }
}

fn fields_of(shape: Shape, prefix: &str, out: &mut Vec<FieldSurface>) {
    let (fields, item) = match shape {
        Shape::Record(fields) => (fields, false),
        Shape::Collection(item) => {
            fields_of(*item, &format!("{prefix}*."), out);
            return;
        }
        Shape::Value => return,
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
