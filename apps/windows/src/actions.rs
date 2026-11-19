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
    #[effect(view, row, returns, idempotent, on = crate::rows::CompatApp)]
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
    /// Retry installation.
    #[effect(row, job, privileged, kind = "outside", on = crate::rows::CompatApp)]
    #[effect(on = crate::rows::PrivateData)]
    RetryInstall {
        #[param("The application's private capsule")]
        capsule: String,
    },
    #[effect(row, job, privileged, kind = "outside", on = crate::rows::CompatApp)]
    #[effect(on = crate::rows::PrivateData)]
    EscalateInstall {
        #[param("The application's private capsule")]
        capsule: String,
    },
    #[effect(row, job, destructive, kind = "outside", on = crate::rows::CompatApp)]
    RemoveLauncher {
        #[param("The application's id")]
        app: String,
    },
    #[effect(row, job, destructive, kind = "outside", on = crate::rows::CompatApp)]
    #[effect(on = crate::rows::PrivateData)]
    Uninstall {
        #[param("The application's id")]
        app: String,
    },
    #[effect(row, job, destructive, kind = "outside", phrase = "Self::data_name", on = crate::rows::CompatApp)]
    #[effect(on = crate::rows::PrivateData)]
    DeleteData {
        #[param("The application's private capsule")]
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

/// The application whose data a destructive invocation loses, for its caller
/// to type: the app row's or the private-data row's id names the app.
impl WindowsEffect {
    fn data_name(&self) -> String {
        match self {
            Self::DeleteData { capsule } => crate::rows::app_of_row(capsule).to_owned(),
            _ => String::new(),
        }
    }
}

/// What the service reports, as rows beneath the manager: the install awaiting a choice,
/// the appliance and every catalogue application.
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
    rows.extend(catalogue.apps.iter().map(app_row));
    rows
}

pub const APPLIANCE_ROW: &str = "windows.appliance";

pub const PENDING_INSTALL_ROW: &str = "windows.pending-install";

/// One catalogue application's row, and beneath it the private data it keeps.
fn app_row(app: &crate::model::WindowsApp) -> LiveRow {
    let row = LiveRow::typed::<crate::rows::CompatApp>(
        crate::search::app_resource_id(&app.id),
        &app.name,
    );
    if app.route == Route::Vm || app.capsule == "Pending private capsule" {
        return row;
    }
    row.row(LiveRow::typed::<crate::rows::PrivateData>(
        format!("{}:{}", crate::rows::PRIVATE_DATA_PREFIX, app.id),
        format!("{} private data", app.capsule),
    ))
}

/// What the row `id` is like now, as the service reports it: an appliance
/// that is not set up offers no lifecycle, and an application offers the
/// operations of its installation state, with the service's reason why not.
pub fn row_status(catalogue: &Catalogue, id: &str) -> Vec<EffectStatus> {
    const CODE: &str = "unavailable";
    const NOT_OFFERED: &str =
        "The service did not offer this operation for the current installation state";
    const CANNOT_LAUNCH: &str = "This application cannot launch right now";
    if id == APPLIANCE_ROW {
        if catalogue.vm.ready {
            return Vec::new();
        }
        return [
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
        .collect();
    }
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
            assert_eq!(
                rows.iter()
                    .filter(|resource| resource.kind == "windows.compat-app")
                    .count(),
                3
            );
        }
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
