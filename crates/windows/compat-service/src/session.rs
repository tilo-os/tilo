use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use collab_types::caps::CapOverride;
use compat_catalogue::{ApplicationQuery, Catalogue, VmFallback};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tilo_clock::try_unix_now;
use wire_frame::{FrameError, Framing};

use crate::service::published_vm_explanation;
use crate::{
    CapsuleStore, CompatibilityService, HostSession, InstallOutcome, InstallationStatus,
    LaunchInput, LaunchResourceBroker, ManagedApplication, ManagedInstallation, PROTOCOL_VERSION,
    ProgramKind, ProtocolError, ProtocolRequest, ProtocolResponse, RegisteredProgram, RouteKind,
    RuntimeRegistry, SandboxExecutor, ServiceError, VmDelegate,
};

const MANAGER_APP_ID: &str = "so.tilo.windows";
/// Requests and responses: a little-endian `u32` length, then JSON. A request
/// that passes a descriptor carries it with the length. Empty frames are
/// invalid.
const FRAMING: Framing = Framing::little_endian(1024 * 1024).reject_empty();
const MAX_CLIENT_WORKERS: usize = 8;
const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(2);
// An `i32` is exactly four bytes by definition; `SCM_RIGHTS` carries one raw fd.
const SCM_RIGHTS_FD_BYTES: u32 = 4;
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

/// Source of fresh compositor-owned grants. An implementation must look up
/// `workspace_id` and `app_id`; it must never deserialize grants from this
/// service connection.
pub trait CapabilityAuthority: Send + Sync + 'static {
    fn capabilities_for(
        &self,
        workspace_id: &str,
        app_id: &str,
    ) -> Result<Vec<CapOverride>, String>;
}

/// Compositor-owned, single-use registry of launcher children. `/proc` facts
/// are only corroboration; this record is the authority that a launch was
/// actually initiated by Tilo for this app/workspace.
pub trait TrustedLauncherRegistry: Send + Sync + 'static {
    fn consume(
        &self,
        pid: u32,
        process_start_time: u64,
        app_id: &str,
        workspace_id: &str,
    ) -> Option<BTreeSet<String>>;
}

pub struct ManagedCompatibility<R, B, E, V> {
    service: CompatibilityService<R, B, E, V>,
    store: CapsuleStore,
}

impl<R, B, E, V> ManagedCompatibility<R, B, E, V>
where
    R: RuntimeRegistry,
    B: LaunchResourceBroker,
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
        session: HostSession,
    ) -> Result<Self, ServiceError> {
        let service = CompatibilityService::new(
            catalogue,
            store.clone(),
            runtimes,
            resources,
            executor,
            vm,
            session,
        )?;
        Ok(Self { service, store })
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

    fn explain_entry(
        &self,
        entry: &RegisteredProgram,
        capabilities: &[CapOverride],
    ) -> Result<crate::ExplainStatus, SessionError> {
        self.service
            .explain(
                &entry.query,
                VmFallback::Disabled,
                &entry.app_id,
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
        // Query, program, target app identity, and disabled VM fallback all
        // come from server state. The client contributes only `entry.id`.
        self.service
            .launch(&LaunchInput {
                app_id: entry.app_id.clone(),
                workspace_id: workspace_id.to_string(),
                program_id: entry.program.id.clone(),
                query: entry.query.clone(),
                vm_fallback: VmFallback::Disabled,
                capabilities,
                now_unix: try_unix_now(),
            })
            .map_err(Into::into)
    }

    fn launch_published_vm(
        &self,
        application: &crate::VmPublishedApplication,
        workspace_id: &str,
        capabilities: &[CapOverride],
    ) -> Result<crate::LaunchReceipt, SessionError> {
        self.service
            .launch_published_vm(application, workspace_id, capabilities, try_unix_now())
            .map_err(Into::into)
    }

    fn launch_published_vm_with_portal_file(
        &self,
        application: &crate::VmPublishedApplication,
        workspace_id: &str,
        capabilities: &[CapOverride],
        now_unix: Option<u64>,
        source: File,
        access: windows_vm::PortalDocumentAccess,
    ) -> Result<crate::LaunchReceipt, SessionError> {
        self.service
            .launch_published_vm_with_portal_file(
                application,
                workspace_id,
                capabilities,
                now_unix,
                source,
                access,
            )
            .map_err(Into::into)
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

    fn install_in_capsule(
        &self,
        source: File,
        source_name: &str,
        workspace_id: &str,
        capabilities: &[CapOverride],
        expected: &crate::VmInstallerInput,
    ) -> Result<crate::InstallReceipt, SessionError> {
        self.service
            .install_in_capsule(
                source,
                source_name,
                workspace_id,
                capabilities,
                try_unix_now(),
                expected,
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

    fn retry_install(
        &self,
        capsule_id: &str,
        workspace_id: &str,
        capabilities: &[CapOverride],
    ) -> Result<crate::InstallReceipt, SessionError> {
        self.service
            .retry_install(capsule_id, workspace_id, capabilities, try_unix_now())
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

    fn vendor_uninstall(
        &self,
        capsule_id: &str,
        workspace_id: &str,
        capabilities: &[CapOverride],
    ) -> Result<crate::InstallReceipt, SessionError> {
        self.service
            .vendor_uninstall(capsule_id, workspace_id, capabilities, try_unix_now())
            .map_err(Into::into)
    }

    fn delete_data(&self, capsule_id: &str) -> Result<(), SessionError> {
        self.service.delete_capsule(capsule_id).map_err(Into::into)
    }
}

#[derive(Clone)]
pub struct LauncherAuthenticator {
    expected_uid: u32,
    expected_parent_pid: u32,
    expected_executable: PathBuf,
    registry: Arc<dyn TrustedLauncherRegistry>,
}

impl LauncherAuthenticator {
    #[must_use]
    pub fn new(
        expected_executable: impl Into<PathBuf>,
        expected_parent_pid: u32,
        registry: Arc<dyn TrustedLauncherRegistry>,
    ) -> Self {
        // SAFETY: `geteuid` has no preconditions and does not mutate memory.
        let expected_uid = unsafe { libc::geteuid() };
        Self {
            expected_uid,
            expected_parent_pid,
            expected_executable: expected_executable.into(),
            registry,
        }
    }

    fn authenticate(&self, stream: &UnixStream) -> Result<(), SessionError> {
        let credentials = peer_credentials(stream)?;
        if credentials.uid != self.expected_uid || credentials.pid <= 0 {
            return Err(SessionError::UnauthenticatedPeer);
        }
        let pid = u32::try_from(credentials.pid).map_err(|_| SessionError::UnauthenticatedPeer)?;
        let executable = fs::read_link(format!("/proc/{pid}/exe"))?;
        let expected = fs::canonicalize(&self.expected_executable)?;
        if executable != expected || process_parent(pid)? != self.expected_parent_pid {
            return Err(SessionError::UnauthenticatedPeer);
        }
        Ok(())
    }

    fn consume_handoff(
        &self,
        stream: &UnixStream,
        handoff: &SessionHandoff,
    ) -> Result<BTreeSet<String>, SessionError> {
        let credentials = peer_credentials(stream)?;
        let pid = u32::try_from(credentials.pid).map_err(|_| SessionError::UnauthenticatedPeer)?;
        let start_time = process_start_time(pid)?;
        // Registration happens immediately after `Command::spawn`; the child
        // may reach connect first on another CPU. Wait briefly for that one
        // parent-side insertion, never for an unregistered identity change.
        for _ in 0..20 {
            if let Some(entry_ids) =
                self.registry
                    .consume(pid, start_time, &handoff.app_id, &handoff.workspace_id)
            {
                return Ok(entry_ids);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(SessionError::UnauthenticatedPeer)
    }
}

#[derive(Clone, Copy)]
struct PeerCredentials {
    pid: i32,
    uid: u32,
}

fn peer_credentials(stream: &UnixStream) -> Result<PeerCredentials, SessionError> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>())
        .map_err(|_| SessionError::UnauthenticatedPeer)?;
    // SAFETY: both pointers name initialized writable storage of `length`
    // bytes and `stream` owns a valid Unix-domain socket descriptor.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(credentials).cast(),
            std::ptr::addr_of_mut!(length),
        )
    };
    if result != 0 || usize::try_from(length).ok() != Some(std::mem::size_of::<libc::ucred>()) {
        return Err(SessionError::UnauthenticatedPeer);
    }
    Ok(PeerCredentials {
        pid: credentials.pid,
        uid: credentials.uid,
    })
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
}

impl Drop for CompatSessionHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = UnixStream::connect(&self.path);
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        join_workers(&self.workers);
        let _ = fs::remove_file(&self.path);
    }
}

pub fn spawn_session_service<R, B, E, V, A>(
    path: PathBuf,
    managed: Arc<ManagedCompatibility<R, B, E, V>>,
    authority: Arc<A>,
    authenticator: LauncherAuthenticator,
) -> Result<CompatSessionHandle, SessionError>
where
    R: RuntimeRegistry + 'static,
    B: LaunchResourceBroker + 'static,
    E: SandboxExecutor + 'static,
    V: VmDelegate + 'static,
    A: CapabilityAuthority,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            return Err(SessionError::AlreadyRunning);
        }
        fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let workers = Arc::new(Mutex::new(Vec::new()));
    let thread_workers = Arc::clone(&workers);
    let active_workers = Arc::new(AtomicUsize::new(0));
    let thread_active_workers = Arc::clone(&active_workers);
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
                            // descriptors into unbounded compositor threads.
                            drop(stream);
                            continue;
                        };
                        if stream.set_read_timeout(Some(CLIENT_IO_TIMEOUT)).is_err()
                            || stream.set_write_timeout(Some(CLIENT_IO_TIMEOUT)).is_err()
                        {
                            drop(permit);
                            continue;
                        }
                        let managed = Arc::clone(&managed);
                        let authority = Arc::clone(&authority);
                        let authenticator = authenticator.clone();
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
    Ok(CompatSessionHandle {
        path,
        stop,
        listener: Some(listener_thread),
        workers,
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

fn join_workers(workers: &Mutex<Vec<JoinHandle<()>>>) {
    reap_finished_workers(workers);
    let pending = workers
        .lock()
        .map(|mut workers| std::mem::take(&mut *workers))
        .unwrap_or_default();
    for worker in pending {
        let _ = worker.join();
    }
}

fn serve_client<R, B, E, V>(
    mut stream: UnixStream,
    managed: &ManagedCompatibility<R, B, E, V>,
    authority: &dyn CapabilityAuthority,
    authenticator: &LauncherAuthenticator,
) -> Result<(), SessionError>
where
    R: RuntimeRegistry,
    B: LaunchResourceBroker,
    E: SandboxExecutor,
    V: VmDelegate,
{
    let handoff: SessionHandoff =
        read_frame(&mut stream, launcher::services::COMPAT_HANDOFF_FRAMING)?;
    validate_handoff(&handoff)?;
    let mut allowed_entry_ids = authenticator.consume_handoff(&stream, &handoff)?;
    let mut pending_vm_install = None;
    loop {
        let (request, descriptor) = match read_request_frame(&mut stream, FRAMING) {
            Ok(request) => request,
            Err(SessionError::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let response = handle_request(
            managed,
            authority,
            &handoff,
            &mut allowed_entry_ids,
            &mut pending_vm_install,
            request,
            descriptor,
        );
        write_frame(&mut stream, &response, FRAMING)?;
    }
}

fn validate_handoff(handoff: &SessionHandoff) -> Result<(), SessionError> {
    if handoff.protocol != PROTOCOL_VERSION
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
    managed: &ManagedCompatibility<R, B, E, V>,
    authority: &dyn CapabilityAuthority,
    handoff: &SessionHandoff,
    allowed_entry_ids: &mut BTreeSet<String>,
    pending_vm_install: &mut Option<PendingVmInstall>,
    request: ProtocolRequest,
    descriptor: Option<OwnedFd>,
) -> ProtocolResponse
where
    R: RuntimeRegistry,
    B: LaunchResourceBroker,
    E: SandboxExecutor,
    V: VmDelegate,
{
    let version = match &request {
        ProtocolRequest::List { version }
        | ProtocolRequest::Launch { version, .. }
        | ProtocolRequest::LaunchVmFallback { version, .. }
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
        | ProtocolRequest::DeleteData { version, .. } => *version,
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
    let result: Result<ProtocolResponse, SessionError> = (|| match request {
        ProtocolRequest::List { .. } => {
            list_applications(managed, authority, handoff, allowed_entry_ids)
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
                        .launch_published_vm(&application, &handoff.workspace_id, &capabilities)
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
                .launch_published_vm_with_portal_file(
                    &application,
                    &handoff.workspace_id,
                    &capabilities,
                    try_unix_now(),
                    source,
                    // This protocol action is intentionally read-only. A
                    // future write-back action must have a distinct portal
                    // decision rather than trusting a client-supplied bit.
                    windows_vm::PortalDocumentAccess::ReadOnly,
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
                .map(|entry| entry.entry_id)
                .collect::<Vec<_>>();
            managed.delete_data(&capsule_id)?;
            for entry in retiring {
                allowed_entry_ids.remove(&entry);
            }
            Ok(ProtocolResponse::DataDeleted { capsule_id })
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
) -> Result<ProtocolResponse, SessionError>
where
    R: RuntimeRegistry,
    B: LaunchResourceBroker,
    E: SandboxExecutor,
    V: VmDelegate,
{
    let mut applications = Vec::new();
    // A list refresh reports real delegate readiness, but never wakes the
    // fallback appliance merely to draw a manager badge.
    let vm_inventory = managed.vm_inventory();
    let vm_availability = vm_inventory.availability.clone();
    let vm_installation = managed.vm_installation_status(&handoff.workspace_id);
    let vm_uninstallation = managed.vm_uninstallation_status(&handoff.workspace_id);
    if let Some(installation) = &vm_installation {
        authorize_vm_candidates(allowed_entry_ids, installation);
    }
    for entry in managed.entries()?.into_iter().filter(|entry| {
        allowed_entry_ids.contains(&entry.entry_id)
            && entry.program.kind == ProgramKind::Application
    }) {
        let capabilities = authority
            .capabilities_for(&handoff.workspace_id, &entry.app_id)
            .map_err(SessionError::Authority)?;
        let explanation = managed.explain_entry(&entry, &capabilities)?;
        let installation_status = managed
            .installation_states()?
            .into_iter()
            .find(|state| state.id == entry.capsule_id)
            .and_then(|state| state.installation.map(|installation| installation.status));
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
        });
    }
    for application in vm_inventory
        .applications
        .into_iter()
        .filter(|application| allowed_entry_ids.contains(&application.entry_id))
    {
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
    })
}

fn authorized_capsules<R, B, E, V>(
    managed: &ManagedCompatibility<R, B, E, V>,
    allowed_entry_ids: &BTreeSet<String>,
) -> Result<BTreeSet<String>, SessionError>
where
    R: RuntimeRegistry,
    B: LaunchResourceBroker,
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
    B: LaunchResourceBroker,
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
    B: LaunchResourceBroker,
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
    B: LaunchResourceBroker,
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

    /// Take ownership of the launcher-inherited `compat` descriptor.
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
        write_frame_with_fd(&mut self.stream, request, FRAMING, file.as_raw_fd())?;
        read_frame(&mut self.stream, FRAMING)
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
    let mut iov = libc::iovec {
        iov_base: length.as_mut_ptr().cast(),
        iov_len: length.len(),
    };
    let control_bytes = cmsg_space_for_fd();
    let mut control = vec![0 as libc::c_long; control_words(control_bytes)];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = std::ptr::addr_of_mut!(iov);
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control_bytes;
    // SAFETY: every msghdr pointer names live writable storage, and the
    // control allocation is aligned to `c_long`/cmsghdr requirements.
    let received = unsafe {
        libc::recvmsg(
            stream.as_raw_fd(),
            std::ptr::addr_of_mut!(message),
            libc::MSG_WAITALL | libc::MSG_CMSG_CLOEXEC,
        )
    };
    if received < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if received == 0 {
        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
    }
    if usize::try_from(received).ok() != Some(length.len())
        || message.msg_flags & libc::MSG_CTRUNC != 0
    {
        return Err(SessionError::DescriptorInvalid);
    }

    let mut descriptor = None;
    // SAFETY: `message` was initialized by recvmsg and its control buffer
    // remains alive. We accept exactly one integer SCM_RIGHTS item.
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(std::ptr::addr_of!(message));
        while !header.is_null() {
            let expected = libc::CMSG_LEN(SCM_RIGHTS_FD_BYTES) as usize;
            if (*header).cmsg_level != libc::SOL_SOCKET
                || (*header).cmsg_type != libc::SCM_RIGHTS
                || (*header).cmsg_len != expected
                || descriptor.is_some()
            {
                return Err(SessionError::DescriptorInvalid);
            }
            let raw = std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<i32>());
            if raw < 0 {
                return Err(SessionError::DescriptorInvalid);
            }
            descriptor = Some(OwnedFd::from_raw_fd(raw));
            header = libc::CMSG_NXTHDR(std::ptr::addr_of!(message), header);
        }
    }
    Ok((length, descriptor))
}

fn write_frame_with_fd<T: Serialize>(
    stream: &mut UnixStream,
    value: &T,
    framing: Framing,
    descriptor: i32,
) -> Result<(), SessionError> {
    let bytes = serde_json::to_vec(value)?;
    if descriptor < 0 {
        return Err(SessionError::FrameTooLarge);
    }
    let length = framing.encode_prefix(bytes.len()).map_err(frame_error)?;
    send_length_with_fd(stream, length, descriptor)?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}

fn send_length_with_fd(
    stream: &UnixStream,
    mut length: [u8; 4],
    descriptor: i32,
) -> Result<(), SessionError> {
    let mut iov = libc::iovec {
        iov_base: length.as_mut_ptr().cast(),
        iov_len: length.len(),
    };
    let control_bytes = cmsg_space_for_fd();
    let mut control = vec![0 as libc::c_long; control_words(control_bytes)];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = std::ptr::addr_of_mut!(iov);
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control_bytes;
    // SAFETY: the control allocation is suitably aligned and sized for one
    // cmsghdr plus one descriptor, and all referenced values outlive sendmsg.
    unsafe {
        let header = libc::CMSG_FIRSTHDR(std::ptr::addr_of!(message));
        if header.is_null() {
            return Err(SessionError::DescriptorInvalid);
        }
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(SCM_RIGHTS_FD_BYTES) as usize;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<i32>(), descriptor);
        let sent = libc::sendmsg(
            stream.as_raw_fd(),
            std::ptr::addr_of!(message),
            libc::MSG_NOSIGNAL,
        );
        if sent < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if usize::try_from(sent).ok() != Some(length.len()) {
            return Err(SessionError::DescriptorInvalid);
        }
    }
    Ok(())
}

fn cmsg_space_for_fd() -> usize {
    // SAFETY: CMSG_SPACE is a pure size/alignment calculation.
    unsafe { libc::CMSG_SPACE(SCM_RIGHTS_FD_BYTES) as usize }
}

fn control_words(bytes: usize) -> usize {
    bytes.div_ceil(std::mem::size_of::<libc::c_long>())
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
            Self::Authority(_) => "authority_unavailable",
            Self::Io(_) | Self::Capsule(_) | Self::Service(_) => "service_error",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Seek;

    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn session_handoffs_accept_only_the_manager_and_authorized_workspaces() {
        // Phase `handoff_accepts_only_windows_manager_and_real_workspace_shape`.
        {
            assert!(
                validate_handoff(&SessionHandoff {
                    protocol: PROTOCOL_VERSION,
                    app_id: MANAGER_APP_ID.to_string(),
                    workspace_id: "workspace-1".to_string(),
                })
                .is_ok()
            );
            assert!(matches!(
                validate_handoff(&SessionHandoff {
                    protocol: PROTOCOL_VERSION,
                    app_id: "arbitrary.client".to_string(),
                    workspace_id: "workspace-1".to_string(),
                }),
                Err(SessionError::InvalidHandoff)
            ));
        }
        // Phase `launch_wire_rejects_query_caps_identity_and_vm_intent`.
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
        // Phase `publishing_authorizes_the_opaque_vm_entry_only_for_its_workspace_session`.
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
        // Phase `direct_same_process_is_not_a_trusted_launcher_child`.
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
                selected.as_file().as_raw_fd(),
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
            send_length_with_fd(&writer, prefix, selected.as_file().as_raw_fd()).unwrap();
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
        // Phase `oversized_frame_is_rejected_before_allocation`.
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
        // Phase `install_frame_transfers_exactly_one_open_descriptor`.
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
                selected.as_file().as_raw_fd(),
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
        // Phase `client_transfers_a_portal_descriptor_for_a_remoteapp_launch`.
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

    #[test]
    fn session_clients_time_out_and_bound_workers() {
        // Phase `client_reads_time_out_when_a_peer_holds_the_connection_open`.
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
        // Phase `client_worker_capacity_is_bounded_and_released`.
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
        fn consume(&self, _: u32, _: u64, _: &str, _: &str) -> Option<BTreeSet<String>> {
            None
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
        fn execute(
            &self,
            _: &crate::plan::CapsuleCommandPlan,
        ) -> Result<crate::execute::ExecutionOutcome, crate::execute::ExecutionError> {
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
        fn consume(&self, _: u32, _: u64, _: &str, _: &str) -> Option<BTreeSet<String>> {
            None
        }
    }

    #[test]
    fn idle_accept_thread_sleeps_without_clients() {
        let dir = tempfile::tempdir().unwrap();
        let store = CapsuleStore::new(dir.path()).unwrap();
        let catalogue = Catalogue::from_json(r#"{"schema_version":1,"records":[]}"#).unwrap();
        let managed = Arc::new(
            ManagedCompatibility::new(
                catalogue,
                store,
                crate::plan::SystemRuntimeRegistry::assuming_provider(),
                IdleResources,
                IdleExecutor,
                IdleVm,
                HostSession {
                    uid: 1000,
                    wayland_socket: PathBuf::from("/run/user/1000/tilo-ws-test"),
                    render_node: PathBuf::from("/dev/dri/renderD128"),
                },
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
            )
            .unwrap()
        });
        assert_stays_asleep(&tid, "compat session accept");
        drop(handle);
    }
}
