//! Publishing tilolang resources on the action door (implementation design 7.4).
//!
//! Every resource in the room record is published as a `tilo_actions::Resource` of kind
//! `tl.<app>.<Type>`, with one typed action per tilolang action whose subject is its type. The
//! desktop agent, the runtime door, shortcuts and grants therefore reach tilolang actions with
//! no agent-side code. The build closure of each action runs the wasm **planner** and encodes
//! the resulting [`DynAction`].

use std::sync::Arc;

use serde_json::Value as Json;
use tilo_actions::resource::INVALID_ARGUMENTS_TYPE;
use tilo_actions::{
    ActionArgs, ActionParam, ActionSpec, EncodedAction, ExecutionLocus, OperationKind, ParamType,
    Resource,
};

use crate::action::DynAction;
use crate::meta::{ActionMeta, PackageMeta, ParamMeta};
use crate::surface::{CATALOGUE, HOST_KIND, RUN_BLOCK, RUN_FLOW, arguments, schema_of_type};

/// What the planner is asked to run.
#[derive(Clone, Debug)]
pub struct PlanRequest<'a> {
    pub app: &'a str,
    /// The kind the action was invoked on.
    pub kind: &'a str,
    pub action_id: &'a str,
    /// The subject resource's id, when the action has one.
    pub resource: Option<&'a str>,
    /// The arguments beyond the subject, by parameter name.
    pub args: &'a serde_json::Map<String, Json>,
}

/// Runs a tilolang action as a planner and records what it wrote.
///
/// The implementation is the host's runtime: a transaction over a copy of the room record
/// collects the module's mutations as edits, and nothing is committed here.
pub trait Planner: Send + Sync {
    /// # Errors
    /// Why the action cannot run: a trap, a refused argument, an unknown action.
    fn plan(&self, request: &PlanRequest<'_>) -> Result<DynAction, String>;
}

/// The id of the published resource for the tilolang resource `id` of `app`.
#[must_use]
pub fn resource_id(app: &str, id: &str) -> String {
    format!("tl:{app}:{id}")
}

/// The id of a package's own resource, for its record-less actions.
#[must_use]
pub fn package_resource_id(app: &str) -> String {
    format!("tl:{app}")
}

/// The id of the host resource.
pub const HOST_RESOURCE: &str = "tl:host";

fn param_type(meta: &PackageMeta, ty: &str) -> ParamType {
    let ty = ty.trim();
    let ty = ty
        .strip_prefix("Option<")
        .and_then(|rest| rest.strip_suffix('>'))
        .map_or(ty, str::trim);
    match ty {
        "Int" => ParamType::Integer,
        "Float" => ParamType::Number,
        "Bool" => ParamType::Boolean,
        "Text" => ParamType::Text,
        other if meta.type_named(other).is_some() => ParamType::Text,
        other if other.starts_with("Map<") => ParamType::Object,
        _ => ParamType::Array,
    }
}

fn action_param(meta: &PackageMeta, param: &ParamMeta) -> ActionParam {
    let ty = param_type(meta, &param.ty);
    let description = format!("{}: {}", param.name, param.ty);
    let optional = param.ty.trim().starts_with("Option<");
    let built = if optional {
        ActionParam::optional_owned(param.name.clone(), ty, description)
    } else {
        ActionParam::required_owned(param.name.clone(), ty, description)
    };
    built.with_schema(schema_of_type(meta, &param.ty))
}

/// The typed action spec of `action`, whose build runs `planner`.
///
/// `subject` is the id of the resource the action is published on, when it has one.
#[must_use]
pub fn action_spec(
    meta: &PackageMeta,
    kind: &str,
    subject: Option<&str>,
    action: &ActionMeta,
    planner: &Arc<dyn Planner>,
) -> ActionSpec {
    let params: Vec<ActionParam> = arguments(meta, &action.params)
        .iter()
        .map(|param| action_param(meta, param))
        .collect();
    let (app, kind, id) = (meta.app.clone(), kind.to_owned(), action.id.clone());
    let subject = subject.map(str::to_owned);
    let planner = Arc::clone(planner);
    let build = move |args: &ActionArgs| {
        let json = args.to_json();
        let request = PlanRequest {
            app: &app,
            kind: &kind,
            action_id: &id,
            resource: subject.as_deref(),
            args: &json,
        };
        planner
            .plan(&request)
            .and_then(|planned| planned.encode().map_err(|error| error.to_string()))
            .unwrap_or_else(|message| EncodedAction {
                type_id: INVALID_ARGUMENTS_TYPE.to_owned(),
                payload: message.into_bytes(),
            })
    };
    let placeholder = action.doc.clone().unwrap_or_else(|| action.name.clone());
    let mut spec = ActionSpec::typed(
        action.id.clone(),
        action.name.clone(),
        placeholder,
        ExecutionLocus::ClientLocal,
        params,
        build,
    )
    .of_kind(action.class.into());
    if let Some(doc) = &action.doc {
        spec = spec.with_description(doc.clone());
    }
    spec
}

/// The resource `id` of the type `type_name`, published with the actions of that type.
#[must_use]
pub fn publish_record(
    meta: &PackageMeta,
    type_name: &str,
    id: &str,
    label: &str,
    planner: &Arc<dyn Planner>,
) -> Resource {
    let kind = meta.kind_of(type_name);
    let mut resource = Resource::new(resource_id(&meta.app, id), kind.clone(), label);
    for spec in edit_specs(meta, type_name, id) {
        resource = resource.with_action(spec);
    }
    for action in &meta.actions {
        let on_type = meta
            .subject_of(&action.params)
            .is_some_and(|subject| subject.name == type_name);
        if on_type {
            resource = resource.with_action(action_spec(meta, &kind, Some(id), action, planner));
        }
    }
    resource
}

/// The package's own resource, carrying the actions that take no record.
#[must_use]
pub fn publish_package(meta: &PackageMeta, planner: &Arc<dyn Planner>) -> Resource {
    let kind = meta.package_kind();
    let mut resource = Resource::new(package_resource_id(&meta.app), kind.clone(), &meta.title);
    for action in &meta.actions {
        if meta.subject_of(&action.params).is_none() {
            resource = resource.with_action(action_spec(meta, &kind, None, action, planner));
        }
    }
    resource
}

/// The host's own resource: codemode blocks, flows and the catalogue.
///
/// They are effects the host widget starts, since they compile source and read every package.
#[must_use]
pub fn host_resource() -> Resource {
    let source = || {
        vec![ActionParam::required(
            "source",
            ParamType::Text,
            "tilolang source",
        )]
    };
    let operation = |id: &'static str, label: &'static str, params: Vec<ActionParam>| {
        let type_id = format!("tl.host.{id}");
        ActionSpec::typed_operation(
            id,
            label,
            label,
            OperationKind::Effect,
            type_id.clone(),
            params,
            move |args| {
                EncodedAction::encode_operation(type_id.clone(), &args.to_json()).unwrap_or(
                    EncodedAction {
                        type_id: INVALID_ARGUMENTS_TYPE.to_owned(),
                        payload: Vec::new(),
                    },
                )
            },
        )
    };
    Resource::new(HOST_RESOURCE, HOST_KIND, "tilolang")
        .with_action(operation(RUN_BLOCK, "Run a block", source()))
        .with_action(operation(RUN_FLOW, "Run a flow", source()))
        .with_action(
            operation(CATALOGUE, "List the catalogue", Vec::new())
                .of_kind(tilo_actions::ActionKind::Read),
        )
}

/// The plain edit a bound control makes: set `value` into the field `field` of the resource
/// `id` of the type `type_name`. It needs no planner, since it is not module code.
///
/// # Errors
/// The type or field is not registered, or the value does not fit the field.
pub fn edit_action(
    meta: &PackageMeta,
    type_name: &str,
    id: &str,
    field: &str,
    value: &Json,
) -> Result<DynAction, String> {
    use crate::schema::{MergeKind, lookup};
    use tilo_resource::{Edit, EditOp, Segment};

    let declared = meta
        .type_named(type_name)
        .ok_or_else(|| format!("the package declares no type `{type_name}`"))?;
    let schema =
        lookup(&declared.id).ok_or_else(|| format!("the type `{type_name}` is not registered"))?;
    let schema_field = schema
        .field(field)
        .ok_or_else(|| format!("the type `{type_name}` has no field `{field}`"))?;
    let op = match (schema_field.kind, value) {
        (MergeKind::Text, Json::String(text)) => EditOp::Replace { text: text.clone() },
        (MergeKind::Register, value) => EditOp::Set {
            value: value.clone(),
        },
        _ => return Err(format!("the field `{field}` cannot be set to that value")),
    };
    let action_id = if value.is_boolean() {
        crate::action::EDIT_FLAG_ACTION
    } else {
        crate::action::EDIT_TEXT_ACTION
    };
    Ok(DynAction {
        app: meta.app.clone(),
        kind: meta.kind_of(type_name),
        action_id: action_id.to_owned(),
        args: serde_json::json!({"field": field, "value": value}),
        authorship: crate::action::Authorship {
            module: meta.module_hash.clone(),
            ..crate::action::Authorship::default()
        },
        edits: vec![crate::action::DynEdit::Edit {
            id: id.to_owned(),
            edit: Edit::new(vec![Segment::Field(field.to_owned())], op),
        }],
    })
}

/// The two direct edit actions of the resource `id`: a bound text input and a bound checkbox.
#[must_use]
pub fn edit_specs(meta: &PackageMeta, type_name: &str, id: &str) -> [ActionSpec; 2] {
    let make = |action: &'static str, label: &'static str, ty: ParamType| {
        let (meta, type_name, id) = (meta.clone(), type_name.to_owned(), id.to_owned());
        ActionSpec::typed(
            action,
            label,
            label,
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required("field", ParamType::Text, "The field id"),
                ActionParam::required("value", ty, "The new value"),
            ],
            move |args: &ActionArgs| {
                let json = args.to_json();
                let field = json.get("field").and_then(Json::as_str).unwrap_or_default();
                let value = json.get("value").cloned().unwrap_or(Json::Null);
                edit_action(&meta, &type_name, &id, field, &value)
                    .and_then(|planned| planned.encode().map_err(|error| error.to_string()))
                    .unwrap_or_else(|message| EncodedAction {
                        type_id: INVALID_ARGUMENTS_TYPE.to_owned(),
                        payload: message.into_bytes(),
                    })
            },
        )
    };
    [
        make(
            crate::action::EDIT_TEXT_ACTION,
            "Edit a text field",
            ParamType::Text,
        ),
        make(
            crate::action::EDIT_FLAG_ACTION,
            "Set a flag field",
            ParamType::Boolean,
        ),
    ]
}
