//! The Windows programs this device's manager reads and drives, for the device's daemon: what a
//! desktop app shows of this computer's compatibility layer, without a manager connection of its
//! own.
//!
//! A manager that a launcher started keeps its own connection, whose entries the service admits
//! one handoff at a time. This side answers for the device: every entry the computer has, read
//! through the same cap checks, so what an app shows is the service's own projection and not a
//! second copy. The session's system service publishes it as the `windows-apps` resource.

use std::fs::File;
use std::io::{Seek as _, SeekFrom};
use std::sync::{Arc, Mutex};

use collab_types::caps::CapOverride;

use super::{
    CapabilityAuthority, CLOSE_BEFORE_DELETE, InstallOutcome, ManagedApplication,
    ManagedCompatibility, ManagedInstallation, ProgramKind, RouteKind, SessionError,
};
use crate::{VmAvailability, VmInstallConfirmation, VmInstallation, VmInstallerInput};

/// What the device's manager reads of this computer now.
#[derive(Clone, Debug)]
pub struct ManagerList {
    pub applications: Vec<ManagedApplication>,
    pub installations: Vec<ManagedInstallation>,
    pub vm_availability: VmAvailability,
    /// The appliance setup this device is in, whichever workspace asked for it.
    pub installation: Option<VmInstallation>,
    /// The inspected setup waiting for the person to choose where it goes.
    pub pending_install: Option<VmInstallConfirmation>,
    /// The entries open now, in any workspace.
    pub running: Vec<String>,
    /// The last program that ended.
    pub ended: Option<EndedProgram>,
}

/// A program that ended, as its manager hears it once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndedProgram {
    pub program: String,
    pub name: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub said: Option<String>,
}

/// A setup that was inspected and waits for the person to choose where it goes.
pub struct PendingSetup {
    pub confirmation: VmInstallConfirmation,
    source: File,
    source_name: String,
    input: VmInstallerInput,
}

/// What the device's manager can do, as the session's system service reaches it.
///
/// The calls name the workspace the app runs in: a launch is admitted by that workspace's
/// sandbox, and the programs it starts are reported there.
pub trait SessionManager: Send + Sync {
    /// What this computer's programs and appliance read as now.
    ///
    /// # Errors
    /// The capsule store could not be read.
    fn list(&self, workspace: &str) -> Result<ManagerList, String>;
    /// Open a program of a private capsule.
    ///
    /// # Errors
    /// Why it did not open.
    fn launch(&self, workspace: &str, entry_id: &str) -> Result<(), String>;
    /// Open an appliance application, with a document when one is given.
    ///
    /// # Errors
    /// Why it did not open.
    fn launch_vm(
        &self,
        workspace: &str,
        entry_id: &str,
        file: Option<File>,
    ) -> Result<(), String>;
    /// End a program of a private capsule that is open in `workspace`.
    ///
    /// # Errors
    /// It is not open there.
    fn stop(&self, workspace: &str, entry_id: &str) -> Result<(), String>;
    /// Inspect a setup; it then waits for [`Self::install_in_capsule`] or
    /// [`Self::install_in_vm`].
    ///
    /// # Errors
    /// Why the setup was not inspected.
    fn install(
        &self,
        workspace: &str,
        source: File,
        source_name: &str,
    ) -> Result<VmInstallConfirmation, String>;
    /// Set up the waiting setup in a private capsule.
    ///
    /// # Errors
    /// Why the setup did not start, or no setup waits.
    fn install_in_capsule(&self, workspace: &str) -> Result<(), String>;
    /// Set up the waiting setup in the Windows appliance.
    ///
    /// # Errors
    /// Why the setup did not start, or no setup waits.
    fn install_in_vm(&self, workspace: &str) -> Result<(), String>;
    /// Add an application the appliance published to this computer's apps.
    ///
    /// # Errors
    /// Why it was not published.
    fn publish(&self, workspace: &str, entry_id: &str) -> Result<(), String>;
    /// Read the appliance's setup and uninstall status again.
    ///
    /// # Errors
    /// Why the appliance could not be asked.
    fn refresh(&self, workspace: &str) -> Result<(), String>;
    /// Run a failed setup again.
    ///
    /// # Errors
    /// Why the setup did not start.
    fn retry(&self, workspace: &str, capsule_id: &str) -> Result<(), String>;
    /// Continue a setup that did not finish in the appliance.
    ///
    /// # Errors
    /// Why the appliance could not take it.
    fn escalate(&self, workspace: &str, capsule_id: &str) -> Result<(), String>;
    /// Remove a program's launcher; its files stay.
    ///
    /// # Errors
    /// Why it was not removed.
    fn remove_launcher(&self, entry_id: &str) -> Result<(), String>;
    /// Run a program's own uninstaller: an appliance application's by its entry, a private
    /// capsule's by its entry or capsule.
    ///
    /// # Errors
    /// Why it did not start.
    fn uninstall(&self, workspace: &str, app: &str) -> Result<(), String>;
    /// Delete everything a setup put in its private capsule.
    ///
    /// # Errors
    /// A program of it would not end, or why the data was not deleted.
    fn delete_data(&self, capsule_id: &str) -> Result<(), String>;
    /// Choose how a program opens, or let the catalogue recommend it.
    ///
    /// # Errors
    /// Only an administrator may choose, or why it was not set.
    fn set_route(
        &self,
        workspace: &str,
        entry_id: &str,
        route: Option<RouteKind>,
    ) -> Result<(), String>;
}

/// Where the service puts its [`SessionManager`] as it starts: the session's system service
/// reads the device's Windows programs through it from then on.
#[derive(Clone, Default)]
pub struct ManagerOut(Arc<Mutex<Option<Arc<dyn SessionManager>>>>);

impl ManagerOut {
    /// The cell the service fills once it runs.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish `manager` from now on.
    pub fn set(&self, manager: Arc<dyn SessionManager>) {
        *lock(&self.0) = Some(manager);
    }

    /// The manager the service published, once it runs.
    #[must_use]
    pub fn get(&self) -> Option<Arc<dyn SessionManager>> {
        lock(&self.0).clone()
    }
}

/// The cell's value, whatever a panicking caller left behind.
fn lock<T>(cell: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    cell.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The manager over one assembled service.
pub struct DeviceManager<R, B, E, V> {
    managed: Arc<ManagedCompatibility<R, B, E, V>>,
    authority: Arc<dyn CapabilityAuthority>,
    /// The setup inspected and waiting for the person to choose where it goes.
    pending: Mutex<Option<PendingSetup>>,
    /// What each workspace's manager has read of the programs that ended.
    ends: Mutex<std::collections::BTreeMap<String, super::EndsRead>>,
    /// The last program that ended, kept until another does.
    last_end: Mutex<Option<EndedProgram>>,
}

impl<R, B, E, V> DeviceManager<R, B, E, V>
where
    R: super::RuntimeRegistry + 'static,
    B: super::LaunchResourceBroker + 'static,
    E: super::SandboxExecutor + 'static,
    V: super::VmDelegate + 'static,
{
    /// The manager over `managed`, checking every launch against `authority`.
    #[must_use]
    pub fn new(
        managed: Arc<ManagedCompatibility<R, B, E, V>>,
        authority: Arc<dyn CapabilityAuthority>,
    ) -> Self {
        Self {
            managed,
            authority,
            pending: Mutex::default(),
            ends: Mutex::default(),
            last_end: Mutex::default(),
        }
    }

    fn capabilities(
        &self,
        workspace: &str,
        app: &str,
    ) -> Result<Vec<CapOverride>, String> {
        self.authority
            .capabilities_for(workspace, app)
            .map_err(|error| error.to_string())
    }

    fn entry(&self, entry_id: &str) -> Result<super::RegisteredProgram, String> {
        self.managed
            .entries()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|entry| {
                entry.entry_id == entry_id && entry.program.kind == ProgramKind::Application
            })
            .ok_or_else(|| SessionError::UnknownEntry.to_string())
    }

    fn application(&self, entry_id: &str) -> Option<crate::VmPublishedApplication> {
        self.managed
            .vm_inventory()
            .applications
            .into_iter()
            .find(|application| application.entry_id == entry_id)
    }

    fn take_pending(&self) -> Result<(File, String, VmInstallerInput), String> {
        let pending = lock(&self.pending)
            .take()
            .ok_or_else(|| SessionError::VmInstallConfirmationExpired.to_string())?;
        Ok((pending.source, pending.source_name, pending.input))
    }
}

impl<R, B, E, V> SessionManager for DeviceManager<R, B, E, V>
where
    R: super::RuntimeRegistry + 'static,
    B: super::LaunchResourceBroker + 'static,
    E: super::SandboxExecutor + 'static,
    V: super::VmDelegate + 'static,
{
    fn list(&self, workspace: &str) -> Result<ManagerList, String> {
        let managed = &self.managed;
        let entries = managed.entries().map_err(|error| error.to_string())?;
        let administrator = (managed.administrator)();
        let running = managed.programs.running();
        {
            let mut ends = lock(&self.ends);
            let read = ends.entry(workspace.to_owned()).or_default();
            let ended = managed.programs.ends_for(workspace, read);
            if let Some(end) = ended.into_iter().next_back() {
                *lock(&self.last_end) = Some(EndedProgram {
                    program: end.entry_id,
                    name: end.display_name,
                    success: end.success,
                    exit_code: end.exit_code,
                    error: end.error,
                    said: end.said,
                });
            }
        }
        let vm_inventory = managed.vm_inventory();
        let vm_availability = vm_inventory.availability.clone();
        let installation = managed.vm_installation_status(workspace);
        let mut applications = Vec::new();
        for entry in entries
            .into_iter()
            .filter(|entry| entry.program.kind == ProgramKind::Application)
        {
            let capabilities = self.capabilities(workspace, &entry.app_id)?;
            let (explanation, recommended_route) = managed
                .explain_entry(&entry, &capabilities)
                .map_err(|error| error.to_string())?;
            let installation_status = managed
                .installation_states()
                .map_err(|error| error.to_string())?
                .into_iter()
                .find(|state| state.id == entry.capsule_id)
                .and_then(|state| state.installation.map(|installation| installation.status));
            applications.push(ManagedApplication {
                id: entry.entry_id.clone(),
                app_id: entry.app_id.clone(),
                display_name: entry.program.display_name.clone(),
                publisher: entry
                    .query
                    .publisher
                    .clone()
                    .or(entry.query.store.clone())
                    .unwrap_or_else(|| "Windows application".to_string()),
                capsule_id: Some(entry.capsule_id.clone()),
                program_id: entry.program.id.clone(),
                explanation,
                vm_launch_available: false,
                vm_unavailable_reason: Some(
                    "Open in Windows VM is available only for published Windows guest applications."
                        .to_string(),
                ),
                presentation: None,
                portal_file_open_available: false,
                portal_file_open_unavailable_reason: Some(
                    "Open file is available only for published Windows guest applications."
                        .to_string(),
                ),
                audio_output_available: true,
                audio_unavailable_reason: None,
                installation_status,
                remove_launcher_available: true,
                vendor_uninstall_available: false,
                running: running.contains(&entry.entry_id),
                recommended_route,
                opens_with_choice_available: administrator,
                opens_with_unavailable_reason: (!administrator)
                    .then(|| super::NOT_ADMINISTRATOR.to_owned()),
            });
        }
        for application in vm_inventory.applications {
            let is_running = running.contains(&application.entry_id);
            applications.push(ManagedApplication {
                id: application.entry_id.clone(),
                app_id: application.app_id.clone(),
                display_name: application.display_name.clone(),
                publisher: "Managed Windows appliance".to_string(),
                capsule_id: None,
                program_id: application.published_application_id.clone(),
                explanation: super::published_vm_explanation(application.presentation.label()),
                vm_launch_available: vm_availability.available,
                vm_unavailable_reason: (!vm_availability.available)
                    .then(|| vm_availability.detail.clone()),
                presentation: Some(application.presentation),
                portal_file_open_available: application.presentation.supports_portal_files()
                    && vm_availability.available,
                portal_file_open_unavailable_reason: (!application
                    .presentation
                    .supports_portal_files())
                .then(|| {
                    "Open file is unavailable in app-focused SPICE because this presentation has no portal file bridge."
                        .to_string()
                })
                .or_else(|| {
                    (!vm_availability.available).then(|| vm_availability.detail.clone())
                }),
                audio_output_available: true,
                audio_unavailable_reason: None,
                installation_status: None,
                remove_launcher_available: true,
                vendor_uninstall_available: application.uninstall_available,
                running: is_running,
                recommended_route: RouteKind::Vm,
                opens_with_choice_available: false,
                opens_with_unavailable_reason: None,
            });
        }
        let installations = managed
            .installation_states()
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter_map(|state| {
                let installation = state.installation?;
                let program_count = state
                    .programs
                    .values()
                    .filter(|program| program.kind == ProgramKind::Application)
                    .count();
                let detail = installation.last_error.clone().unwrap_or_else(|| {
                    installation_words(installation.status, program_count)
                });
                Some(ManagedInstallation {
                    capsule_id: state.id,
                    display_name: installation.display_name,
                    route: RouteKind::from(state.layer),
                    runner: format!("{} {}", state.runner.family, state.runner.version),
                    status: installation.status,
                    program_count,
                    can_retry: matches!(
                        installation.status,
                        super::InstallationStatus::Failed
                            | super::InstallationStatus::NeedsAttention
                    ),
                    can_vendor_uninstall: installation.uninstaller_relative_path.is_some()
                        && !matches!(
                            installation.status,
                            super::InstallationStatus::Installing
                                | super::InstallationStatus::Uninstalling
                                | super::InstallationStatus::Uninstalled
                        ),
                    can_delete_data: !matches!(
                        installation.status,
                        super::InstallationStatus::Installing
                            | super::InstallationStatus::Uninstalling
                    ),
                    detail,
                })
            })
            .collect();
        Ok(ManagerList {
            applications,
            installations,
            vm_availability,
            installation,
            pending_install: lock(&self.pending)
                .as_ref()
                .map(|pending| pending.confirmation.clone()),
            running: running.into_iter().collect(),
            ended: lock(&self.last_end).clone(),
        })
    }

    fn launch(&self, workspace: &str, entry_id: &str) -> Result<(), String> {
        let entry = self.entry(entry_id)?;
        let capabilities = self.capabilities(workspace, &entry.app_id)?;
        self.managed
            .launch_entry(&entry, workspace, capabilities)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn launch_vm(
        &self,
        workspace: &str,
        entry_id: &str,
        file: Option<File>,
    ) -> Result<(), String> {
        let application = self
            .application(entry_id)
            .ok_or_else(|| SessionError::UnknownEntry.to_string())?;
        let capabilities = self.capabilities(workspace, &application.app_id)?;
        self.managed
            .launch_published_vm(application, workspace, capabilities, file)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn stop(&self, workspace: &str, entry_id: &str) -> Result<(), String> {
        if self.managed.programs.stop(workspace, entry_id) {
            Ok(())
        } else {
            Err(SessionError::NotRunning.to_string())
        }
    }

    fn install(
        &self,
        workspace: &str,
        source: File,
        source_name: &str,
    ) -> Result<VmInstallConfirmation, String> {
        let capabilities = self.capabilities(workspace, "so.tilo.windows.import")?;
        // The inspected installer stays open for the person's choice: the same open file goes to
        // the setup, never a path that may since have changed.
        let kept = source
            .try_clone()
            .map_err(|error| format!("could not keep the installer open: {error}"))?;
        let InstallOutcome::Confirmation { confirmation, input } = self
            .managed
            .install(source, source_name, workspace, &capabilities)
            .map_err(|error| error.to_string())?;
        *lock(&self.pending) = Some(PendingSetup {
            confirmation: confirmation.clone(),
            source: kept,
            source_name: source_name.to_owned(),
            input,
        });
        Ok(confirmation)
    }

    fn install_in_capsule(&self, workspace: &str) -> Result<(), String> {
        let (mut source, source_name, input) = self.take_pending()?;
        source
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("could not read the installer again: {error}"))?;
        let capabilities = self.capabilities(workspace, "so.tilo.windows.import")?;
        self.managed
            .install_in_capsule(source, &source_name, workspace, &capabilities, &input)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn install_in_vm(&self, workspace: &str) -> Result<(), String> {
        let (mut source, source_name, input) = self.take_pending()?;
        source
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("could not read the installer again: {error}"))?;
        self.managed
            .install_in_vm(source, &source_name, workspace, &input)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn publish(&self, workspace: &str, entry_id: &str) -> Result<(), String> {
        self.managed
            .publish_vm_candidate(workspace, entry_id)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn refresh(&self, workspace: &str) -> Result<(), String> {
        self.managed
            .refresh_vm_installation_status(workspace)
            .map_err(|error| error.to_string())?;
        self.managed
            .refresh_vm_uninstallation_status(workspace)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn retry(&self, workspace: &str, capsule_id: &str) -> Result<(), String> {
        let capabilities = self.capabilities(workspace, "so.tilo.windows.import")?;
        self.managed
            .retry_install(capsule_id, workspace, &capabilities)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn escalate(&self, workspace: &str, capsule_id: &str) -> Result<(), String> {
        self.managed
            .escalate_install_to_vm(capsule_id, workspace)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn remove_launcher(&self, entry_id: &str) -> Result<(), String> {
        if self.application(entry_id).is_some() {
            return self
                .managed
                .remove_vm_published_application(entry_id)
                .map_err(|error| error.to_string());
        }
        let entry = self.entry(entry_id)?;
        self.managed
            .remove_launcher(&entry)
            .map_err(|error| error.to_string())
    }

    fn uninstall(&self, workspace: &str, app: &str) -> Result<(), String> {
        if self
            .application(app)
            .is_some_and(|application| application.uninstall_available)
        {
            return self
                .managed
                .uninstall_vm_published_application(workspace, app)
                .map(|_| ())
                .map_err(|error| error.to_string());
        }
        let capsule_id = match self.entry(app) {
            Ok(entry) => entry.capsule_id,
            Err(_) => app.to_owned(),
        };
        let capabilities = self.capabilities(workspace, "so.tilo.windows.import")?;
        self.managed
            .vendor_uninstall(&capsule_id, workspace, &capabilities)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn delete_data(&self, capsule_id: &str) -> Result<(), String> {
        let retiring: Vec<_> = self
            .managed
            .entries()
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|entry| entry.capsule_id == capsule_id)
            .collect();
        let entries: std::collections::BTreeSet<_> = retiring
            .iter()
            .map(|entry| entry.entry_id.clone())
            .collect();
        let still_open = self.managed.programs.close_entries(&entries, CLOSE_BEFORE_DELETE);
        if !still_open.is_empty() {
            let names: Vec<&str> = retiring
                .iter()
                .filter(|entry| still_open.contains(&entry.entry_id))
                .map(|entry| entry.program.display_name.as_str())
                .collect();
            return Err(format!(
                "a program of this setup would not end: {}",
                names.join(", ")
            ));
        }
        self.managed
            .delete_data(capsule_id)
            .map_err(|error| error.to_string())
    }

    fn set_route(
        &self,
        workspace: &str,
        entry_id: &str,
        route: Option<RouteKind>,
    ) -> Result<(), String> {
        let entry = self.entry(entry_id)?;
        if !(self.managed.administrator)() {
            return Err(SessionError::NotAdministrator.to_string());
        }
        let _ = workspace;
        self.managed
            .service
            .set_opens_with(&entry.capsule_id, &entry.program.id, route)
            .map_err(|error| error.to_string())
    }
}

/// How a capsule's installation stands, in the service's own words.
fn installation_words(status: super::InstallationStatus, programs: usize) -> String {
    match status {
        super::InstallationStatus::Installed => {
            format!("{programs} application launcher(s) in this private capsule.")
        }
        super::InstallationStatus::NeedsAttention => {
            "Setup completed but no launcher was found; retry setup or delete its private data."
                .to_string()
        }
        super::InstallationStatus::Failed => {
            "Setup failed; the exact selected installer is retained for retry.".to_string()
        }
        super::InstallationStatus::Uninstalled => {
            "Vendor uninstall completed; private leftover data is retained.".to_string()
        }
        super::InstallationStatus::Installing => "Setup is running.".to_string(),
        super::InstallationStatus::Uninstalling => "The vendor uninstaller is running.".to_string(),
    }
}
