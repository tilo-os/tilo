//! A resource's public surface as data (`tilo_actions::surface`), read from
//! its declarations and stored shape. `tilo-app surface` writes it to the
//! app's `surface.toml` and `tilo-app check` compares it with the code, so a
//! rename or an incompatible storage change shows up in review, and it is
//! what the OS knows about an app that is not running.

pub use tilo_actions::surface::{FieldSurface, ParamSurface, Surface, VerbSurface};

use crate::resource::Resource;
use crate::shape::Shape;
use crate::vocabulary::{Declaration, vocabulary};

fn verb_of(decl: &Declaration) -> VerbSurface {
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
    surface_in(vocabulary::<R>(), R::VERSION)
}

/// [`surface_of`] the resource `vocabulary`, stored at `version`.
fn surface_in(vocabulary: &crate::Vocabulary, version: u32) -> Surface {
    let mut fields = Vec::new();
    if let Some(shape) = (vocabulary.shape)() {
        fields_of(shape, "", &mut fields);
    }
    for (path, schema) in (vocabulary.op_schemas)() {
        if let Some(field) = fields.iter_mut().find(|field| field.path == path) {
            field.ops = Some(schema);
        }
    }
    let mut verbs = Vec::new();
    for decl in (vocabulary.declarations)() {
        verbs.push(verb_of(&decl));
    }
    Surface {
        kind: vocabulary.kind.to_owned(),
        label: vocabulary.label.to_owned(),
        version,
        person: false,
        verbs,
        fields,
    }
}
