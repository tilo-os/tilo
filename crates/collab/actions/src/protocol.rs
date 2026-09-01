//! Runtime validation for an application's published protocol surface.
//!
//! Static checks can prove that an app opts into the model. This validator
//! proves facts about the surface it actually built: unique stable addresses,
//! registered document decoders, safe destructive declarations, usable
//! unavailable reasons, and coherent execution/result contracts.

use std::collections::HashSet;

use crate::{ActionInput, ActionRegistry, Resource};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractViolation {
    pub address: String,
    pub reason: String,
}

pub fn validate_surface(
    resources: &[Resource],
    registry: &ActionRegistry,
    operation_handlers: &HashSet<String>,
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
    validate_surface_inner(resources, registry, None)
}

fn validate_surface_inner(
    resources: &[Resource],
    registry: &ActionRegistry,
    operation_handlers: Option<&HashSet<String>>,
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

fn validate_resource(
    resource: &Resource,
    registry: &ActionRegistry,
    operation_handlers: Option<&HashSet<String>>,
    seen_resources: &mut HashSet<String>,
    seen_actions: &mut HashSet<String>,
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
        let encoded = match &action.input {
            ActionInput::Immediate(encoded) => Some(encoded),
            ActionInput::Prompted { .. } => None,
        };
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

fn issue(address: &str, reason: &str) -> ContractViolation {
    ContractViolation {
        address: address.to_string(),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionSpec, EncodedAction, OperationKind};

    #[test]
    fn rejects_duplicate_unhandled_and_unsafe_surface_entries() {
        let unsafe_effect =
            ActionSpec::operation("erase", "Erase", OperationKind::Effect, "test.erase", &())
                .unwrap()
                .with_contract(crate::ActionContract {
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
}
