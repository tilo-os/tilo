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
    Action, ActionArgs, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec,
    Availability, ConfirmationPolicy, DiscoveryScope, Doc, EncodedAction, ExecutionLocus,
    ExecutionMode, OperationKind, ParamType, PermissionRequirement, Resource, ResultSpec,
    RetryPolicy, RiskLevel,
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

/// Read one collaborative route override from the shared document.
pub fn route_override(doc: &Doc, app_id: &str) -> Option<Route> {
    let (_, map) = doc.get(&ROOT, ROOT_KEY).ok().flatten()?;
    match doc.get(&map, app_id).ok().flatten()?.0.to_str()? {
        "Wine" => Some(Route::Wine),
        "Proton" => Some(Route::Proton),
        "Windows VM" => Some(Route::Vm),
        _ => None,
    }
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
    let apps: Vec<Resource> = catalogue
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
                    execution: ExecutionMode::Background { cancellable: false },
                    discovery: DiscoveryScope::Launchable,
                    result: ResultSpec::Job,
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
                    execution: ExecutionMode::Background { cancellable: false },
                    result: ResultSpec::Job,
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
                        execution: ExecutionMode::Background { cancellable: false },
                        result: ResultSpec::Job,
                        ..ActionContract::default()
                    },
                ));
            }
            if app.route == Route::Vm && app.launch_available {
                let app_id = app.id.clone();
                resource = resource.with_action(
                    ActionSpec::typed_operation(
                        "open-file",
                        "Open file in Windows",
                        "path (omit to use the system file picker)",
                        OperationKind::Effect,
                        "windows.open-file",
                        vec![ActionParam::optional_owned(
                            "path",
                            ParamType::Text,
                            "Direct path; omit to use the system file picker",
                        )],
                        move |args: &ActionArgs| {
                            EncodedAction::encode_operation(
                                "windows.open-file",
                                &vec![
                                    app_id.clone(),
                                    args.text("path").unwrap_or("").to_string(),
                                ],
                            )
                            .unwrap_or_else(|_| EncodedAction {
                                type_id: "windows.open-file".into(),
                                payload: Vec::new(),
                            })
                        },
                    )
                    .execution(ExecutionMode::Background { cancellable: false })
                    .returns(ResultSpec::Job),
                );
            }
            resource
        })
        .collect();
    let mut manager = Resource::new("windows.manager", "windows.manager", "Windows manager");
    manager = manager.with_action(
        ActionSpec::typed_operation(
            "install",
            "Install Windows application",
            "path (omit to use the system file picker)",
            OperationKind::Effect,
            "windows.install",
            vec![ActionParam::optional_owned(
                "path",
                ParamType::Text,
                "installer path",
            )],
            |args: &ActionArgs| {
                EncodedAction::encode_operation("windows.install", &args.text("path").unwrap_or(""))
                    .unwrap_or_else(|_| EncodedAction {
                        type_id: "windows.install".into(),
                        payload: Vec::new(),
                    })
            },
        )
        .requiring(PermissionRequirement::Admin)
        .privileged()
        .execution(ExecutionMode::Background { cancellable: false })
        .returns(ResultSpec::Job),
    );
    if let Some(confirmation) = &catalogue.vm_install_confirmation {
        let contract = || ActionContract {
            permission: PermissionRequirement::Admin,
            risk: RiskLevel::Privileged,
            confirmation: ConfirmationPolicy::Confirm,
            retry: RetryPolicy::AtMostOnce,
            execution: ExecutionMode::Background { cancellable: false },
            result: ResultSpec::Job,
            ..ActionContract::default()
        };
        manager = manager.with_child(
            Resource::new(
                "windows.pending-install",
                "windows.pending-install",
                format!("Install {}", confirmation.display_name),
            )
            .with_action(effect(
                "install-private-capsule",
                "Install in a private capsule",
                "windows.confirm-capsule-install",
                "",
                contract(),
            ))
            .with_action(effect(
                "install-windows-appliance",
                "Install in the Windows appliance",
                "windows.confirm-vm-install",
                "",
                contract(),
            )),
        );
    }

    let mut appliance = Resource::new(
        "windows.appliance",
        "windows.appliance",
        "Managed Windows appliance",
    );
    appliance = appliance.with_action(
        ActionSpec::typed_operation(
            "import-iso",
            "Import Windows ISO",
            "path (omit to use the system file picker)",
            OperationKind::Effect,
            "windows.import-iso",
            vec![ActionParam::optional_owned(
                "path",
                ParamType::Text,
                "ISO path",
            )],
            |args: &ActionArgs| {
                EncodedAction::encode_operation(
                    "windows.import-iso",
                    &args.text("path").unwrap_or(""),
                )
                .unwrap_or_else(|_| EncodedAction {
                    type_id: "windows.import-iso".into(),
                    payload: Vec::new(),
                })
            },
        )
        .requiring(PermissionRequirement::Admin)
        .privileged()
        .execution(ExecutionMode::Background { cancellable: false })
        .returns(ResultSpec::Job),
    );
    for (id, label, type_id) in [
        ("recovery", "Open recovery desktop", "windows.recovery"),
        ("start", "Start Windows", "windows.start"),
        ("restart", "Restart Windows", "windows.restart"),
        ("suspend", "Suspend Windows", "windows.suspend"),
        ("resume", "Resume Windows", "windows.resume"),
        ("shutdown", "Shut down Windows", "windows.shutdown"),
        (
            "temporary-network",
            "Connect temporarily for activation",
            "windows.temporary-network",
        ),
        (
            "update-network",
            "Connect temporarily for Windows Update",
            "windows.update-network",
        ),
        (
            "revoke-network",
            "Disconnect temporary network access",
            "windows.revoke-network",
        ),
    ] {
        appliance = appliance.with_action(effect(
            id,
            label,
            type_id,
            "",
            ActionContract {
                permission: PermissionRequirement::Admin,
                risk: RiskLevel::Privileged,
                confirmation: ConfirmationPolicy::Confirm,
                retry: RetryPolicy::AtMostOnce,
                execution: ExecutionMode::Background { cancellable: false },
                result: ResultSpec::Job,
                availability: availability(
                    catalogue.vm.ready,
                    Some("Set up the managed Windows appliance first"),
                ),
                ..ActionContract::default()
            },
        ));
    }
    manager = manager.with_child(appliance).with_children(apps);
    vec![manager]
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
        let flattened = qos_actions::flatten(&res);
        let manager = res.first().expect("manager root");
        assert!(
            manager
                .children
                .iter()
                .filter(|resource| resource.kind == "windows.compat-app")
                .all(|resource| resource.actions.iter().any(|action| action.id == "launch"))
        );
        assert!(flattened.iter().map(|action| action.action).any(|action| {
            action.id == "delete-data" && action.contract.risk == RiskLevel::Destructive
        }));
    }

    #[test]
    fn every_background_operation_declares_a_job_result() {
        let resources = provide_resources(&crate::provider::SampleProvider::new().catalogue());
        for action in qos_actions::flatten(&resources)
            .into_iter()
            .map(|action| action.action)
        {
            if matches!(action.contract.execution, ExecutionMode::Background { .. }) {
                assert_eq!(action.contract.result, ResultSpec::Job, "{}", action.id);
                assert!(
                    action.contract.validate_declaration().is_empty(),
                    "{}",
                    action.id
                );
            }
        }
    }

    #[test]
    fn resources_form_a_manager_appliance_application_hierarchy() {
        let resources = provide_resources(&crate::provider::SampleProvider::new().catalogue());
        let manager = resources.first().expect("manager root");
        assert_eq!(manager.id.as_str(), "windows.manager");
        let appliance = manager
            .children
            .iter()
            .find(|resource| resource.id.as_str() == "windows.appliance")
            .expect("appliance child");
        for expected in [
            "start",
            "restart",
            "suspend",
            "resume",
            "shutdown",
            "temporary-network",
            "update-network",
            "revoke-network",
        ] {
            assert!(
                appliance.actions.iter().any(|action| action.id == expected),
                "missing appliance action {expected}"
            );
        }
        assert_eq!(
            manager
                .children
                .iter()
                .filter(|resource| resource.kind == "windows.compat-app")
                .count(),
            3
        );
    }
}
