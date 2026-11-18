//! An installed app's actions, read from the signed surface its package
//! carries: what the catalogue offers a caller of an app this device never
//! built, and what decides whether an invocation of one asks first.
//!
//! The package's `surface.toml` is the app version's declaration
//! ([`collab_types::surface`]): every resource kind it publishes with its
//! verbs, their kinds, their parameters and the one flag that says a verb
//! is destructive. A device that holds the workspace's keys applies those
//! verbs from the module the same package carries, so the surface is the
//! only thing the action API needs to list them and to put one that asks
//! first to the person.

use std::sync::Arc;

use collab_types::surface::SurfaceFile;
use tilo_actions::api::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::surface::{FieldType, ParamSurface, Surface, VerbSurface};
use tilo_actions::{
    ActionContract, ActionParam, ActionState, ConfirmationPolicy, ExecutionLocus, OperationKind,
    ParamType, RiskLevel,
};

use super::{Worker, WorkspaceId};

impl Worker {
    /// The signed surface of the version of `app` the workspace installs, when
    /// this device keeps its package: what lists the app's actions and decides
    /// an invocation of one. `None` for an app the workspace installs no such
    /// version of, or one this device holds no package of.
    pub(super) fn packaged_surface(
        &self,
        workspace_id: WorkspaceId,
        app: &str,
    ) -> Option<Arc<SurfaceFile>> {
        let bytes = self
            .shared
            .link
            .get()
            .and_then(|link| link.packaged_surface(workspace_id, app))?;
        SurfaceFile::parse(&bytes).ok().map(Arc::new)
    }
}

/// The descriptors of every action the surface `file` declares, in the order
/// its resources and verbs stand: what one resource of the app offers a caller.
pub(super) fn descriptors(file: &SurfaceFile) -> Vec<ActionDescriptor> {
    file.resources
        .iter()
        .flat_map(|resource| resource.verbs.iter().map(|verb| descriptor(resource, verb)))
        .collect()
}

/// The resource and verb of the surface `file` that `address` names: a verb of
/// the resource kind the address names, or of the only resource declaring the
/// action when the address names none.
pub(super) fn find<'a>(
    file: &'a SurfaceFile,
    address: &ActionAddress,
) -> Option<(&'a Surface, &'a VerbSurface)> {
    file.resources
        .iter()
        .filter(|resource| address.resource.is_empty() || resource.kind == address.resource)
        .find_map(|resource| {
            resource
                .verbs
                .iter()
                .find(|verb| verb.id == address.action)
                .map(|verb| (resource, verb))
        })
}

/// The one resource kind `file` declares the verb `action` under, when exactly
/// one does: what an invocation that names no resource is resolved to, so a
/// caller that knows the app and the action but not its code reaches it.
pub(super) fn resource_of<'a>(file: &'a SurfaceFile, action: &str) -> Option<&'a Surface> {
    let mut declaring = file
        .resources
        .iter()
        .filter(|resource| resource.verbs.iter().any(|verb| verb.id == action));
    let first = declaring.next()?;
    declaring.next().is_none().then_some(first)
}

/// One verb as the action API offers it: its address, the words its app gives
/// it, and the contract that decides it.
pub(super) fn descriptor(resource: &Surface, verb: &VerbSurface) -> ActionDescriptor {
    ActionDescriptor {
        semantic_identity: None,
        address: ActionAddress::new(resource.kind.clone(), verb.id.clone()),
        resource_kind: resource.kind.clone(),
        resource_label: resource.label.clone(),
        action_label: if verb.label.is_empty() {
            verb.id.clone()
        } else {
            verb.label.clone()
        },
        keywords: Vec::new(),
        did: None,
        icon: None,
        description: verb.description.clone(),
        state: ActionState::default(),
        locus: ExecutionLocus::ServerPreferred,
        kind: operation(verb.kind),
        contract: contract(verb),
        input: InputSpec::None,
        params: verb.params.iter().map(param).collect(),
        view: None,
    }
}

/// What a verb of `kind` changes.
fn operation(kind: tilo_actions::ActionKind) -> OperationKind {
    match kind {
        tilo_actions::ActionKind::Read => OperationKind::View,
        tilo_actions::ActionKind::Undoable => OperationKind::Document,
        tilo_actions::ActionKind::Outside => OperationKind::Effect,
    }
}

/// The contract a verb declares: its kind, the field it writes, and the risk
/// and confirmation its app's declaration carries. A destructive verb is the
/// destructive risk, which Undo only takes back when it changes the document
/// alone, so one that reaches outside asks the person first.
fn contract(verb: &VerbSurface) -> ActionContract {
    let risk = if verb.destructive {
        RiskLevel::Destructive
    } else {
        RiskLevel::Low
    };
    let kind = verb.kind;
    let undoable = kind == tilo_actions::ActionKind::Undoable;
    let confirmation = if risk.requires_confirmation_when(undoable) {
        ConfirmationPolicy::Confirm
    } else {
        ConfirmationPolicy::None
    };
    ActionContract {
        kind,
        field: verb.field.clone(),
        risk,
        confirmation,
        ..ActionContract::default()
    }
}

/// One parameter, as the surface declares it and the action API binds it.
fn param(param: &ParamSurface) -> ActionParam {
    ActionParam {
        name: param.name.clone().into(),
        ty: param_type(&param.ty),
        required: param.required,
        entry: false,
        description: String::new().into(),
        choices: Vec::new(),
        schema: (!param.schema.is_null()).then(|| Arc::new(param.schema.clone())),
        nullable: matches!(param.ty, FieldType::Option(_)),
    }
}

/// The parameter type a surface field type is written as.
fn param_type(ty: &FieldType) -> ParamType {
    match ty {
        FieldType::Int => ParamType::Integer,
        FieldType::Float => ParamType::Number,
        FieldType::Bool => ParamType::Boolean,
        FieldType::Time | FieldType::Date => ParamType::DateTime,
        FieldType::Resource(_) => ParamType::Resource,
        FieldType::Option(inner) => param_type(inner),
        FieldType::List(_) | FieldType::Set(_) | FieldType::Collection(_) => ParamType::Array,
        FieldType::Text
        | FieldType::RichText
        | FieldType::Identifier
        | FieldType::Actor
        | FieldType::Named(_)
        | FieldType::Map(_)
        | FieldType::Union(_)
        | FieldType::Reactions
        | FieldType::Outbox(_)
        | FieldType::Duration
        | FieldType::Any => ParamType::Text,
    }
}
