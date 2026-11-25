//! A resource's public surface as data (`tilo_actions::surface`), read from
//! its declarations and stored shape. `tilolang surface` writes it to the
//! app's `surface.toml` and compiled `surface.json`, and `tilolang check` compares it with the code, so a
//! rename or an incompatible storage change shows up in review, and it is
//! what the OS knows about an app that is not running and what another
//! language compiles its actions and types against.

pub use tilo_actions::surface::{
    CONTENT_VERBS, FieldSurface, FieldType, MIGRATE_VERB, NamedType, ParamSurface, ReturnsSurface,
    Surface, TYPE_VERB, TypeCase, TypeField, TypeSet, VerbSurface,
};

pub mod file;

use crate::resource::Resource;
use crate::shape::Shape;
use crate::vocabulary::{Declaration, vocabulary};

/// A flattened parameter's record fields and their published types, by name, as the code
/// declares them.
pub type FlatTypes = fn(&mut TypeSet) -> Vec<(String, FieldType)>;

/// The published parameters of an action, each typed by its Rust type when
/// the code declares one and by its schema otherwise.
fn params_of(
    params: &[tilo_actions::ActionParam],
    param_types: &[(&'static str, FieldTypeOf)],
    flat_types: Option<FlatTypes>,
    types: &mut TypeSet,
) -> Vec<ParamSurface> {
    // A flattened parameter is published as its record's own fields: their
    // types are the ones the code declares, so a nested argument that is an
    // enum names that enum rather than "any".
    let flat: Vec<(String, FieldType)> = flat_types.map_or_else(Vec::new, |flat| flat(types));
    params
        .iter()
        .map(|param| {
            let schema = param
                .schema
                .as_deref()
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let declared = param_types
                .iter()
                .find(|(name, _)| *name == param.name.as_ref());
            let ty = if param.ty == tilo_actions::ParamType::Secret {
                // What it takes is handed over once and kept nowhere, whatever Rust type holds it.
                FieldType::Secret
            } else {
                match declared {
                    Some((_, ty)) => ty(types),
                    None => flat
                        .iter()
                        .find(|(name, _)| name == param.name.as_ref())
                        .map_or_else(|| FieldType::from_schema(&schema), |(_, ty)| ty.clone()),
                }
            };
            ParamSurface {
                name: param.name.to_string(),
                required: param.required,
                schema,
                ty,
                within: None,
            }
        })
        .collect()
}

/// The value an action answers with, from its result: a JSON value's schema
/// and its type, typed by `declared` when the code declares one.
fn returns_of(
    result: Option<&tilo_actions::ResultSpec>,
    declared: Option<FieldTypeOf>,
    types: &mut TypeSet,
) -> Option<ReturnsSurface> {
    let tilo_actions::ResultSpec::Json { schema } = result? else {
        return None;
    };
    Some(ReturnsSurface {
        schema: schema.clone(),
        ty: declared.map_or_else(|| FieldType::from_schema(schema), |ty| ty(types)),
    })
}

fn verb_of(decl: &Declaration, types: &mut TypeSet) -> VerbSurface {
    VerbSurface {
        id: decl.id.to_owned(),
        name: String::new(),
        label: decl.shown_label(),
        description: decl.description.clone(),
        kind: decl.kind,
        destructive: decl.destructive,
        incoming_message: decl
            .tags
            .iter()
            .any(|tag| tag == tilo_actions::INCOMING_MESSAGE_TAG),
        params: params_of(&decl.params, &decl.param_types, decl.flat_types, types),
        returns: returns_of(decl.result.as_ref(), decl.result_type, types),
        writes: decl.writes.clone(),
        field: decl.field.map(str::to_owned),
        tags: Vec::new(),
    }
}

/// A verb the framework writes under on a record of `shape`: its id, and the units of the
/// record a change of it may write. It takes no parameters and answers nothing.
fn framework_verb(id: &str, writes: Vec<String>) -> VerbSurface {
    VerbSurface {
        id: id.to_owned(),
        name: String::new(),
        label: String::new(),
        description: None,
        kind: crate::ActionKind::Undoable,
        destructive: false,
        incoming_message: false,
        params: Vec::new(),
        returns: None,
        writes,
        field: None,
        tags: Vec::new(),
    }
}

/// The units a change of `shape` may write: the record itself, and each field kept as its own
/// storage unit (`tilo_resource::footprint`), a unit collection by any of its items.
fn units_of(shape: Shape) -> Vec<String> {
    let mut units = vec![tilo_actions::surface::RESOURCE_UNIT.to_owned()];
    if let Shape::Record(fields) = shape {
        for field in fields.iter().filter(|field| field.unit) {
            let unit = match field.shape {
                Shape::Collection(_) | Shape::Keyed(_) => format!("{}.*", field.key),
                _ => field.key.to_owned(),
            };
            if !units.contains(&unit) {
                units.push(unit);
            }
        }
    }
    units
}

/// Whether `shape` holds something a person changes directly, with no finer action: a field
/// that is not read-only, beneath fields that are not either. A record whose every field is
/// read-only changes only by its verbs.
fn edits_content(shape: Shape) -> bool {
    match shape {
        Shape::Record(fields) => fields
            .iter()
            .any(|field| !field.readonly && edits_content(field.shape)),
        Shape::Collection(_) | Shape::Keyed(_) | Shape::Map(_) | Shape::Value => true,
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

fn fields_of(shape: Shape, prefix: &str, types: &mut TypeSet, out: &mut Vec<FieldSurface>) {
    let (fields, item) = match shape {
        Shape::Record(fields) => (fields, false),
        Shape::Collection(item) | Shape::Keyed(item) => {
            fields_of(*item, &format!("{prefix}*."), types, out);
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
            ty: (field.field_type)(types),
            readonly: field.readonly,
            unit: field.unit,
            label: field.label,
            whole: false,
            ops: None,
        });
        fields_of(field.shape, &format!("{path}."), types, out);
    }
}

/// The surface of `R`, from its declarations and stored shape.
#[must_use]
pub fn surface_of<R: Resource>() -> Surface {
    surface_in(vocabulary::<R>(), R::VERSION, R::MIGRATES)
}

/// [`surface_of`] the resource `vocabulary`, stored at `version`, with migrations when
/// `migrates` says the type registers one.
fn surface_in(vocabulary: &crate::Vocabulary, version: u32, migrates: bool) -> Surface {
    let mut fields = Vec::new();
    let mut types = TypeSet::new();
    let shape = (vocabulary.shape)();
    if let Some(shape) = shape {
        fields_of(shape, "", &mut types, &mut fields);
    }
    for (path, schema) in (vocabulary.op_schemas)() {
        if let Some(field) = fields.iter_mut().find(|field| field.path == path) {
            field.ops = Some(schema);
        }
    }
    let mut verbs = Vec::new();
    for decl in vocabulary.declarations().iter() {
        verbs.push(verb_of(decl, &mut types));
    }
    // The content the framework writes itself: direct edits with no finer action, and the
    // migrations a record's type registers. A kind whose every field is read-only has neither.
    if let Some(shape) = shape {
        if edits_content(shape) {
            verbs.push(framework_verb(TYPE_VERB, units_of(shape)));
        }
        if migrates {
            verbs.push(framework_verb(MIGRATE_VERB, units_of(shape)));
        }
    }
    Surface {
        kind: vocabulary.kind.to_owned(),
        label: vocabulary.label.to_owned(),
        version,
        person: false,
        readers: reader_rule(vocabulary.readers),
        verbs,
        fields,
        types: types.into_types(),
        shows: Vec::new(),
        live: None,
        setters: false,
    }
}

/// The rule `#[resource(readers = "...")]` wrote, which the macro checked:
/// `None` for a kind every member reads.
fn reader_rule(readers: &str) -> Option<tilo_actions::surface::ReaderRule> {
    tilo_actions::surface::ReaderRule::parse(readers)
}

/// The surface of a resource a service publishes outside any record (the
/// sound, the network, the battery): its fields are its reading `R`, as the
/// service serialises it, each read-only since only the service's effects
/// change it, and its verbs are the effects `E` it offers on it. What a
/// view of another language reads and presses of it, typed as the service
/// declares it.
#[must_use]
pub fn reading_surface<R: crate::JsonSchema, E: crate::Effect>(kind: &str, label: &str) -> Surface {
    let mut types = TypeSet::new();
    let root = R::field_type(&mut types);
    let verbs: Vec<VerbSurface> = E::declarations()
        .iter()
        .map(|decl| effect_verb(decl, &mut types))
        .collect();
    let mut declared = types.into_types();
    // The reading's own record is the resource itself: its fields are the
    // resource's, and only the records it holds stay named types.
    let own = match &root {
        FieldType::Named(name) => declared
            .iter()
            .position(|ty| ty.name == *name && !ty.is_enum())
            .map(|at| declared.remove(at)),
        _ => None,
    };
    let fields = own
        .map(|own| {
            own.fields
                .into_iter()
                .map(|field| FieldSurface {
                    path: field.name,
                    owner: String::new(),
                    title: field.title,
                    stored: "value".to_owned(),
                    ty: field.ty,
                    readonly: true,
                    unit: false,
                    label: false,
                    whole: false,
                    ops: None,
                })
                .collect()
        })
        .unwrap_or_default();
    Surface {
        kind: kind.to_owned(),
        label: label.to_owned(),
        version: 1,
        person: false,
        readers: None,
        verbs,
        fields,
        types: declared,
        shows: Vec::new(),
        live: None,
        setters: false,
    }
}

/// The surface of the row type `R`, whose rows a reading carries with their
/// addresses ([`crate::Handle`]): its kind, its name, and the effects `E` it
/// offers on each row, called on the row's address. Its fields are the
/// record the reading declares for it, so it lists none of its own.
#[must_use]
pub fn row_surface<R: crate::words::RowType, E: crate::Effect>() -> Surface {
    let mut types = TypeSet::new();
    let verbs = E::declarations()
        .iter()
        .map(|decl| effect_verb(decl, &mut types))
        .collect();
    Surface {
        kind: R::KIND.to_owned(),
        label: R::LABEL.to_owned(),
        version: 1,
        person: false,
        readers: None,
        verbs,
        fields: Vec::new(),
        types: types.into_types(),
        shows: Vec::new(),
        live: None,
        setters: false,
    }
}

/// An effect's declaration as a verb of the surface: its id, words,
/// parameters and the value it answers with, each typed by its schema.
#[must_use]
pub fn effect_verb(decl: &crate::EffectDecl, types: &mut TypeSet) -> VerbSurface {
    VerbSurface {
        id: decl.id.to_owned(),
        name: String::new(),
        label: decl.label.read(),
        description: (!decl.description.is_empty()).then(|| decl.description.to_owned()),
        kind: decl.action_kind,
        destructive: decl.risk == tilo_actions::RiskLevel::Destructive,
        incoming_message: false,
        params: params_of(&decl.params, &decl.param_types, None, types),
        returns: decl.result.map(|result| ReturnsSurface {
            schema: (result.schema)(),
            ty: result.ty.map_or_else(|| FieldType::Any, |ty| ty(types)),
        }),
        writes: vec!["record".to_owned()],
        field: None,
        tags: Vec::new(),
    }
}

/// The ids of an app's effects that show something in its window
/// (`#[effect(view)]`): what its surface declares it [`Surface::shows`].
#[must_use]
pub fn view_effects(rows: &[crate::effect::EffectRow]) -> Vec<String> {
    let mut shows: Vec<String> = rows
        .iter()
        .filter(|row| row.kind == tilo_actions::OperationKind::View)
        .map(|row| row.id.to_owned())
        .collect();
    shows.sort();
    shows.dedup();
    shows
}

/// An app's effects as actions a caller of another language reads: every one
/// that only reads (`kind = "read"`) and that is offered on the app's own
/// resource, answering with the value it declares. An effect offered on a row
/// of a declared type is one of them: the resource's action, its parameter
/// naming the row it acts on. What its window shows (a view's zoom, its caret)
/// is no action of the catalogue, and neither is one offered on an item of the
/// resource's collection: that is published on the item itself.
#[must_use]
pub fn effect_verbs(rows: &[crate::effect::EffectRow], types: &mut TypeSet) -> Vec<VerbSurface> {
    rows.iter()
        .filter(|row| row.kind != tilo_actions::OperationKind::View)
        .filter(|row| row.action_kind == crate::ActionKind::Read)
        .filter(|row| !row.row)
        .filter(|row| {
            !row.items
                .iter()
                .any(|target| matches!(target, crate::effect::EffectTarget::Field(_)))
        })
        .map(|row| effect_verb(&crate::EffectDecl::of_row(row), types))
        .collect()
}

/// The type of a record of `shape` named `name` (the Rust type `identity`):
/// what `#[derive(Model)]` publishes for a struct, its fields read from its
/// shape.
#[doc(hidden)]
pub fn resource_type(
    types: &mut TypeSet,
    identity: &str,
    name: &str,
    doc: &str,
    shape: Shape,
) -> FieldType {
    types.named(identity, name, |types| NamedType {
        name: name.to_owned(),
        doc: doc.to_owned(),
        fields: shape
            .fields()
            .iter()
            .map(|field| TypeField {
                name: field.key.to_owned(),
                ty: (field.field_type)(types),
                title: field.title.to_owned(),
            })
            .collect(),
        cases: Vec::new(),
    })
}

/// How a value's [`FieldType`] is worked out, declaring the records and
/// enums it names: a type's `field_type`.
pub type FieldTypeOf = fn(&mut TypeSet) -> FieldType;

/// The type of a record named `name` (the Rust type `identity`) with the
/// fields `fields`, their names and types: what `#[derive(JsonSchema)]`
/// publishes for a struct.
#[doc(hidden)]
pub fn resource_of(
    types: &mut TypeSet,
    identity: &str,
    name: &str,
    doc: &str,
    fields: &[(&str, FieldTypeOf)],
) -> FieldType {
    types.named(identity, name, |types| NamedType {
        name: name.to_owned(),
        doc: doc.to_owned(),
        fields: fields
            .iter()
            .map(|(field, ty)| TypeField {
                name: (*field).to_owned(),
                ty: ty(types),
                title: String::new(),
            })
            .collect(),
        cases: Vec::new(),
    })
}

/// One case of an enum as a derive lists it: its stored name, how it reads
/// (empty: its name in words), and its fields' keys with their types.
#[doc(hidden)]
pub type CaseOf<'a> = (&'a str, &'a str, &'a [(&'a str, FieldTypeOf)]);

/// The type of an enum named `name` (the Rust type `identity`) with the
/// cases `cases`: what `#[derive(Model)]` and `#[derive(JsonSchema)]`
/// publish for an enum.
#[doc(hidden)]
pub fn enum_type(
    types: &mut TypeSet,
    identity: &str,
    name: &str,
    doc: &str,
    cases: &[CaseOf<'_>],
) -> FieldType {
    types.named(identity, name, |types| NamedType {
        name: name.to_owned(),
        doc: doc.to_owned(),
        fields: Vec::new(),
        cases: cases
            .iter()
            .map(|(case, label, fields)| TypeCase {
                name: (*case).to_owned(),
                label: if label.is_empty() {
                    tilo_actions::field_type::label_of(case)
                } else {
                    (*label).trim_end_matches('.').to_owned()
                },
                fields: fields
                    .iter()
                    .map(|(field, ty)| TypeField {
                        name: (*field).to_owned(),
                        ty: ty(types),
                        title: String::new(),
                    })
                    .collect(),
            })
            .collect(),
    })
}
