//! Palette/automation contract for Windows compatibility decisions.
//!
//! Launch, VM resume, permission changes, repair, and snapshot operations are
//! external service calls and therefore are deliberately not CRDT actions. The
//! selected route is the one durable, user-authored policy value, so it is a
//! replayable action. This keeps collaboration from becoming remote process
//! control while allowing a team to agree on a launch policy.
use crate::model::{Catalogue, Route};
use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
use serde::{Deserialize, Serialize};
use tilo_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Availability, ConfirmationPolicy, Doc,
    EncodedAction, ExecutionLocus, Resource,
};
use ui::framework::LiveRow;
use ui::tilo_resource::{self, Effect};

pub const WINDOWS_ACTION_TYPE: &str = "windows.compat.action";
/// Stable id of the manager resource published by [`tilo_resource::publish`].
pub const MANAGER_RESOURCE_ID: &str = "windows.manager";
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

/// What Windows Apps asks the compatibility service and the managed
/// appliance to do. None of it is document state: a launch, an install or a
/// lifecycle change happens on this machine, where it is invoked.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "windows.effect")]
pub enum WindowsEffect {
    /// Show this application in the manager.
    #[effect(view, row, idempotent)]
    Show {
        #[param("The application id")]
        app: String,
    },
    /// Install Windows application.
    #[effect(job, privileged, permission = "admin")]
    Install {
        #[param("Path of the installer; omit to choose one")]
        path: Option<String>,
    },
    /// Import Windows ISO.
    #[effect(job, privileged, permission = "admin")]
    ImportIso {
        #[param("Path of the ISO; omit to choose one")]
        path: Option<String>,
    },
    /// Open recovery desktop.
    #[effect(row, job, privileged, permission = "admin")]
    Recovery,
    /// Start Windows.
    #[effect(row, job, privileged, permission = "admin")]
    Start,
    /// Restart Windows.
    #[effect(row, job, privileged, permission = "admin")]
    Restart,
    /// Suspend Windows.
    #[effect(row, job, privileged, permission = "admin")]
    Suspend,
    /// Resume Windows.
    #[effect(row, job, privileged, permission = "admin")]
    Resume,
    /// Shut down Windows.
    #[effect(row, job, privileged, permission = "admin")]
    Shutdown,
    /// Connect temporarily for activation.
    #[effect(row, job, privileged, permission = "admin")]
    TemporaryNetwork,
    /// Connect temporarily for Windows Update.
    #[effect(row, job, privileged, permission = "admin")]
    UpdateNetwork,
    /// Disconnect temporary network access.
    #[effect(row, job, privileged, permission = "admin")]
    RevokeNetwork,
    /// Install in a private capsule.
    #[effect(row, job, privileged, permission = "admin")]
    InstallPrivateCapsule,
    /// Install in the Windows appliance.
    #[effect(row, job, privileged, permission = "admin")]
    InstallWindowsAppliance,
    /// Launch.
    #[effect(row, job)]
    Launch {
        #[param("The application's id")]
        app: String,
    },
    /// Open in Windows appliance.
    #[effect(row, job, privileged, permission = "admin")]
    LaunchVmFallback {
        #[param("The application's id")]
        app: String,
    },
    /// Open file in Windows.
    #[effect(job, permission = "read")]
    OpenFile {
        #[param("The application's id")]
        app: String,
        #[param("Path of the file; omit to choose one")]
        path: Option<String>,
    },
    /// Publish launcher.
    #[effect(row, job, permission = "admin")]
    Publish {
        #[param("The application's id")]
        app: String,
    },
    /// Retry installation.
    #[effect(row, job, privileged, permission = "admin")]
    RetryInstall {
        #[param("The application's private capsule")]
        capsule: String,
    },
    /// Install in Windows appliance.
    #[effect(row, job, privileged, permission = "admin")]
    EscalateInstall {
        #[param("The application's private capsule")]
        capsule: String,
    },
    /// Remove launcher.
    #[effect(row, job, destructive, permission = "admin")]
    RemoveLauncher {
        #[param("The application's id")]
        app: String,
    },
    /// Uninstall.
    #[effect(row, job, destructive, permission = "admin")]
    Uninstall {
        #[param("The application's id")]
        app: String,
    },
    /// Delete application data.
    #[effect(row, job, destructive, permission = "admin")]
    DeleteData {
        #[param("The application's private capsule")]
        capsule: String,
    },
}

impl WindowsEffect {
    /// The service operation this effect asks for, and the item it names.
    pub(crate) fn operation(&self) -> (&'static str, String) {
        let none = String::new;
        match self {
            Self::Show { app } => ("windows.show", app.clone()),
            Self::Install { path } => ("windows.install", path.clone().unwrap_or_default()),
            Self::ImportIso { path } => ("windows.import-iso", path.clone().unwrap_or_default()),
            Self::Recovery => ("windows.recovery", none()),
            Self::Start => ("windows.start", none()),
            Self::Restart => ("windows.restart", none()),
            Self::Suspend => ("windows.suspend", none()),
            Self::Resume => ("windows.resume", none()),
            Self::Shutdown => ("windows.shutdown", none()),
            Self::TemporaryNetwork => ("windows.temporary-network", none()),
            Self::UpdateNetwork => ("windows.update-network", none()),
            Self::RevokeNetwork => ("windows.revoke-network", none()),
            Self::InstallPrivateCapsule => ("windows.confirm-capsule-install", none()),
            Self::InstallWindowsAppliance => ("windows.confirm-vm-install", none()),
            Self::Launch { app } => ("windows.launch", app.clone()),
            Self::LaunchVmFallback { app } => ("windows.launch-vm-fallback", app.clone()),
            Self::OpenFile { app, .. } => ("windows.open-file", app.clone()),
            Self::Publish { app } => ("windows.publish", app.clone()),
            Self::RetryInstall { capsule } => ("windows.retry-install", capsule.clone()),
            Self::EscalateInstall { capsule } => ("windows.escalate-install", capsule.clone()),
            Self::RemoveLauncher { app } => ("windows.remove-launcher", app.clone()),
            Self::Uninstall { app } => ("windows.uninstall", app.clone()),
            Self::DeleteData { capsule } => ("windows.delete-data", capsule.clone()),
        }
    }

    /// The operation's argument as the service handlers read it.
    pub(crate) fn payload(&self) -> Vec<u8> {
        let encoded = match self {
            Self::OpenFile { app, path } => {
                serde_json::to_vec(&[app.clone(), path.clone().unwrap_or_default()])
            }
            other => serde_json::to_vec(&other.operation().1),
        };
        encoded.unwrap_or_default()
    }

    /// Whether its answer waits on the managed appliance.
    pub(crate) fn waits_for_appliance(&self) -> bool {
        matches!(
            self,
            Self::ImportIso { .. }
                | Self::Recovery
                | Self::Start
                | Self::Restart
                | Self::Suspend
                | Self::Resume
                | Self::Shutdown
                | Self::TemporaryNetwork
                | Self::UpdateNetwork
                | Self::RevokeNetwork
        )
    }
}

/// `effect` as an item offers it now: available or not, as the service
/// reports.
fn offered(effect: &WindowsEffect, available: bool, reason: Option<&str>) -> ActionSpec {
    let mut spec = effect.bound();
    spec.contract.availability = availability(available, reason);
    spec
}

/// Read one collaborative route override from the shared document.
pub fn route_override(doc: &Doc, app_id: &str) -> Option<Route> {
    let (_, map) = doc.get(&ROOT, ROOT_KEY).ok().flatten()?;
    match doc.get(&map, app_id).ok().flatten()?.0.as_str()? {
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
pub(crate) fn immediate(id: &str, label: &str, action: &WindowsAction) -> ActionSpec {
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

/// The appliance's lifecycle, available once it is `ready`.
fn appliance_effects(ready: bool) -> Vec<ActionSpec> {
    [
        WindowsEffect::Recovery,
        WindowsEffect::Start,
        WindowsEffect::Restart,
        WindowsEffect::Suspend,
        WindowsEffect::Resume,
        WindowsEffect::Shutdown,
        WindowsEffect::TemporaryNetwork,
        WindowsEffect::UpdateNetwork,
        WindowsEffect::RevokeNetwork,
    ]
    .iter()
    .map(|effect| {
        offered(
            effect,
            ready,
            Some("Set up the managed Windows appliance first"),
        )
    })
    .collect()
}

/// Document-bound route shortcuts for one override already in shared state.
pub fn override_app_resource(app_id: &str, route: Route) -> Resource {
    let _ = route;
    let mut resource = Resource::new(
        format!("windows.app:{app_id}"),
        "windows.compat-app",
        app_id.to_owned(),
    );
    for candidate in Route::ALL {
        resource = resource.with_action(immediate(
            &format!("route-{}", candidate.short().to_ascii_lowercase()),
            &format!("Use {}", candidate.label()),
            &WindowsAction::SetRoute {
                app_id: app_id.to_owned(),
                route: candidate,
            },
        ));
    }
    // An override is recorded in the document, so reset is always offered.
    resource.with_action(immediate(
        "reset-route",
        "Use catalogue recommendation",
        &WindowsAction::ResetRoute {
            app_id: app_id.to_owned(),
        },
    ))
}

/// What the service reports, as rows beneath the manager: the install
/// awaiting a choice, the appliance and every catalogue application, each
/// with the effects the service offers on it now. A route change is not
/// among them: it is the resource's own action (the `set-route` declaration,
/// and the override rows [`override_app_resource`] publishes), which these
/// rows merge into by id.
pub fn live_rows(catalogue: &Catalogue) -> Vec<LiveRow> {
    let mut rows = Vec::new();
    if let Some(confirmation) = &catalogue.vm_install_confirmation {
        rows.push(pending_install_row(confirmation));
    }
    rows.push(appliance_row(catalogue));
    rows.extend(catalogue.apps.iter().map(app_row));
    rows
}

fn app_row(app: &crate::model::WindowsApp) -> LiveRow {
    let maintenance = maintenance_specs(app);
    let launch = offered(
        &WindowsEffect::Launch {
            app: app.id.clone(),
        },
        app.launch_available,
        app.launch_unavailable_reason.as_deref(),
    );
    let mut row = LiveRow::new(
        format!("windows.app:{}", app.id),
        "windows.compat-app",
        &app.name,
    )
    .operation(
        WindowsEffect::Show {
            app: app.id.clone(),
        }
        .bound(),
    )
    .operation(launch)
    .operation(offered(
        &WindowsEffect::LaunchVmFallback {
            app: app.id.clone(),
        },
        app.vm_fallback_available,
        app.vm_fallback_unavailable_reason.as_deref(),
    ));
    for spec in &maintenance {
        row = row.operation(spec.clone());
    }
    if let Some(open_file) = open_file_spec(app) {
        row = row.operation(open_file);
    }
    match prefix_row(app, &maintenance) {
        Some(prefix) => row.row(prefix),
        None => row,
    }
}

fn maintenance_specs(app: &crate::model::WindowsApp) -> Vec<ActionSpec> {
    const REASON: &str =
        "The service did not offer this operation for the current installation state";
    let mut delete = offered(
        &WindowsEffect::DeleteData {
            capsule: app.capsule.clone(),
        },
        app.can_delete_data,
        Some(REASON),
    );
    // Erasing an application's data asks for its name.
    delete.contract.confirmation = ConfirmationPolicy::Phrase {
        phrase: app.name.clone(),
    };
    vec![
        offered(
            &WindowsEffect::Publish {
                app: app.id.clone(),
            },
            app.can_publish_vm_candidate,
            Some(REASON),
        ),
        offered(
            &WindowsEffect::RetryInstall {
                capsule: app.capsule.clone(),
            },
            app.can_retry_install,
            Some(REASON),
        ),
        offered(
            &WindowsEffect::EscalateInstall {
                capsule: app.capsule.clone(),
            },
            app.can_escalate_install_to_vm,
            Some(REASON),
        ),
        offered(
            &WindowsEffect::RemoveLauncher {
                app: app.id.clone(),
            },
            app.can_remove_launcher,
            Some(REASON),
        ),
        offered(
            &WindowsEffect::Uninstall {
                app: app.id.clone(),
            },
            app.can_vendor_uninstall,
            Some(REASON),
        ),
        delete,
    ]
}

fn open_file_spec(app: &crate::model::WindowsApp) -> Option<ActionSpec> {
    // Keep the operation discoverable for every published guest application,
    // including app-focused SPICE entries where it is deliberately disabled.
    // Omitting its availability made search and agents advertise a file
    // bridge that the rendered control correctly refused.
    if app.route != Route::Vm {
        return None;
    }
    Some(offered(
        &WindowsEffect::OpenFile {
            app: app.id.clone(),
            path: None,
        },
        app.launch_available && app.portal_file_open_available,
        app.portal_file_open_unavailable_reason
            .as_deref()
            .or(app.launch_unavailable_reason.as_deref()),
    ))
}

fn prefix_row(app: &crate::model::WindowsApp, maintenance: &[ActionSpec]) -> Option<LiveRow> {
    if app.route == Route::Vm || app.capsule == "Pending private capsule" {
        return None;
    }
    let mut prefix = LiveRow::new(
        format!("windows.prefix:{}", app.id),
        "windows.prefix",
        format!("{} private data", app.capsule),
    );
    for spec in maintenance.iter().filter(|spec| {
        matches!(
            spec.id.as_str(),
            "retry-install" | "escalate-install" | "uninstall" | "delete-data"
        )
    }) {
        prefix = prefix.operation(spec.clone());
    }
    Some(prefix)
}

fn pending_install_row(confirmation: &crate::model::VmInstallConfirmation) -> LiveRow {
    LiveRow::new(
        "windows.pending-install",
        "windows.pending-install",
        format!("Install {}", confirmation.display_name),
    )
    .effect(&WindowsEffect::InstallPrivateCapsule)
    .effect(&WindowsEffect::InstallWindowsAppliance)
}

fn appliance_row(catalogue: &Catalogue) -> LiveRow {
    let mut appliance = LiveRow::new(
        "windows.appliance",
        "windows.appliance",
        "Managed Windows appliance",
    );
    for spec in appliance_effects(catalogue.vm.ready) {
        appliance = appliance.operation(spec);
    }
    appliance
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::CompatibilityProvider;
    use tilo_actions::{
        ExecutionMode, OperationKind, PermissionRequirement, ResultSpec, RiskLevel,
    };

    fn registry() -> ActionRegistry {
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        registry
    }

    fn set_route(app_id: &str, route: Route) -> WindowsAction {
        WindowsAction::SetRoute {
            app_id: app_id.into(),
            route,
        }
    }

    fn reset_route(app_id: &str) -> WindowsAction {
        WindowsAction::ResetRoute {
            app_id: app_id.into(),
        }
    }

    /// A shared document in which a collaborator has moved the game off its
    /// catalogue route, authored through the route action itself.
    fn seeded_doc() -> Doc {
        let mut doc = Doc::new();
        let action = set_route("cyberpunk-2077", Route::Wine);
        tilo_actions::apply_local(&mut doc, &action, encode(&action)).unwrap();
        doc
    }

    /// The tree the resource publishes for `doc`.
    fn resources(doc: &Doc) -> Vec<Resource> {
        use ui::tilo_resource::Resource as _;
        let state = crate::resource::Windows::read(doc).unwrap();
        vec![ui::tilo_resource::publish::<crate::resource::Windows>(
            MANAGER_RESOURCE_ID,
            "Windows manager",
            &state,
        )]
    }

    /// The rows the sample catalogue publishes beneath the manager.
    fn sample_rows() -> Vec<Resource> {
        live_rows(&crate::provider::SampleProvider::new().catalogue())
            .into_iter()
            .map(LiveRow::into_resource)
            .collect()
    }

    ui::action_contract_tests! {
        seed: seeded_doc(),
        registry: registry(),
        resources: |doc| resources(doc),
        palette: [
            "Use Wine",
            "Use Proton",
            "Use catalogue recommendation",
        ],
        replay: [
            route_override => |_| encode(&set_route("cyberpunk-2077", Route::Proton)),
            route_reset => |_| encode(&reset_route("cyberpunk-2077")),
        ],
        undo: [
            route_override => |_| encode(&set_route("adobe-reader", Route::Proton)),
            route_reset => |_| encode(&reset_route("cyberpunk-2077")),
        ],
        encodes: [
            use_proton => ("windows.app:cyberpunk-2077", "route-proton", |_, _| {
                encode(&set_route("cyberpunk-2077", Route::Proton))
            }),
            catalogue_recommendation => ("windows.app:cyberpunk-2077", "reset-route", |_, _| {
                encode(&reset_route("cyberpunk-2077"))
            }),
        ],
    }

    #[test]
    fn route_policy_is_explicit_deterministic_and_resettable() {
        // Case `route_policy_is_deterministic_and_reset_removes_it`.
        {
            let mut doc = Doc::new();
            WindowsAction::SetRoute {
                app_id: "reader".into(),
                route: Route::Proton,
            }
            .apply(&mut doc)
            .unwrap();
            let (_, map) = doc.get(&ROOT, ROOT_KEY).unwrap().unwrap();
            assert_eq!(
                doc.get(&map, "reader").unwrap().unwrap().0.as_str(),
                Some("Proton")
            );
            WindowsAction::ResetRoute {
                app_id: "reader".into(),
            }
            .apply(&mut doc)
            .unwrap();
            assert!(doc.get(&map, "reader").unwrap().is_none());
        }
        // Case `every_catalogue_app_can_launch_and_no_row_edits_the_document`.
        {
            let res = sample_rows();
            let flattened = tilo_actions::flatten(&res);
            assert!(
                res.iter()
                    .filter(|resource| resource.kind == "windows.compat-app")
                    .all(|resource| resource.actions.iter().any(|action| action.id == "launch"))
            );
            assert!(
                flattened
                    .iter()
                    .all(|action| action.action.kind != OperationKind::Document),
                "a live row carries effects only"
            );
            assert!(flattened.iter().map(|action| action.action).any(|action| {
                action.id == "delete-data" && action.contract.risk == RiskLevel::Destructive
            }));
        }
        // Case `every_background_operation_declares_a_job_result`.
        {
            let resources = sample_rows();
            for action in tilo_actions::flatten(&resources)
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
    }

    #[test]
    fn windows_resources_form_an_honest_hierarchy() {
        // Case `rows_form_an_appliance_application_hierarchy`.
        {
            let rows = sample_rows();
            let appliance = rows
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
                rows.iter()
                    .filter(|resource| resource.kind == "windows.compat-app")
                    .count(),
                3
            );
        }
        // Case `private_capsules_are_hierarchical_resources_with_recovery_actions`.
        {
            let rows = sample_rows();
            let reader = rows
                .iter()
                .find(|resource| resource.id.as_str() == "windows.app:adobe-reader")
                .expect("reader resource");
            let prefix = reader
                .children
                .iter()
                .find(|resource| resource.kind == "windows.prefix")
                .expect("private prefix resource");
            assert!(
                prefix
                    .actions
                    .iter()
                    .any(|action| action.id == "delete-data")
            );
            assert!(prefix.actions.iter().any(|action| action.id == "uninstall"));
        }
        // Case `unavailable_guest_file_bridge_is_not_advertised_as_actionable`.
        {
            let rows = sample_rows();
            let studio = rows
                .iter()
                .find(|resource| resource.id.as_str() == "windows.app:visual-studio")
                .expect("Visual Studio resource");
            let open_file = studio
                .actions
                .iter()
                .find(|action| action.id == "open-file")
                .expect("file operation is published for the guest application");
            assert!(matches!(
                open_file.contract.availability,
                Availability::Unavailable { .. }
            ));
            assert_eq!(open_file.contract.permission, PermissionRequirement::Read);
        }
    }
}
