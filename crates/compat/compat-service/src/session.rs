use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{IoSlice, IoSliceMut, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use collab_types::caps::CapOverride;
use compat_catalogue::{ApplicationQuery, Catalogue, Layer, VmFallback};
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tilo_clock::try_unix_now;
use wire_frame::{FrameError, Framing};

mod manager;

use crate::programs::{EndsRead, Programs};
use crate::service::published_vm_explanation;
use crate::{
    CapsuleStore, CompatibilityService, EntryDirectory, InstallOutcome, InstallationStatus,
    LaunchDisplays, LaunchInput, LaunchResourceBroker, ManagedApplication, ManagedInstallation,
    PROTOCOL_VERSION, ProgramKind, ProtocolError, ProtocolRequest, ProtocolResponse,
    RegisteredProgram, Reopenings, RouteKind, RuntimeRegistry, SandboxExecutor, ServiceError,
    VmDelegate,
};

pub use manager::{DeviceManager, EndedProgram, ManagerList, ManagerOut, SessionManager};

use tilo_app_id::apps::WINDOWS as MANAGER_APP_ID;
/// Requests and responses: a little-endian `u32` length, then JSON. A request
/// that passes a descriptor carries it with the length. Empty frames are
/// invalid.
const FRAMING: Framing = Framing::little_endian(1024 * 1024).reject_empty();
const MAX_CLIENT_WORKERS: usize = 8;
const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a capsule's open programs are given to end before its data is deleted: a keeper
/// ends its program at once, so this is only reached by one that refuses to go.
const CLOSE_BEFORE_DELETE: Duration = Duration::from_secs(10);
const VM_INSTALL_CONFIRMATION_SECONDS: u64 = 5 * 60;

struct PendingVmInstall {
    input: crate::VmInstallerInput,
    expires_at_unix: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHandoff {
    pub protocol: u16,
    pub app_id: String,
    pub workspace_id: String,
}

/// Source of fresh session-owned grants. An implementation must look up
/// `workspace_id` and `app_id`; it must never deserialize grants from this
/// service connection.
pub trait CapabilityAuthority: Send + Sync + 'static {
    fn capabilities_for(
        &self,
        workspace_id: &str,
        app_id: &str,
    ) -> Result<Vec<CapOverride>, String>;
}

/// The session's own account of the launcher processes it started. `/proc`
/// facts are only corroboration; this is the authority that a launch was
/// actually started by Tilo for this app in this workspace. Each launch hands
/// off to the service once ([`LauncherAuthenticator`] keeps that).
pub trait TrustedLauncherRegistry: Send + Sync + 'static {
    /// Whether `pid`, started at `process_start_time`, is a launch of `app_id`
    /// in `workspace_id` the session started and that still lives.
    fn is_launch(
        &self,
        pid: u32,
        process_start_time: u64,
        app_id: &str,
        workspace_id: &str,
    ) -> bool;
}

pub struct ManagedCompatibility<R, B, E, V> {
    service: CompatibilityService<R, B, E, V>,
    store: CapsuleStore,
    /// The programs started through the service that are still open, and how the others ended.
    programs: Arc<Programs>,
    /// Whether the desktop's person administers the computer.
    administrator: Arc<dyn Fn() -> bool + Send + Sync>,
    /// What each entry's open programs opened with: Wine and Proton never have one prefix open
    /// at once.
    opened_with: Mutex<std::collections::BTreeMap<String, Layer>>,
    /// Held from a launch's check of what is open until its program is watched, so two launches
    /// never both pass the check.
    launching: Mutex<()>,
}

impl<R, B, E, V> ManagedCompatibility<R, B, E, V>
where
    R: RuntimeRegistry,
    B: LaunchResourceBroker + 'static,
    E: SandboxExecutor,
    V: VmDelegate,
{
    pub fn new(
        catalogue: Catalogue,
        store: CapsuleStore,
        runtimes: R,
        resources: B,
        executor: E,
        vm: V,
        displays: Arc<dyn LaunchDisplays>,
    ) -> Result<Self, ServiceError> {
        let service = CompatibilityService::new(
            catalogue,
            store.clone(),
            runtimes,
            resources,
            executor,
            vm,
            displays,
        )?;
        // Programs a logout or a crash ended left their leases behind, with nobody to give them
        // back: this desktop does, before it lends any of its own.
        service.release_taken_back()();
        Ok(Self {
            service,
            store,
            programs: Arc::default(),
            administrator: Arc::new(account_administers_computer),
            opened_with: Mutex::default(),
            launching: Mutex::new(()),
        })
    }

    /// Answer whether the person administers the computer with `check` instead of the
    /// account's groups: what the session says of its person, or a test's stand-in for one.
    #[must_use]
    pub fn with_administrator_check(mut self, check: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        self.administrator = check;
        self
    }

    /// The entries a manager connecting now may address: the programs set up
    /// in capsules and the appliance's published applications, read as it
    /// connects. A manager that is already connected keeps its own set, which
    /// only what it installs itself widens.
    fn allowed_entries(&self) -> Result<BTreeSet<String>, SessionError> {
        Ok(self
            .entries()?
            .into_iter()
            .map(|entry| entry.entry_id)
            .chain(
                self.vm_inventory()
                    .applications
                    .into_iter()
                    .map(|application| application.entry_id),
            )
            .collect())
    }

    fn entries(&self) -> Result<Vec<RegisteredProgram>, SessionError> {
        self.store.registered_programs().map_err(Into::into)
    }

    fn installation_states(&self) -> Result<Vec<crate::CapsuleState>, SessionError> {
        self.store.installations().map_err(Into::into)
    }

    fn vm_inventory(&self) -> crate::VmInventory {
        self.service.vm_inventory()
    }

    fn vm_installation_status(&self, workspace_id: &str) -> Option<crate::VmInstallation> {
        self.service.vm_installation_status(workspace_id)
    }

    fn refresh_vm_installation_status(
        &self,
        workspace_id: &str,
    ) -> Result<Option<crate::VmInstallation>, SessionError> {
        self.service
            .refresh_vm_installation_status(workspace_id)
            .map_err(Into::into)
    }

    fn vm_uninstallation_status(&self, workspace_id: &str) -> Option<crate::VmUninstallation> {
        self.service.vm_uninstallation_status(workspace_id)
    }

    fn refresh_vm_uninstallation_status(
        &self,
        workspace_id: &str,
    ) -> Result<Option<crate::VmUninstallation>, SessionError> {
        self.service
            .refresh_vm_uninstallation_status(workspace_id)
            .map_err(Into::into)
    }

    /// How `entry` opens now, and how it opens unless someone chooses otherwise.
    fn explain_entry(
        &self,
        entry: &RegisteredProgram,
        capabilities: &[CapOverride],
    ) -> Result<(crate::ExplainStatus, crate::RouteKind), SessionError> {
        self.service
            .explain_program(
                &entry.query,
                &entry.app_id,
                &entry.capsule_id,
                &entry.program,
                capabilities,
                try_unix_now(),
            )
            .map_err(Into::into)
    }

    fn preview(&self, query: &ApplicationQuery) -> Result<crate::ExplainStatus, SessionError> {
        self.service
            .explain(
                query,
                VmFallback::Disabled,
                "so.tilo.windows.preview",
                None,
                &[],
                try_unix_now(),
            )
            .map_err(Into::into)
    }

    fn launch_entry(
        &self,
        entry: &RegisteredProgram,
        workspace_id: &str,
        capabilities: Vec<CapOverride>,
    ) -> Result<crate::LaunchReceipt, SessionError> {
        let _launching = self
            .launching
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let layer = self
            .service
            .opens_with(&entry.capsule_id, &entry.program.id)?;
        self.refuse_beside_other_runner(entry, layer)?;
        // Query, program, target app identity, and disabled VM fallback all
        // come from server state. The client contributes only `entry.id`.
        let crate::Launched {
            receipt,
            program,
            release,
        } = self.service.launch(&LaunchInput {
            app_id: entry.app_id.clone(),
            workspace_id: workspace_id.to_string(),
            program_id: entry.program.id.clone(),
            capsule_id: Some(entry.capsule_id.clone()),
            query: entry.query.clone(),
            vm_fallback: VmFallback::Disabled,
            capabilities,
            now_unix: try_unix_now(),
        })?;
        crate::programs::watch(
            &self.programs,
            &entry.entry_id,
            workspace_id,
            &entry.program.display_name,
            program,
            true,
            release,
        )?;
        self.opened(&entry.entry_id, layer);
        Ok(receipt)
    }

    /// Refuse to open `entry` with `layer` while a program of its capsule is open with the other
    /// one: Wine and Proton would both keep the one prefix's registry and files in their own
    /// server, and each would overwrite what the other wrote.
    fn refuse_beside_other_runner(
        &self,
        entry: &RegisteredProgram,
        layer: Layer,
    ) -> Result<(), SessionError> {
        let running = self.programs.running();
        let mut opened_with = self
            .opened_with
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        opened_with.retain(|entry_id, _| running.contains(entry_id));
        for open in self.entries()? {
            if open.capsule_id != entry.capsule_id {
                continue;
            }
            if let Some(&other) = opened_with.get(&open.entry_id)
                && other != layer
            {
                return Err(SessionError::OpenWithOtherRunner {
                    open: open.program.display_name,
                    open_with: runner_name(other),
                    opening: entry.program.display_name.clone(),
                    with: runner_name(layer),
                });
            }
        }
        Ok(())
    }

    /// A program of `entry_id` is open with `layer` from now on.
    fn opened(&self, entry_id: &str, layer: Layer) {
        self.opened_with
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(entry_id.to_owned(), layer);
    }

    fn install(
        &self,
        mut source: File,
        source_name: &str,
        workspace_id: &str,
        capabilities: &[CapOverride],
    ) -> Result<InstallOutcome, SessionError> {
        self.service
            .install(
                &mut source,
                source_name,
                workspace_id,
                capabilities,
                try_unix_now(),
            )
            .map_err(Into::into)
    }

    fn install_in_vm(
        &self,
        source: File,
        source_name: &str,
        workspace_id: &str,
        expected: &crate::VmInstallerInput,
    ) -> Result<crate::VmInstallation, SessionError> {
        self.service
            .install_in_vm(source, source_name, workspace_id, expected)
            .map_err(Into::into)
    }

    fn publish_vm_candidate(
        &self,
        workspace_id: &str,
        entry_id: &str,
    ) -> Result<crate::VmCandidatePublication, SessionError> {
        self.service
            .publish_vm_install_candidate(workspace_id, entry_id)
            .map_err(Into::into)
    }

    fn remove_vm_published_application(&self, entry_id: &str) -> Result<(), SessionError> {
        self.service
            .remove_vm_published_application(entry_id)
            .map_err(Into::into)
    }

    fn uninstall_vm_published_application(
        &self,
        workspace_id: &str,
        entry_id: &str,
    ) -> Result<crate::VmUninstallation, SessionError> {
        self.service
            .uninstall_vm_published_application(workspace_id, entry_id)
            .map_err(Into::into)
    }

    fn escalate_install_to_vm(
        &self,
        capsule_id: &str,
        workspace_id: &str,
    ) -> Result<crate::VmInstallation, SessionError> {
        self.service
            .escalate_install_to_vm(capsule_id, workspace_id)
            .map_err(Into::into)
    }

    fn remove_launcher(&self, entry: &RegisteredProgram) -> Result<(), SessionError> {
        self.service
            .remove_launcher(&entry.capsule_id, &entry.program.id)
            .map_err(Into::into)
    }

    fn delete_data(&self, capsule_id: &str) -> Result<(), SessionError> {
        self.service.delete_capsule(capsule_id).map_err(Into::into)
    }
}

impl<R, B, E, V> ManagedCompatibility<R, B, E, V>
where
    R: RuntimeRegistry + 'static,
    B: LaunchResourceBroker + 'static,
    E: SandboxExecutor + 'static,
    V: VmDelegate + 'static,
{
    /// Take up the programs the desktop kept through its last restart: each is open again, ended
    /// and reported as one this service started. A setup none of them is ended with the desktop
    /// before.
    fn take_back(self: &Arc<Self>) {
        let entries = self.entries().unwrap_or_default();
        for taken in self.service.taken_back() {
            // A kept program opened with what its entry opens with now: nothing changes that while
            // it is open but a choice made since, which its next start takes.
            if let Some(entry) = entries
                .iter()
                .find(|entry| entry.entry_id == taken.entry_id)
                && let Ok(layer) = self
                    .service
                    .opens_with(&entry.capsule_id, &entry.program.id)
            {
                self.opened(&taken.entry_id, layer);
            }
            let release = self.service.release_taken_back();
            // A setup kept through the restart closes its transaction when it ends, as it would
            // have before.
            let program = match self.service.setup_entry(&taken.entry_id) {
                Some(capsule) => self.finishing(capsule, taken.program),
                None => taken.program,
            };
            if let Err(error) = crate::programs::watch(
                &self.programs,
                &taken.entry_id,
                &taken.workspace_id,
                &taken.display_name,
                program,
                // An appliance program closes inside Windows, as it did before the restart.
                !crate::vm_adapter::is_appliance_entry(&taken.entry_id),
                release,
            ) {
                tracing::warn!(%error, "a kept Windows program is not watched");
            }
        }
        self.service.settle_lost_setups();
    }

    /// `program`, the setup or uninstaller of `capsule`, closing its transaction when it ended.
    fn finishing(
        self: &Arc<Self>,
        capsule: String,
        program: Box<dyn crate::StartedProgram>,
    ) -> Box<dyn crate::StartedProgram> {
        // Weak: the service may end before the program does, and then has nothing to close.
        let managed = Arc::downgrade(self);
        crate::programs::finishing(program, move |outcome| {
            if let Some(managed) = managed.upgrade()
                && let Err(error) = managed.service.finish_setup(&capsule, outcome)
            {
                tracing::warn!(%error, "a Windows setup's end is not recorded");
            }
        })
    }

    /// Watch `setup` as a program of `workspace_id`, and answer the manager that it is open.
    fn watch_setup(
        self: &Arc<Self>,
        workspace_id: &str,
        setup: crate::service::StartedSetup,
    ) -> Result<crate::InstallReceipt, SessionError> {
        let crate::service::StartedSetup {
            receipt,
            entry_id,
            display_name,
            program,
            release,
        } = setup;
        let program = self.finishing(receipt.capsule_id.clone(), program);
        crate::programs::watch(
            &self.programs,
            &entry_id,
            workspace_id,
            &display_name,
            program,
            true,
            release,
        )?;
        Ok(receipt)
    }

    fn install_in_capsule(
        self: &Arc<Self>,
        source: File,
        source_name: &str,
        workspace_id: &str,
        capabilities: &[CapOverride],
        expected: &crate::VmInstallerInput,
    ) -> Result<crate::InstallReceipt, SessionError> {
        let setup = self.service.install_in_capsule(
            source,
            source_name,
            workspace_id,
            capabilities,
            try_unix_now(),
            expected,
        )?;
        self.watch_setup(workspace_id, setup)
    }

    fn retry_install(
        self: &Arc<Self>,
        capsule_id: &str,
        workspace_id: &str,
        capabilities: &[CapOverride],
    ) -> Result<crate::InstallReceipt, SessionError> {
        let setup =
            self.service
                .retry_install(capsule_id, workspace_id, capabilities, try_unix_now())?;
        self.watch_setup(workspace_id, setup)
    }

    fn vendor_uninstall(
        self: &Arc<Self>,
        capsule_id: &str,
        workspace_id: &str,
        capabilities: &[CapOverride],
    ) -> Result<crate::InstallReceipt, SessionError> {
        let setup = self.service.vendor_uninstall(
            capsule_id,
            workspace_id,
            capabilities,
            try_unix_now(),
        )?;
        self.watch_setup(workspace_id, setup)
    }

    /// Show `application` from the Windows appliance, with the portal-chosen `file` when one is
    /// given. Answered at once: the appliance starting and its window showing happen on a
    /// thread of their own, and how it went is reported as the program's end.
    fn launch_published_vm(
        self: &Arc<Self>,
        application: crate::VmPublishedApplication,
        workspace_id: &str,
        capabilities: Vec<CapOverride>,
        file: Option<File>,
    ) -> Result<crate::LaunchReceipt, SessionError> {
        let receipt = crate::LaunchReceipt {
            explanation: published_vm_explanation(application.presentation.label()),
        };
        let name = application.display_name.clone();
        let id = self
            .programs
            .started(&application.entry_id, workspace_id, None);
        let (managed, workspace_id) = (Arc::clone(self), workspace_id.to_owned());
        let ended_name = name.clone();
        let spawned = std::thread::Builder::new()
            .name("tilo-windows-appliance-program".to_owned())
            .spawn(move || {
                // The appliance has one interactive Windows session: a second client connecting
                // to it takes the session over, and the windows of the one before go with it.
                // An appliance program opens once the ones asked for before it closed, whether
                // this service showed them or took them back open after a desktop restart.
                managed
                    .programs
                    .wait_for_earlier(id, crate::vm_adapter::is_appliance_entry);
                let now_unix = try_unix_now();
                let service = &managed.service;
                let outcome = match file {
                    Some(file) => service.launch_published_vm_with_portal_file(
                        &application,
                        &workspace_id,
                        &capabilities,
                        now_unix,
                        file,
                        // This protocol action is intentionally read-only. A
                        // future write-back action must have a distinct portal
                        // decision rather than trusting a client-supplied bit.
                        windows_vm::PortalDocumentAccess::ReadOnly,
                    ),
                    None => service.launch_published_vm(
                        &application,
                        &workspace_id,
                        &capabilities,
                        now_unix,
                    ),
                };
                managed
                    .programs
                    .ended(id, &ended_name, outcome.map_err(|error| error.to_string()));
            });
        if let Err(error) = spawned {
            self.programs.ended(id, &name, Err(error.to_string()));
            return Err(error.into());
        }
        Ok(receipt)
    }

    /// Open `entry_id` again in `workspace_id`, for the session that kept the tab of
    /// it. Checked as a manager's launch is: the entry must be one set up here now,
    /// and the workspace one whose sandbox the session can grant the program.
    fn reopen(
        self: &Arc<Self>,
        entry_id: &str,
        workspace_id: &str,
        authority: &dyn CapabilityAuthority,
    ) -> Result<(), SessionError> {
        if let Some(entry) = self.entries()?.into_iter().find(|entry| {
            entry.entry_id == entry_id && entry.program.kind == ProgramKind::Application
        }) {
            let capabilities = authority
                .capabilities_for(workspace_id, &entry.app_id)
                .map_err(SessionError::Authority)?;
            self.launch_entry(&entry, workspace_id, capabilities)?;
            return Ok(());
        }
        if let Some(application) = self
            .vm_inventory()
            .applications
            .into_iter()
            .find(|application| application.entry_id == entry_id)
        {
            let capabilities = authority
                .capabilities_for(workspace_id, &application.app_id)
                .map_err(SessionError::Authority)?;
            self.launch_published_vm(application, workspace_id, capabilities, None)?;
            return Ok(());
        }
        Err(SessionError::UnknownEntry)
    }

    /// The entries a manager reading the service now may address, and the session
    /// reads as it reopens a saved tab.
    fn refresh_entries(&self, directory: &EntryDirectory) {
        if let Ok(entries) = self.allowed_entries() {
            directory.replace(entries);
        }
    }
}

#[derive(Clone)]
pub struct LauncherAuthenticator {
    expected_uid: u32,
    expected_parent_pid: u32,
    expected_executable: PathBuf,
    registry: Arc<dyn TrustedLauncherRegistry>,
    /// The launches that handed off already, by process and start time: each
    /// hands off once, so a launcher cannot open a second connection with a
    /// fresh entry set.
    handed_off: Arc<Mutex<BTreeSet<(u32, u64)>>>,
}

impl LauncherAuthenticator {
    #[must_use]
    pub fn new(
        expected_executable: impl Into<PathBuf>,
        expected_parent_pid: u32,
        registry: Arc<dyn TrustedLauncherRegistry>,
    ) -> Self {
        let expected_uid = rustix::process::geteuid().as_raw();
        Self {
            expected_uid,
            expected_parent_pid,
            expected_executable: expected_executable.into(),
            registry,
            handed_off: Arc::default(),
        }
    }

    fn authenticate(&self, stream: &UnixStream) -> Result<(), SessionError> {
        let facts = peer_facts::facts(stream).map_err(|_| SessionError::UnauthenticatedPeer)?;
        let (Some(pid), true) = (facts.pid, facts.uid == self.expected_uid) else {
            return Err(SessionError::UnauthenticatedPeer);
        };
        let expected = fs::canonicalize(&self.expected_executable)?;
        if facts.exe.as_deref() != Some(expected.as_path())
            || process_parent(pid)? != self.expected_parent_pid
        {
            return Err(SessionError::UnauthenticatedPeer);
        }
        Ok(())
    }

    fn consume_handoff(
        &self,
        stream: &UnixStream,
        handoff: &SessionHandoff,
    ) -> Result<(), SessionError> {
        let facts = peer_facts::facts(stream).map_err(|_| SessionError::UnauthenticatedPeer)?;
        let pid = facts.pid.ok_or(SessionError::UnauthenticatedPeer)?;
        let start_time = process_start_time(pid)?;
        // Registration happens immediately after `Command::spawn`; the child
        // may reach connect first on another CPU. Wait briefly for that one
        // parent-side insertion, never for an unregistered identity change.
        for _ in 0..20 {
            if self
                .registry
                .is_launch(pid, start_time, &handoff.app_id, &handoff.workspace_id)
            {
                return self.take_handoff(pid, start_time);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(SessionError::UnauthenticatedPeer)
    }

    /// Spend the one handoff of the launch `pid` started at `start_time`.
    /// Launches that ended are forgotten, so a later process given the same
    /// pid is a launch of its own.
    fn take_handoff(&self, pid: u32, start_time: u64) -> Result<(), SessionError> {
        let mut handed_off = self
            .handed_off
            .lock()
            .map_err(|_| SessionError::UnauthenticatedPeer)?;
        handed_off.retain(|&(earlier, started)| {
            process_start_time(earlier).is_ok_and(|now| now == started)
        });
        if handed_off.insert((pid, start_time)) {
            Ok(())
        } else {
            Err(SessionError::UnauthenticatedPeer)
        }
    }
}

/// The name a person knows `layer` by.
const fn runner_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Wine => "Wine",
        Layer::Proton => "Proton",
        Layer::Vm => "Windows",
    }
}

/// Why someone who does not administer the computer cannot choose how a program opens.
const NOT_ADMINISTRATOR: &str =
    "Only an administrator of this computer can change how this app opens.";

/// The group whose members administer the computer: the one its own administrator rules (polkit's,
/// `sudo`'s) name.
const ADMINISTRATORS_GROUP: &str = "wheel";

/// Whether the desktop's own account administers the computer: it is in the
/// administrators' group. A manager is let in only when its process is this account's (its peer
/// credentials say so), so the desktop's own groups answer for the person; the client's word
/// counts for nothing. Like every check by group, a group given to an account takes effect at
/// its next sign-in.
#[must_use]
pub fn account_administers_computer() -> bool {
    let Some(administrators) = group_id(ADMINISTRATORS_GROUP) else {
        return false;
    };
    rustix::process::getegid().as_raw() == administrators
        || rustix::process::getgroups()
            .is_ok_and(|groups| groups.iter().any(|group| group.as_raw() == administrators))
}

/// The id of the group `name`, as the computer's group database (files, `systemd-userdb`, an
/// image's `/usr/lib/group`) answers it.
fn group_id(name: &str) -> Option<u32> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut length = 16 * 1024;
    while length <= 1024 * 1024 {
        let mut group = std::mem::MaybeUninit::<libc::group>::uninit();
        let mut buffer = vec![0 as libc::c_char; length];
        let mut found = std::ptr::null_mut();
        // SAFETY: every pointer names live storage of the stated length; `getgrnam_r` writes the
        // entry into `group` and its strings into `buffer`, and sets `found` to `group` or null.
        let status = unsafe {
            libc::getgrnam_r(
                name.as_ptr(),
                group.as_mut_ptr(),
                buffer.as_mut_ptr(),
                buffer.len(),
                &raw mut found,
            )
        };
        if status == libc::ERANGE {
            length = length.saturating_mul(4);
            continue;
        }
        if status != 0 || found.is_null() {
            return None;
        }
        // SAFETY: `found` is non-null, so `getgrnam_r` filled `group` in.
        return Some(unsafe { group.assume_init() }.gr_gid);
    }
    None
}

fn process_parent(pid: u32) -> Result<u32, SessionError> {
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))
        .and_then(|value| value.trim().parse().ok())
        .ok_or(SessionError::UnauthenticatedPeer)
}

fn process_start_time(pid: u32) -> Result<u64, SessionError> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let end = stat.rfind(')').ok_or(SessionError::UnauthenticatedPeer)?;
    // Fields after comm begin at field 3 (state); starttime is field 22.
    stat.get(end.saturating_add(2)..)
        .and_then(|tail| tail.split_whitespace().nth(19))
        .and_then(|value| value.parse().ok())
        .ok_or(SessionError::UnauthenticatedPeer)
}

pub struct CompatSessionHandle {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    /// What the session asks as itself: the worker answering it ends with the service.
    reopenings: Reopenings,
}

impl Drop for CompatSessionHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = tilo_files::native::socket::connect(&self.path);
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        // A worker still answering a request is left to end on its own: one may wait on what a
        // program or the session does next, and a desktop restarting or logging out never waits
        // on a program. Each stops at its next request.
        reap_finished_workers(&self.workers);
        self.reopenings.stop();
        let _ = fs::remove_file(&self.path);
    }
}

pub fn spawn_session_service<R, B, E, V>(
    path: PathBuf,
    managed: Arc<ManagedCompatibility<R, B, E, V>>,
    authority: Arc<dyn CapabilityAuthority>,
    authenticator: LauncherAuthenticator,
    entries: EntryDirectory,
    reopenings: Reopenings,
) -> Result<CompatSessionHandle, SessionError>
where
    R: RuntimeRegistry + 'static,
    B: LaunchResourceBroker + 'static,
    E: SandboxExecutor + 'static,
    V: VmDelegate + 'static,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    if tilo_files::native::socket::connect(&path).is_ok() {
        return Err(SessionError::AlreadyRunning);
    }
    let listener = tilo_files::native::private::bind_socket(&path)?;
    // What the session reads as it reopens a saved tab, before it asks for one.
    managed.refresh_entries(&entries);
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let workers = Arc::new(Mutex::new(Vec::new()));
    let thread_workers = Arc::clone(&workers);
    let active_workers = Arc::new(AtomicUsize::new(0));
    let thread_active_workers = Arc::clone(&active_workers);
    let listener_managed = Arc::clone(&managed);
    let listener_authority = Arc::clone(&authority);
    let listener_thread = std::thread::Builder::new()
        .name("tilo-win-accept".to_string())
        .spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                reap_finished_workers(&thread_workers);
                match listener.accept() {
                    Ok((stream, _)) if thread_stop.load(Ordering::Acquire) => {
                        drop(stream);
                        break;
                    }
                    Ok((stream, _)) => {
                        let Some(permit) = WorkerPermit::try_acquire(&thread_active_workers) else {
                            // The only accepted peers are launcher children, but a
                            // compromised manager must still not turn retained
                            // descriptors into unbounded session threads.
                            drop(stream);
                            continue;
                        };
                        if stream.set_read_timeout(Some(CLIENT_IO_TIMEOUT)).is_err()
                            || stream.set_write_timeout(Some(CLIENT_IO_TIMEOUT)).is_err()
                        {
                            drop(permit);
                            continue;
                        }
                        let managed = Arc::clone(&listener_managed);
                        let authority = Arc::clone(&listener_authority);
                        let entries = entries.clone();
                        let authenticator = authenticator.clone();
                        let stop = Arc::clone(&thread_stop);
                        let worker = std::thread::Builder::new()
                            .name("tilo-windows-compat-client".to_string())
                            .spawn(move || {
                                let _permit = permit;
                                if authenticator.authenticate(&stream).is_ok() {
                                    let _ = serve_client(
                                        stream,
                                        &managed,
                                        authority.as_ref(),
                                        &authenticator,
                                        &entries,
                                        &stop,
                                    );
                                }
                            });
                        if let Ok(worker) = worker
                            && let Ok(mut workers) = thread_workers.lock()
                        {
                            workers.push(worker);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        })?;
    // The session asking as itself: a saved tab's program, opened on the same route
    // a manager's launch takes, and answered on the loop that asked.
    {
        let reopenings = reopenings.clone();
        std::thread::Builder::new()
            .name("tilo-win-saved".to_string())
            .spawn(move || {
                while let Some(ask) = reopenings.take() {
                    let outcome = managed
                        .reopen(&ask.entry_id, &ask.workspace_id, authority.as_ref())
                        .map_err(|error| error.to_string());
                    ask.answer.send(outcome);
                }
            })?;
    }
    Ok(CompatSessionHandle {
        path,
        stop,
        listener: Some(listener_thread),
        workers,
        reopenings,
    })
}

struct WorkerPermit {
    active: Arc<AtomicUsize>,
}

impl WorkerPermit {
    fn try_acquire(active: &Arc<AtomicUsize>) -> Option<Self> {
        let mut current = active.load(Ordering::Acquire);
        loop {
            if current >= MAX_CLIENT_WORKERS {
                return None;
            }
            let next = current.checked_add(1)?;
            match active.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => {
                    return Some(Self {
                        active: Arc::clone(active),
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }
}

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn reap_finished_workers(workers: &Mutex<Vec<JoinHandle<()>>>) {
    let finished = workers
        .lock()
        .map(|mut workers| {
            let (finished, pending): (Vec<_>, Vec<_>) = std::mem::take(&mut *workers)
                .into_iter()
                .partition(JoinHandle::is_finished);
            *workers = pending;
            finished
        })
        .unwrap_or_default();
    for worker in finished {
        let _ = worker.join();
    }
}

fn serve_client<R, B, E, V>(
    mut stream: UnixStream,
    managed: &Arc<ManagedCompatibility<R, B, E, V>>,
    authority: &dyn CapabilityAuthority,
    authenticator: &LauncherAuthenticator,
    entries: &EntryDirectory,
    stop: &AtomicBool,
) -> Result<(), SessionError>
where
    R: RuntimeRegistry + 'static,
    B: LaunchResourceBroker + 'static,
    E: SandboxExecutor + 'static,
    V: VmDelegate + 'static,
{
    let handoff: SessionHandoff =
        read_frame(&mut stream, launcher::services::COMPAT_HANDOFF_FRAMING)?;
    validate_handoff(&handoff)?;
    authenticator.consume_handoff(&stream, &handoff)?;
    let mut allowed_entry_ids = managed.allowed_entries()?;
    let mut pending_vm_install = None;
    let mut ends_read = EndsRead::default();
    loop {
        if !next_request_arrives(&stream, stop)? {
            return Ok(());
        }
        let (request, descriptor) = match read_request_frame(&mut stream, FRAMING) {
            Ok(request) => request,
            Err(SessionError::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let changes_entries = changes_entries(&request);
        let response = handle_request(
            managed,
            authority,
            &handoff,
            &mut allowed_entry_ids,
            &mut pending_vm_install,
            &mut ends_read,
            request,
            descriptor,
        );
        if changes_entries && !matches!(response, ProtocolResponse::Error { .. }) {
            managed.refresh_entries(entries);
        }
        write_frame(&mut stream, &response, FRAMING)?;
    }
}

/// Whether `request` can change which entries the service can open: an installation,
/// a removal of one, or the appliance publishing an application a manager may launch.
fn changes_entries(request: &ProtocolRequest) -> bool {
    matches!(
        request,
        ProtocolRequest::Install { .. }
            | ProtocolRequest::InstallInVm { .. }
            | ProtocolRequest::InstallInCapsule { .. }
            | ProtocolRequest::PublishVmCandidate { .. }
            | ProtocolRequest::RetryInstall { .. }
            | ProtocolRequest::EscalateInstallToVm { .. }
            | ProtocolRequest::RemoveLauncher { .. }
            | ProtocolRequest::VendorUninstall { .. }
            | ProtocolRequest::VendorUninstallVm { .. }
            | ProtocolRequest::DeleteData { .. }
            | ProtocolRequest::RefreshVmInstallation { .. }
    )
}

fn validate_handoff(handoff: &SessionHandoff) -> Result<(), SessionError> {
    if handoff.protocol != launcher::services::COMPAT_HANDOFF_PROTOCOL
        || handoff.app_id != MANAGER_APP_ID
        || !valid_id(&handoff.workspace_id)
    {
        return Err(SessionError::InvalidHandoff);
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive protocol dispatch keeps descriptor and authority checks in one boundary"
)]
fn handle_request<R, B, E, V>(
    managed: &Arc<ManagedCompatibility<R, B, E, V>>,
    authority: &dyn CapabilityAuthority,
    handoff: &SessionHandoff,
    allowed_entry_ids: &mut BTreeSet<String>,
    pending_vm_install: &mut Option<PendingVmInstall>,
    ends_read: &mut EndsRead,
    request: ProtocolRequest,
    descriptor: Option<OwnedFd>,
) -> ProtocolResponse
where
    R: RuntimeRegistry + 'static,
    B: LaunchResourceBroker + 'static,
    E: SandboxExecutor + 'static,
    V: VmDelegate + 'static,
{
    let version = match &request {
        ProtocolRequest::List { version }
        | ProtocolRequest::Launch { version, .. }
        | ProtocolRequest::LaunchVmFallback { version, .. }
        | ProtocolRequest::Stop { version, .. }
        | ProtocolRequest::LaunchVmWithPortalFile { version, .. }
        | ProtocolRequest::Install { version, .. }
        | ProtocolRequest::InstallInVm { version, .. }
        | ProtocolRequest::InstallInCapsule { version, .. }
        | ProtocolRequest::PublishVmCandidate { version, .. }
        | ProtocolRequest::RefreshVmInstallation { version }
        | ProtocolRequest::RefreshVmUninstallation { version }
        | ProtocolRequest::RetryInstall { version, .. }
        | ProtocolRequest::EscalateInstallToVm { version, .. }
        | ProtocolRequest::RemoveLauncher { version, .. }
        | ProtocolRequest::VendorUninstall { version, .. }
        | ProtocolRequest::VendorUninstallVm { version, .. }
        | ProtocolRequest::DeleteData { version, .. }
        | ProtocolRequest::SetOpensWith { version, .. } => *version,
        ProtocolRequest::Resolve(request) => request.version,
    };
    if version != PROTOCOL_VERSION {
        return ProtocolResponse::Error {
            error: ProtocolError::unsupported_version(version),
        };
    }
    let has_descriptor = descriptor.is_some();
    let expects_descriptor = matches!(
        &request,
        ProtocolRequest::Install { .. }
            | ProtocolRequest::InstallInVm { .. }
            | ProtocolRequest::InstallInCapsule { .. }
            | ProtocolRequest::LaunchVmWithPortalFile { .. }
    );
    if has_descriptor != expects_descriptor {
        return ProtocolResponse::Error {
            error: ProtocolError::new(
                "invalid_descriptor",
                "only install requests accept exactly one portal file descriptor",
            ),
        };
    }
    // What the desktop kept through its last restart is open again before anything is read of
    // the programs.
    managed.take_back();
    let result: Result<ProtocolResponse, SessionError> = (|| match request {
        ProtocolRequest::List { .. } => {
            list_applications(managed, authority, handoff, allowed_entry_ids, ends_read)
        }
        ProtocolRequest::Stop { entry_id, .. } => {
            if valid_id(&entry_id)
                && allowed_entry_ids.contains(&entry_id)
                && managed.programs.stop(&handoff.workspace_id, &entry_id)
            {
                Ok(ProtocolResponse::Stopped { entry_id })
            } else {
                Err(SessionError::NotRunning)
            }
        }
        ProtocolRequest::Resolve(request) => {
            managed
                .preview(&request.query)
                .map(|explanation| ProtocolResponse::Resolved {
                    explanation,
                    preview_only: true,
                })
        }
        ProtocolRequest::Launch { entry_id, .. } => {
            if valid_id(&entry_id) && allowed_entry_ids.contains(&entry_id) {
                let entry = managed
                    .entries()?
                    .into_iter()
                    .find(|entry| {
                        entry.entry_id == entry_id && entry.program.kind == ProgramKind::Application
                    })
                    .ok_or(SessionError::UnknownEntry)?;
                let capabilities = authority
                    .capabilities_for(&handoff.workspace_id, &entry.app_id)
                    .map_err(SessionError::Authority)?;
                managed
                    .launch_entry(&entry, &handoff.workspace_id, capabilities)
                    .map(|receipt| ProtocolResponse::Launched { receipt })
            } else {
                Err(SessionError::UnknownEntry)
            }
        }
        ProtocolRequest::LaunchVmFallback { entry_id, .. } => {
            if valid_id(&entry_id) && allowed_entry_ids.contains(&entry_id) {
                if let Some(application) = managed
                    .vm_inventory()
                    .applications
                    .into_iter()
                    .find(|application| application.entry_id == entry_id)
                {
                    let capabilities = authority
                        .capabilities_for(&handoff.workspace_id, &application.app_id)
                        .map_err(SessionError::Authority)?;
                    managed
                        .launch_published_vm(application, &handoff.workspace_id, capabilities, None)
                        .map(|receipt| ProtocolResponse::Launched { receipt })
                } else {
                    // Private capsule entries cannot be mapped to an
                    // authenticated guest publication. Treating their local
                    // program ID as one would create an action guaranteed to
                    // fail guest catalogue validation.
                    Err(SessionError::UnknownEntry)
                }
            } else {
                Err(SessionError::UnknownEntry)
            }
        }
        ProtocolRequest::LaunchVmWithPortalFile { entry_id, .. } => {
            if !valid_id(&entry_id) || !allowed_entry_ids.contains(&entry_id) {
                return Err(SessionError::UnknownEntry);
            }
            let application = managed
                .vm_inventory()
                .applications
                .into_iter()
                .find(|app| app.entry_id == entry_id)
                .ok_or(SessionError::UnknownEntry)?;
            let capabilities = authority
                .capabilities_for(&handoff.workspace_id, &application.app_id)
                .map_err(SessionError::Authority)?;
            let source = File::from(descriptor.ok_or(SessionError::DescriptorMissing)?);
            managed
                .launch_published_vm(
                    application,
                    &handoff.workspace_id,
                    capabilities,
                    Some(source),
                )
                .map(|receipt| ProtocolResponse::Launched { receipt })
        }
        ProtocolRequest::Install { source_name, .. } => {
            let capabilities = authority
                .capabilities_for(&handoff.workspace_id, "so.tilo.windows.import")
                .map_err(SessionError::Authority)?;
            let source = File::from(descriptor.ok_or(SessionError::DescriptorMissing)?);
            match managed.install(source, &source_name, &handoff.workspace_id, &capabilities)? {
                InstallOutcome::Confirmation {
                    confirmation,
                    input,
                } => {
                    let expires_at_unix = try_unix_now()
                        .unwrap_or_default()
                        .saturating_add(VM_INSTALL_CONFIRMATION_SECONDS);
                    *pending_vm_install = Some(PendingVmInstall {
                        input,
                        expires_at_unix,
                    });
                    Ok(ProtocolResponse::VmInstallConfirmation { confirmation })
                }
            }
        }
        ProtocolRequest::InstallInCapsule { source_name, .. } => {
            let source = File::from(descriptor.ok_or(SessionError::DescriptorMissing)?);
            let pending = pending_vm_install
                .take()
                .filter(|pending| try_unix_now().is_some_and(|now| now <= pending.expires_at_unix))
                .ok_or(SessionError::VmInstallConfirmationExpired)?;
            let capabilities = authority
                .capabilities_for(&handoff.workspace_id, "so.tilo.windows.import")
                .map_err(SessionError::Authority)?;
            let receipt = managed.install_in_capsule(
                source,
                &source_name,
                &handoff.workspace_id,
                &capabilities,
                &pending.input,
            )?;
            authorize_capsule_entries(managed, allowed_entry_ids, &receipt.capsule_id)?;
            Ok(ProtocolResponse::Installed { receipt })
        }
        ProtocolRequest::InstallInVm { source_name, .. } => {
            let source = File::from(descriptor.ok_or(SessionError::DescriptorMissing)?);
            let pending = pending_vm_install
                .take()
                .filter(|pending| try_unix_now().is_some_and(|now| now <= pending.expires_at_unix))
                .ok_or(SessionError::VmInstallConfirmationExpired)?;
            let installation = managed.install_in_vm(
                source,
                &source_name,
                &handoff.workspace_id,
                &pending.input,
            )?;
            authorize_vm_candidates(allowed_entry_ids, &installation);
            Ok(ProtocolResponse::VmInstallStarted { installation })
        }
        ProtocolRequest::PublishVmCandidate { entry_id, .. } => {
            if !valid_id(&entry_id) || !allowed_entry_ids.contains(&entry_id) {
                return Err(SessionError::UnknownEntry);
            }
            let publication = managed.publish_vm_candidate(&handoff.workspace_id, &entry_id)?;
            authorize_vm_publication(allowed_entry_ids, &publication);
            Ok(ProtocolResponse::VmCandidatePublished {
                installation: publication.installation,
            })
        }
        ProtocolRequest::RefreshVmInstallation { .. } => {
            let installation = managed.refresh_vm_installation_status(&handoff.workspace_id)?;
            if let Some(installation) = &installation {
                authorize_vm_candidates(allowed_entry_ids, installation);
            }
            Ok(ProtocolResponse::VmInstallationRefreshed { installation })
        }
        ProtocolRequest::RefreshVmUninstallation { .. } => {
            let uninstallation = managed.refresh_vm_uninstallation_status(&handoff.workspace_id)?;
            if let Some(status) = &uninstallation
                && status.phase == crate::VmInstallationPhase::Completed
            {
                allowed_entry_ids.remove(&status.entry_id);
            }
            Ok(ProtocolResponse::VmUninstallationRefreshed { uninstallation })
        }
        ProtocolRequest::RetryInstall { capsule_id, .. } => {
            require_authorized_capsule(managed, allowed_entry_ids, &capsule_id)?;
            let capabilities = authority
                .capabilities_for(&handoff.workspace_id, "so.tilo.windows.import")
                .map_err(SessionError::Authority)?;
            let receipt =
                managed.retry_install(&capsule_id, &handoff.workspace_id, &capabilities)?;
            authorize_capsule_entries(managed, allowed_entry_ids, &capsule_id)?;
            Ok(ProtocolResponse::Retried { receipt })
        }
        ProtocolRequest::EscalateInstallToVm { capsule_id, .. } => {
            require_authorized_capsule(managed, allowed_entry_ids, &capsule_id)?;
            let installation =
                managed.escalate_install_to_vm(&capsule_id, &handoff.workspace_id)?;
            authorize_vm_candidates(allowed_entry_ids, &installation);
            Ok(ProtocolResponse::VmInstallStarted { installation })
        }
        ProtocolRequest::RemoveLauncher { entry_id, .. } => {
            if !valid_id(&entry_id) || !allowed_entry_ids.contains(&entry_id) {
                return Err(SessionError::UnknownEntry);
            }
            if managed
                .vm_inventory()
                .applications
                .iter()
                .any(|application| application.entry_id == entry_id)
            {
                managed.remove_vm_published_application(&entry_id)?;
                allowed_entry_ids.remove(&entry_id);
                return Ok(ProtocolResponse::LauncherRemoved { entry_id });
            }
            let entry = managed
                .entries()?
                .into_iter()
                .find(|entry| {
                    entry.entry_id == entry_id && entry.program.kind == ProgramKind::Application
                })
                .ok_or(SessionError::UnknownEntry)?;
            managed.remove_launcher(&entry)?;
            allowed_entry_ids.remove(&entry_id);
            Ok(ProtocolResponse::LauncherRemoved { entry_id })
        }
        ProtocolRequest::VendorUninstall { capsule_id, .. } => {
            require_authorized_capsule(managed, allowed_entry_ids, &capsule_id)?;
            let capabilities = authority
                .capabilities_for(&handoff.workspace_id, "so.tilo.windows.import")
                .map_err(SessionError::Authority)?;
            let receipt =
                managed.vendor_uninstall(&capsule_id, &handoff.workspace_id, &capabilities)?;
            retain_capsule_control_entry(managed, allowed_entry_ids, &capsule_id)?;
            Ok(ProtocolResponse::VendorUninstalled { receipt })
        }
        ProtocolRequest::VendorUninstallVm { entry_id, .. } => {
            let authorized = managed
                .vm_inventory()
                .applications
                .iter()
                .any(|application| {
                    application.entry_id == entry_id && application.uninstall_available
                });
            if !valid_id(&entry_id) || !allowed_entry_ids.contains(&entry_id) || !authorized {
                return Err(SessionError::UnknownEntry);
            }
            let uninstallation =
                managed.uninstall_vm_published_application(&handoff.workspace_id, &entry_id)?;
            if uninstallation.phase == crate::VmInstallationPhase::Completed {
                allowed_entry_ids.remove(&entry_id);
            }
            Ok(ProtocolResponse::VmVendorUninstallation { uninstallation })
        }
        ProtocolRequest::DeleteData { capsule_id, .. } => {
            require_authorized_capsule(managed, allowed_entry_ids, &capsule_id)?;
            let retiring = managed
                .entries()?
                .into_iter()
                .filter(|entry| entry.capsule_id == capsule_id)
                .collect::<Vec<_>>();
            // A program of the capsule that is open is ended before its files go: the keeper
            // takes its window and tab with it, and nothing is deleted under a program still
            // using it. A program that does not end says so, and nothing is deleted.
            let entries = retiring
                .iter()
                .map(|entry| entry.entry_id.clone())
                .collect::<BTreeSet<_>>();
            let still_open = managed
                .programs
                .close_entries(&entries, CLOSE_BEFORE_DELETE);
            if !still_open.is_empty() {
                let names: Vec<&str> = retiring
                    .iter()
                    .filter(|entry| still_open.contains(&entry.entry_id))
                    .map(|entry| entry.program.display_name.as_str())
                    .collect();
                return Err(SessionError::StillOpen(names.join(", ")));
            }
            managed.delete_data(&capsule_id)?;
            for entry in retiring {
                allowed_entry_ids.remove(&entry.entry_id);
            }
            Ok(ProtocolResponse::DataDeleted { capsule_id })
        }
        ProtocolRequest::SetOpensWith {
            entry_id, route, ..
        } => {
            if !valid_id(&entry_id) || !allowed_entry_ids.contains(&entry_id) {
                return Err(SessionError::UnknownEntry);
            }
            if !(managed.administrator)() {
                return Err(SessionError::NotAdministrator);
            }
            let entry = managed
                .entries()?
                .into_iter()
                .find(|entry| {
                    entry.entry_id == entry_id && entry.program.kind == ProgramKind::Application
                })
                .ok_or(SessionError::UnknownEntry)?;
            managed
                .service
                .set_opens_with(&entry.capsule_id, &entry.program.id, route)?;
            Ok(ProtocolResponse::OpensWithSet { entry_id, route })
        }
    })();
    result.unwrap_or_else(|error| ProtocolResponse::Error {
        error: ProtocolError::new(error.code(), error.to_string()),
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "one projection builds the complete manager DTO from server-owned state"
)]
fn list_applications<R, B, E, V>(
    managed: &ManagedCompatibility<R, B, E, V>,
    authority: &dyn CapabilityAuthority,
    handoff: &SessionHandoff,
    allowed_entry_ids: &mut BTreeSet<String>,
    ends_read: &mut EndsRead,
) -> Result<ProtocolResponse, SessionError>
where
    R: RuntimeRegistry,
    B: LaunchResourceBroker + 'static,
    E: SandboxExecutor,
    V: VmDelegate,
{
    // A setup goes on after the request that started it: the launchers it found since are this
    // manager's as the capsule it may already address is.
    let entries = managed.entries()?;
    let capsules: BTreeSet<&str> = entries
        .iter()
        .filter(|entry| allowed_entry_ids.contains(&entry.entry_id))
        .map(|entry| entry.capsule_id.as_str())
        .collect();
    let widened: Vec<String> = entries
        .iter()
        .filter(|entry| capsules.contains(entry.capsule_id.as_str()))
        .map(|entry| entry.entry_id.clone())
        .collect();
    allowed_entry_ids.extend(widened);
    let mut applications = Vec::new();
    let administrator = (managed.administrator)();
    // Read before the ends, so a program that ends in between is reported
    // ended rather than both running and ended.
    let running = managed.programs.running_in(&handoff.workspace_id);
    let ended = managed.programs.ends_for(&handoff.workspace_id, ends_read);
    // A list refresh reports real delegate readiness, but never wakes the
    // fallback appliance merely to draw a manager badge.
    let vm_inventory = managed.vm_inventory();
    let vm_availability = vm_inventory.availability.clone();
    let vm_installation = managed.vm_installation_status(&handoff.workspace_id);
    let vm_uninstallation = managed.vm_uninstallation_status(&handoff.workspace_id);
    if let Some(installation) = &vm_installation {
        authorize_vm_candidates(allowed_entry_ids, installation);
    }
    for entry in entries.into_iter().filter(|entry| {
        allowed_entry_ids.contains(&entry.entry_id)
            && entry.program.kind == ProgramKind::Application
    }) {
        let capabilities = authority
            .capabilities_for(&handoff.workspace_id, &entry.app_id)
            .map_err(SessionError::Authority)?;
        let (explanation, recommended_route) = managed.explain_entry(&entry, &capabilities)?;
        let installation_status = managed
            .installation_states()?
            .into_iter()
            .find(|state| state.id == entry.capsule_id)
            .and_then(|state| state.installation.map(|installation| installation.status));
        let is_running = running.contains(&entry.entry_id);
        applications.push(ManagedApplication {
            id: entry.entry_id,
            app_id: entry.app_id,
            display_name: entry.program.display_name,
            publisher: entry
                .query
                .publisher
                .or(entry.query.store)
                .unwrap_or_else(|| "Windows application".to_string()),
            capsule_id: Some(entry.capsule_id),
            program_id: entry.program.id,
            explanation,
            // Only an authenticated guest catalogue publication can be sent
            // to the VM. A private Wine/Proton program has no trusted guest
            // application ID, so it never exposes a predictably failing VM
            // fallback action.
            vm_launch_available: false,
            vm_unavailable_reason: Some(
                "Open in Windows VM is available only for published Windows guest applications."
                    .to_string(),
            ),
            presentation: None,
            portal_file_open_available: false,
            portal_file_open_unavailable_reason: Some(
                "Open file is available only for published Windows guest applications.".to_string(),
            ),
            // The resource broker gives every Wine/UMU launch a fresh
            // playback-only PipeWire/Pulse graph. A startup failure is a
            // launch error, not an unavailable feature in the UI.
            audio_output_available: true,
            audio_unavailable_reason: None,
            installation_status,
            remove_launcher_available: true,
            vendor_uninstall_available: false,
            running: is_running,
            recommended_route,
            opens_with_choice_available: administrator,
            opens_with_unavailable_reason: (!administrator).then(|| NOT_ADMINISTRATOR.to_owned()),
        });
    }
    for application in vm_inventory
        .applications
        .into_iter()
        .filter(|application| allowed_entry_ids.contains(&application.entry_id))
    {
        let is_running = running.contains(&application.entry_id);
        applications.push(ManagedApplication {
            id: application.entry_id,
            app_id: application.app_id,
            display_name: application.display_name,
            publisher: "Managed Windows appliance".to_string(),
            capsule_id: None,
            program_id: application.published_application_id,
            explanation: published_vm_explanation(application.presentation.label()),
            vm_launch_available: vm_availability.available,
            vm_unavailable_reason: (!vm_availability.available)
                .then(|| vm_availability.detail.clone()),
            presentation: Some(application.presentation),
            portal_file_open_available: application.presentation.supports_portal_files()
                && vm_availability.available,
            portal_file_open_unavailable_reason: (!application.presentation.supports_portal_files())
                .then(|| {
                    "Open file is unavailable in app-focused SPICE because this presentation has no portal file bridge."
                        .to_string()
                })
                .or_else(|| (!vm_availability.available).then(|| vm_availability.detail.clone())),
            audio_output_available: true,
            audio_unavailable_reason: None,
            installation_status: None,
            remove_launcher_available: true,
            vendor_uninstall_available: application.uninstall_available,
            running: is_running,
            recommended_route: crate::RouteKind::Vm,
            // An application of the Windows appliance opens there and nowhere else.
            opens_with_choice_available: false,
            opens_with_unavailable_reason: None,
        });
    }
    let authorized_capsules = authorized_capsules(managed, allowed_entry_ids)?;
    let installations = managed
        .installation_states()?
        .into_iter()
        .filter(|state| authorized_capsules.contains(&state.id))
        .filter_map(|state| {
            let installation = state.installation?;
            let program_count = state
                .programs
                .values()
                .filter(|program| program.kind == ProgramKind::Application)
                .count();
            let detail = installation.last_error.clone().unwrap_or_else(|| {
                match installation.status {
                    InstallationStatus::Installed => {
                        format!("{program_count} application launcher(s) in this private capsule.")
                    }
                    InstallationStatus::NeedsAttention => {
                        "Setup completed but no launcher was found; retry setup or delete its private data."
                            .to_string()
                    }
                    InstallationStatus::Failed => {
                        "Setup failed; the exact selected installer is retained for retry."
                            .to_string()
                    }
                    InstallationStatus::Uninstalled => {
                        "Vendor uninstall completed; private leftover data is retained."
                            .to_string()
                    }
                    InstallationStatus::Installing => "Setup is running.".to_string(),
                    InstallationStatus::Uninstalling => {
                        "The vendor uninstaller is running.".to_string()
                    }
                }
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
                    InstallationStatus::Failed | InstallationStatus::NeedsAttention
                ),
                can_vendor_uninstall: installation.uninstaller_relative_path.is_some()
                    && !matches!(
                        installation.status,
                        InstallationStatus::Installing
                            | InstallationStatus::Uninstalling
                            | InstallationStatus::Uninstalled
                    ),
                can_delete_data: !matches!(
                    installation.status,
                    InstallationStatus::Installing | InstallationStatus::Uninstalling
                ),
                detail,
            })
        })
        .collect();
    Ok(ProtocolResponse::Listed {
        applications,
        installations,
        vm_availability,
        vm_installation,
        vm_uninstallation,
        ended,
    })
}

fn authorized_capsules<R, B, E, V>(
    managed: &ManagedCompatibility<R, B, E, V>,
    allowed_entry_ids: &BTreeSet<String>,
) -> Result<BTreeSet<String>, SessionError>
where
    R: RuntimeRegistry,
    B: LaunchResourceBroker + 'static,
    E: SandboxExecutor,
    V: VmDelegate,
{
    Ok(managed
        .entries()?
        .into_iter()
        .filter(|entry| allowed_entry_ids.contains(&entry.entry_id))
        .map(|entry| entry.capsule_id)
        .collect())
}

fn require_authorized_capsule<R, B, E, V>(
    managed: &ManagedCompatibility<R, B, E, V>,
    allowed_entry_ids: &BTreeSet<String>,
    capsule_id: &str,
) -> Result<(), SessionError>
where
    R: RuntimeRegistry,
    B: LaunchResourceBroker + 'static,
    E: SandboxExecutor,
    V: VmDelegate,
{
    if valid_id(capsule_id) && authorized_capsules(managed, allowed_entry_ids)?.contains(capsule_id)
    {
        Ok(())
    } else {
        Err(SessionError::UnknownEntry)
    }
}

fn authorize_capsule_entries<R, B, E, V>(
    managed: &ManagedCompatibility<R, B, E, V>,
    allowed_entry_ids: &mut BTreeSet<String>,
    capsule_id: &str,
) -> Result<(), SessionError>
where
    R: RuntimeRegistry,
    B: LaunchResourceBroker + 'static,
    E: SandboxExecutor,
    V: VmDelegate,
{
    allowed_entry_ids.extend(
        managed
            .entries()?
            .into_iter()
            .filter(|entry| entry.capsule_id == capsule_id)
            .map(|entry| entry.entry_id),
    );
    Ok(())
}

fn authorize_vm_candidates(
    allowed_entry_ids: &mut BTreeSet<String>,
    installation: &crate::VmInstallation,
) {
    allowed_entry_ids.extend(
        installation
            .candidates
            .iter()
            .map(|candidate| candidate.entry_id.clone()),
    );
}

fn authorize_vm_publication(
    allowed_entry_ids: &mut BTreeSet<String>,
    publication: &crate::VmCandidatePublication,
) {
    // This set belongs to exactly one authenticated connection.  Its caller
    // has already proved that the connection's workspace owns the candidate.
    // Never add the resulting guest catalogue entry to any other connection.
    allowed_entry_ids.insert(publication.published_entry_id.clone());
    authorize_vm_candidates(allowed_entry_ids, &publication.installation);
}

fn retain_capsule_control_entry<R, B, E, V>(
    managed: &ManagedCompatibility<R, B, E, V>,
    allowed_entry_ids: &mut BTreeSet<String>,
    capsule_id: &str,
) -> Result<(), SessionError>
where
    R: RuntimeRegistry,
    B: LaunchResourceBroker + 'static,
    E: SandboxExecutor,
    V: VmDelegate,
{
    let current = managed
        .entries()?
        .into_iter()
        .map(|entry| entry.entry_id)
        .collect::<BTreeSet<_>>();
    allowed_entry_ids.retain(|entry| current.contains(entry));
    authorize_capsule_entries(managed, allowed_entry_ids, capsule_id)
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub struct SessionClient {
    stream: UnixStream,
}

impl SessionClient {
    #[must_use]
    pub fn from_owned_fd(owned: OwnedFd) -> Self {
        Self {
            stream: UnixStream::from(owned),
        }
    }

    pub fn from_env() -> Result<Self, SessionError> {
        let raw = std::env::var("TILO_SVC_COMPAT_FD")
            .map_err(|_| SessionError::ServiceFdMissing)?
            .parse::<i32>()
            .map_err(|_| SessionError::ServiceFdMissing)?;
        if raw < 0 {
            return Err(SessionError::ServiceFdMissing);
        }
        // SAFETY: the launcher owns this slot for the application and hands
        // over a connected Unix stream. `SessionClient` becomes its sole owner.
        let owned = unsafe { OwnedFd::from_raw_fd(raw) };
        Ok(Self::from_owned_fd(owned))
    }

    pub fn request(&mut self, request: &ProtocolRequest) -> Result<ProtocolResponse, SessionError> {
        write_frame(&mut self.stream, request, FRAMING)?;
        read_frame(&mut self.stream, FRAMING)
    }

    /// Send one request together with the already-open file selected by the
    /// standard Tilo/XDG portal.  The only consumers are the installation
    /// flow and the descriptor-only `RemoteApp` file-launch flow; neither ever
    /// accepts a pathname from the client.
    pub fn request_with_file(
        &mut self,
        request: &ProtocolRequest,
        file: &File,
    ) -> Result<ProtocolResponse, SessionError> {
        if !matches!(
            request,
            ProtocolRequest::Install { .. }
                | ProtocolRequest::InstallInVm { .. }
                | ProtocolRequest::InstallInCapsule { .. }
                | ProtocolRequest::LaunchVmWithPortalFile { .. }
        ) {
            return Err(SessionError::DescriptorInvalid);
        }
        write_frame_with_fd(&mut self.stream, request, FRAMING, file.as_fd())?;
        read_frame(&mut self.stream, FRAMING)
    }
}

/// Waits, as long as the manager stays connected and the service is up, for the first byte of
/// its next request: `false` once it hung up or the service stops.
///
/// A manager asks again only when its window draws, so a connection may stay quiet for minutes
/// (the person is choosing an installer, or looking at another window). The read timeout bounds
/// a request that has begun, not the wait between requests; it serves here as the interval at
/// which the wait looks whether the service stops.
fn next_request_arrives(stream: &UnixStream, stop: &AtomicBool) -> Result<bool, SessionError> {
    let mut first = [0_u8; 1];
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(false);
        }
        match rustix::net::recv(stream, &mut first[..], RecvFlags::PEEK) {
            Ok((0, _)) => return Ok(false),
            Ok(_) => return Ok(true),
            Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {}
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
    }
}

fn read_request_frame(
    stream: &mut UnixStream,
    framing: Framing,
) -> Result<(ProtocolRequest, Option<OwnedFd>), SessionError> {
    let (length, descriptor) = recv_length_with_fd(stream)?;
    let length = framing.decode_prefix(length).map_err(frame_error)?;
    let mut bytes = vec![0_u8; length];
    stream.read_exact(&mut bytes)?;
    let request = serde_json::from_slice(&bytes)?;
    Ok((request, descriptor))
}

fn recv_length_with_fd(stream: &UnixStream) -> Result<([u8; 4], Option<OwnedFd>), SessionError> {
    let mut length = [0_u8; 4];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let received = rustix::net::recvmsg(
        stream,
        &mut [IoSliceMut::new(&mut length)],
        &mut control,
        RecvFlags::WAITALL | RecvFlags::CMSG_CLOEXEC,
    )
    .map_err(std::io::Error::from)?;
    if received.bytes == 0 {
        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
    }
    if received.bytes != length.len() || received.flags.contains(ReturnFlags::CTRUNC) {
        return Err(SessionError::DescriptorInvalid);
    }

    let mut descriptor = None;
    for message in control.drain() {
        let RecvAncillaryMessage::ScmRights(fds) = message else {
            return Err(SessionError::DescriptorInvalid);
        };
        for fd in fds {
            if descriptor.replace(fd).is_some() {
                return Err(SessionError::DescriptorInvalid);
            }
        }
    }
    Ok((length, descriptor))
}

fn write_frame_with_fd<T: Serialize>(
    stream: &mut UnixStream,
    value: &T,
    framing: Framing,
    descriptor: BorrowedFd<'_>,
) -> Result<(), SessionError> {
    let bytes = serde_json::to_vec(value)?;
    let length = framing.encode_prefix(bytes.len()).map_err(frame_error)?;
    send_length_with_fd(stream, length, descriptor)?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}

fn send_length_with_fd(
    stream: &UnixStream,
    length: [u8; 4],
    descriptor: BorrowedFd<'_>,
) -> Result<(), SessionError> {
    let descriptors = [descriptor];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !control.push(SendAncillaryMessage::ScmRights(&descriptors)) {
        return Err(SessionError::DescriptorInvalid);
    }
    let sent = rustix::net::sendmsg(
        stream,
        &[IoSlice::new(&length)],
        &mut control,
        SendFlags::NOSIGNAL,
    )
    .map_err(std::io::Error::from)?;
    if sent != length.len() {
        return Err(SessionError::DescriptorInvalid);
    }
    Ok(())
}

fn read_frame<T: serde::de::DeserializeOwned>(
    stream: &mut UnixStream,
    framing: Framing,
) -> Result<T, SessionError> {
    let bytes = framing
        .read(stream)
        .and_then(|bytes| bytes.ok_or(FrameError::Closed))
        .map_err(frame_error)?;
    serde_json::from_slice(&bytes).map_err(Into::into)
}

fn write_frame<T: Serialize>(
    stream: &mut UnixStream,
    value: &T,
    framing: Framing,
) -> Result<(), SessionError> {
    let bytes = serde_json::to_vec(value)?;
    framing.write(stream, &bytes).map_err(frame_error)?;
    stream.flush()?;
    Ok(())
}

/// Stream failures and closes stay I/O errors, so a peer hanging up between
/// requests ends the session cleanly; anything wrong with a frame's size is
/// [`SessionError::FrameTooLarge`].
fn frame_error(error: FrameError) -> SessionError {
    match error {
        FrameError::Io(error) => SessionError::Io(error),
        FrameError::Truncated | FrameError::Closed => SessionError::Io(error.into()),
        _ => SessionError::FrameTooLarge,
    }
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("compatibility service descriptor was not inherited")]
    ServiceFdMissing,
    #[error("an unauthenticated process connected to the compatibility service")]
    UnauthenticatedPeer,
    #[error("the launcher handoff was invalid")]
    InvalidHandoff,
    #[error("compatibility request frame exceeded its bounded size")]
    FrameTooLarge,
    #[error("another compatibility service already owns this session")]
    AlreadyRunning,
    #[error("managed Windows application is not registered")]
    UnknownEntry,
    #[error("no program of this application is open that can be ended here")]
    NotRunning,
    #[error("this setup still has open programs ({0}); close them and try again")]
    StillOpen(String),
    #[error("{NOT_ADMINISTRATOR}")]
    NotAdministrator,
    #[error(
        "{open} is open with {open_with}, and it shares its files with {opening}: close {open} \
         before opening {opening} with {with}."
    )]
    OpenWithOtherRunner {
        open: String,
        open_with: &'static str,
        opening: String,
        with: &'static str,
    },
    #[error("the Windows installer confirmation expired; inspect the portal file again")]
    VmInstallConfirmationExpired,
    #[error("the request did not carry one portal file descriptor")]
    DescriptorMissing,
    #[error("the compatibility request carried invalid ancillary data")]
    DescriptorInvalid,
    #[error("workspace capability authority refused the request: {0}")]
    Authority(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Capsule(#[from] crate::CapsuleError),
    #[error(transparent)]
    Service(#[from] ServiceError),
}

impl SessionError {
    fn code(&self) -> &'static str {
        match self {
            Self::ServiceFdMissing | Self::UnauthenticatedPeer | Self::InvalidHandoff => {
                "unauthenticated"
            }
            Self::FrameTooLarge
            | Self::Json(_)
            | Self::DescriptorMissing
            | Self::DescriptorInvalid => "invalid_request",
            Self::AlreadyRunning => "already_running",
            Self::VmInstallConfirmationExpired => "confirmation_expired",
            Self::UnknownEntry => "unknown_entry",
            Self::NotRunning => "not_running",
            Self::StillOpen(_) => "still_open",
            Self::NotAdministrator => "not_administrator",
            Self::OpenWithOtherRunner { .. } => crate::protocol::OPEN_WITH_OTHER_RUNNER,
            Self::Authority(_) => "authority_unavailable",
            Self::Io(_) | Self::Capsule(_) | Self::Service(_) => "service_error",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Seek;

    use crate::LaunchDisplay;

    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn session_handoffs_accept_only_the_manager_and_authorized_workspaces() {
        {
            assert!(
                validate_handoff(&SessionHandoff {
                    protocol: launcher::services::COMPAT_HANDOFF_PROTOCOL,
                    app_id: MANAGER_APP_ID.to_string(),
                    workspace_id: "workspace-1".to_string(),
                })
                .is_ok()
            );
            assert!(matches!(
                validate_handoff(&SessionHandoff {
                    protocol: launcher::services::COMPAT_HANDOFF_PROTOCOL,
                    app_id: "arbitrary.client".to_string(),
                    workspace_id: "workspace-1".to_string(),
                }),
                Err(SessionError::InvalidHandoff)
            ));
        }
        {
            let injected = serde_json::json!({
                "method": "launch",
                "version": PROTOCOL_VERSION,
                "entry_id": "windows-entry",
                "app_id": "victim",
                "workspace_id": "other",
                "query": {"executable": "evil.exe"},
                "capabilities": [],
                "explicit_vm_fallback": true
            });
            assert!(serde_json::from_value::<ProtocolRequest>(injected).is_err());
        }
        {
            let publication = crate::VmCandidatePublication {
                installation: crate::VmInstallation {
                    detail: "Published".to_string(),
                    phase: crate::VmInstallationPhase::Completed,
                    exit_code: Some(0),
                    candidates: vec![crate::VmInstallCandidate {
                        entry_id: "windows-vm-candidate-remaining".to_string(),
                        display_name: "Another app".to_string(),
                        publisher: None,
                        version: None,
                    }],
                },
                published_entry_id: "windows-vm-published".to_string(),
            };
            let mut owner_entries = BTreeSet::new();
            let other_workspace_entries = BTreeSet::<String>::new();

            authorize_vm_publication(&mut owner_entries, &publication);

            assert!(owner_entries.contains("windows-vm-published"));
            assert!(owner_entries.contains("windows-vm-candidate-remaining"));
            assert!(!other_workspace_entries.contains("windows-vm-published"));
            assert!(!other_workspace_entries.contains("windows-vm-candidate-remaining"));
        }
        {
            let (stream, _other) = UnixStream::pair().unwrap();
            let executable = fs::read_link("/proc/self/exe").unwrap();
            let auth =
                LauncherAuthenticator::new(executable, std::process::id(), Arc::new(DenyRegistry));
            assert!(matches!(
                auth.authenticate(&stream),
                Err(SessionError::UnauthenticatedPeer)
            ));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn session_wire_frames_are_pinned_bounded_and_carry_exact_descriptors() {
        // Phase `session_wire_bytes_are_pinned`.
        // The session socket's bytes: a little-endian `u32` length (carrying the
        // descriptor, when there is one), then JSON. Empty frames are refused.
        {
            const FRAME: [u8; 11] = [7, 0, 0, 0, b'{', b'"', b'k', b'"', b':', b'1', b'}'];
            let (mut writer, mut reader) = UnixStream::pair().unwrap();
            write_frame(&mut writer, &serde_json::json!({"k": 1}), FRAMING).unwrap();
            let mut sent = [0_u8; FRAME.len()];
            reader.read_exact(&mut sent).unwrap();
            assert_eq!(sent, FRAME);

            let selected = NamedTempFile::new().unwrap();
            write_frame_with_fd(
                &mut writer,
                &serde_json::json!({"k": 1}),
                FRAMING,
                selected.as_file().as_fd(),
            )
            .unwrap();
            let mut sent = [0_u8; FRAME.len()];
            reader.read_exact(&mut sent).unwrap();
            assert_eq!(sent, FRAME);

            writer.write_all(&FRAME).unwrap();
            let value: serde_json::Value = read_frame(&mut reader, FRAMING).unwrap();
            assert_eq!(value["k"], 1);

            let body = serde_json::to_vec(&ProtocolRequest::Install {
                version: PROTOCOL_VERSION,
                source_name: "a.exe".to_string(),
            })
            .unwrap();
            let prefix = u32::try_from(body.len()).unwrap().to_le_bytes();
            send_length_with_fd(&writer, prefix, selected.as_file().as_fd()).unwrap();
            writer.write_all(&body).unwrap();
            let (request, descriptor) = read_request_frame(&mut reader, FRAMING).unwrap();
            assert!(
                matches!(request, ProtocolRequest::Install { source_name, .. } if source_name == "a.exe")
            );
            assert!(descriptor.is_some());

            writer.write_all(&[0, 0, 0, 0]).unwrap();
            assert!(matches!(
                read_frame::<serde_json::Value>(&mut reader, FRAMING),
                Err(SessionError::FrameTooLarge)
            ));
        }
        {
            let (mut writer, mut reader) = UnixStream::pair().unwrap();
            writer
                .write_all(&(u32::try_from(FRAMING.max_len()).unwrap() + 1).to_le_bytes())
                .unwrap();
            assert!(matches!(
                read_frame::<ProtocolRequest>(&mut reader, FRAMING),
                Err(SessionError::FrameTooLarge)
            ));
        }
        {
            // An answer names its variant in `result`, beside the variant's fields,
            // in both directions.
            let (mut writer, mut reader) = UnixStream::pair().unwrap();
            let stopped = ProtocolResponse::Stopped {
                entry_id: "windows-entry".to_string(),
            };
            write_frame(&mut writer, &stopped, FRAMING).unwrap();
            let value: serde_json::Value = read_frame(&mut reader, FRAMING).unwrap();
            assert_eq!(
                value,
                serde_json::json!({"result": "stopped", "entry_id": "windows-entry"})
            );

            let refusal = serde_json::json!({
                "error": {"code": "unsupported_version", "message": "too new"},
                "result": "error",
            });
            write_frame(&mut writer, &refusal, FRAMING).unwrap();
            assert!(matches!(
                read_frame::<ProtocolResponse>(&mut reader, FRAMING),
                Ok(ProtocolResponse::Error { error }) if error.code == "unsupported_version"
            ));

            for unreadable in [
                serde_json::json!({"entry_id": "windows-entry"}),
                serde_json::json!({"result": 3, "entry_id": "windows-entry"}),
                serde_json::json!({"result": "exploded"}),
                serde_json::json!({"result": "stopped"}),
            ] {
                write_frame(&mut writer, &unreadable, FRAMING).unwrap();
                assert!(matches!(
                    read_frame::<ProtocolResponse>(&mut reader, FRAMING),
                    Err(SessionError::Json(_))
                ));
            }
        }
        {
            let (mut writer, mut reader) = UnixStream::pair().unwrap();
            let mut selected = NamedTempFile::new().unwrap();
            selected.write_all(b"portal-selected bytes").unwrap();
            selected.flush().unwrap();
            write_frame_with_fd(
                &mut writer,
                &ProtocolRequest::Install {
                    version: PROTOCOL_VERSION,
                    source_name: "favorite.exe".to_string(),
                },
                FRAMING,
                selected.as_file().as_fd(),
            )
            .unwrap();

            let (request, descriptor) = read_request_frame(&mut reader, FRAMING).unwrap();
            assert!(matches!(
                request,
                ProtocolRequest::Install { source_name, .. } if source_name == "favorite.exe"
            ));
            let mut received = File::from(descriptor.unwrap());
            received.rewind().unwrap();
            let mut bytes = Vec::new();
            received.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"portal-selected bytes");
        }
        {
            let (writer, mut reader) = UnixStream::pair().unwrap();
            let mut client = SessionClient { stream: writer };
            let mut selected = NamedTempFile::new().unwrap();
            selected.write_all(b"portal-selected document").unwrap();
            selected.flush().unwrap();

            let worker = std::thread::spawn(move || {
                let (request, descriptor) = read_request_frame(&mut reader, FRAMING).unwrap();
                assert!(matches!(
                    request,
                    ProtocolRequest::LaunchVmWithPortalFile { entry_id, .. }
                        if entry_id == "windows-entry"
                ));
                let mut received = File::from(descriptor.expect("SCM_RIGHTS descriptor"));
                received.rewind().unwrap();
                let mut bytes = Vec::new();
                received.read_to_end(&mut bytes).unwrap();
                assert_eq!(bytes, b"portal-selected document");
                write_frame(
                    &mut reader,
                    &ProtocolResponse::Error {
                        error: ProtocolError::new("test_complete", "test response"),
                    },
                    FRAMING,
                )
                .unwrap();
            });

            let response = client
                .request_with_file(
                    &ProtocolRequest::LaunchVmWithPortalFile {
                        version: PROTOCOL_VERSION,
                        entry_id: "windows-entry".to_string(),
                    },
                    selected.as_file(),
                )
                .unwrap();
            assert!(matches!(response, ProtocolResponse::Error { .. }));
            worker.join().unwrap();
        }
    }

    /// A manager asks only when its window draws: a quiet connection stays open past the read
    /// timeout and its next request is read whole, while a hang-up or a stopping service ends it.
    #[test]
    fn a_quiet_manager_connection_stays_open_for_its_next_request() {
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_millis(10)))
            .unwrap();
        let stop = AtomicBool::new(false);
        let later = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            write_frame(
                &mut writer,
                &ProtocolRequest::List {
                    version: PROTOCOL_VERSION,
                },
                FRAMING,
            )
            .unwrap();
            writer
        });
        assert!(next_request_arrives(&reader, &stop).unwrap());
        let (request, _) = read_request_frame(&mut reader, FRAMING).unwrap();
        assert!(matches!(request, ProtocolRequest::List { .. }));
        drop(later.join().unwrap());
        assert!(
            !next_request_arrives(&reader, &stop).unwrap(),
            "a hang-up ends it"
        );

        let (_writer, reader) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_millis(10)))
            .unwrap();
        stop.store(true, Ordering::Release);
        assert!(
            !next_request_arrives(&reader, &stop).unwrap(),
            "a stopping service ends it"
        );
    }

    #[test]
    fn session_clients_time_out_and_bound_workers() {
        {
            let (_writer, mut reader) = UnixStream::pair().unwrap();
            reader
                .set_read_timeout(Some(Duration::from_millis(10)))
                .unwrap();
            assert!(matches!(
                read_frame::<ProtocolRequest>(&mut reader, FRAMING),
                Err(SessionError::Io(error))
                    if matches!(error.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock)
            ));
        }
        {
            let active = Arc::new(AtomicUsize::new(0));
            let permits = (0..MAX_CLIENT_WORKERS)
                .map(|_| WorkerPermit::try_acquire(&active).unwrap())
                .collect::<Vec<_>>();
            assert!(WorkerPermit::try_acquire(&active).is_none());
            drop(permits);
            assert!(WorkerPermit::try_acquire(&active).is_some());
        }
    }

    struct DenyRegistry;

    impl TrustedLauncherRegistry for DenyRegistry {
        fn is_launch(&self, _: u32, _: u64, _: &str, _: &str) -> bool {
            false
        }
    }

    use tilo_test_env::idle::{assert_stays_asleep, spawned_thread};

    struct IdleResources;

    impl LaunchResourceBroker for IdleResources {
        fn materialize(
            &self,
            _: crate::plan::ResourceRequest<'_>,
        ) -> Result<crate::plan::LaunchLease, crate::plan::ResourceError> {
            Err(crate::plan::ResourceError::Unavailable)
        }

        fn revoke(&self, _: &str) {}
    }

    struct IdleExecutor;

    impl crate::SandboxExecutor for IdleExecutor {
        fn command(
            &self,
            _: &crate::plan::CapsuleCommandPlan,
        ) -> Result<crate::SandboxCommand, crate::execute::ExecutionError> {
            Err(crate::execute::ExecutionError::Seccomp(
                "idle accept test".into(),
            ))
        }
    }

    struct IdleVm;

    impl crate::VmDelegate for IdleVm {
        fn launch(
            &self,
            _: crate::VmRouteRequest<'_>,
        ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError> {
            Err(windows_vm::LaunchError::InvalidRequest("idle accept test"))
        }
    }

    struct IdleAuthority;

    impl CapabilityAuthority for IdleAuthority {
        fn capabilities_for(&self, _: &str, _: &str) -> Result<Vec<CapOverride>, String> {
            Ok(Vec::new())
        }
    }

    struct IdleLaunchers;

    impl TrustedLauncherRegistry for IdleLaunchers {
        fn is_launch(&self, _: u32, _: u64, _: &str, _: &str) -> bool {
            false
        }
    }

    /// A launch hands off once: a second connection of the same launcher process is refused,
    /// whatever the session says of it.
    #[test]
    fn a_launch_hands_off_once() {
        struct Every;

        impl TrustedLauncherRegistry for Every {
            fn is_launch(&self, _: u32, _: u64, _: &str, _: &str) -> bool {
                true
            }
        }

        let executable = fs::read_link("/proc/self/exe").unwrap();
        let auth = LauncherAuthenticator::new(executable, std::process::id(), Arc::new(Every));
        let pid = std::process::id();
        let started = process_start_time(pid).unwrap();
        assert!(auth.take_handoff(pid, started).is_ok());
        assert!(matches!(
            auth.take_handoff(pid, started),
            Err(SessionError::UnauthenticatedPeer)
        ));
    }

    /// A program the desktop before a restart started ends under this one: the lease that desktop
    /// lent it (its private sound daemons' directory) is given back as it ends, not left until
    /// some later launch happens to look. A desktop that starts gives back the leases of programs
    /// that ended while none was there, as a logout ends them all.
    #[test]
    fn a_program_taken_back_after_a_restart_gives_its_lease_back_when_it_ends() {
        #[derive(Default)]
        struct Released(Arc<(Mutex<bool>, std::sync::Condvar)>);

        impl LaunchResourceBroker for Released {
            fn materialize(
                &self,
                _: crate::plan::ResourceRequest<'_>,
            ) -> Result<crate::plan::LaunchLease, crate::plan::ResourceError> {
                Err(crate::plan::ResourceError::Unavailable)
            }

            fn revoke(&self, _: &str) {}

            fn release_ended(&self) {
                *self.0.0.lock().unwrap() = true;
                self.0.1.notify_all();
            }
        }

        struct Ended;

        impl crate::StartedProgram for Ended {
            fn wait(&self) -> Result<crate::ExecutionOutcome, crate::execute::ExecutionError> {
                Ok(crate::ExecutionOutcome::exited(Some(0)))
            }

            fn stop(&self) {}
        }

        struct Kept(Mutex<Vec<crate::TakenBack>>);

        impl crate::LaunchDisplays for Kept {
            fn display_for(&self, _: &str) -> Result<LaunchDisplay, String> {
                Err("no display in this test".to_owned())
            }

            fn taken_back(&self) -> Vec<crate::TakenBack> {
                std::mem::take(&mut self.0.lock().unwrap())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let released = Released::default();
        let heard = Arc::clone(&released.0);
        let managed = ManagedCompatibility::new(
            Catalogue::from_json(r#"{"schema_version":1}"#).unwrap(),
            CapsuleStore::new(dir.path()).unwrap(),
            crate::plan::SystemRuntimeRegistry::assuming_provider(),
            released,
            IdleExecutor,
            IdleVm,
            Arc::new(Kept(Mutex::new(vec![crate::TakenBack {
                entry_id: "entry".to_owned(),
                workspace_id: "workspace".to_owned(),
                display_name: "Kept".to_owned(),
                program: Box::new(Ended),
            }]))),
        )
        .map(Arc::new)
        .unwrap();
        assert!(
            std::mem::take(&mut *heard.0.lock().unwrap()),
            "a starting desktop gives back what ended programs held"
        );

        managed.take_back();

        let (given_back, _) = heard
            .1
            .wait_timeout_while(heard.0.lock().unwrap(), Duration::from_secs(10), |given| {
                !*given
            })
            .unwrap();
        assert!(*given_back, "the ended program's lease is given back");
    }

    /// Two programs of one setup share its prefix: while one is open with Wine, the other is not
    /// opened with Proton (nor the other way), and the refusal names both and what to do. Once
    /// nothing of the capsule is open, either opens with what was chosen.
    #[test]
    fn a_prefix_is_never_open_with_wine_and_proton_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = CapsuleStore::new(dir.path()).unwrap();
        let catalogue = Catalogue::from_json(r#"{"schema_version":1,"records":[]}"#).unwrap();
        let route = catalogue.resolver().unwrap().resolve(
            &ApplicationQuery::executable("putty.exe"),
            VmFallback::Disabled,
        );
        let capsule = store.ensure_for_plan(&route).unwrap();
        for (id, name) in [("putty", "PuTTY"), ("puttygen", "PuTTYgen")] {
            store
                .register_program(
                    &capsule.id,
                    crate::CapsuleProgram {
                        id: id.to_owned(),
                        display_name: name.to_owned(),
                        prefix_relative_executable: PathBuf::from(format!("drive_c/{id}.exe")),
                        query: ApplicationQuery::executable(format!("{id}.exe")),
                        kind: ProgramKind::Application,
                        arguments: Vec::new(),
                        opens_with: None,
                    },
                )
                .unwrap();
        }
        let managed = ManagedCompatibility::new(
            catalogue,
            store,
            crate::plan::SystemRuntimeRegistry::assuming_provider(),
            IdleResources,
            IdleExecutor,
            IdleVm,
            Arc::new(LaunchDisplay {
                uid: 1000,
                runtime_dir: PathBuf::from("/xdg/1000"),
                wayland_socket: PathBuf::from("/xdg/1000/tilo-ws-test"),
                render_node: None,
            }),
        )
        .unwrap();
        let entry = |id: &str| {
            managed
                .entries()
                .unwrap()
                .into_iter()
                .find(|entry| entry.program.id == id)
                .unwrap()
        };
        let (putty, puttygen) = (entry("putty"), entry("puttygen"));

        // PuTTYgen is open with Wine.
        let open = managed
            .programs
            .started(&puttygen.entry_id, "workspace", None);
        managed.opened(&puttygen.entry_id, Layer::Wine);
        assert!(
            managed
                .refuse_beside_other_runner(&putty, Layer::Wine)
                .is_ok(),
            "Wine beside Wine is one server's prefix"
        );
        let refused = managed
            .refuse_beside_other_runner(&putty, Layer::Proton)
            .unwrap_err();
        assert_eq!(refused.code(), "open_with_other_runner");
        assert_eq!(
            refused.to_string(),
            "PuTTYgen is open with Wine, and it shares its files with PuTTY: close PuTTYgen \
             before opening PuTTY with Proton."
        );
        // PuTTYgen chosen for Proton while its Wine program is open: that program still holds
        // the prefix, so it is not opened again with Proton beside it.
        assert_eq!(
            managed
                .refuse_beside_other_runner(&puttygen, Layer::Proton)
                .unwrap_err()
                .code(),
            "open_with_other_runner"
        );

        managed.programs.ended(
            open,
            "PuTTYgen",
            Ok(crate::ExecutionOutcome::exited(Some(0))),
        );
        assert!(
            managed
                .refuse_beside_other_runner(&putty, Layer::Proton)
                .is_ok(),
            "once nothing of the capsule is open, Proton may open it"
        );
    }

    #[test]
    fn idle_accept_thread_sleeps_without_clients() {
        let dir = tempfile::tempdir().unwrap();
        let store = CapsuleStore::new(dir.path()).unwrap();
        let catalogue = Catalogue::from_json(r#"{"schema_version":1}"#).unwrap();
        let managed = Arc::new(
            ManagedCompatibility::new(
                catalogue,
                store,
                crate::plan::SystemRuntimeRegistry::assuming_provider(),
                IdleResources,
                IdleExecutor,
                IdleVm,
                Arc::new(LaunchDisplay {
                    uid: 1000,
                    runtime_dir: PathBuf::from("/xdg/1000"),
                    wayland_socket: PathBuf::from("/xdg/1000/tilo-ws-test"),
                    render_node: Some(PathBuf::from("/dev/dri/renderD128")),
                }),
            )
            .unwrap(),
        );
        let sock = dir.path().join("compat.sock");
        let exe = fs::read_link("/proc/self/exe").unwrap();
        let (handle, tid) = spawned_thread("tilo-win-accept", || {
            spawn_session_service(
                sock.clone(),
                Arc::clone(&managed),
                Arc::new(IdleAuthority),
                LauncherAuthenticator::new(exe, std::process::id(), Arc::new(IdleLaunchers)),
                EntryDirectory::default(),
                Reopenings::default(),
            )
            .unwrap()
        });
        assert_stays_asleep(&tid, "compat session accept");
        drop(handle);
    }
}
