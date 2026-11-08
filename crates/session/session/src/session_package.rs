//! What the session publishes, read as a package: `apps/tl/session/surface.toml` .
//! A shell, the agent and the REPL call the session's actions as an app's:
//! `use so.tilo.session as session` , then `session.overview()` or `session.focus(window: w)` . The
//! device's resources and the desk are `so.tilo.device` 's ([`crate::shell::device_surfaces`]),
//! never described here too. The description is the native apps' (`surface.toml`, read by
//! `tilolang_tilo::catalogue` ), generated here from the very resources the session's door
//! publishes, and a test holds the committed file to it, so it never drifts from what the door
//! takes.
//! Each resource the session publishes under one address says it (`id`): a call of one of its
//! actions is sent there. A kind the session publishes one resource of per thing (a window, an
//! agent's run, an app) says `instances` : it reads as a type, and its actions are called on one of
//! them.

use serde::Serialize;
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::surface::{FieldType, ParamSurface, RECORD_UNIT, Surface, TypeSet, VerbSurface};
use tilo_actions::{ActionSpec, ResourceSpec, RiskLevel};
use tilo_desktop_surface::navigation::{ChromeEffect, DeskEffect};
use tilo_resource::Effect;
use tilo_resource::effect::EffectDecl;

/// The package id a shell `use` s.
pub const APP: &str = "so.tilo.session";

/// Where the description is committed, from the repository's root.
pub const PATH: &str = "apps/tl/session/surface.toml";

/// The actions of a resource the package leaves out, each because another resource's action
/// of the same name is the one a call of that name means: `desktop:chrome`'s
/// `switch-workspace` steps by a count, for the keys; the desk's (`so.tilo.device`'s) goes to
/// the workspace a person picked, by its key. Focus and close are a window's own actions,
/// declared once by the window: the desk's `focus` and `close-tab`, which name the window by
/// its tab, are left out, so `session.focus(window: w)` and a window's close mean the window's.
const LEFT_OUT: &[(&str, &str)] = &[
    (
        tilo_desktop::desk_verbs::CHROME_RESOURCE,
        "switch-workspace",
    ),
    (tilo_desktop::desk_verbs::DESK_RESOURCE, "focus"),
    (tilo_desktop::desk_verbs::DESK_RESOURCE, "close-tab"),
    (tilo_desktop::desk_verbs::DESK_RESOURCE, "wait-window"),
    (tilo_desktop::desk_verbs::DESK_RESOURCE, "force-quit-app"),
];

/// The declaration of a resource's action, by its id: where the type of a parameter its wire type
/// says nothing of (a record or an enum, sent as an object) is read.
type Declared = fn(&str) -> Option<EffectDecl>;

#[derive(Serialize)]
struct Entry {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    instances: bool,
    #[serde(flatten)]
    surface: Surface,
}

#[derive(Serialize)]
struct File {
    resources: Vec<Entry>,
}

/// The resources the session publishes under one address each, but the device's: the desk,
/// the sound, the network and Bluetooth are `so.tilo.device`'s
/// ([`crate::shell::device_surfaces`]), so each action is described once, by its owner.
fn singletons() -> Vec<(ResourceSpec, Option<Declared>)> {
    vec![
        (
            tilo_desktop::desk_verbs::chrome_resource(
                &ui::shortcut_table::ShortcutConfig::default(),
            ),
            Some(<ChromeEffect as Effect>::declaration),
        ),
        (
            tilo_desktop::desk_verbs::desk_resource(&[], &[], &[], None),
            Some(<DeskEffect as Effect>::declaration),
        ),
        (crate::arrangement::resources().swap_remove(0), None),
    ]
}

/// The kinds the session publishes one resource of per thing, each as one of them.
fn instances() -> Vec<(ResourceSpec, Option<Declared>)> {
    let mut out: Vec<(ResourceSpec, Option<Declared>)> = Vec::new();
    // The desk describes each window as a resource of its own; a desk that names none leaves the
    // kind out rather than failing the description.
    if let Some(window) = tilo_desktop::desk_verbs::desk_resource(
        &[tilo_desktop::desk_verbs::Shown {
            tab: 0,
            title: String::new(),
            foreign: None,
        }],
        &[0],
        &[],
        None,
    )
    .children
    .into_iter()
    .find(|child| child.kind == tilo_desktop::desk_verbs::WINDOW_KIND)
    {
        out.push((window, Some(<DeskEffect as Effect>::declaration)));
    }
    out.push((application(), None));
    // What is already arranged: a place, a strip and a row are resources of their own, each with
    // the actions it offers.
    out.push((crate::arrangement::row_kind_resource(), None));
    out.push((crate::arrangement::strip_kind_resource(), None));
    out.push((crate::arrangement::place_kind_resource(), None));
    out
}

/// An app on this device, by `app.<id>` : its own `open` , which the session carries out by
/// starting it in the workspace in front ([`crate::Session`]'s shell serve).
fn application() -> ResourceSpec {
    ResourceSpec::new(
        String::new(),
        tilo_actions::remote::APPLICATION_RESOURCE_KIND,
        String::new(),
    )
    .with_action(ActionSpec::typed_operation(
        tilo_actions::remote::APPLICATION_OPEN_ACTION,
        String::new(),
        String::new(),
        tilo_actions::OperationKind::Effect,
        tilo_actions::remote::ApplicationLaunch::TYPE,
        Vec::new(),
        |_| {
            tilo_actions::EncodedAction::encode_operation(
                tilo_actions::remote::ApplicationLaunch::TYPE,
                &(),
            )
            .unwrap_or_else(|error| tilo_actions::EncodedAction {
                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.to_owned(),
                payload: error.to_string().into_bytes(),
            })
        },
    ))
}

/// The type of a parameter as the description writes it. A resource is a resource of the kind it
/// names, or of any kind: code passes the value it has (a note it found), never its id's text. An
/// object or a list has no type tilolang code gives, so the action is left out.
fn field_type(param: &ActionParam) -> FieldType {
    match param.ty {
        ParamType::Text | ParamType::Secret | ParamType::DateTime => FieldType::Text,
        ParamType::Resource => {
            FieldType::Resource(param.resource_kind().unwrap_or_default().to_owned())
        }
        ParamType::Integer => FieldType::Int,
        ParamType::Number => FieldType::Float,
        ParamType::Boolean => FieldType::Bool,
        ParamType::Object | ParamType::Array => FieldType::Any,
    }
}

/// A parameter as the description writes it. An enum its declaration types (sent as an
/// object) is that enum, declared in `types`, so a call in tilolang builds its value. A plain
/// record of named fields, which tilolang has no value of (its records are resources), is its
/// fields: each a parameter of its own, optional when the field is, nested back under the
/// parameter's name on the wire ([`ParamSurface::within`]).
///
/// # Panics
/// For a record whose fields are records or enums themselves: one level is flattened, no more.
fn params(
    param: &ActionParam,
    decl: Option<&EffectDecl>,
    types: &mut TypeSet,
) -> Vec<ParamSurface> {
    let plain = |ty: FieldType| ParamSurface {
        name: param.name.to_string(),
        required: param.required,
        schema: serde_json::Value::Null,
        ty: if ty.is_any() { field_type(param) } else { ty },
        within: None,
    };
    let declared = decl.and_then(|decl| {
        decl.param_types
            .iter()
            .find(|(name, _)| *name == param.name.as_ref())
            .map(|(_, ty)| *ty)
    });
    let (ParamType::Object | ParamType::Array, Some(declared)) = (param.ty, declared) else {
        return vec![plain(FieldType::Any)];
    };
    // Whether it may be left out is `required` 's to say, as for every other parameter.
    let present = |ty: FieldType| match ty {
        FieldType::Option(inner) => (*inner, true),
        ty => (ty, false),
    };
    let mut scratch = TypeSet::new();
    let (ty, _) = present(declared(&mut scratch));
    let record = match &ty {
        FieldType::Named(name) => scratch
            .into_types()
            .into_iter()
            .find(|named| named.name == *name && !named.is_enum()),
        _ => None,
    };
    let Some(record) = record else {
        return vec![plain(present(declared(types)).0)];
    };
    record
        .fields
        .into_iter()
        .map(|field| {
            let (ty, optional) = present(field.ty);
            assert!(
                !nests(&ty),
                "`{}`'s field `{}` is a record or enum itself: a parameter is flattened one \
                 level, no deeper",
                param.name,
                field.name
            );
            ParamSurface {
                name: field.name,
                required: param.required && !optional,
                schema: serde_json::Value::Null,
                ty,
                within: Some(param.name.to_string()),
            }
        })
        .collect()
}

/// Whether a type holds a record or enum of its own.
fn nests(ty: &FieldType) -> bool {
    match ty {
        FieldType::Named(_) => true,
        FieldType::Option(inner)
        | FieldType::List(inner)
        | FieldType::Set(inner)
        | FieldType::Collection(inner)
        | FieldType::Map(inner)
        | FieldType::Outbox(inner) => nests(inner),
        FieldType::Union(all) => all.iter().any(nests),
        _ => false,
    }
}

/// # Panics
/// For two parameters of one name, a flattened record's field among them.
fn verb(action: &ActionSpec, declared: Option<Declared>, types: &mut TypeSet) -> VerbSurface {
    let decl = declared.and_then(|declared| declared(&action.id));
    let params: Vec<ParamSurface> = action
        .params()
        .iter()
        .flat_map(|one| params(one, decl.as_ref(), types))
        .collect();
    for (at, param) in params.iter().enumerate() {
        assert!(
            !params
                .get(..at)
                .unwrap_or_default()
                .iter()
                .any(|before| before.name == param.name),
            "`{}` has two parameters named `{}` once its records are flattened",
            action.id,
            param.name
        );
    }
    VerbSurface {
        id: action.id.clone(),
        label: String::new(),
        description: None,
        kind: action.contract.settled_kind(),
        destructive: action.contract.risk == RiskLevel::Destructive,
        incoming_message: false,
        params,
        writes: vec![RECORD_UNIT.to_owned()],
        field: None,
    }
}

fn entry((spec, declared): &(ResourceSpec, Option<Declared>), instances: bool) -> Entry {
    let id = spec.id.as_str();
    let mut types = TypeSet::new();
    let verbs = spec
        .actions
        .iter()
        .filter(|action| !LEFT_OUT.contains(&(id, action.id.as_str())))
        .map(|action| verb(action, *declared, &mut types))
        .collect();
    Entry {
        id: (!instances).then(|| id.to_owned()),
        instances,
        surface: Surface {
            kind: spec.kind.clone(),
            label: String::new(),
            version: 0,
            person: false,
            verbs,
            fields: Vec::new(),
            types: types.into_types(),
        },
    }
}

/// The description, as `apps/tl/session/surface.toml` holds it.
///
/// # Panics
/// For a parameter that does not flatten ([`params`], [`verb`]): a record nested in a record's
/// field, or a field named as another parameter is. Never otherwise: the resources serialize.
#[must_use]
pub fn description() -> String {
    let resources = singletons()
        .iter()
        .map(|spec| entry(spec, false))
        .chain(instances().iter().map(|spec| entry(spec, true)))
        .collect();
    let body = toml::to_string(&File { resources }).unwrap_or_else(|error| {
        // A description that does not serialize is a defect to read in the file it generates, not a
        // reason to take the session down.
        format!("# the description does not serialize: {error}")
    });
    format!(
        "# What the session publishes, read as the package `{APP}`.\n\
         # Generated by `tilo_session::session_package::description`; a test holds it to it.\n\n{body}"
    )
}
