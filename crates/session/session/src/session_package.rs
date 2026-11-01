//! What the session publishes, read as a package: `apps/tl/session/surface.toml`.
//!
//! A shell, the agent and the REPL call the session's actions as an app's: `use
//! so.tilo.session as session`, then `session.show_surface(surface: "launcher")` or
//! `session.focus(window: w)`. The description is the native apps' (`surface.toml`, read by
//! `tilolang_tilo::catalogue`), generated here from the very resources the session's door
//! publishes, and a test holds the committed file to it, so it never drifts from what the door
//! takes.
//!
//! Each resource the session publishes under one address says it (`id`): a call of one of its
//! actions is sent there. A kind the session publishes one resource of per thing (a window, an
//! agent's run, an app) says `instances`: it reads as a type, and its actions are called on one
//! of them.

use serde::Serialize;
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::surface::{FieldType, ParamSurface, RECORD_UNIT, Surface, VerbSurface};
use tilo_actions::{ActionSpec, ResourceSpec, RiskLevel};

/// The package id a shell `use`s.
pub const APP: &str = "so.tilo.session";

/// Where the description is committed, from the repository's root.
pub const PATH: &str = "apps/tl/session/surface.toml";

/// The actions of a resource the package leaves out, each because another resource's action
/// of the same name is the one a call of that name means: `desktop:chrome`'s
/// `switch-workspace` steps by a count, for the keys; the desk's goes to the workspace a
/// person picked, by its key.
const LEFT_OUT: &[(&str, &str)] = &[(
    tilo_desktop::desktop_actions::CHROME_RESOURCE,
    "switch-workspace",
)];

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

/// The resources the session publishes under one address each.
fn singletons() -> Vec<ResourceSpec> {
    vec![
        crate::desk_resource::desk_spec(String::new()),
        tilo_desktop::desktop_actions::chrome_resource(),
        tilo_desktop::desktop_actions::desk_resource(&[], &[], None),
        crate::arrangement::resources().swap_remove(0),
        tilo_service::offer::<tilo_agent_runs::RunsAction>(ResourceSpec::new(
            tilo_agent_runs::wire::RESOURCE,
            tilo_agent_runs::RUNS_KIND,
            String::new(),
        )),
        tilo_connectivity_service::network_resource(
            &tilo_connectivity_service::model::Network::default(),
        ),
        tilo_audio_service::sound::resource(&tilo_audio_service::sound::Sound::default()),
    ]
}

/// The kinds the session publishes one resource of per thing, each as one of them.
fn instances() -> Vec<ResourceSpec> {
    let window = tilo_desktop::desktop_actions::desk_resource(&[(0, String::new())], &[], None)
        .children
        .into_iter()
        .find(|child| child.kind == tilo_desktop::desktop_actions::WINDOW_KIND)
        .expect("the desk publishes each window as a resource of its own");
    let mut run = ResourceSpec::new(String::new(), tilo_agent_runs::RUN_KIND, String::new());
    run.actions
        .extend(<tilo_agent_runs::RunAction as tilo_resource::Effect>::specs());
    vec![window, run, application()]
}

/// An app on this device, by `app.<id>`: its own `open`, which the session carries out by
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
            .expect("an empty launch encodes")
        },
    ))
}

/// The type of a parameter as the description writes it. A resource's address is its id's
/// text; an object or a list has no type tilolang code gives, so the action is left out.
const fn field_type(ty: ParamType) -> FieldType {
    match ty {
        ParamType::Text | ParamType::Secret | ParamType::DateTime | ParamType::Resource => {
            FieldType::Text
        }
        ParamType::Integer => FieldType::Int,
        ParamType::Number => FieldType::Float,
        ParamType::Boolean => FieldType::Bool,
        ParamType::Object | ParamType::Array => FieldType::Any,
    }
}

fn param(param: &ActionParam) -> ParamSurface {
    ParamSurface {
        name: param.name.to_string(),
        required: param.required,
        schema: serde_json::Value::Null,
        ty: field_type(param.ty),
    }
}

fn verb(action: &ActionSpec) -> VerbSurface {
    VerbSurface {
        id: action.id.clone(),
        label: String::new(),
        description: None,
        kind: action.contract.settled_kind(),
        destructive: action.contract.risk == RiskLevel::Destructive,
        incoming_message: false,
        params: action.params().iter().map(param).collect(),
        writes: vec![RECORD_UNIT.to_owned()],
        field: None,
    }
}

fn entry(spec: &ResourceSpec, instances: bool) -> Entry {
    let id = spec.id.as_str();
    let verbs = spec
        .actions
        .iter()
        .filter(|action| !LEFT_OUT.contains(&(id, action.id.as_str())))
        .map(verb)
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
            types: Vec::new(),
        },
    }
}

/// The description, as `apps/tl/session/surface.toml` holds it.
///
/// # Panics
/// Never for the session's own resources: they serialize.
#[must_use]
pub fn description() -> String {
    let resources = singletons()
        .iter()
        .map(|spec| entry(spec, false))
        .chain(instances().iter().map(|spec| entry(spec, true)))
        .collect();
    let body = toml::to_string(&File { resources }).expect("the description serializes");
    format!(
        "# What the session publishes, read as the package `{APP}`.\n\
         # Generated by `tilo_session::session_package::description`; a test holds it to it.\n\n{body}"
    )
}
