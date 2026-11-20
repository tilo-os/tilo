//! What Windows Apps does on this machine, as typed effects, and the live rows it publishes
//! for what the compatibility service reports. Launch, VM resume, permission changes,
//! repair, and snapshot operations are external service calls and therefore never document
//! changes.
use crate::model::{Catalogue, Route};
use ui::framework::{EffectStatus, LiveRow};
use ui::tilo_resource::{self, AbsolutePath, Effect};

/// What Windows Apps asks the compatibility service and the managed appliance to do.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "windows.effect")]
pub enum WindowsEffect {
    #[effect(view, row, idempotent, on = crate::rows::CompatApp)]
    Show {
        #[param("The application id")]
        app: String,
    },
    #[effect(
        view,
        row,
        returns,
        idempotent,
        on = crate::rows::CompatApp,
        tag = tilo_actions::DOCUMENT_READ_TAG
    )]
    ReadApp {
        #[param("The application id")]
        app: String,
    },
    #[effect(job, privileged, kind = "outside")]
    Install {
        #[param("Absolute path of the installer; omit to choose one")]
        path: Option<AbsolutePath>,
    },
    #[effect(job, privileged, kind = "outside")]
    ImportIso {
        #[param("Absolute path of the ISO; omit to choose one")]
        path: Option<AbsolutePath>,
    },
    #[effect(row, job, privileged, kind = "outside", on = crate::rows::Appliance)]
    Recovery,
    #[effect(row, job, privileged, kind = "outside", on = crate::rows::Appliance)]
    Start,
    #[effect(row, job, privileged, kind = "outside", on = crate::rows::Appliance)]
    Restart,
    #[effect(row, job, privileged, kind = "outside", on = crate::rows::Appliance)]
    Suspend,
    #[effect(row, job, privileged, kind = "outside", on = crate::rows::Appliance)]
    Resume,
    #[effect(row, job, privileged, kind = "outside", on = crate::rows::Appliance)]
    Shutdown,
    #[effect(row, job, privileged, kind = "outside", on = crate::rows::Appliance)]
    TemporaryNetwork,
    #[effect(row, job, privileged, kind = "outside", on = crate::rows::Appliance)]
    UpdateNetwork,
    #[effect(row, job, privileged, kind = "outside", on = crate::rows::Appliance)]
    RevokeNetwork,
    #[effect(row, job, privileged, kind = "outside", on = crate::rows::PendingInstall)]
    InstallPrivateCapsule,
    #[effect(row, job, privileged, kind = "outside", on = crate::rows::PendingInstall)]
    InstallWindowsAppliance,
    /// Download and add Windows support (Proton), which programs opened with Proton need.
    #[effect(job, privileged, kind = "outside")]
    InstallWindowsSupport,
    #[effect(kind = "outside", on = crate::rows::CompatApp)]
    #[effect(row, job)]
    Launch {
        #[param("The application's id")]
        app: String,
    },
    #[effect(row, job, kind = "outside", on = crate::rows::CompatApp)]
    Stop {
        #[param("The application's id")]
        app: String,
    },
    #[effect(row, job, privileged, kind = "outside", on = crate::rows::CompatApp)]
    LaunchVmFallback {
        #[param("The application's id")]
        app: String,
    },
    #[effect(job, kind = "read", on = crate::rows::CompatApp)]
    OpenFile {
        #[param("The application's id")]
        app: String,
        #[param("Absolute path of the file; omit to choose one")]
        path: Option<AbsolutePath>,
    },
    #[effect(row, job, kind = "outside", on = crate::rows::CompatApp)]
    Publish {
        #[param("The application's id")]
        app: String,
    },
    /// Retry the setup whose private capsule this is.
    #[effect(row, job, privileged, kind = "outside", on = crate::rows::CompatApp)]
    #[effect(on = crate::rows::Setup)]
    RetryInstall {
        #[param("The private capsule the setup installed into")]
        capsule: String,
    },
    #[effect(row, job, privileged, kind = "outside", on = crate::rows::CompatApp)]
    #[effect(on = crate::rows::Setup)]
    EscalateInstall {
        #[param("The private capsule the setup installed into")]
        capsule: String,
    },
    #[effect(row, job, destructive, kind = "outside", on = crate::rows::CompatApp)]
    RemoveLauncher {
        #[param("The application's id")]
        app: String,
    },
    #[effect(row, job, destructive, kind = "outside", on = crate::rows::CompatApp)]
    #[effect(on = crate::rows::Setup)]
    Uninstall {
        #[param("The application's id")]
        app: String,
    },
    /// Delete the setup's data: everything stored in the private capsule it installed into,
    /// which every program of the setup shares.
    #[effect(row, job, destructive, kind = "outside", phrase = "Self::data_name", on = crate::rows::Setup)]
    DeleteData {
        #[param("The private capsule the setup installed into")]
        capsule: String,
    },
}

impl WindowsEffect {
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

/// The capsule a destructive invocation loses, for its caller to type: the setup row's id
/// names the capsule whose files go.
impl WindowsEffect {
    fn data_name(&self) -> String {
        match self {
            Self::DeleteData { capsule } => crate::rows::app_of_row(capsule).to_owned(),
            _ => String::new(),
        }
    }
}

/// What the service reports, as rows beneath the manager: the install awaiting a choice,
/// the appliance, and the setups the catalogue's programs came from.
pub fn live_rows(catalogue: &Catalogue) -> Vec<LiveRow> {
    let mut rows = Vec::new();
    if let Some(confirmation) = &catalogue.vm_install_confirmation {
        rows.push(LiveRow::typed::<crate::rows::PendingInstall>(
            PENDING_INSTALL_ROW,
            format!("Install {}", confirmation.display_name),
        ));
    }
    rows.push(LiveRow::typed::<crate::rows::Appliance>(
        APPLIANCE_ROW,
        "Managed Windows appliance",
    ));
    rows.extend(
        catalogue
            .apps
            .iter()
            .filter(|app| !keeps_a_capsule(app))
            .map(app_row),
    );
    // One row per setup, holding every program it installed: the programs share one capsule,
    // so what acts on the capsule acts on all of them at once.
    let mut setups: Vec<(&str, &crate::model::WindowsApp)> = Vec::new();
    for app in catalogue.apps.iter().filter(|app| keeps_a_capsule(app)) {
        if !setups.iter().any(|(capsule, _)| *capsule == app.capsule) {
            setups.push((&app.capsule, app));
        }
    }
    for (capsule, first) in setups {
        let programs: Vec<&crate::model::WindowsApp> = catalogue
            .apps
            .iter()
            .filter(|app| keeps_a_capsule(app) && app.capsule == capsule)
            .collect();
        let mut setup = LiveRow::typed::<crate::rows::Setup>(
            setup_resource_id(capsule),
            setup_label(first, &programs),
        );
        for program in programs {
            setup = setup.row(app_row(program));
        }
        rows.push(setup);
    }
    rows
}

pub const APPLIANCE_ROW: &str = "windows.appliance";

pub const PENDING_INSTALL_ROW: &str = "windows.pending-install";

/// Whether `app` is installed in a private capsule of its own (rather than running in the
/// Windows appliance, or still being set up).
fn keeps_a_capsule(app: &crate::model::WindowsApp) -> bool {
    app.route != Route::Vm && app.capsule != crate::model::PENDING_CAPSULE
}

/// The name a setup's row carries: what its installer called it, or the programs it put
/// there when the service reports no name.
fn setup_label(first: &crate::model::WindowsApp, programs: &[&crate::model::WindowsApp]) -> String {
    if let Some(name) = first
        .setup_name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
    {
        return name.to_owned();
    }
    let names: Vec<String> = programs
        .iter()
        .map(|program| program.name.clone())
        .collect();
    format!("{} setup", crate::strings::names(&names))
}

/// One catalogue application's row.
fn app_row(app: &crate::model::WindowsApp) -> LiveRow {
    LiveRow::typed::<crate::rows::CompatApp>(crate::search::app_resource_id(&app.id), &app.name)
}

/// The row of the setup that installed into `capsule`.
pub fn setup_resource_id(capsule: &str) -> String {
    format!("{}:{capsule}", crate::rows::SETUP_PREFIX)
}

const CODE: &str = "unavailable";
const NOT_OFFERED: &str =
    "The service did not offer this operation for the current installation state";
const CANNOT_LAUNCH: &str = "This application cannot launch right now";

/// What the row `id` is like now, as the service reports it: an appliance
/// that is not set up offers no lifecycle, and an application offers the
/// operations of its installation state, with the service's reason why not.
pub fn row_status(catalogue: &Catalogue, id: &str) -> Vec<EffectStatus> {
    if id == APPLIANCE_ROW {
        return appliance_status(catalogue);
    }
    if let Some(capsule) = crate::rows::capsule_of_row(id) {
        return setup_status(catalogue, capsule);
    }
    app_status(catalogue, id)
}

/// What a ready appliance offers, and what one that is not set up refuses.
fn appliance_status(catalogue: &Catalogue) -> Vec<EffectStatus> {
    if catalogue.vm.ready {
        return Vec::new();
    }
    [
        WindowsEffect::RECOVERY,
        WindowsEffect::START,
        WindowsEffect::RESTART,
        WindowsEffect::SUSPEND,
        WindowsEffect::RESUME,
        WindowsEffect::SHUTDOWN,
        WindowsEffect::TEMPORARY_NETWORK,
        WindowsEffect::UPDATE_NETWORK,
        WindowsEffect::REVOKE_NETWORK,
    ]
    .into_iter()
    .map(|effect| {
        EffectStatus::unavailable(effect, CODE, "Set up the managed Windows appliance first")
    })
    .collect()
}

/// What a setup's row refuses: its operations act on its capsule, so what one program's
/// installation state allows, the setup allows, since every program shares the state.
fn setup_status(catalogue: &Catalogue, capsule: &str) -> Vec<EffectStatus> {
    let Some(app) = catalogue.apps.iter().find(|app| app.capsule == capsule) else {
        return Vec::new();
    };
    [
        refusal(
            WindowsEffect::RETRY_INSTALL,
            app.can_retry_install,
            None,
            NOT_OFFERED,
        ),
        refusal(
            WindowsEffect::ESCALATE_INSTALL,
            app.can_escalate_install_to_vm,
            None,
            NOT_OFFERED,
        ),
        refusal(
            WindowsEffect::DELETE_DATA,
            app.can_delete_data,
            None,
            NOT_OFFERED,
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// What an app's row refuses of its own operations.
fn app_status(catalogue: &Catalogue, id: &str) -> Vec<EffectStatus> {
    let Some(app) = catalogue.app(crate::rows::app_of_row(id)) else {
        return Vec::new();
    };
    [
        refusal(
            WindowsEffect::LAUNCH,
            app.launch_available,
            app.launch_unavailable_reason.as_deref(),
            CANNOT_LAUNCH,
        ),
        refusal(
            WindowsEffect::OPEN_FILE,
            app.launch_available && app.portal_file_open_available,
            app.portal_file_open_unavailable_reason
                .as_deref()
                .or(app.launch_unavailable_reason.as_deref()),
            CANNOT_LAUNCH,
        ),
        refusal(
            WindowsEffect::STOP,
            app.stop_available(),
            None,
            "Only a running app in a private capsule can be ended here",
        ),
        refusal(
            WindowsEffect::LAUNCH_VM_FALLBACK,
            app.vm_fallback_available,
            app.vm_fallback_unavailable_reason.as_deref(),
            NOT_OFFERED,
        ),
        refusal(
            WindowsEffect::PUBLISH,
            app.can_publish_vm_candidate,
            None,
            NOT_OFFERED,
        ),
        refusal(
            WindowsEffect::RETRY_INSTALL,
            app.can_retry_install,
            None,
            NOT_OFFERED,
        ),
        refusal(
            WindowsEffect::ESCALATE_INSTALL,
            app.can_escalate_install_to_vm,
            None,
            NOT_OFFERED,
        ),
        refusal(
            WindowsEffect::REMOVE_LAUNCHER,
            app.can_remove_launcher,
            None,
            NOT_OFFERED,
        ),
        refusal(
            WindowsEffect::UNINSTALL,
            app.can_vendor_uninstall,
            None,
            NOT_OFFERED,
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The row's refusal of `effect` when the service says it is not offered.
fn refusal<A>(
    effect: tilo_resource::ActionRef<WindowsEffect, A>,
    available: bool,
    reason: Option<&str>,
    default: &str,
) -> Option<EffectStatus> {
    (!available).then(|| {
        EffectStatus::unavailable(effect, "unavailable", reason.unwrap_or(default).to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::CompatibilityProvider;
    use tilo_actions::{
        ActionKind, ExecutionMode, OperationKind, ResourceSpec, ResultSpec, RiskLevel,
    };

    /// The rows the sample catalogue publishes beneath the manager.
    fn sample_rows() -> Vec<ResourceSpec> {
        live_rows(&crate::provider::SampleProvider::new().catalogue())
            .into_iter()
            .map(LiveRow::into_resource)
            .collect()
    }

    /// Reading an application is a document read: it is what discloses its
    /// row, so the operations bound to that row reach agents. A migration
    /// that moves the read onto the type keeps that tag, or every action on
    /// the row silently leaves the agents' survey (the reach test drives
    /// every scenario for the same thing).
    #[test]
    fn reading_an_application_discloses_its_row_to_agents() {
        use ui::tilo_resource::Effect as _;
        let read = WindowsEffect::spec(WindowsEffect::READ_APP.id)
            .expect("reading an application is declared");
        assert!(
            read.contract.reads_document(),
            "read-app is a document read, so its row is disclosed"
        );
    }

    /// A relative installer path is refused before the service sees it.
    #[test]
    fn a_relative_installer_path_is_refused() {
        let arguments =
            serde_json::Map::from_iter([("path".to_owned(), serde_json::json!("setup.exe"))]);
        let refused = WindowsEffect::decode("install", &arguments).unwrap_err();
        assert!(
            matches!(
                &refused,
                ui::tilo_resource::EffectDecodeError::Argument { name, source }
                    if name == "path" && source.to_string().contains("absolute")
            ),
            "{refused}"
        );
    }

    #[test]
    fn every_row_offers_effects_with_honest_contracts() {
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
            // A program the Windows appliance shows keeps no private capsule of its own, so it
            // stands on its own; the private-capsule programs sit under their setups.
            assert!(
                rows.iter()
                    .any(|resource| resource.id.as_str() == "windows.app:visual-studio")
            );
            assert!(
                rows.iter()
                    .all(|resource| resource.id.as_str() != "windows.app:adobe-reader"),
                "a program of a private capsule is listed under its setup"
            );
            let setups: Vec<&str> = rows
                .iter()
                .filter(|resource| resource.kind == "windows.setup")
                .map(|resource| resource.id.as_str())
                .collect();
            assert_eq!(
                setups,
                ["windows.setup:Adobe Reader", "windows.setup:Steam library"]
            );
        }
        {
            let rows = sample_rows();
            let setup = rows
                .iter()
                .find(|resource| resource.id.as_str() == "windows.setup:Adobe Reader")
                .expect("the setup holding Reader");
            let reader = setup
                .children
                .iter()
                .find(|resource| resource.id.as_str() == "windows.app:adobe-reader")
                .expect("reader resource");
            assert!(
                !reader
                    .actions
                    .iter()
                    .any(|action| action.id == "delete-data"),
                "deleting acts on the setup's capsule, never on one program as if it were alone"
            );
            assert!(
                setup
                    .actions
                    .iter()
                    .any(|action| action.id == "delete-data")
            );
            assert!(setup.actions.iter().any(|action| action.id == "uninstall"));
            let delete = setup
                .actions
                .iter()
                .find(|action| action.id == "delete-data")
                .unwrap();
            assert_eq!(
                delete.contract.confirmation,
                tilo_actions::ConfirmationPolicy::Phrase {
                    phrase: "Adobe Reader".to_owned(),
                },
                "an agent or a script must type what it loses, as a person does"
            );
        }
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
            assert_eq!(open_file.contract.kind, ActionKind::Read);
            // What the service reports makes operations unavailable, so search
            // and agents see the same refusal the rendered control shows.
            let catalogue = crate::provider::SampleProvider::new().catalogue();
            assert!(
                !row_status(&catalogue, "windows.app:visual-studio").is_empty(),
                "the studio row refuses what the service does not offer"
            );
            assert!(
                row_status(&catalogue, APPLIANCE_ROW).is_empty(),
                "a ready appliance offers its lifecycle"
            );
        }
    }
}
