//! A resource's public surface as data (`tilo_actions::surface`), read from
//! its declarations and stored shape. `tilo-app surface` writes it to the
//! app's `surface.toml` and `tilo-app check` compares it with the code, so a
//! rename or an incompatible storage change shows up in review, and it is
//! what the OS knows about an app that is not running.

pub use tilo_actions::surface::{FieldSurface, ParamSurface, Surface, VerbSurface};

use crate::resource::{ActionDecl, Resource};
use crate::shape::Shape;

fn verb_of<R: Resource>(decl: &ActionDecl<R>) -> VerbSurface {
    VerbSurface {
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
        verbs: R::declarations().iter().map(verb_of).collect(),
        fields,
    }
}
