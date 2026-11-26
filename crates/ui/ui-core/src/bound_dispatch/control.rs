//! What a bound control invokes, resolved without executing it: the resource
//! an address still names, the spec it declares, and the identity, label and
//! continuation arguments the history and palette read back.

use tilo_actions::ResourceSpec;
use tilo_ui_base::widget::Widget;

use super::{
    AutoCommit, BoundActionError, DeferredIntent, PreparedActionIntent, collect_unbound_resources,
    discover_bound_resources_with_filter, encode_resource_action, filter_resource, local_target,
    resolve_unbound_resource, validate_deferred_arguments,
};

fn control_input_surface(spec: &tilo_actions::ActionSpec) -> ControlInputSurface {
    match spec.input() {
        tilo_actions::ActionInput::Immediate(_) => ControlInputSurface::Immediate,
        tilo_actions::ActionInput::Deferred { type_id } => ControlInputSurface::Deferred {
            type_id: type_id.to_owned(),
        },
        tilo_actions::ActionInput::Prompted {
            prompt,
            placeholder,
            collects,
        } => ControlInputSurface::Prompted {
            prompt: prompt.to_owned(),
            placeholder: placeholder.to_owned(),
            collects,
        },
    }
}

#[derive(Clone)]
pub(crate) struct ControlObservation {
    identity: ControlIdentity,
    intent: PreparedActionIntent,
    confirmation: tilo_actions::ConfirmationPolicy,
    /// What the action is called, as its declaration says ("Mark as read"): how a held control is named
    /// to people.
    label: String,
}

impl PartialEq for ControlObservation {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
            && self.intent == other.intent
            && self.confirmation == other.confirmation
    }
}

#[derive(Clone, PartialEq, Eq)]
enum ControlInputSurface {
    Immediate,
    Deferred {
        type_id: String,
    },
    Prompted {
        prompt: String,
        placeholder: String,
        collects: tilo_actions::Collects,
    },
}

#[derive(Clone, PartialEq)]
pub(crate) struct ControlIdentity {
    continuation: tilo_actions::ContinuationEvidence,
    locus: tilo_actions::ExecutionLocus,
    kind: tilo_actions::OperationKind,
    operation_type: Option<String>,
    risk: tilo_actions::RiskLevel,
    retry: tilo_actions::RetryPolicy,
    execution: tilo_actions::ExecutionMode,
    result: tilo_actions::ResultSpec,
    tags: Vec<String>,
    params: Vec<tilo_actions::ActionParam>,
    input_surface: ControlInputSurface,
}

impl ControlObservation {
    pub(crate) fn identity(&self) -> &ControlIdentity {
        &self.identity
    }
    pub(crate) fn label(&self) -> &str {
        &self.label
    }
}

#[derive(Clone)]
pub(crate) enum ControlContinuationArguments {
    Immediate,
    Text(String),
    Json(serde_json::Map<String, serde_json::Value>),
}

impl ControlContinuationArguments {
    pub(crate) fn as_api_arguments(
        &self,
    ) -> (
        Option<&str>,
        Option<&serde_json::Map<String, serde_json::Value>>,
    ) {
        match self {
            Self::Immediate => (None, None),
            Self::Text(text) => (Some(text), None),
            Self::Json(json) => (None, Some(json)),
        }
    }
    pub(super) fn query(&self) -> tilo_actions::ContinuationArguments<'_> {
        match self {
            Self::Immediate => tilo_actions::ContinuationArguments::Immediate,
            Self::Text(value) => tilo_actions::ContinuationArguments::Text(value),
            Self::Json(arguments) => tilo_actions::ContinuationArguments::Json(arguments),
        }
    }
}

/// Re-resolve one exact ownership target and derive inert comparison data.
/// The returned encoding is never executed.
pub(crate) fn control_observation(
    root: &dyn Widget,
    doc: &AutoCommit,
    target: &local_target::LocalResourceTarget,
    invocation: &tilo_actions::ControlInvocation,
) -> Result<ControlObservation, tilo_actions::ApiError> {
    control_observation_with_arguments(
        root,
        doc,
        target,
        invocation,
        &ControlContinuationArguments::Json(invocation.arguments.clone()),
    )
}

pub(crate) fn control_observation_with_arguments(
    root: &dyn Widget,
    doc: &AutoCommit,
    target: &local_target::LocalResourceTarget,
    invocation: &tilo_actions::ControlInvocation,
    original_arguments: &ControlContinuationArguments,
) -> Result<ControlObservation, tilo_actions::ApiError> {
    let resources = resources_for_control_target(root, doc, target, &invocation.address)?;
    let (resource, spec) = unique_control_spec(&resources, &invocation.address)?;
    let (argument, arguments) = match original_arguments {
        ControlContinuationArguments::Immediate => (None, None),
        ControlContinuationArguments::Text(value) => (Some(value.as_str()), None),
        ControlContinuationArguments::Json(arguments) => (None, Some(arguments)),
    };
    let (intent, continuation) =
        if matches!(spec.input(), tilo_actions::ActionInput::Deferred { .. }) {
            validate_deferred_arguments(spec, arguments, argument)?;
            let intent = DeferredIntent::capture(spec)?;
            let continuation = intent.draft().clone();
            (PreparedActionIntent::Deferred(intent), continuation)
        } else {
            let encoded =
                encode_resource_action(&resources, &invocation.address, arguments, argument)?;
            let continuation = spec
                .continuation_identity(tilo_actions::ContinuationQuery::Exact(
                    original_arguments.query(),
                ))
                .ok_or_else(|| tilo_actions::ApiError::Unavailable {
                    code: "unsupported_continuation_identity".into(),
                    reason:
                        "The action provider cannot prove the original target and handler identity"
                            .into(),
                })?;
            (PreparedActionIntent::Encoded(encoded), continuation)
        };
    Ok(ControlObservation {
        identity: control_identity_from_spec(resource, spec, continuation),
        intent,
        confirmation: spec.contract.confirmation.clone(),
        label: spec.label.clone(),
    })
}

/// What a control's action is declared as, for people: its name ("Mark as read") and whether it is
/// housekeeping.
pub(crate) struct ControlDeclaration {
    pub(crate) label: Option<String>,
    pub(crate) housekeeping: bool,
}

/// [`ControlDeclaration`] of the action a control invokes, when `address` still resolves to one.
pub(crate) fn control_declaration(
    root: &dyn Widget,
    doc: &AutoCommit,
    target: &local_target::LocalResourceTarget,
    address: &tilo_actions::ActionAddress,
) -> Option<ControlDeclaration> {
    let resources = resources_for_control_target(root, doc, target, address).ok()?;
    let (_, spec) = unique_control_spec(&resources, address).ok()?;
    Some(ControlDeclaration {
        label: Some(spec.label.trim().to_owned()).filter(|label| !label.is_empty()),
        housekeeping: spec
            .contract
            .tags
            .iter()
            .any(|tag| tag == tilo_actions::HOUSEKEEPING_TAG),
    })
}

fn resources_for_control_target(
    root: &dyn Widget,
    doc: &AutoCommit,
    target: &local_target::LocalResourceTarget,
    address: &tilo_actions::ActionAddress,
) -> Result<Vec<ResourceSpec>, tilo_actions::ApiError> {
    match target {
        local_target::LocalResourceTarget::Component(target) => {
            if target.address.address != *address {
                return Err(tilo_actions::ApiError::Unknown(address.clone()));
            }
            let snapshots = discover_bound_resources_with_filter(root, doc, Some(address), false)
                .map_err(|error| BoundActionError::Dispatch(error).into_api())?;
            let snapshot = snapshots
                .into_iter()
                .find(|snapshot| snapshot.binding.same_component(&target.binding))
                .ok_or_else(|| tilo_actions::ApiError::Unknown(address.clone()))?;
            unique_control_spec(snapshot.resources(), address)?;
            let binding = snapshot.binding.clone();
            Ok(snapshot
                .into_resources()
                .into_iter()
                .filter_map(|resource| filter_resource(&binding, resource))
                .collect())
        }
        local_target::LocalResourceTarget::Outer => {
            let mut resources = Vec::new();
            collect_unbound_resources(root, doc, &mut resources);
            if tilo_actions::api::resolve(&resources, address).is_none()
                && let Some(resource) = resolve_unbound_resource(root, doc, &address.resource)
            {
                resources.push(resource);
            }
            Ok(resources)
        }
    }
}

fn unique_control_spec<'a>(
    resources: &'a [ResourceSpec],
    address: &tilo_actions::ActionAddress,
) -> Result<(&'a ResourceSpec, &'a tilo_actions::ActionSpec), tilo_actions::ApiError> {
    fn matches<'a>(
        resources: &'a [ResourceSpec],
        address: &tilo_actions::ActionAddress,
        found: &mut Vec<&'a ResourceSpec>,
    ) {
        for resource in resources {
            if resource.id.as_str() == address.resource {
                found.push(resource);
            }
            matches(&resource.children, address, found);
        }
    }
    let mut found = Vec::new();
    matches(resources, address, &mut found);
    let [resource] = found.as_slice() else {
        return if found.is_empty() {
            Err(tilo_actions::ApiError::Unknown(address.clone()))
        } else {
            Err(tilo_actions::ApiError::Unavailable {
                code: "ambiguous_action_identity".into(),
                reason: "More than one declaration in the retained owner claims this address"
                    .into(),
            })
        };
    };
    let actions = resource
        .actions
        .iter()
        .filter(|spec| spec.id == address.action)
        .collect::<Vec<_>>();
    let [spec] = actions.as_slice() else {
        return if actions.is_empty() {
            Err(tilo_actions::ApiError::Unknown(address.clone()))
        } else {
            Err(tilo_actions::ApiError::Unavailable {
                code: "ambiguous_action_identity".into(),
                reason: "More than one action declaration in the retained resource claims this id"
                    .into(),
            })
        };
    };
    Ok((*resource, *spec))
}

fn control_identity_from_spec(
    _resource: &ResourceSpec,
    spec: &tilo_actions::ActionSpec,
    continuation: tilo_actions::ContinuationEvidence,
) -> ControlIdentity {
    let input_surface = control_input_surface(spec);
    ControlIdentity {
        continuation,
        locus: spec.locus,
        kind: spec.kind,
        operation_type: spec.operation_type.clone(),
        risk: spec.contract.risk,
        retry: spec.contract.retry,
        execution: spec.contract.execution.clone(),
        result: spec.contract.result.clone(),
        tags: spec.contract.tags.clone(),
        params: spec.params().to_vec(),
        input_surface,
    }
}

/// The identity a draft accrues over the resources its control target resolves to: read by the
/// window's History.
pub(crate) fn control_draft_identity(
    root: &dyn Widget,
    doc: &AutoCommit,
    target: &local_target::LocalResourceTarget,
    address: &tilo_actions::ActionAddress,
) -> Result<ControlIdentity, tilo_actions::ApiError> {
    let resources = resources_for_control_target(root, doc, target, address)?;
    let (resource, spec) = unique_control_spec(&resources, address)?;
    let continuation = spec
        .continuation_identity(tilo_actions::ContinuationQuery::Draft)
        .ok_or_else(|| tilo_actions::ApiError::Unavailable {
            code: "unsupported_continuation_identity".into(),
            reason: "The action provider cannot prove a stable draft resolution domain".into(),
        })?;
    Ok(control_identity_from_spec(resource, spec, continuation))
}
