//! Palette/automation contract for Windows compatibility decisions.
//!
//! Launch, VM resume, permission changes, repair, and snapshot operations are
//! external service calls and therefore are deliberately not CRDT actions. The
//! selected route is the one durable, user-authored policy value, so it is a
//! replayable action. This keeps collaboration from becoming remote process
//! control while allowing a team to agree on a launch policy.
use crate::model::{Catalogue, Route};
use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
use qos_actions::{
    Action, ActionContract, ActionError, ActionRegistry, ActionSpec, Availability,
    ConfirmationPolicy, DiscoveryScope, Doc, EncodedAction, ExecutionLocus, ExecutionMode,
    OperationKind, PermissionRequirement, Resource, ResultSpec, RetryPolicy, RiskLevel,
};
use serde::{Deserialize, Serialize};

pub const WINDOWS_ACTION_TYPE: &str = "windows.compat.action";
const ROOT_KEY: &str = "windows-compat";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowsAction {
    SetRoute { app_id: String, route: Route },
    ResetRoute { app_id: String },
}

impl Action for WindowsAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let map = match doc.get(&ROOT, ROOT_KEY)? {
            Some((automerge::Value::Object(ObjType::Map), id)) => id,
            _ => doc.put_object(&ROOT, ROOT_KEY, ObjType::Map)?,
        };
        match self {
            Self::SetRoute { app_id, route } => doc.put(&map, app_id, route.label())?,
            Self::ResetRoute { app_id } => {
                doc.delete(&map, app_id)?;
            }
        }
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

pub fn register(registry: &mut ActionRegistry) {
    registry.register::<WindowsAction>(WINDOWS_ACTION_TYPE);
}
pub fn encode(action: &WindowsAction) -> EncodedAction {
    EncodedAction::encode(WINDOWS_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: WINDOWS_ACTION_TYPE.into(),
        payload: Vec::new(),
    })
}
fn immediate(id: &str, label: &str, action: &WindowsAction) -> ActionSpec {
    ActionSpec::immediate(id, label, WINDOWS_ACTION_TYPE, action).unwrap_or_else(|_| {
        ActionSpec::immediate_encoded(id, label, action.locus(), encode(action))
    })
}

fn availability(available: bool, reason: Option<&str>) -> Availability {
    if available {
        Availability::Available
    } else {
        Availability::Unavailable {
            code: "unavailable".into(),
            reason: reason
                .unwrap_or("This operation is not available right now")
                .into(),
        }
    }
}

fn effect(
    id: &str,
    label: &str,
    type_id: &str,
    payload: &str,
    contract: ActionContract,
) -> ActionSpec {
    ActionSpec::operation(id, label, OperationKind::Effect, type_id, &payload)
        .expect("Windows string payload is serializable")
        .with_contract(contract)
}

pub fn provide_resources(catalogue: &Catalogue) -> Vec<Resource> {
    catalogue
        .apps
        .iter()
        .map(|app| {
            let mut resource = Resource::new(
                format!("windows.app:{}", app.id),
                "windows.compat-app",
                &app.name,
            );
            for route in Route::ALL {
                resource = resource.with_action(immediate(
                    &format!("route-{}", route.short().to_ascii_lowercase()),
                    &format!("Use {}", route.label()),
                    &WindowsAction::SetRoute {
                        app_id: app.id.clone(),
                        route,
                    },
                ));
            }
            if app.uses_override() {
                resource = resource.with_action(immediate(
                    "reset-route",
                    "Use catalogue recommendation",
                    &WindowsAction::ResetRoute {
                        app_id: app.id.clone(),
                    },
                ));
            }
            resource = resource.with_action(effect(
                "launch",
                "Launch",
                "windows.launch",
                &app.id,
                ActionContract {
                    availability: availability(
                        app.launch_available,
                        app.launch_unavailable_reason.as_deref(),
                    ),
                    retry: RetryPolicy::AtMostOnce,
                    discovery: DiscoveryScope::Launchable,
                    result: ResultSpec::None,
                    ..ActionContract::default()
                },
            ));
            resource = resource.with_action(effect(
                "launch-vm-fallback",
                "Open in Windows appliance",
                "windows.launch-vm-fallback",
                &app.id,
                ActionContract {
                    availability: availability(
                        app.vm_fallback_available,
                        app.vm_fallback_unavailable_reason.as_deref(),
                    ),
                    risk: RiskLevel::Privileged,
                    permission: PermissionRequirement::Admin,
                    confirmation: ConfirmationPolicy::Confirm,
                    retry: RetryPolicy::AtMostOnce,
                    ..ActionContract::default()
                },
            ));
            for (id, label, type_id, enabled, payload, confirmation, risk) in [
                ("publish", "Publish launcher", "windows.publish", app.can_publish_vm_candidate, app.id.as_str(), ConfirmationPolicy::None, RiskLevel::Low),
                ("retry-install", "Retry installation", "windows.retry-install", app.can_retry_install, app.capsule.as_str(), ConfirmationPolicy::Confirm, RiskLevel::Privileged),
                ("escalate-install", "Install in Windows appliance", "windows.escalate-install", app.can_escalate_install_to_vm, app.capsule.as_str(), ConfirmationPolicy::Confirm, RiskLevel::Privileged),
                ("remove-launcher", "Remove launcher", "windows.remove-launcher", app.can_remove_launcher, app.id.as_str(), ConfirmationPolicy::Confirm, RiskLevel::Destructive),
                ("uninstall", "Uninstall", "windows.uninstall", app.can_vendor_uninstall, app.id.as_str(), ConfirmationPolicy::Confirm, RiskLevel::Destructive),
                ("delete-data", "Delete application data", "windows.delete-data", app.can_delete_data, app.capsule.as_str(), ConfirmationPolicy::Phrase { phrase: app.name.clone() }, RiskLevel::Destructive),
            ] {
                resource = resource.with_action(effect(
                    id,
                    label,
                    type_id,
                    payload,
                    ActionContract {
                        availability: availability(enabled, Some("The service did not offer this operation for the current installation state")),
                        permission: PermissionRequirement::Admin,
                        risk,
                        confirmation,
                        retry: RetryPolicy::AtMostOnce,
                        execution: ExecutionMode::Immediate,
                        result: ResultSpec::None,
                        ..ActionContract::default()
                    },
                ));
            }
            resource
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::CompatibilityProvider;
    #[test]
    fn route_policy_is_deterministic_and_reset_removes_it() {
        let mut doc = Doc::new();
        WindowsAction::SetRoute {
            app_id: "reader".into(),
            route: Route::Proton,
        }
        .apply(&mut doc)
        .unwrap();
        let (_, map) = doc.get(&ROOT, ROOT_KEY).unwrap().unwrap();
        assert_eq!(
            doc.get(&map, "reader").unwrap().unwrap().0.to_str(),
            Some("Proton")
        );
        WindowsAction::ResetRoute {
            app_id: "reader".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert!(doc.get(&map, "reader").unwrap().is_none());
    }
    #[test]
    fn every_catalogue_app_has_explicit_routes() {
        let res = provide_resources(&crate::provider::SampleProvider::new().catalogue());
        assert!(res.iter().all(|r| r.actions.len() >= 3));
        assert!(
            res.iter()
                .all(|resource| resource.actions.iter().any(|action| action.id == "launch"))
        );
        assert!(
            res.iter()
                .flat_map(|resource| &resource.actions)
                .any(|action| {
                    action.id == "delete-data" && action.contract.risk == RiskLevel::Destructive
                })
        );
    }
}
