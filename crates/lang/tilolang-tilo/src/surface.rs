//! The surface of a tilolang app, generated from its package's metadata.
//!
//! Receivers decide each change against the surface file the signed op names. A tilolang app's
//! surface has a resource per declared type (`tl.<app>.<Type>`), one for the package's
//! document-less actions (`tl.<app>`), and `tl.host`; the kinds of the packages it uses are
//! declared as `reads`. `tilolang build` writes it into the app's payload ([`crate::bundle`]),
//! and the host registers it with `app_surface::register_package` before its first signed
//! operation.

use serde_json::{Value as Json, json};
use tilo_actions::surface::{
    FieldSurface, FieldType, NamedType, ParamSurface, ReaderRule, ReturnsSurface, Surface,
    TypeCase, TypeField, TypeSet, VerbSurface,
};
use tilo_actions::{ActionKind, ActionParam, ParamType, Pick};
use tilolang_runtime::Words;
use tilolang_runtime::meta::Ty;

use crate::action::{BLOCK_EDITS, CREATE_ACTION, DIRECT_EDITS, ORIGIN_ACTION, SCENARIO_ACTION};
use tilolang_runtime::meta::{ActionMeta, Meta, ParamMeta, TypeMeta};

use crate::meta::{MetaExt as _, action_kind};

/// The kind of the host's own resource.
pub const HOST_KIND: &str = "tl.host";

/// The storage version of every generated resource.
pub const VERSION: u32 = 1;

/// The resource kind of the record type `key`: `tl.<app>.<Type>`.
pub(crate) fn resource_type(key: &tilolang_runtime::meta::TypeKey) -> String {
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
    match tilo_failure::ignored!(
        "a type checked where the package is loaded",
        tilolang_runtime::meta::parse_ty(ty.trim(), &meta.app)
    ) {
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
        // A repeat and a reminder are text as they are held, so a picker writes them.
        Ty::Text => FieldType::Text,
        Ty::RichText => FieldType::RichText,
        Ty::Date => FieldType::Date,
        Ty::Time => FieldType::Time,
        Ty::Duration => FieldType::Duration,
        Ty::Identifier => FieldType::Identifier,
        Ty::Actor => FieldType::Actor,
        Ty::Record(key) => FieldType::Resource(resource_type(key)),
        Ty::Union(keys) => FieldType::Union(
            keys.iter()
                .map(|key| FieldType::Resource(resource_type(key)))
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
        Ty::Json => FieldType::Json,
        _ => FieldType::Any,
    }
}

pub(crate) fn schema_of(meta: &Meta, ty: &Ty) -> Json {
    match ty {
        Ty::Int => json!({"type": "integer"}),
        Ty::Duration => json!({"type": "integer", "description": "Duration in whole seconds"}),
        Ty::Float => json!({"type": "number"}),
        Ty::Bool => json!({"type": "boolean"}),
        // A rule and a trigger as RFC 5545 writes them, `FREQ=WEEKLY` and `-PT15M`.
        Ty::Text | Ty::RichText => json!({"type": "string"}),
        Ty::Date => json!({"type": "string", "format": "date"}),
        Ty::Time => json!({"type": "string", "format": "date-time"}),
        // An account of some service, as its URI (`mailto:jane@x.com`).
        Ty::Identifier => json!({"type": "string", "format": "uri"}),
        Ty::Actor => actor_schema(),
        Ty::Record(key) => tilo_actions::picked_resource_schema(&resource_type(key)),
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

/// The schema of a workspace's actor as a member names one: a person by their key, an agent by
/// its id and the person it acts for ([`tilo_action_policy::MemberId`]).
fn actor_schema() -> Json {
    let text = || json!({"type": "string", "minLength": 1});
    json!({
        "type": "object",
        "oneOf": [
            {
                "type": "object",
                "properties": {"person": text()},
                "required": ["person"],
                "additionalProperties": false,
            },
            {
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "object",
                        "properties": {"id": text(), "person": text()},
                        "required": ["id", "person"],
                        "additionalProperties": false,
                    },
                },
                "required": ["agent"],
                "additionalProperties": false,
            },
        ],
    })
}

/// The parameter `param` of an action of `meta`, as the catalogue declares it: what the API
/// binds, a palette offers and the generated surface describes, one declaration for all three.
#[must_use]
pub fn published_param(meta: &Meta, param: &ParamMeta) -> ActionParam {
    let parsed = tilo_failure::ignored!(
        "a loaded package's parameter types all parse, as `meta::unreadable_types` checked",
        tilolang_runtime::meta::parse_ty(param.ty.trim(), &meta.app).ok()
    );
    // What the parameter takes when it is given: an `Option` is its value, left out.
    let present = match &parsed {
        Some(Ty::Option(inner)) => Some(inner.as_ref()),
        other => other.as_ref(),
    };
    let ty = match present {
        // A parameter the declaration says is a secret is handed over once and kept nowhere.
        _ if param.secret => ParamType::Secret,
        Some(Ty::Int | Ty::Duration) => ParamType::Integer,
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
    let unit = if matches!(present, Some(Ty::Duration)) {
        " (whole seconds)"
    } else {
        ""
    };
    let description = match &param.default {
        Some(default) => format!(
            "{}: {}{unit}; when left out, `{}` as it is when invoked",
            param.name, param.ty, default.text
        ),
        None => format!("{}: {}{unit}", param.name, param.ty),
    };
    let optional = param.default.is_some() || matches!(parsed, Some(Ty::Option(_)));
    let built = if optional {
        ActionParam::optional_owned(param.name.clone(), ty, description)
    } else {
        ActionParam::required_owned(param.name.clone(), ty, description)
    };
    let built = if param.entry {
        built.taking_entry()
    } else {
        built
    };
    match present {
        Some(Ty::Record(key)) => built.picking(Pick::Resource {
            kind: resource_type(key),
        }),
        Some(Ty::Enum(key)) => match plain_cases(meta, key) {
            Some(cases) => built
                .with_schema(schema_of(meta, &Ty::Enum(key.clone())))
                .with_labelled_choices(cases),
            None => built.with_schema(schema_of(meta, &Ty::Enum(key.clone()))),
        },
        Some(Ty::Actor) => built.with_schema(actor_schema()).picking(Pick::Grantee),
        // Rich text is given as its text, or encoded whole when it holds more than text
        // (`host::argument_json`).
        Some(Ty::RichText) => built.with_schema(json!({"oneOf": [
            {"type": "string"},
            {"type": "object", "required": ["rich"]},
        ]})),
        // One of several records, chosen when it is invoked (`to: TextChannel | Thread | Direct`):
        // the kinds name what the step reaches, so a grant covers each.
        Some(Ty::Union(keys)) => built
            .with_schema(json!({"type": "object"}))
            .taking_resource_kinds(keys.iter().map(resource_type)),
        Some(present) => built.with_schema(schema_of(meta, present)),
        None => built.with_schema(json!({})),
    }
}

/// Whether `action` is declared `destructive`: it takes away what a person may want back.
#[must_use]
pub fn is_destructive(action: &ActionMeta) -> bool {
    action.role.as_deref() == Some("destructive")
}

/// Whether `action` is declared `primary`: the main thing to do with a resource, whose result
/// opens beside the list when a person runs it.
#[must_use]
pub fn is_primary(action: &ActionMeta) -> bool {
    action.role.as_deref() == Some("primary")
}

/// Whether `ty` is or contains a `Duration`: its return schema carries the unit (`whole seconds`)
/// so a caller reading the surface knows the wire unit of the integer result.
fn mentions_duration(ty: &Ty) -> bool {
    match ty {
        Ty::Duration => true,
        Ty::Option(inner)
        | Ty::List(inner)
        | Ty::Set(inner)
        | Ty::Collection(inner)
        | Ty::Map(inner) => mentions_duration(inner),
        Ty::Tuple(items) => items.iter().any(mentions_duration),
        Ty::Result(ok, err) => mentions_duration(ok) || mentions_duration(err),
        _ => false,
    }
}

fn verb(meta: &Meta, action: &ActionMeta, types: &mut TypeSet, targeted: bool) -> VerbSurface {
    let declared = meta.arguments(&action.params);
    // On a type's surface the resource is the one the verb is offered on; on the package's, an
    // action taking a union of types takes it as a reference argument.
    let declared = if targeted && meta.target_of(&action.params).is_none() {
        declared.get(1..).unwrap_or_default()
    } else {
        declared
    };
    VerbSurface {
        // The action as the document names it, which is what a change's claim names too
        // ([`crate::action::DynAction::describe`]): its id, the one its words keep
        // ([`crate::publish::action_type`]'s `label_of`). A receiver decides the app's own changes
        // by it.
        id: action.id.clone(),
        // And the name the API offers it by ([`crate::publish::action_type`]): what a caller
        // outside the app's code — a script, an agent, the SDK's typed method — invokes it as.
        name: crate::meta::published_name(action).to_owned(),
        label: tilolang_runtime::platform::label_of(action),
        description: (!action.doc.is_empty()).then(|| action.doc.clone()),
        kind: action_kind(action.class),
        destructive: is_destructive(action),
        incoming_message: false,
        params: declared
            .iter()
            .map(|param| {
                let published = published_param(meta, param);
                ParamSurface {
                    name: param.name.clone(),
                    required: published.required,
                    schema: published.schema.as_deref().cloned().unwrap_or(Json::Null),
                    // A parameter declared secret says so in the surface, whatever it is held as:
                    // a view shows a field that keeps what is typed out of every record, and a
                    // host reading the surface keeps the call's argument none of.
                    ty: if param.secret {
                        FieldType::Secret
                    } else {
                        field_type_of_type(meta, &param.ty, types)
                    },
                    within: None,
                }
            })
            .collect(),
        returns: action.result.as_ref().map(|result| {
            let schema = tilo_failure::ignored!(
                "a loaded package's result types all parse, as `meta::unreadable_types` checked",
                tilolang_runtime::meta::parse_ty(result.trim(), &meta.app).ok()
            )
            .filter(mentions_duration)
            .map(|parsed| schema_of(meta, &parsed))
            .filter(|schema| !matches!(schema, Json::Object(map) if map.is_empty()))
            .unwrap_or(Json::Null);
            ReturnsSurface {
                schema,
                ty: field_type_of_type(meta, result, types),
            }
        }),
        writes: vec![RESOURCE_UNIT.to_owned()],
        field: None,
        tags: Vec::new(),
    }
}

/// The document unit, where an action writes unless it says otherwise.
pub const RESOURCE_UNIT: &str = tilo_actions::surface::RESOURCE_UNIT;

/// The names of the fields of `ty` that `edit-text` and `edit-flag` can set, from the type's
/// metadata: a text (or rich-text) field the first, a flag field the second, in declaration
/// order. The edit verbs take these names, never the internal field ids the document stores
/// them under — an agent, and anyone reading a surface, chooses a field by the name the app
/// and its views use, and `edit_action` maps that name back to the id.
pub(crate) fn editable_field_names(ty: &TypeMeta, merge: Merge) -> Vec<String> {
    ty.fields
        .iter()
        .filter(|field| match merge {
            Merge::Text => matches!(
                tilolang_runtime::meta::parse_ty(field.ty.trim(), ""),
                Ok(Ty::Text | Ty::RichText)
            ),
            Merge::Flag => matches!(
                tilolang_runtime::meta::parse_ty(field.ty.trim(), ""),
                Ok(Ty::Bool)
            ),
        })
        .map(|field| field.name.clone())
        .collect()
}

/// The names of the fields of `ty` that hold rich text, the ones a paragraph edit changes.
pub(crate) fn rich_field_names(ty: &TypeMeta) -> Vec<String> {
    ty.fields
        .iter()
        .filter(|field| {
            matches!(
                tilolang_runtime::meta::parse_ty(field.ty.trim(), ""),
                Ok(Ty::RichText)
            )
        })
        .map(|field| field.name.clone())
        .collect()
}

/// Which kinds of field an [`edit_verbs`] verb may set: a text (plain or rich), or a flag.
#[derive(Clone, Copy)]
pub(crate) enum Merge {
    Text,
    Flag,
}

/// The surface's reading of a direct edit's value: its type, and the schema a caller reads it by.
fn direct_edit_value(ty: ParamType) -> (FieldType, Json) {
    match ty {
        ParamType::Text => (FieldType::Text, json!({"type": "string"})),
        ParamType::Boolean => (FieldType::Bool, json!({"type": "boolean"})),
        _ => (FieldType::Any, json!({})),
    }
}

/// The verbs a bound control's edit of a field names, and the one that creates the resource:
/// [`DIRECT_EDITS`]', with the arguments a change of it carries. A text edit names the field
/// alone — what a person typed is the change itself — and a caller may name the value it sets,
/// so the value is one it may give. The field is chosen by name, from the type's fields that edit
/// sets ([`editable_field_names`]).
fn edit_verbs(declared: &TypeMeta) -> Vec<VerbSurface> {
    // The host creates a package's root resource on its own the first time a window wants it,
    // as a committed change of its own; receivers decide that change against this verb.
    let create = VerbSurface {
        id: CREATE_ACTION.to_owned(),
        name: String::new(),
        label: "Create".to_owned(),
        description: Some("The resource is made the first time it is wanted.".to_owned()),
        kind: ActionKind::Undoable,
        destructive: false,
        incoming_message: false,
        params: Vec::new(),
        returns: None,
        writes: vec![RESOURCE_UNIT.to_owned()],
        field: None,
        tags: Vec::new(),
    };
    let mut verbs = vec![create];
    verbs.extend(DIRECT_EDITS.iter().map(|direct| {
        let (ty, schema) = direct_edit_value(direct.value);
        VerbSurface {
            id: direct.id.to_owned(),
            name: String::new(),
            label: direct.label.to_owned(),
            description: Some("A bound control changes the field it shows.".to_owned()),
            kind: ActionKind::Undoable,
            destructive: false,
            incoming_message: false,
            params: vec![
                ParamSurface {
                    name: "field".to_owned(),
                    required: true,
                    // The choices are the field names, so a caller reads the one it wants from
                    // the list instead of guessing at an internal id.
                    schema: json!({
                        "type": "string",
                        "description": "The name of the field to set.",
                        "enum": editable_field_names(
                            declared,
                            if direct.value == ParamType::Boolean { Merge::Flag } else { Merge::Text },
                        ),
                    }),
                    ty: FieldType::Text,
                    within: None,
                },
                ParamSurface {
                    name: "value".to_owned(),
                    required: direct.names_value,
                    schema,
                    ty,
                    within: None,
                },
            ],
            returns: None,
            writes: vec![RESOURCE_UNIT.to_owned()],
            field: None,
            tags: Vec::new(),
        }
    }));
    // A type with rich text offers an agent the edits of one paragraph of it, by the address
    // block search publishes for that paragraph. A plain text has no paragraphs to address.
    verbs.extend(block_verbs(declared));
    verbs
}

/// The verbs an agent changes one paragraph of a rich-text field of `declared` by
/// ([`BLOCK_EDITS`]): each names the paragraph by the address block search gives it
/// (`tl:<app>:<id>#<field>.<place>`), and what sets it takes the paragraph's Markdown.
fn block_verbs(declared: &TypeMeta) -> Vec<VerbSurface> {
    let rich = rich_field_names(declared);
    if rich.is_empty() {
        return Vec::new();
    }
    let described = format!(
        concat!(
            "The paragraph's address, `tl:<app>:<id>#<field>.<place>`, ",
            "as block search names one; the text's fields: {}",
        ),
        rich.join(", ")
    );
    BLOCK_EDITS
        .into_iter()
        .map(|edit| {
            let mut params = vec![ParamSurface {
                name: "block".to_owned(),
                required: true,
                schema: json!({"type": "string", "description": described.clone()}),
                ty: FieldType::Text,
                within: None,
            }];
            if edit.text {
                params.push(ParamSurface {
                    name: "text".to_owned(),
                    required: true,
                    schema: json!({
                        "type": "string",
                        "description": "What the paragraph says, as Markdown.",
                    }),
                    ty: FieldType::Text,
                    within: None,
                });
            }
            VerbSurface {
                id: edit.id.to_owned(),
                name: String::new(),
                label: edit.label.to_owned(),
                description: Some(edit.description.to_owned()),
                kind: ActionKind::Undoable,
                destructive: false,
                incoming_message: false,
                params,
                returns: None,
                writes: vec![RESOURCE_UNIT.to_owned()],
                field: None,
                tags: Vec::new(),
            }
        })
        .collect()
}

/// The verb of what an outside origin writes: its service's answer, taken into the resources the
/// package keeps in step with it, as one change of the host's own. Receivers decide that change
/// against this verb.
fn origin_verb() -> VerbSurface {
    VerbSurface {
        id: ORIGIN_ACTION.to_owned(),
        name: String::new(),
        label: "Keep in step with its service".to_owned(),
        description: Some("What the outside service said is taken in.".to_owned()),
        kind: ActionKind::Undoable,
        destructive: false,
        incoming_message: false,
        params: Vec::new(),
        returns: None,
        writes: vec![RESOURCE_UNIT.to_owned()],
        field: None,
        tags: Vec::new(),
    }
}

fn scenario_verb() -> VerbSurface {
    VerbSurface {
        id: SCENARIO_ACTION.to_owned(),
        name: String::new(),
        label: "Fill with its scenario".to_owned(),
        description: Some("The data the app's shots and demos start from is written.".to_owned()),
        kind: ActionKind::Undoable,
        destructive: false,
        incoming_message: false,
        params: Vec::new(),
        returns: None,
        writes: vec![RESOURCE_UNIT.to_owned()],
        field: None,
        tags: Vec::new(),
    }
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
            whole: false,
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

/// When a resource of `ty` is live, as the resource index keeps it: its `live when` as filters
/// over the fields the store holds, with the view that shows it. `None` for a type that says
/// nothing of it.
fn live_surface(meta: &Meta, ty: &TypeMeta) -> Option<tilo_actions::surface::LiveSurface> {
    use tilolang_runtime::meta::{field_kind, parse_ty};
    use tilolang_runtime::store::FieldRead;
    let live = ty.live.as_ref()?;
    let mut kinds = crate::resources::Kinds::default();
    kinds.declare(meta, &[]);
    let entry = kinds.of_kind(&meta.kind_of(&ty.name))?;
    let field = |id: &String| -> Option<(String, tilolang::pred::Lit)> {
        let held = entry.fields.get(id)?;
        if !held.indexed(&kinds) {
            return None;
        }
        let declared = ty.fields.iter().find(|field| field.id == *id)?;
        let parsed = parse_ty(&declared.ty, &meta.app).ok()?;
        let read = FieldRead::of(
            id,
            field_kind(declared, &parsed),
            &parsed,
            declared.default.as_ref(),
        )?;
        Some((held.name.clone(), read.absent))
    };
    let Some(filter) = crate::resources::store_filter(&live.when, &field, &|lit| {
        crate::resources::store_value(lit, &kinds)
    }) else {
        tracing::error!(
            ty = %ty.name,
            "`live when` reads what the resource index does not hold, so it is never live"
        );
        return None;
    };
    let view = meta
        .views
        .iter()
        .find(|view| view.id == live.view)
        .map_or_else(|| "live".to_owned(), |view| view.name.clone());
    Some(tilo_actions::surface::LiveSurface {
        when: serde_json::to_value(crate::resources::conjunction(filter)).ok()?,
        view,
    })
}

/// The surface resources of one package, named in the language `words` reads.
#[must_use]
pub fn package_surfaces(meta: &Meta, words: &dyn Words) -> Vec<Surface> {
    let mut surfaces = Vec::new();
    // A package with an outside origin writes what its service says into the resources holding
    // those of the origin's types, the package's root among them: each resource takes that change.
    let kept_in_step = meta.types.iter().any(|ty| ty.origin.is_some());
    let seeded = !meta.scenarios.is_empty();
    for ty in &meta.types {
        let mut types = TypeSet::new();
        let mut verbs = edit_verbs(ty);
        if kept_in_step {
            verbs.push(origin_verb());
        }
        // A package's scenario fills its resources with the data its shots and demos start
        // from, writing any of its types.
        if seeded {
            verbs.push(scenario_verb());
        }
        // An action on a union of the package's types is one on each of them: the change a call
        // makes is claimed on the kind of the resource it was called on.
        verbs.extend(
            meta.offered_actions()
                .filter(|action| {
                    meta.targets_of(&action.params)
                        .iter()
                        .any(|target| target.id == ty.id)
                })
                .map(|action| verb(meta, action, &mut types, true)),
        );
        let mut fields = Vec::new();
        field_surfaces(meta, ty, "", &mut Vec::new(), &mut types, &mut fields);
        let live = live_surface(meta, ty);
        surfaces.push(Surface {
            kind: meta.kind_of(&ty.name),
            label: ty.name.clone(),
            version: VERSION,
            person: false,
            readers: ty.readers.clone(),
            verbs,
            fields,
            types: types.into_types(),
            shows: Vec::new(),
            live,
            setters: false,
        });
    }
    let mut types = TypeSet::new();
    let loose: Vec<VerbSurface> = meta
        .offered_actions()
        .filter(|action| meta.targets_of(&action.params).is_empty())
        .map(|action| verb(meta, action, &mut types, false))
        .collect();
    surfaces.push(Surface {
        kind: meta.package_kind(),
        label: crate::publish::app_name(words, meta),
        version: VERSION,
        person: meta.keeps_own_resources(),
        readers: None,
        verbs: loose,
        fields: Vec::new(),
        types: types.into_types(),
        shows: Vec::new(),
        live: None,
        setters: false,
    });
    surfaces
}

/// The surface of the host's own resource. It declares no verbs: an agent's host effects are
/// answered by the window that publishes them, never written to a document.
#[must_use]
pub fn host_surface() -> Surface {
    Surface {
        kind: HOST_KIND.to_owned(),
        label: "tilolang".to_owned(),
        version: VERSION,
        person: false,
        readers: None,
        verbs: Vec::new(),
        fields: Vec::new(),
        types: Vec::new(),
        shows: Vec::new(),
        live: None,
        setters: false,
    }
}

/// The surface of each person's own state of an app's windows ([`crate::personal`]): a
/// member's own resource, which the verbs keeping a slot, a seen mark or the reviewed runs write.
#[must_use]
pub fn personal_surface() -> Surface {
    use crate::personal::{KIND, RUNS, SEEN, VIEW_STATE};
    let verbs = [
        (VIEW_STATE, "Keep a view's state"),
        (SEEN, "Keep what is seen"),
        (RUNS, "Keep reviewed runs"),
    ]
    .into_iter()
    .map(|(id, label)| VerbSurface {
        id: id.to_owned(),
        name: String::new(),
        label: label.to_owned(),
        description: None,
        kind: ActionKind::Undoable,
        destructive: false,
        incoming_message: false,
        params: Vec::new(),
        returns: None,
        writes: vec![RESOURCE_UNIT.to_owned()],
        field: None,
        tags: Vec::new(),
    })
    .collect();
    Surface {
        kind: KIND.to_owned(),
        label: "Your place in the app".to_owned(),
        version: VERSION,
        person: false,
        readers: Some(ReaderRule::Author { until: None }),
        verbs,
        fields: Vec::new(),
        types: Vec::new(),
        shows: Vec::new(),
        live: None,
        setters: false,
    }
}

/// The surface of the workspace's moderation record (`tilo.moderation`), which Chat keeps and
/// the workspace's moderators change: the file beside the record's own model, written from that
/// model by its crate's test, so a package keeping the record publishes what its changes are
/// decided against without this host linking an app's model crate.
const MODERATION_SURFACE: &str = include_str!("../../../../apps/moderation-model/surface.json");

/// A committed surface file's resources (`{"resources": […]}`).
#[derive(serde::Deserialize)]
struct SurfaceFileResources {
    resources: Vec<Surface>,
}

/// The native records `app` keeps for its whole workspace ([`collab_types::WORKSPACE_DOCUMENTS`]),
/// as their surfaces: a change to one is decided by its app's surface, so the package of an app
/// that keeps one publishes it beside its own types.
fn kept_for_workspace(app: &str) -> Vec<Surface> {
    use tilo_resource::Resource as _;
    let committed: Vec<Surface> = tilo_failure::ignored!(
        "the moderation surface beside its model is what that model publishes: the crate's own \
         test holds the file to it, so a file that does not read names no record in a build that \
         was already broken",
        serde_json::from_str::<SurfaceFileResources>(MODERATION_SURFACE)
            .map(|file| file.resources)
            .unwrap_or_default()
    );
    [
        vec![tilo_workspace_emoji::WorkspaceEmoji::surface()],
        committed,
    ]
    .into_iter()
    .flatten()
    .filter(|surface| {
        collab_types::workspace_document_of_kind(&surface.kind).is_some_and(|kept| kept.app == app)
    })
    .collect()
}

/// The surface of the host's `Notice`: what any package's action writes to tell a person, and
/// the person's devices mark seen. Every package's store declares it, so a notice an action
/// makes is decided like any other record it writes.
#[must_use]
pub fn notice_surfaces(words: &dyn Words) -> Vec<Surface> {
    let host = tilolang::host_meta();
    let notice = host.kind_of("Notice");
    package_surfaces(&host, words)
        .into_iter()
        .filter(|surface| surface.kind == notice)
        .collect()
}

/// The compiled surface bytes of the host with these packages: the one form the system commits,
/// ships and addresses.
///
/// The host resource comes first: it is the host document's own kind.
pub fn surface_bytes(packages: &[&Meta], words: &dyn Words) -> Result<Vec<u8>, String> {
    surface_bytes_reading(packages, &[], words)
}

/// The compiled surface bytes of an app of `packages` that reads the documents of the packages
/// `reads` (those it `use`s): it declares their resource kinds as `reads`, which is what the
/// broker admits its read-only join to their documents by.
pub fn surface_bytes_reading(
    packages: &[&Meta],
    reads: &[Meta],
    words: &dyn Words,
) -> Result<Vec<u8>, String> {
    let mut resources = vec![host_surface(), personal_surface()];
    resources.extend(notice_surfaces(words));
    for meta in packages {
        resources.extend(package_surfaces(meta, words));
        resources.extend(kept_for_workspace(&meta.app));
    }
    let reads = reads
        .iter()
        .flat_map(|meta| {
            package_surfaces(meta, words)
                .into_iter()
                .map(|surface| surface.kind)
        })
        .filter(|kind| kind != HOST_KIND)
        .collect::<Vec<String>>();
    let values = resources
        .iter()
        .map(|surface| serde_json::to_value(surface).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(tilo_surface_compile::compile_value(&values, &reads).into_bytes())
}
