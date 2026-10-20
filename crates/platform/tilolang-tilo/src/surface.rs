//! The generated surface of the host's packages (implementation design 7.4).
//!
//! The host runs many tilolang apps under one Tilo app id. Receivers decide each change against
//! the surface file the signed op names, so the host generates one `surface.toml` at run time
//! from the packages' metadata: a resource per declared type (`tl.<app>.<Type>`), one per
//! package for its record-less actions (`tl.<app>`), and `tl.host`. The host registers the bytes
//! with `app_surface::register_package` before its first signed operation.

use serde::Serialize;
use serde_json::{Value as Json, json};
use tilo_actions::ActionKind;
use tilo_actions::surface::{FieldSurface, ParamSurface, Surface, VerbSurface};

use crate::action::{EDIT_FLAG_ACTION, EDIT_TEXT_ACTION};
use tilolang_runtime::meta::{ActionMeta, Meta, ParamMeta, TypeMeta};

use crate::meta::{MetaExt as _, action_kind};

/// The kind of the host's own resource.
pub const HOST_KIND: &str = "tl.host";
pub use crate::publish::COMMIT;

/// The storage version of every generated resource.
pub const VERSION: u32 = 1;

/// Split `text` at commas outside brackets and parentheses.
fn split_top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_u32;
    let mut start = 0;
    for (index, ch) in text.char_indices() {
        match ch {
            '<' | '(' => depth = depth.saturating_add(1),
            '>' | ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(text.get(start..index).unwrap_or_default().trim());
                start = index.saturating_add(1);
            }
            _ => {}
        }
    }
    parts.push(text.get(start..).unwrap_or_default().trim());
    parts
}

fn wrapped<'a>(ty: &'a str, wrapper: &str) -> Option<&'a str> {
    ty.strip_prefix(wrapper)?
        .strip_prefix('<')?
        .strip_suffix('>')
        .map(str::trim)
}

/// The JSON Schema of a value of the tilolang type `ty`, as the door takes it.
#[must_use]
pub fn schema_of_type(meta: &Meta, ty: &str) -> Json {
    let ty = ty.trim();
    if let Some(inner) = ty.strip_prefix('(').and_then(|rest| rest.strip_suffix(')')) {
        let items: Vec<Json> = split_top_level(inner)
            .into_iter()
            .map(|part| schema_of_type(meta, part))
            .collect();
        let count = items.len();
        return json!({"type": "array", "prefixItems": items, "minItems": count, "maxItems": count});
    }
    if let Some(inner) = wrapped(ty, "Option") {
        return json!({"anyOf": [schema_of_type(meta, inner), {"type": "null"}]});
    }
    if let Some(inner) = wrapped(ty, "List") {
        return json!({"type": "array", "items": schema_of_type(meta, inner)});
    }
    if let Some(inner) = wrapped(ty, "Set") {
        return json!({"type": "array", "uniqueItems": true, "items": schema_of_type(meta, inner)});
    }
    if let Some(inner) = wrapped(ty, "Collection") {
        return json!({"type": "array", "items": schema_of_type(meta, inner)});
    }
    if let Some(inner) = wrapped(ty, "Map") {
        return json!({"type": "object", "additionalProperties": schema_of_type(meta, inner)});
    }
    match ty {
        "Int" => json!({"type": "integer"}),
        "Float" => json!({"type": "number"}),
        "Bool" => json!({"type": "boolean"}),
        "Text" => json!({"type": "string"}),
        other => match meta.type_named(other) {
            Some(declared) => json!({
                "type": "string",
                "description": format!("the id of a {} resource", declared.name),
                "resource_kind": meta.kind_of(&declared.name),
            }),
            None => json!({}),
        },
    }
}

/// The parameters of an action beyond its subject resource, which the address carries.
#[must_use]
pub fn arguments<'a>(meta: &Meta, params: &'a [ParamMeta]) -> &'a [ParamMeta] {
    match (meta.subject_of(params), params.split_first()) {
        (Some(_), Some((_, rest))) => rest,
        _ => params,
    }
}

fn is_optional(ty: &str) -> bool {
    ty.trim().starts_with("Option<")
}

fn verb(meta: &Meta, action: &ActionMeta) -> VerbSurface {
    VerbSurface {
        id: action.id.clone(),
        label: action.name.clone(),
        description: (!action.doc.is_empty()).then(|| action.doc.clone()),
        kind: action_kind(action.class),
        destructive: false,
        incoming_message: false,
        params: arguments(meta, &action.params)
            .iter()
            .map(|param| ParamSurface {
                name: param.name.clone(),
                required: !is_optional(&param.ty),
                schema: schema_of_type(meta, &param.ty),
            })
            .collect(),
        writes: vec![RECORD_UNIT.to_owned()],
        field: None,
    }
}

/// The record unit, where an action writes unless it says otherwise.
pub const RECORD_UNIT: &str = tilo_actions::surface::RECORD_UNIT;

fn edit_verbs() -> Vec<VerbSurface> {
    let verb = |id: &str, label: &str, value: Json| VerbSurface {
        id: id.to_owned(),
        label: label.to_owned(),
        description: Some("A bound control changes the field it shows.".to_owned()),
        kind: ActionKind::Undoable,
        destructive: false,
        incoming_message: false,
        params: vec![
            ParamSurface {
                name: "field".to_owned(),
                required: true,
                schema: json!({"type": "string"}),
            },
            ParamSurface {
                name: "value".to_owned(),
                required: true,
                schema: value,
            },
        ],
        writes: vec![RECORD_UNIT.to_owned()],
        field: None,
    };
    vec![
        verb(
            EDIT_TEXT_ACTION,
            "Edit a text field",
            json!({"type": "string"}),
        ),
        verb(
            EDIT_FLAG_ACTION,
            "Set a flag field",
            json!({"type": "boolean"}),
        ),
    ]
}

fn field_surfaces(
    meta: &Meta,
    ty: &TypeMeta,
    prefix: &str,
    stack: &mut Vec<String>,
    out: &mut Vec<FieldSurface>,
) {
    if stack.contains(&ty.id) {
        return;
    }
    stack.push(ty.id.clone());
    for field in &ty.fields {
        let path = format!("{prefix}{}", field.id);
        let declared = |name: &str| meta.type_named(name);
        let collection = field
            .ty
            .trim()
            .strip_prefix("Collection<")
            .and_then(|rest| rest.strip_suffix('>').map(str::trim));
        let (stored, nested) = match (collection, declared(field.ty.trim())) {
            (Some(item), _) => ("collection", declared(item).map(|item| (item, true))),
            (None, Some(record)) => ("record", Some((record, false))),
            (None, None) => ("value", None),
        };
        out.push(FieldSurface {
            path: path.clone(),
            owner: ty.name.to_lowercase(),
            title: field.name.clone(),
            stored: stored.to_owned(),
            ty: field.ty.clone(),
            readonly: false,
            unit: false,
            label: false,
            ops: None,
        });
        if let Some((inner, items)) = nested {
            let below = if items {
                format!("{path}.*.")
            } else {
                format!("{path}.")
            };
            field_surfaces(meta, inner, &below, stack, out);
        }
    }
    stack.pop();
}

/// The surface resources of one package.
#[must_use]
pub fn package_surfaces(meta: &Meta) -> Vec<Surface> {
    let mut surfaces = Vec::new();
    for ty in &meta.types {
        let mut verbs = edit_verbs();
        verbs.extend(
            meta.offered_actions()
                .filter(|action| {
                    meta.subject_of(&action.params)
                        .is_some_and(|subject| subject.id == ty.id)
                })
                .map(|action| verb(meta, action)),
        );
        let mut fields = Vec::new();
        field_surfaces(meta, ty, "", &mut Vec::new(), &mut fields);
        surfaces.push(Surface {
            kind: meta.kind_of(&ty.name),
            label: ty.name.clone(),
            version: VERSION,
            person: false,
            verbs,
            fields,
        });
    }
    let loose: Vec<VerbSurface> = meta
        .offered_actions()
        .filter(|action| meta.subject_of(&action.params).is_none())
        .map(|action| verb(meta, action))
        .collect();
    surfaces.push(Surface {
        kind: meta.package_kind(),
        label: meta.title.clone(),
        version: VERSION,
        person: false,
        verbs: loose,
        fields: Vec::new(),
    });
    surfaces
}

fn host_verb(id: &str, label: &str, kind: ActionKind, params: Vec<ParamSurface>) -> VerbSurface {
    VerbSurface {
        id: id.to_owned(),
        label: label.to_owned(),
        description: None,
        kind,
        destructive: false,
        incoming_message: false,
        params,
        writes: vec![RECORD_UNIT.to_owned()],
        field: None,
    }
}

/// The surface of the host's own resource: the commit of an action the runtime planned.
#[must_use]
pub fn host_surface() -> Surface {
    Surface {
        kind: HOST_KIND.to_owned(),
        label: "tilolang".to_owned(),
        version: VERSION,
        person: false,
        verbs: vec![host_verb(
            COMMIT,
            "Commit a planned action",
            ActionKind::Undoable,
            vec![ParamSurface {
                name: "ticket".to_owned(),
                required: true,
                schema: json!({"type": "integer"}),
            }],
        )],
        fields: Vec::new(),
    }
}

#[derive(Serialize)]
struct SurfaceFile<'a> {
    resources: &'a [Surface],
}

/// The `surface.toml` bytes of the host with these packages.
///
/// The host resource comes first: it is the host record's own kind.
///
/// # Errors
/// The surface does not serialize (a schema holding a value TOML cannot write).
pub fn surface_toml(packages: &[&Meta]) -> Result<Vec<u8>, toml::ser::Error> {
    let mut resources = vec![host_surface()];
    for meta in packages {
        resources.extend(package_surfaces(meta));
    }
    let text = toml::to_string(&SurfaceFile {
        resources: &resources,
    })?;
    let mut bytes = b"# Generated by the tilolang host from its packages' metadata.\n\n".to_vec();
    bytes.extend_from_slice(text.as_bytes());
    Ok(bytes)
}
