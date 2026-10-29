//! The surface of a tilolang app, generated from its package's metadata.
//!
//! Receivers decide each change against the surface file the signed op names. A tilolang app's
//! surface has a resource per declared type (`tl.<app>.<Type>`), one for the package's
//! record-less actions (`tl.<app>`), and `tl.host`; the kinds of the packages it uses are
//! declared as `reads`. `tilolang build` writes it into the app's payload ([`crate::bundle`]),
//! and the host registers it with `app_surface::register_package` before its first signed
//! operation.

use serde::Serialize;
use serde_json::{Value as Json, json};
use tilo_actions::surface::{
    FieldSurface, FieldType, NamedType, ParamSurface, Surface, TypeCase, TypeField, TypeSet,
    VerbSurface,
};
use tilo_actions::{ActionKind, ActionParam, ParamType, Pick};
use tilolang_runtime::meta::Ty;

use crate::action::{CREATE_ACTION, EDIT_FLAG_ACTION, EDIT_TEXT_ACTION};
use tilolang_runtime::meta::{ActionMeta, Meta, ParamMeta, TypeMeta};

use crate::meta::{MetaExt as _, action_kind};

/// The kind of the host's own resource.
pub const HOST_KIND: &str = "tl.host";
pub use crate::publish::COMMIT;

/// The storage version of every generated resource.
pub const VERSION: u32 = 1;

/// The resource kind and type name of `ty` when it names a record type of another app than
/// `meta`'s: a type of a package it uses, written qualified (`so.tilo.lang.notes.Notebook`).
#[must_use]
pub fn other_apps_record(meta: &Meta, ty: &str) -> Option<(String, String)> {
    // optional-decode: a probe: a type that does not parse names no other app's record.
    match tilolang_runtime::meta::parse_ty(ty.trim(), &meta.app).ok()? {
        tilolang_runtime::meta::Ty::Record(key) if key.app != meta.app => {
            Some((format!("tl.{}.{}", key.app, key.name), key.name))
        }
        _ => None,
    }
}

/// The resource kind of the record type `key`: `tl.<app>.<Type>`.
fn record_kind(key: &tilolang_runtime::meta::TypeKey) -> String {
    format!("tl.{}.{}", key.app, key.name)
}

/// The cases of `meta`'s own enum `key` with how each reads, when every case is a plain one (no
/// payload): what a parameter of it chooses from. `None` for an enum of another package, whose
/// cases this one does not know, or one whose cases carry values.
fn plain_cases(
    meta: &Meta,
    key: &tilolang_runtime::meta::TypeKey,
) -> Option<Vec<(String, String)>> {
    if key.app != meta.app {
        return None;
    }
    let declared = meta
        .enums
        .iter()
        .find(|declared| declared.name == key.name)?;
    declared
        .variants
        .iter()
        .map(|variant| {
            variant.fields.is_empty().then(|| {
                let label = if variant.label.is_empty() {
                    variant.name.clone()
                } else {
                    variant.label.clone()
                };
                (variant.name.clone(), label)
            })
        })
        .collect()
}

/// The language-neutral type of a value of the tilolang type `ty` (as metadata writes it), the
/// enums of `meta` it names declared in `types`: what the generated surface publishes of a
/// field, as a native model's derive publishes its fields.
#[must_use]
pub fn field_type_of_type(meta: &Meta, ty: &str, types: &mut TypeSet) -> FieldType {
    // A loaded package's types all parse (`meta::unreadable_types`, checked where it is
    // loaded); one that does not is published as any value.
    // optional-decode: checked where the package is loaded
    match tilolang_runtime::meta::parse_ty(ty.trim(), &meta.app) {
        Ok(parsed) => field_type_of(meta, &parsed, types),
        Err(_) => FieldType::Any,
    }
}

fn field_type_of(meta: &Meta, ty: &Ty, types: &mut TypeSet) -> FieldType {
    let boxed = |inner: &Ty, types: &mut TypeSet| Box::new(field_type_of(meta, inner, types));
    match ty {
        Ty::Int => FieldType::Int,
        Ty::Float => FieldType::Float,
        Ty::Bool => FieldType::Bool,
        Ty::Text => FieldType::Text,
        Ty::RichText => FieldType::RichText,
        Ty::Date => FieldType::Date,
        Ty::Time => FieldType::Time,
        Ty::Identifier => FieldType::Identifier,
        Ty::Principal => FieldType::Principal,
        Ty::Record(key) => FieldType::Resource(record_kind(key)),
        Ty::Union(keys) => FieldType::Union(
            keys.iter()
                .map(|key| FieldType::Resource(record_kind(key)))
                .collect(),
        ),
        Ty::Enum(key) if key.app == meta.app => {
            let Some(declared) = meta.enums.iter().find(|declared| declared.name == key.name)
            else {
                return FieldType::Any;
            };
            types.named(&format!("{}.{}", key.app, key.name), &key.name, |types| {
                NamedType {
                    name: declared.name.clone(),
                    doc: declared.doc.clone(),
                    fields: Vec::new(),
                    cases: declared
                        .variants
                        .iter()
                        .map(|variant| TypeCase {
                            name: variant.name.clone(),
                            label: if variant.label.is_empty() {
                                tilo_actions::field_type::label_of(&variant.name)
                            } else {
                                variant.label.clone()
                            },
                            fields: variant
                                .fields
                                .iter()
                                .map(|field| TypeField {
                                    name: field.name.clone(),
                                    ty: field_type_of_type(meta, &field.ty, types),
                                    title: String::new(),
                                })
                                .collect(),
                        })
                        .collect(),
                }
            })
        }
        Ty::Option(inner) => FieldType::Option(boxed(inner, types)),
        Ty::List(inner) => FieldType::List(boxed(inner, types)),
        Ty::Set(inner) => FieldType::Set(boxed(inner, types)),
        Ty::Collection(inner) => FieldType::Collection(boxed(inner, types)),
        Ty::Map(inner) => FieldType::Map(boxed(inner, types)),
        _ => FieldType::Any,
    }
}

/// The JSON Schema of a value of the tilolang type `ty`, as the door takes it: a record as the
/// catalogue's reference to a resource of its kind ([`tilo_actions::picked_resource_schema`]),
/// a plain enum as the names of its cases, a principal as the workspace subject it is.
#[must_use]
pub fn schema_of_type(meta: &Meta, ty: &str) -> Json {
    // A loaded package's types all parse (`meta::unreadable_types`, checked where it is
    // loaded); one that does not takes any value.
    // optional-decode: checked where the package is loaded
    match tilolang_runtime::meta::parse_ty(ty.trim(), &meta.app) {
        Ok(parsed) => schema_of(meta, &parsed),
        Err(_) => json!({}),
    }
}

fn schema_of(meta: &Meta, ty: &Ty) -> Json {
    match ty {
        Ty::Int => json!({"type": "integer"}),
        Ty::Float => json!({"type": "number"}),
        Ty::Bool => json!({"type": "boolean"}),
        Ty::Text | Ty::RichText => json!({"type": "string"}),
        Ty::Date => json!({"type": "string", "format": "date"}),
        Ty::Time => json!({"type": "string", "format": "date-time"}),
        // An account of some service, as its URI (`mailto:jane@x.com`).
        Ty::Identifier => json!({"type": "string", "format": "uri"}),
        Ty::Principal => subject_schema(),
        Ty::Record(key) => tilo_actions::picked_resource_schema(&record_kind(key)),
        Ty::Enum(key) => match plain_cases(meta, key) {
            Some(cases) => {
                let names: Vec<&str> = cases.iter().map(|(name, _)| name.as_str()).collect();
                let labels: serde_json::Map<String, Json> = cases
                    .iter()
                    .map(|(name, label)| (name.clone(), Json::String(label.clone())))
                    .collect();
                json!({
                    "type": "string",
                    "enum": names,
                    tilo_actions::params::CHOICE_LABELS_ANNOTATION: labels,
                })
            }
            None => json!({"type": "object"}),
        },
        Ty::Tuple(items) => {
            let items: Vec<Json> = items.iter().map(|item| schema_of(meta, item)).collect();
            let count = items.len();
            json!({"type": "array", "prefixItems": items, "minItems": count, "maxItems": count})
        }
        Ty::Option(inner) => json!({"anyOf": [schema_of(meta, inner), {"type": "null"}]}),
        Ty::List(inner) | Ty::Collection(inner) => {
            json!({"type": "array", "items": schema_of(meta, inner)})
        }
        Ty::Set(inner) => {
            json!({"type": "array", "uniqueItems": true, "items": schema_of(meta, inner)})
        }
        Ty::Map(inner) => json!({"type": "object", "additionalProperties": schema_of(meta, inner)}),
        _ => json!({}),
    }
}

/// The schema of a workspace subject as `tilo_action_policy::Subject` writes it: a person by
/// their key, an agent by its id.
fn subject_schema() -> Json {
    let one = |tag: &str| {
        json!({
            "type": "object",
            "properties": {tag: {"type": "string", "minLength": 1}},
            "required": [tag],
            "additionalProperties": false,
        })
    };
    json!({"type": "object", "oneOf": [one("person"), one("agent")]})
}

/// The parameter `param` of an action of `meta`, as the catalogue declares it: what the door
/// binds, a palette offers and the generated surface describes, one declaration for all three.
#[must_use]
pub fn published_param(meta: &Meta, param: &ParamMeta) -> ActionParam {
    // optional-decode: a loaded package's parameter types all parse (`meta::unreadable_types`).
    let parsed = tilolang_runtime::meta::parse_ty(param.ty.trim(), &meta.app).ok();
    // What the parameter takes when it is given: an `Option` is its value, left out.
    let present = match &parsed {
        Some(Ty::Option(inner)) => Some(inner.as_ref()),
        other => other.as_ref(),
    };
    let ty = match present {
        Some(Ty::Int) => ParamType::Integer,
        Some(Ty::Float) => ParamType::Number,
        Some(Ty::Bool) => ParamType::Boolean,
        Some(Ty::Time) => ParamType::DateTime,
        Some(Ty::Text | Ty::RichText | Ty::Date | Ty::Identifier) => ParamType::Text,
        Some(Ty::Enum(key)) if plain_cases(meta, key).is_some() => ParamType::Text,
        Some(Ty::List(_) | Ty::Set(_) | Ty::Collection(_) | Ty::Tuple(_)) => ParamType::Array,
        _ => ParamType::Object,
    };
    // A default is the action's: what an invocation that leaves the argument out gets, computed
    // when it is invoked, on what the invoker sees, and recorded as the argument.
    let description = match &param.default {
        Some(default) => format!(
            "{}: {}; when left out, `{}` as it is when invoked",
            param.name, param.ty, default.text
        ),
        None => format!("{}: {}", param.name, param.ty),
    };
    let optional = param.default.is_some() || matches!(parsed, Some(Ty::Option(_)));
    let built = if optional {
        ActionParam::optional_owned(param.name.clone(), ty, description)
    } else {
        ActionParam::required_owned(param.name.clone(), ty, description)
    };
    match present {
        Some(Ty::Record(key)) => built.picking(Pick::Resource {
            kind: record_kind(key),
        }),
        Some(Ty::Enum(key)) => match plain_cases(meta, key) {
            Some(cases) => built
                .with_schema(schema_of(meta, &Ty::Enum(key.clone())))
                .with_labelled_choices(cases),
            None => built.with_schema(schema_of(meta, &Ty::Enum(key.clone()))),
        },
        Some(Ty::Principal) => built.with_schema(subject_schema()).picking(Pick::Subject),
        Some(present) => built.with_schema(schema_of(meta, present)),
        None => built.with_schema(json!({})),
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

/// Whether `action` is declared `destructive`: it takes away what a person may want back.
#[must_use]
pub fn is_destructive(action: &ActionMeta) -> bool {
    action.role.as_deref() == Some("destructive")
}

fn verb(meta: &Meta, action: &ActionMeta, types: &mut TypeSet) -> VerbSurface {
    VerbSurface {
        id: action.id.clone(),
        label: if action.label.is_empty() {
            action.name.clone()
        } else {
            action.label.clone()
        },
        description: (!action.doc.is_empty()).then(|| action.doc.clone()),
        kind: action_kind(action.class),
        destructive: is_destructive(action),
        incoming_message: false,
        params: arguments(meta, &action.params)
            .iter()
            .map(|param| {
                let published = published_param(meta, param);
                ParamSurface {
                    name: param.name.clone(),
                    required: published.required,
                    schema: published.schema.as_deref().cloned().unwrap_or(Json::Null),
                    ty: field_type_of_type(meta, &param.ty, types),
                }
            })
            .collect(),
        writes: vec![RECORD_UNIT.to_owned()],
        field: None,
    }
}

/// The record unit, where an action writes unless it says otherwise.
pub const RECORD_UNIT: &str = tilo_actions::surface::RECORD_UNIT;

fn edit_verbs() -> Vec<VerbSurface> {
    let verb = |id: &str, label: &str, value: Json, ty: FieldType| VerbSurface {
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
                ty: FieldType::Text,
            },
            ParamSurface {
                name: "value".to_owned(),
                required: true,
                schema: value,
                ty,
            },
        ],
        writes: vec![RECORD_UNIT.to_owned()],
        field: None,
    };
    // The host creates a package's root resource on its own the first time a window wants it,
    // as a committed change of its own; receivers decide that change against this verb.
    let create = VerbSurface {
        id: CREATE_ACTION.to_owned(),
        label: "Create".to_owned(),
        description: Some("The resource is made the first time it is wanted.".to_owned()),
        kind: ActionKind::Undoable,
        destructive: false,
        incoming_message: false,
        params: Vec::new(),
        writes: vec![RECORD_UNIT.to_owned()],
        field: None,
    };
    vec![
        create,
        verb(
            EDIT_TEXT_ACTION,
            "Edit a text field",
            json!({"type": "string"}),
            FieldType::Text,
        ),
        verb(
            EDIT_FLAG_ACTION,
            "Set a flag field",
            json!({"type": "boolean"}),
            FieldType::Bool,
        ),
    ]
}

fn field_surfaces(
    meta: &Meta,
    ty: &TypeMeta,
    prefix: &str,
    stack: &mut Vec<String>,
    types: &mut TypeSet,
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
            ty: field_type_of_type(meta, &field.ty, types),
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
            field_surfaces(meta, inner, &below, stack, types, out);
        }
    }
    stack.pop();
}

/// The surface resources of one package.
#[must_use]
pub fn package_surfaces(meta: &Meta) -> Vec<Surface> {
    let mut surfaces = Vec::new();
    for ty in &meta.types {
        let mut types = TypeSet::new();
        let mut verbs = edit_verbs();
        verbs.extend(
            meta.offered_actions()
                .filter(|action| {
                    meta.subject_of(&action.params)
                        .is_some_and(|subject| subject.id == ty.id)
                })
                .map(|action| verb(meta, action, &mut types)),
        );
        let mut fields = Vec::new();
        field_surfaces(meta, ty, "", &mut Vec::new(), &mut types, &mut fields);
        surfaces.push(Surface {
            kind: meta.kind_of(&ty.name),
            label: ty.name.clone(),
            version: VERSION,
            person: false,
            verbs,
            fields,
            types: types.into_types(),
        });
    }
    let mut types = TypeSet::new();
    let loose: Vec<VerbSurface> = meta
        .offered_actions()
        .filter(|action| meta.subject_of(&action.params).is_none())
        .map(|action| verb(meta, action, &mut types))
        .collect();
    surfaces.push(Surface {
        kind: meta.package_kind(),
        label: meta.title.clone(),
        version: VERSION,
        person: false,
        verbs: loose,
        fields: Vec::new(),
        types: types.into_types(),
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
                ty: FieldType::Int,
            }],
        )],
        fields: Vec::new(),
        types: Vec::new(),
    }
}

#[derive(Serialize)]
struct SurfaceFile<'a> {
    resources: &'a [Surface],
    /// The kinds of the packages this one uses: it reads their records, live and read-only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reads: Vec<String>,
}

/// The `surface.toml` bytes of the host with these packages.
///
/// The host resource comes first: it is the host record's own kind.
///
/// # Errors
/// The surface does not serialize (a schema holding a value TOML cannot write).
pub fn surface_toml(packages: &[&Meta]) -> Result<Vec<u8>, toml::ser::Error> {
    surface_toml_reading(packages, &[])
}

/// The `surface.toml` bytes of an app of `packages` that reads the records of the packages
/// `reads` (those it `use`s): it declares their resource kinds as `reads`, which is what the
/// broker admits its read-only join to their records by.
///
/// # Errors
/// The surface does not serialize (a schema holding a value TOML cannot write).
pub fn surface_toml_reading(
    packages: &[&Meta],
    reads: &[Meta],
) -> Result<Vec<u8>, toml::ser::Error> {
    let mut resources = vec![host_surface()];
    for meta in packages {
        resources.extend(package_surfaces(meta));
    }
    let reads = reads
        .iter()
        .flat_map(|meta| {
            package_surfaces(meta)
                .into_iter()
                .map(|surface| surface.kind)
        })
        .filter(|kind| kind != HOST_KIND)
        .collect();
    let text = toml::to_string(&SurfaceFile {
        resources: &resources,
        reads,
    })?;
    let mut bytes = b"# Generated by the tilolang host from its packages' metadata.\n\n".to_vec();
    bytes.extend_from_slice(text.as_bytes());
    Ok(bytes)
}
