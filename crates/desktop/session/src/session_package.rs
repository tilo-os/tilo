//! What the session publishes, read as a package: `apps/tl/session/surface.toml` .
//! A shell, the agent and the REPL call the session's actions as an app's:
//! `use so.tilo.session as session` , then `session.overview()` or `session.focus(window: w)` . The
//! device's resources and the shell are `so.tilo.device` 's ([`crate::shell::device_surfaces`]),
//! never described here too. The description is the native apps' (`surface.toml`, read by
//! `tilolang_tilo::catalogue` ), generated here from the very resources the session's API
//! publishes, and a test holds the committed file to it, so it never drifts from what the API
//! takes.
//! Each resource the session publishes under one address says it (`id`): a call of one of its
//! actions is sent there. A kind the session publishes one resource of per thing (a window, an
//! agent's run, an app) says `instances` : it reads as a type, and its actions are called on one of
//! them.

use serde::Serialize;
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::surface::{
    FieldType, ParamSurface, Surface, TypeSet, VerbSurface, param_type, verb_of,
};
use tilo_actions::{ActionSpec, ResourceSpec};
use tilo_resource::Effect;
use tilo_resource::effect::EffectDecl;
use tilo_session_core::desktop::navigation::{ChromeEffect, LayoutEffect};

/// The package id a shell `use` s.
pub const APP: &str = "so.tilo.session";

/// Where the description is committed, from the repository's root.
pub const PATH: &str = "apps/tl/session/surface.toml";

/// The actions of a resource the package leaves out, each because another resource's action
/// of the same name is the one a call of that name means. Focus and close are a window's own actions,
/// declared once by the window: the shell's `focus` and `close-tab`, which name the window by
/// its tab, are left out, so `session.focus(window: w)` and a window's close mean the window's.
const LEFT_OUT: &[(&str, &str)] = &[
    (tilo_desktop::verbs::SHELL_RESOURCE, "focus"),
    (tilo_desktop::verbs::SHELL_RESOURCE, "close-tab"),
    (tilo_desktop::verbs::SHELL_RESOURCE, "wait-window"),
    (tilo_desktop::verbs::SHELL_RESOURCE, "force-quit-app"),
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

/// The resources the session publishes under one address each, but the device's: the shell,
/// the sound, the network and Bluetooth are `so.tilo.device`'s
/// ([`crate::shell::device_surfaces`]), so each action is described once, by its owner.
fn singletons() -> Vec<(ResourceSpec, Option<Declared>)> {
    let words: tilo_session_core::layout::Words = std::rc::Rc::new(crate::i18n::word);
    vec![
        (
            tilo_desktop::verbs::chrome_resource(&ui::shortcut_table::ShortcutConfig::default()),
            Some(<ChromeEffect as Effect>::declaration),
        ),
        (
            tilo_desktop::verbs::shell_resource(&[], &[], &[], None),
            Some(<LayoutEffect as Effect>::declaration),
        ),
        (
            tilo_session_core::layout::resources(&words).swap_remove(0),
            None,
        ),
        // The person's own styles as packages in their workspace: "make it mine", `check`
        // and `install` (`shell.md` §6, "Agents make styles with the same commands a person
        // types"), so a shell view and the canvas chat's agent call them as an app's.
        (crate::style_packages::resource(), None),
    ]
}

/// The kinds the session publishes one resource of per thing, each as one of them.
fn instances() -> Vec<(ResourceSpec, Option<Declared>)> {
    let mut out: Vec<(ResourceSpec, Option<Declared>)> = Vec::new();
    // The shell describes each window as a resource of its own; a shell that names none leaves the
    // kind out rather than failing the description.
    if let Some(window) = tilo_desktop::verbs::shell_resource(
        &[tilo_desktop::verbs::Shown {
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
    .find(|child| child.kind == tilo_desktop::verbs::WINDOW_KIND)
    {
        out.push((window, Some(<LayoutEffect as Effect>::declaration)));
    }
    out.push((application(), None));
    // What is already arranged: a place, a strip and a row are resources of their own, each with
    // the actions it offers.
    let words: tilo_session_core::layout::Words = std::rc::Rc::new(crate::i18n::word);
    out.push((tilo_session_core::layout::row_kind_resource(), None));
    out.push((tilo_session_core::layout::strip_kind_resource(&words), None));
    out.push((tilo_session_core::layout::place_kind_resource(&words), None));
    out
}

/// An app on this device, as a declared type: its own `open` , which the session carries out by
/// starting it in the workspace in front ([`crate::Session`]'s shell serve).
pub struct Application;

impl tilo_resource::words::RowType for Application {
    const KIND: &'static str = tilo_actions::remote::APPLICATION_RESOURCE_KIND;
    const LABEL: &'static str = "Application";
}

/// What an app offers: opening it in the workspace in front.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.application.launch")]
pub enum ApplicationOpen {
    /// Open the app
    #[effect(kind = "undoable", label = "session-application-open")]
    Open,
}

fn application() -> ResourceSpec {
    let mut resource = tilo_resource::offer::<ApplicationOpen>(tilo_resource::typed_resource::<
        Application,
    >(String::new(), String::new()));
    // The kind stands for whatever app a package names: neither its verb nor its resource is
    // labelled here.
    for action in &mut resource.actions {
        action.type_mut().label.clear();
        action.type_mut().description = None;
    }
    resource
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
        ty: if ty.is_any() { param_type(param) } else { ty },
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
    verb_of(action, params)
}

/// `described`: the kinds the package describes per thing ([`instances`]).
fn entry(
    (spec, declared): &(ResourceSpec, Option<Declared>),
    instances: bool,
    described: &[&str],
) -> Entry {
    let id = spec.id.as_str();
    let mut types = TypeSet::new();
    let verbs = spec
        .actions
        .iter()
        .filter(|action| !LEFT_OUT.contains(&(id, action.id.as_str())))
        // An action that acts on an item of a kind the package describes is that kind's own,
        // bound to the item: `session.size(place: p, …)` is the place's `size`. Its unbound
        // copy, published here for a caller that names the item, is no second verb of the
        // package.
        .filter(|action| {
            action
                .contract
                .item
                .as_deref()
                .is_none_or(|item| !described.contains(&item))
        })
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
            readers: None,
            verbs,
            fields: Vec::new(),
            types: types.into_types(),
            shows: Vec::new(),
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
    let instances = instances();
    let described: Vec<&str> = instances
        .iter()
        .map(|(spec, _)| spec.kind.as_str())
        .collect();
    let resources = singletons()
        .iter()
        .map(|spec| entry(spec, false, &described))
        .chain(instances.iter().map(|spec| entry(spec, true, &described)))
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
