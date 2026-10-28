//! Runtime validation for an application's published protocol surface.
//!
//! Static checks can prove that an app opts into the model. This validator
//! proves facts about the surface it actually built: unique stable addresses,
//! registered document decoders, safe destructive declarations, usable
//! unavailable reasons, and coherent execution/result contracts.

use std::{collections::HashSet, hash::BuildHasher};

use crate::{ActionInput, ActionRegistry, Resource};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractViolation {
    pub address: String,
    pub reason: String,
}

pub fn validate_surface<S: BuildHasher>(
    resources: &[Resource],
    registry: &ActionRegistry,
    operation_handlers: &HashSet<String, S>,
) -> Vec<ContractViolation> {
    validate_surface_inner(resources, registry, Some(operation_handlers))
}

/// Validate every transport-independent invariant. Hosts call this before
/// publication even when their dynamic widget dispatch cannot enumerate its
/// effect/view handler table. Tooling that owns an explicit handler table
/// should use [`validate_surface`] for the additional coverage.
pub fn validate_surface_contracts(
    resources: &[Resource],
    registry: &ActionRegistry,
) -> Vec<ContractViolation> {
    validate_surface_inner(resources, registry, None::<&HashSet<String>>)
}

fn validate_surface_inner<S: BuildHasher>(
    resources: &[Resource],
    registry: &ActionRegistry,
    operation_handlers: Option<&HashSet<String, S>>,
) -> Vec<ContractViolation> {
    let mut seen_resources = HashSet::new();
    let mut seen_actions = HashSet::new();
    let mut violations = Vec::new();
    for resource in resources {
        validate_resource(
            resource,
            registry,
            operation_handlers,
            &mut seen_resources,
            &mut seen_actions,
            &mut violations,
        );
    }
    violations
}

fn validate_resource<S: BuildHasher>(
    resource: &Resource,
    registry: &ActionRegistry,
    operation_handlers: Option<&HashSet<String, S>>,
    seen_resources: &mut HashSet<String>,
    seen_actions: &mut HashSet<String>,
    violations: &mut Vec<ContractViolation>,
) {
    let resource_id = resource.id.as_str();
    validate_resource_identity(resource, seen_resources, violations);
    for action in &resource.actions {
        let address = format!("{}{}{}", resource_id, crate::KEY_SEP, action.id);
        if action.id.is_empty() || action.id.contains(crate::KEY_SEP) {
            violations.push(issue(
                &address,
                "action id is empty or contains the separator",
            ));
        }
        if !seen_actions.insert(address.clone()) {
            violations.push(issue(&address, "duplicate action address"));
        }
        for reason in action.contract.validate_declaration() {
            violations.push(issue(&address, &reason));
        }
        let mut parameter_names = HashSet::new();
        for parameter in action.params() {
            if !parameter.name.trim().is_empty() && !parameter_names.insert(parameter.name.as_ref())
            {
                violations.push(issue(&address, "parameter names must be unique"));
            }
            for reason in parameter.validate_declaration() {
                violations.push(issue(
                    &address,
                    &format!("parameter `{}` {reason}", parameter.name),
                ));
            }
        }
        let encoded = action.encode_immediate();
        if let Err(error) = action.validate_deferred_shape() {
            violations.push(issue(&address, &error.to_string()));
        }
        if let ActionInput::Deferred { type_id } = action.input() {
            if action.kind == crate::OperationKind::Document && !registry.contains(type_id) {
                violations.push(issue(&address, "document action type is not registered"));
            }
            if action
                .operation_type
                .as_ref()
                .is_some_and(|declared| declared != type_id)
            {
                violations.push(issue(
                    &address,
                    "deferred operation type does not match its declaration",
                ));
            }
        }
        if action.kind == crate::OperationKind::Document && action.operation_type.is_some() {
            violations.push(issue(
                &address,
                "document action declares an effect/view handler",
            ));
        }
        if action.kind != crate::OperationKind::Document && action.operation_type.is_none() {
            violations.push(issue(
                &address,
                "effect/view action has no declared operation type",
            ));
        }
        if let Some(encoded) = encoded {
            if action.kind == crate::OperationKind::Document && !registry.contains(&encoded.type_id)
            {
                violations.push(issue(&address, "document action type is not registered"));
            }
            if let Some(declared) = &action.operation_type
                && encoded.type_id != *declared
            {
                violations.push(issue(
                    &address,
                    "encoded operation type does not match its declaration",
                ));
            }
        }
        if action.kind != crate::OperationKind::Document
            && action.operation_type.as_ref().is_some_and(|type_id| {
                operation_handlers.is_some_and(|handlers| !handlers.contains(type_id))
            })
        {
            violations.push(issue(
                &address,
                "effect/view operation has no declared handler",
            ));
        }
    }
    validate_views(resource, violations);
    for child in &resource.children {
        validate_resource(
            child,
            registry,
            operation_handlers,
            seen_resources,
            seen_actions,
            violations,
        );
    }
}

fn validate_resource_identity(
    resource: &Resource,
    seen_resources: &mut HashSet<String>,
    violations: &mut Vec<ContractViolation>,
) {
    let resource_id = resource.id.as_str();
    if resource_id.is_empty() || resource.kind.is_empty() {
        violations.push(issue(resource_id, "resource id and kind must be non-empty"));
    }
    if resource_id.contains(crate::KEY_SEP) {
        violations.push(issue(
            resource_id,
            "resource id contains the address separator",
        ));
    }
    if !seen_resources.insert(resource_id.to_string()) {
        violations.push(issue(resource_id, "duplicate resource id"));
    }
}

fn validate_views(resource: &Resource, violations: &mut Vec<ContractViolation>) {
    let mut view_ids = HashSet::new();
    let mut view_open_actions = HashSet::new();
    let mut primary_views = 0usize;
    for view in &resource.views {
        let address = format!("{}{}view:{}", resource.id.as_str(), crate::KEY_SEP, view.id);
        if !view_ids.insert(view.id.as_str()) {
            violations.push(issue(&address, "duplicate view id"));
        }
        if !view_open_actions.insert(view.open_action.as_str()) {
            violations.push(issue(
                &address,
                "at most one view may reference an open action",
            ));
        }
        if view.primary {
            primary_views = primary_views.saturating_add(1);
            if primary_views > 1 {
                violations.push(issue(&address, "at most one view may be primary"));
            }
        }
        for reason in view.validate(resource) {
            violations.push(issue(&address, &reason));
        }
    }
}

fn issue(address: &str, reason: &str) -> ContractViolation {
    ContractViolation {
        address: address.to_string(),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, ResourceViewMountSpec,
        ResourceViewSpec,
    };

    /// Duplicate, unhandled and unsafe surface entries are rejected, and explicit resource-view
    /// cardinality and bindings are validated.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn protocol_surfaces_are_validated_before_publication() {
        rejects_duplicate_unhandled_and_unsafe_surface_entries();
        validates_explicit_resource_view_cardinality_and_bindings();
    }

    fn rejects_duplicate_unhandled_and_unsafe_surface_entries() {
        let unsafe_effect =
            ActionSpec::operation("erase", "Erase", OperationKind::Effect, "test.erase", &())
                .unwrap()
                .with_contract(crate::ActionContract {
                    kind: crate::ActionKind::Outside,
                    risk: crate::RiskLevel::Destructive,
                    confirmation: crate::ConfirmationPolicy::None,
                    ..crate::ActionContract::default()
                });
        let resources = vec![
            Resource::new("same", "test.item", "One").with_action(unsafe_effect),
            Resource::new("same", "test.item", "Two").with_action(ActionSpec::immediate_encoded(
                "edit",
                "Edit",
                crate::ExecutionLocus::ClientLocal,
                EncodedAction::encode_operation("test.missing", &()).unwrap(),
            )),
        ];
        let violations = validate_surface(&resources, &ActionRegistry::new(), &HashSet::new());
        let text = violations
            .iter()
            .map(|issue| issue.reason.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("duplicate resource"));
        assert!(text.contains("requires confirmation"));
        assert!(text.contains("no declared handler"));
        assert!(text.contains("not registered"));
    }

    fn validates_explicit_resource_view_cardinality_and_bindings() {
        let open =
            ActionSpec::operation("open", "Open", OperationKind::View, "test.open", &()).unwrap();
        let mount = ActionSpec::typed_operation(
            "mount",
            "Mount",
            "",
            OperationKind::Effect,
            "test.mount",
            vec![
                ActionParam::required("cap", ParamType::Text, "Capability"),
                ActionParam::optional("target", ParamType::Integer, "View"),
            ],
            |_| EncodedAction::encode_operation("test.mount", &()).unwrap(),
        )
        .with_contract(crate::ActionContract {
            confirmation: crate::ConfirmationPolicy::Confirm,
            ..crate::ActionContract::default()
        });
        let resource = Resource::new("thing:1", "test.thing", "Thing")
            .with_action(open)
            .with_action(mount)
            .with_view(
                ResourceViewSpec::new("editor", "Editor", "open")
                    .with_primary(true)
                    .with_mount(ResourceViewMountSpec::new("mount", "cap", "target")),
            )
            .with_view(ResourceViewSpec::new("preview", "Preview", "open").with_primary(true));

        let text = validate_surface_contracts(&[resource], &ActionRegistry::new())
            .into_iter()
            .map(|issue| issue.reason)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("at most one view may reference an open action"));
        assert!(text.contains("at most one view may be primary"));
        assert!(text.contains("mount action `mount` must be a view operation"));
        assert!(text.contains("mount action `mount` must not require confirmation"));
        assert!(text.contains("mount action `mount` must not require caller parameters"));
        assert!(text.contains("mount capability parameter `cap` must name an optional text"));
        assert!(text.contains("mount view parameter `target` must name an optional text"));
    }
}
