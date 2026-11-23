//! An installed app's actions, read from the signed surface its package
//! carries: what the catalogue offers a caller of an app this device never
//! built, and what decides whether an invocation of one asks first — and the
//! application of one from the model the same package carries, the module
//! every holder of the workspace replays it with, whichever device built the
//! app.
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

use super::{
    ActionCallError, ActionFailure, ActionOutput, ActorPlace, Answer, Caller, DocumentRoute,
    Outcome, PackagedCall, TreePath, Worker, WorkspaceId, confirms, direct, unknown_action,
};

impl Worker {
    /// The `.tapp` `workspace` keeps for `app`, when its governance installs
    /// a version and this device has the package file.
    pub(super) fn installed_package(
        &self,
        workspace_id: WorkspaceId,
        app: &str,
    ) -> Option<Vec<u8>> {
        self.shared
            .link
            .get()
            .and_then(|link| link.installed_package(workspace_id, app))
    }

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

    /// Apply `request` to the resource at `route` from the model the app
    /// version's package carries, as `caller`: what the workspace's installed
    /// app offers, applied by the holder that keeps the package. The device
    /// executes the module, admits and publishes what it wrote under the
    /// caller's own actor, and answers the invocation a peer replays.
    ///
    /// What decides it is the surface the package carries: the verb's kind and
    /// whether it is destructive. An action a caller's grant refuses is
    /// refused here; one the grant leaves to the person is put to them on
    /// their own devices as an app compiled in would be, and applies on their
    /// answer.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt answered"
    )]
    pub(super) fn perform_packaged(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &tilo_actions::remote::InvokeRequest,
        route: &DocumentRoute,
        package: &[u8],
        (target, arguments, withheld): (
            &tilo_actions::remote::RemoteActionTarget,
            serde_json::Map<String, serde_json::Value>,
            &direct::Kept,
        ),
    ) -> Result<(ActionOutput, Option<ActorPlace>), Outcome> {
        let Some(link) = self.shared.link.get().cloned() else {
            return Err(unknown_action());
        };
        let surface = self
            .packaged_surface(workspace_id, &route.app_id)
            .ok_or_else(unknown_action)?;
        let (resource, verb) = find(&surface, &target.address).ok_or_else(unknown_action)?;
        let descriptor = descriptor(resource, verb);
        withheld.refuses(&target.address.resource)?;
        // The person's approve of the record this very invocation was kept as.
        let confirmed = confirms(
            &descriptor.contract.confirmation,
            request.confirmation.as_ref(),
        );
        if caller.roster_created_at().is_some() {
            // The store checks the exact item and field under current policy,
            // while it holds the workspace through admission.
            if !caller.execution_is_live()
                || caller.turn().is_none()
                || target.address.action != ui::tilo_resource::FIELD_EDIT
                || verb.kind != tilo_actions::ActionKind::Undoable
                || verb.destructive
            {
                return Err(Outcome::Rejected(ActionCallError::NotPermitted));
            }
        } else if !self.serves_as_owner(caller) {
            let who = self.bounded(caller).ok_or_else(unknown_action)?;
            if !self.admits_bounded(workspace_id, &who) {
                return Err(Outcome::Rejected(ActionCallError::NotPermitted));
            }
            let Some(permissions) = self.permissions_at(workspace_id, &who, route) else {
                return Err(Outcome::Rejected(ActionCallError::NotPermitted));
            };
            match permissions.answer(&TreePath::root(), descriptor.contract.kind) {
                Answer::Allow => {}
                Answer::Deny => return Err(Outcome::Rejected(ActionCallError::NotPermitted)),
                Answer::Ask if confirmed => {}
                Answer::Ask => {
                    // What it carries is read by the names its resource offers this caller.
                    let carries = super::carried::read(&descriptor, &arguments, |action| {
                        find(
                            &surface,
                            &ActionAddress::new(resource.kind.as_str(), action),
                        )
                        .map(|(resource, verb)| self::descriptor(resource, verb))
                    });
                    let about = self.about(workspace_id, route, &descriptor, arguments, carries);
                    return Err(self.waits_for_person(request.invocation, about));
                }
            }
        } else if descriptor.contract.requires_confirmation() && !confirmed {
            return Err(Outcome::Rejected(ActionCallError::confirmation_required()));
        }
        let now_ms = i64::try_from(tilo_clock::unix_now_ns() / 1_000_000).unwrap_or(i64::MAX);
        let answer = link
            .apply_packaged_action(
                workspace_id,
                caller,
                PackagedCall {
                    invocation: request.invocation,
                    app: &route.app_id,
                    kind: &resource.kind,
                    document: &route.document,
                    package,
                    action: &target.address.action,
                    arguments,
                    now_ms,
                },
            )
            .map_err(|reason| {
                Outcome::Failed(ActionFailure {
                    code: "packaged_action".into(),
                    message: reason,
                    details: None,
                    retryable: false,
                })
            })?;
        // The invocation is the change's, which a peer replays; the action
        // answers nothing more, as a verb that returns nothing does.
        answer
            .map(|_envelope| (ActionOutput::default(), None))
            .ok_or_else(unknown_action)
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
        FieldType::Int | FieldType::Duration => ParamType::Integer,
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
        | FieldType::Any => ParamType::Text,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]
mod tests {
    use super::*;
    use serde_json::json;
    use tilo_actions::{ActionArgs, ActionKind};

    #[test]
    fn packaged_descriptor_binds_duration_surface_params_as_integer_seconds() {
        let duration_schema =
            json!({"type": "integer", "description": "Duration in whole seconds"});
        let surface = Surface {
            kind: "tl.so.tilo.test.Board".to_owned(),
            label: "Board".to_owned(),
            version: 1,
            person: false,
            verbs: vec![VerbSurface {
                id: "defer".to_owned(),
                name: "defer".to_owned(),
                label: "Defer".to_owned(),
                description: None,
                kind: ActionKind::Undoable,
                destructive: false,
                incoming_message: false,
                params: vec![
                    ParamSurface {
                        name: "span".to_owned(),
                        required: true,
                        schema: duration_schema.clone(),
                        ty: FieldType::Duration,
                        within: None,
                    },
                    ParamSurface {
                        name: "extra".to_owned(),
                        required: false,
                        schema: duration_schema,
                        ty: FieldType::Option(Box::new(FieldType::Duration)),
                        within: None,
                    },
                ],
                returns: None,
                writes: vec![tilo_actions::surface::RESOURCE_UNIT.to_owned()],
                field: None,
            }],
            fields: Vec::new(),
            types: Vec::new(),
            shows: Vec::new(),
        };
        let desc = descriptor(&surface, &surface.verbs[0]);
        assert_eq!(desc.params[0].ty, ParamType::Integer);
        assert_eq!(desc.params[1].ty, ParamType::Integer);

        let bound = ActionArgs::bind_json(
            &desc.params,
            json!({"span": 5400, "extra": 900}).as_object().unwrap(),
        )
        .expect("whole seconds bind as integer arguments");
        assert_eq!(bound.integer("span"), Some(5400));
        assert_eq!(bound.integer("extra"), Some(900));

        for wrong in [
            json!({"span": true}),
            json!({"span": "90m"}),
            json!({"span": 1.5}),
            json!({"span": 5400, "extra": {"some": 900}}),
        ] {
            assert!(
                ActionArgs::bind_json(&desc.params, wrong.as_object().unwrap()).is_err(),
                "wrong JSON type {wrong} must be refused"
            );
        }
    }
}
