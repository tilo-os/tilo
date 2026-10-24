//! Signed services whose successful exit proves their private PID namespace
//! has finished. Ordinary bwrap leader exit is deliberately not this witness.
use crate::{
    managed::EnrolledProcessObserver,
    run::{RunError, RunOptions},
};
use std::{
    fs::File,
    io::{self, Read},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    process::{Child, ExitStatus},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(crate) mod capture;
pub(crate) mod retained_io;
mod retained_worker;
pub use retained_worker::{RetainedWorkerConnector, RetainedWorkerWitness};
pub mod target_projection;
pub use target_projection::{
    AdmittedTargetFuse, SelectedParentEditAuthority, SelectedParentScopeFacts, TargetFuseReady,
    TargetProjectionAdmission, TargetProjectionAuthority, TargetProjectionCredentials,
    TargetProjectionEndpoint, TargetProjectionMode,
};
pub type ConnectedTargetProjection =
    Box<dyn FnOnce(TargetProjectionEndpoint) -> io::Result<()> + Send>;
enum RetainedService {
    Source(Box<RetainedIoAdmission>),
    Target(Box<TargetProjectionAdmission>, ConnectedTargetProjection),
}
pub use retained_io::NATIVE_HISTORY_VERIFIER_FD as RETAINED_NATIVE_HISTORY_VERIFIER_FD;
pub use retained_io::NATIVE_OWNER_FD as RETAINED_NATIVE_OWNER_FD;
pub use retained_io::REMOVAL_DIRECTORY_FD as RETAINED_REMOVAL_DIRECTORY_FD;
pub use retained_io::REMOVAL_JOURNAL_FD as RETAINED_REMOVAL_JOURNAL_FD;
pub use retained_io::SELECTED_REPLACEMENT_PERMIT_FD as RETAINED_SELECTED_REPLACEMENT_PERMIT_FD;
pub use retained_io::SOURCE_DATA_FD as RETAINED_SOURCE_DATA_FD;
pub use retained_io::SOURCE_PATH as RETAINED_IO_SOURCE_PATH;
pub use retained_io::{
    RetainedIoAdmission, RetainedIoAuthority, RetainedIoCredentials, RetainedNativeOwner,
};
pub const RETAINED_IO_PACKAGE_ID: &str = "so.tilo.imager.home-export";
pub const RETAINED_IO_WORKER_ARGUMENT: &str = "--tilo-retained-source-io";
pub const RETAINED_HOME_IO_WORKER_ARGUMENT: &str = "--tilo-retained-home-io";
pub use capture::original::verify_original_read_claim;
pub use capture::{
    CaptureAdmission, CaptureOutputAdmission, INVENTORY_SCRATCH_BYTES_ENV, INVENTORY_SCRATCH_PATH,
    InstalledCopyOutputAdmission, InstalledCopyOutputBinding, InstalledCopyOutputCustody,
    InstalledCopyOutputFiles, ORIGINAL_FLATPAK_INVENTORY_PATH, ORIGINAL_FLATPAK_SOURCE_PATH,
    OUTPUT_PATH as CAPTURE_OUTPUT_PATH, OriginalFlatpakBinding, OriginalFlatpakProjection,
    OriginalMountedBinding, OriginalMountedIdentity, OriginalMountedInput, OriginalOutputAdmission,
    OriginalOutputBinding, OriginalOutputCustody, OriginalOutputFiles,
    SOURCE_PATH as CAPTURE_SOURCE_PATH, VerifiedOriginalMountedRoots,
    original_mounted_table_sha256, run_capture, run_capture_connected,
    run_capture_connected_observed, verify_original_mounted_roots,
};

/// Original host admission/custody, checked before spawn and while running.
pub trait ServiceAuthority: Send + Sync {
    /// Temporary protected network decision of the original native host.
    /// # Errors
    /// Refuses a retired review; absence grants no egress.
    fn network_hosts(&self) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Protected original controller's actual reopened per-volume table. Ordinary
    /// authorities and caller-authored metadata confer no mounted read authority.
    ///
    /// # Errors
    /// Refuses unavailable or retired original mounted authority.
    fn original_mounted_binding(&self) -> io::Result<Option<OriginalMountedBinding>> {
        Ok(None)
    }

    /// # Errors
    /// Refuses retired original authority.
    fn check(&self) -> io::Result<()>;
}
/// How often a running service's supervisor re-checks [`ServiceAuthority`]
/// and the host lifetime of [`ServiceCancellation::for_owner`]. Neither can
/// ring the supervisor when it changes, so this bounds how long a retired
/// grant or host keeps the service running. An explicit
/// [`ServiceCancellation::cancel`] (and so every consent revocation built on
/// it) wakes the supervisor at once. The previous 10 ms poll woke the host
/// 100 times a second for a session-long service.
const AUTHORITY_RECHECK: Duration = Duration::from_millis(250);
/// SIGTERM grace before the private namespace init is killed.
const STOP_GRACE: Duration = Duration::from_secs(3);
#[derive(Default)]
struct CancelState {
    cancelled: AtomicBool,
    /// Supervisors to ring on [`ServiceCancellation::cancel`]; dead entries
    /// are pruned whenever another supervisor subscribes.
    waiters: Mutex<Vec<std::sync::Weak<bounded_command::WakeFd>>>,
}
#[derive(Clone, Default)]
pub struct ServiceCancellation {
    state: Arc<CancelState>,
    owner: Option<std::sync::Weak<()>>,
    parent: Option<Arc<Self>>,
}
impl ServiceCancellation {
    pub fn cancel(&self) {
        self.state.cancelled.store(true, Ordering::Release);
        let waiters = self
            .state
            .waiters
            .lock()
            .map(|waiters| {
                waiters
                    .iter()
                    .filter_map(std::sync::Weak::upgrade)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for waiter in waiters {
            waiter.ring();
        }
    }
    pub fn for_owner(owner: std::sync::Weak<()>) -> Self {
        Self {
            state: Arc::default(),
            owner: Some(owner),
            parent: None,
        }
    }
    #[must_use]
    pub fn child(&self) -> Self {
        Self {
            state: Arc::default(),
            owner: self.owner.clone(),
            parent: Some(Arc::new(self.clone())),
        }
    }
    /// Ring `wake` when this cancellation or any parent is cancelled. Check
    /// [`Self::is_cancelled`] after subscribing so no cancel is missed.
    fn subscribe(&self, wake: &Arc<bounded_command::WakeFd>) {
        if let Ok(mut waiters) = self.state.waiters.lock() {
            waiters.retain(|waiter| waiter.strong_count() > 0);
            waiters.push(Arc::downgrade(wake));
        }
        if let Some(parent) = &self.parent {
            parent.subscribe(wake);
        }
    }
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| parent.is_cancelled())
            || self
                .owner
                .as_ref()
                .is_some_and(|owner| owner.upgrade().is_none())
    }
}
/// Nonserializable ownership token retained by a private working image.
#[derive(Clone, Default)]
pub struct ServiceLeaseKey(Arc<()>);
/// Created only after actual signed launch, successful wait and PID-namespace
/// init exit. Forced shutdowns and unobservable namespaces cannot create it.
pub struct QuiescentExit {
    key: ServiceLeaseKey,
    data_home: PathBuf,
    package: String,
}
impl QuiescentExit {
    pub fn matches(&self, key: &ServiceLeaseKey, data_home: &Path, package: &str) -> bool {
        Arc::ptr_eq(&self.key.0, &key.0) && self.data_home == data_home && self.package == package
    }
}
/// Cleanup-only receipt. It proves exact namespace death, never successful
/// export/checkpoint, retained authority or ciphertext integrity.
pub struct ConfirmedShutdown {
    key: ServiceLeaseKey,
    data_home: PathBuf,
    package: String,
}
impl ConfirmedShutdown {
    pub fn matches(&self, key: &ServiceLeaseKey, data_home: &Path, package: &str) -> bool {
        Arc::ptr_eq(&self.key.0, &key.0) && self.data_home == data_home && self.package == package
    }
}
pub struct SupervisedRun {
    pub result: Result<QuiescentExit, RunError>,
    pub shutdown: Option<ConfirmedShutdown>,
}
/// The installed package selected by the native host, never by an app request.
pub struct ServiceLaunch {
    pub manifest: PathBuf,
    pub payload: PathBuf,
    pub options: RunOptions,
}
/// Use the normal signed sandbox pipeline, with the actual provider as PID 1.
/// Linux completes descendant teardown before that init becomes waitable.
pub fn run(
    launch: ServiceLaunch,
    authority: Arc<dyn ServiceAuthority>,
    cancellation: ServiceCancellation,
    key: ServiceLeaseKey,
) -> Result<QuiescentExit, RunError> {
    run_connected(launch, authority, cancellation, key, None)
}

/// Connect only to a peer in the verified private namespace of this exact
/// signed launch. This callback runs on the owning service worker, never the UI.
/// # Errors
/// As [`run`], also refusing an unavailable or substituted private endpoint.
pub fn run_connected(
    launch: ServiceLaunch,
    authority: Arc<dyn ServiceAuthority>,
    cancellation: ServiceCancellation,
    key: ServiceLeaseKey,
    connected: Option<ConnectedService>,
) -> Result<QuiescentExit, RunError> {
    run_admitted(launch, authority, cancellation, key, connected, None)
}

/// Run the one offline signed source-I/O service and observe canonical shutdown.
/// The callback connects/transfers its private stream and returns promptly; it
/// must not run the projector loop while blocking namespace supervision.
pub fn run_retained_io_connected(
    launch: ServiceLaunch,
    admission: RetainedIoAdmission,
    cancellation: ServiceCancellation,
    key: ServiceLeaseKey,
    connected: Option<ConnectedService>,
) -> SupervisedRun {
    struct Authority(Arc<dyn RetainedIoAuthority>);
    impl ServiceAuthority for Authority {
        fn check(&self) -> io::Result<()> {
            self.0.check()?;
            if !self.0.network_hosts()?.is_empty() {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(())
        }
    }
    let authority: Arc<dyn ServiceAuthority> = Arc::new(Authority(admission.authority.clone()));
    let mut shutdown = None;
    let result = run_admitted_inner(
        launch,
        authority,
        cancellation,
        key,
        connected,
        None,
        Some(RetainedService::Source(Box::new(admission))),
        Some(&mut shutdown),
    );
    SupervisedRun { result, shutdown }
}

/// Run the fixed signed target-FUSE worker through canonical native custody.
/// Callback starts connected relay before `observe_ready` and returns promptly;
/// `NamespaceOwner` continues actual service supervision and observed teardown.
pub fn run_target_projection_connected(
    launch: ServiceLaunch,
    admission: TargetProjectionAdmission,
    cancellation: ServiceCancellation,
    key: ServiceLeaseKey,
    connected: ConnectedTargetProjection,
) -> SupervisedRun {
    struct Authority(Arc<dyn TargetProjectionAuthority>);
    impl ServiceAuthority for Authority {
        fn check(&self) -> io::Result<()> {
            self.0.check()?;
            if !self.0.network_hosts()?.is_empty() {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(())
        }
    }
    let authority: Arc<dyn ServiceAuthority> = Arc::new(Authority(admission.authority.clone()));
    let mut shutdown = None;
    let result = run_admitted_inner(
        launch,
        authority,
        cancellation,
        key,
        None,
        None,
        Some(RetainedService::Target(Box::new(admission), connected)),
        Some(&mut shutdown),
    );
    SupervisedRun { result, shutdown }
}

fn run_admitted(
    launch: ServiceLaunch,
    authority: Arc<dyn ServiceAuthority>,
    cancellation: ServiceCancellation,
    key: ServiceLeaseKey,
    connected: Option<ConnectedService>,
    capture: Option<CaptureAdmission>,
) -> Result<QuiescentExit, RunError> {
    run_admitted_inner(
        launch,
        authority,
        cancellation,
        key,
        connected,
        capture,
        None,
        None,
    )
}
fn run_admitted_observed(
    launch: ServiceLaunch,
    authority: Arc<dyn ServiceAuthority>,
    cancellation: ServiceCancellation,
    key: ServiceLeaseKey,
    connected: Option<ConnectedService>,
    capture: Option<CaptureAdmission>,
) -> SupervisedRun {
    let mut shutdown = None;
    let result = run_admitted_inner(
        launch,
        authority,
        cancellation,
        key,
        connected,
        capture,
        None,
        Some(&mut shutdown),
    );
    SupervisedRun { result, shutdown }
}
fn configure_service_options(
    options: &mut RunOptions,
    payload: &Path,
    manifest: &Path,
    capture: Option<&CaptureAdmission>,
    retained_io: Option<&RetainedIoAdmission>,
    target_projection: Option<&TargetProjectionAdmission>,
) -> Result<(), RunError> {
    crate::run::validate_owned_service_options(options)?;
    if options.bwrap_bin.is_some()
        || options.sandbox_shim_bin.is_some()
        || options.minimal_libs
        || options.runtime_root.is_some()
        || options.workspace_id.is_some()
        || !options.no_services
        || options.capabilities_path.is_some()
        || options.wayland_socket.is_some()
        || options.render_node.is_some()
    {
        return Err(RunError::InvalidServiceLaunch(
            "service custody options changed".into(),
        ));
    }
    options.bwrap_bin = Some("/usr/bin/bwrap".into());
    // This API is called in-process by the shell, whose executable deliberately
    // refuses shim mode. Use the OS-owned launcher, never current_exe or a
    // caller-selected replacement that could skip mandatory confinement.
    options.sandbox_shim_bin = Some("/usr/libexec/tilo/tilo-launcher".into());
    if let Some(capture) = capture {
        // Refuse unguarded original readers before workspace creation/spawn.
        // Builder admission remains usable before the private context attaches.
        capture.check_execution()?;
        capture.configure(options);
    }
    if let Some(io) = retained_io {
        io.check()?;
        if capture.is_some()
            || !matches!(&options.mode, crate::run::LaunchMode::Service { passthrough_argv } if passthrough_argv.as_slice() == [io.worker_argument()])
            || options.open_document.is_some()
            || options.open_uri.is_some()
            || options.app_id.as_deref() != Some(RETAINED_IO_PACKAGE_ID)
            || options.headless_console.is_some()
            || options.trusted_identities_host_path.is_some()
            || options.retained_files_host_path.is_some()
            || options.browser_continuity_host_path.is_some()
            || options.action_authority_host_path.is_some()
        {
            return Err(RunError::InvalidServiceLaunch(
                "Retained I/O options differ".into(),
            ));
        }
        io.native()?.configure(options);
        for path in [
            payload,
            manifest
                .parent()
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?,
        ] {
            io.exclude_path(path)?;
        }
    }
    if let Some(target) = target_projection {
        target.native()?;
        if capture.is_some()
            || retained_io.is_some()
            || !matches!(&options.mode,crate::run::LaunchMode::Service {passthrough_argv} if passthrough_argv.as_slice()==[target_projection::WORKER_ARG])
            || options.open_document.is_some()
            || options.open_uri.is_some()
            || options.app_id.as_deref() != Some(target_projection::PACKAGE_ID)
            || options.headless_console.is_some()
            || options.trusted_identities_host_path.is_some()
            || options.retained_files_host_path.is_some()
            || options.browser_continuity_host_path.is_some()
            || options.action_authority_host_path.is_some()
        {
            return Err(RunError::InvalidServiceLaunch(
                "Target projection options differ".into(),
            ));
        }
        target.native()?.configure(options);
    }

    Ok(())
}

fn check_service_exit_authority(owner: &NamespaceOwner) -> Result<(), RunError> {
    if let Some(io) = &owner.retained_io {
        io.check()?;
    }
    if let Some(target) = &owner.target_projection {
        target.check()?;
    }
    if let Some(original) = owner
        .capture
        .as_ref()
        .and_then(CaptureAdmission::original_custody)
    {
        original.check()?;
        if !original.network_hosts()?.is_empty() {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied).into());
        }
    }
    Ok(())
}

fn validate_service_manifest(
    options: &RunOptions,
    manifest: &crate::manifest::ManifestView,
    retained: bool,
) -> Result<(), RunError> {
    if options.app_id.as_deref() != Some(manifest.package_id.as_str()) {
        return Err(RunError::InvalidServiceLaunch(
            "owned service identity changed".into(),
        ));
    }
    if manifest.sandbox.net.mode != "off" || !manifest.sandbox.net.hosts.is_empty() {
        return Err(RunError::InvalidServiceLaunch(
            "owned service egress requires protected native review".into(),
        ));
    }
    if retained
        && (manifest.package_id != RETAINED_IO_PACKAGE_ID
            || manifest.kind != "service"
            || !manifest.optional_runtimes.is_empty()
            || !manifest.sandbox.portals.is_empty()
            || !manifest.sandbox.services.is_empty()
            || !manifest.sandbox.fs.read.is_empty()
            || !manifest.sandbox.fs.write.is_empty())
    {
        return Err(RunError::InvalidServiceLaunch(
            "Retained I/O manifest authority differs".into(),
        ));
    }

    Ok(())
}

fn run_admitted_inner(
    launch: ServiceLaunch,
    authority: Arc<dyn ServiceAuthority>,
    cancellation: ServiceCancellation,
    key: ServiceLeaseKey,
    connected: Option<ConnectedService>,
    capture: Option<CaptureAdmission>,
    retained: Option<RetainedService>,
    observed_shutdown: Option<&mut Option<ConfirmedShutdown>>,
) -> Result<QuiescentExit, RunError> {
    let (retained_io, target_projection, target_connected) = match retained {
        Some(RetainedService::Source(io)) => (Some(*io), None, None),
        Some(RetainedService::Target(target, callback)) => (None, Some(*target), Some(callback)),
        None => (None, None, None),
    };
    let mut options = launch.options;
    configure_service_options(
        &mut options,
        &launch.payload,
        &launch.manifest,
        capture.as_ref(),
        retained_io.as_ref(),
        target_projection.as_ref(),
    )?;
    let expected = options
        .expected_package_hash
        .as_deref()
        .ok_or_else(|| RunError::InvalidServiceLaunch("original installation missing".into()))?;
    let manifest = match capture
        .as_ref()
        .and_then(|capture| capture.original_manifest(&launch.manifest, &launch.payload, expected))
        .or_else(|| {
            retained_io.as_ref().and_then(|io| match io.native() {
                Ok(native) => native.original_manifest(&launch.manifest, &launch.payload, expected),
                Err(error) => Some(Err(error.into())),
            })
        })
        .or_else(|| {
            target_projection
                .as_ref()
                .and_then(|target| match target.native() {
                    Ok(native) => {
                        native.original_manifest(&launch.manifest, &launch.payload, expected)
                    }
                    Err(error) => Some(Err(error.into())),
                })
        }) {
        Some(manifest) => manifest?,
        None => crate::run::verified_original_manifest(
            &launch.manifest,
            &launch.payload,
            &options,
            expected,
        )?,
    };
    validate_service_manifest(
        &options,
        &manifest,
        retained_io.is_some() || target_projection.is_some(),
    )?;
    let data_home = options
        .data_home
        .clone()
        .ok_or_else(|| RunError::InvalidServiceLaunch("private working lease missing".into()))?;
    if let Some(capture) = &capture {
        capture.prepare_original_workspace(&data_home)?;
    }
    if let Some(io) = &retained_io {
        io.prepare_workspace(&data_home)?;
    }
    if let Some(target) = &target_projection {
        target.prepare_workspace(&data_home)?;
    }
    // Endpoint clones must retain and revalidate the mandatory original read
    // context too; caller-selected authority cannot substitute for it.
    let authority = original_read_authority(authority, capture.as_ref());
    let mut owner = NamespaceOwner::new(authority, cancellation)?;
    owner.endpoint = crate::appdir::host_app_dir(
        &data_home,
        crate::appdir::PERSONAL_ROOM,
        &manifest.package_id,
    )
    .join("provider.sock");
    owner.connected = Mutex::new(connected);
    owner.capture = capture;
    owner.retained_io = retained_io;
    owner.target_projection = target_projection;
    owner.target_connected = Mutex::new(target_connected);
    let status =
        crate::run::run_with_context(&manifest, &launch.payload, &options, None, Some(&owner));
    // Even errors return only after canonical ServiceChild teardown. Unknown
    // init/outer wait never sets shutdown. Preserve the original result below.
    if owner.shutdown.load(Ordering::Acquire)
        && let Some(observed) = observed_shutdown
    {
        *observed = owner.confirmed_shutdown(&key, &data_home, &manifest.package_id);
    }
    let status = status?;
    check_service_exit_authority(&owner)?;
    if !status.success() || !owner.quiescent.load(Ordering::Acquire) {
        return Err(RunError::InvalidServiceLaunch(
            "provider exit was not a clean checkpoint".into(),
        ));
    }
    Ok(QuiescentExit {
        key,
        data_home,
        package: manifest.package_id,
    })
}

fn original_read_authority(
    authority: Arc<dyn ServiceAuthority>,
    capture: Option<&CaptureAdmission>,
) -> Arc<dyn ServiceAuthority> {
    match capture.and_then(CaptureAdmission::original_custody) {
        Some(original) => Arc::new(OriginalReadAuthority {
            outer: authority,
            original,
        }),
        None => authority,
    }
}

struct OriginalReadAuthority {
    outer: Arc<dyn ServiceAuthority>,
    original: Arc<dyn ServiceAuthority>,
}
impl ServiceAuthority for OriginalReadAuthority {
    fn network_hosts(&self) -> io::Result<Vec<String>> {
        self.original.check()?;
        if !self.original.network_hosts()?.is_empty() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.outer.network_hosts()
    }
    fn check(&self) -> io::Result<()> {
        self.original.check()?;
        if !self.original.network_hosts()?.is_empty() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.outer.check()
    }
}

pub type ConnectedService = Box<dyn FnOnce(ServiceEndpoint) -> io::Result<()> + Send>;

/// Host-only endpoint pinned to the actual private namespace and lease path.
/// No public constructor or ambient socket path can substitute for it.
pub struct ServiceEndpoint {
    retained_program: Option<Arc<retained_worker::RetainedWorkerProgram>>,
    retained_gid: Option<u32>,
    path: PathBuf,
    namespace: File,
    init: OwnedFd,
    init_pid: u32,
    expected_uid: u32,
    authority: Arc<dyn ServiceAuthority>,
    cancellation: ServiceCancellation,
}
/// Observed kernel facts for durable recovery; this DTO itself grants no authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceCustody {
    pub init_pid: u32,
    pub start_ticks: u64,
    pub namespace_dev: u64,
    pub namespace_ino: u64,
}
impl ServiceEndpoint {
    /// Observe exact retained init pidfd readiness even after cancellation or
    /// authority revocation. This is stop evidence, never a successful exit.
    /// # Errors
    /// Refuses non-root observers or unavailable kernel evidence.
    pub fn observed_stopped(&self) -> io::Result<bool> {
        if !rustix::process::getuid().is_root() || !rustix::process::geteuid().is_root() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        exited(&self.init)
    }
    /// Observe the exact live namespace before issuing any provider command.
    /// # Errors
    /// Refuses retired authority, cancellation, dead init or unavailable held evidence.
    pub fn custody(&self) -> io::Result<ServiceCustody> {
        self.authority.check()?;
        if self.cancellation.is_cancelled() || exited(&self.init)? {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let held = self.namespace.metadata()?;
        // PID namespace membership belongs to this exact process for its
        // lifetime. init_handle authenticated and retained both namespace and
        // pidfd before credential drop; a live held pidfd excludes PID reuse.
        // Reopening /proc/<init>/ns here adds ptrace permissions, not evidence,
        // and fails after the confined original reader changes credentials.
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", self.init_pid))?;
        let start_ticks = stat
            .rsplit_once(") ")
            .and_then(|(_, fields)| fields.split_whitespace().nth(19))
            .ok_or(io::ErrorKind::InvalidData)?
            .parse()
            .map_err(io::Error::other)?;
        self.authority.check()?;
        if self.cancellation.is_cancelled() || exited(&self.init)? {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok(ServiceCustody {
            init_pid: self.init_pid,
            start_ticks,
            namespace_dev: held.dev(),
            namespace_ino: held.ino(),
        })
    }
    /// Open a separate authenticated stream to this one provider. No protocol
    /// byte is sent before peer namespace, ownership and live authority match.
    /// # Errors
    /// Refuses retired custody, missing endpoints and another namespace's peer.
    pub fn connect(&self) -> io::Result<UnixStream> {
        self.authority.check()?;
        if self.cancellation.is_cancelled() || exited(&self.init)? {
            return Err(io::ErrorKind::Interrupted.into());
        }
        // AF_UNIX has a tiny address field. Hold the actual private parent so
        // long preparation roots need neither a global alias nor a shorter lease.
        let parent = self.path.parent().ok_or(io::ErrorKind::InvalidInput)?;
        let name = self.path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        let directory = File::options()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(parent)?;
        let socket = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(name);
        let metadata = std::fs::symlink_metadata(&socket)?;
        // Retained and target workers can run as an admitted UID distinct from
        // the controller's; privacy is checked against that exact worker owner.
        if !metadata.file_type().is_socket()
            || metadata.uid() != self.expected_uid
            || !tilo_files::native::private::is_owner_only(&metadata)
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let stream = UnixStream::connect(&socket)?;
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length =
            u32::try_from(std::mem::size_of::<libc::ucred>()).map_err(io::Error::other)?;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                std::ptr::from_mut(&mut credentials).cast(),
                &raw mut length,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if usize::try_from(length).map_err(io::Error::other)? != std::mem::size_of::<libc::ucred>()
            || credentials.uid != self.expected_uid
            || credentials.pid <= 1
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        // The held live init pidfd prevents reuse of this exact PID. Its
        // namespace was authenticated before credential drop, which can make
        // /proc/<pid>/ns inaccessible to a controller without CAP_SYS_PTRACE.
        // Other peers still need the independent namespace comparison.
        if u32::try_from(credentials.pid).map_err(io::Error::other)? != self.init_pid {
            let peer = File::open(format!("/proc/{}/ns/pid", credentials.pid))?.metadata()?;
            let namespace = self.namespace.metadata()?;
            if peer.dev() != namespace.dev() || peer.ino() != namespace.ino() {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
        self.authority.check()?;
        if self.cancellation.is_cancelled() || exited(&self.init)? {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok(stream)
    }
}

pub(crate) struct NamespaceOwner {
    authority: Arc<dyn ServiceAuthority>,
    cancellation: ServiceCancellation,
    info_reader: Mutex<File>,
    info_writer: Mutex<Option<File>>,
    quiescent: AtomicBool,
    shutdown: Arc<AtomicBool>,
    endpoint: PathBuf,
    connected: Mutex<Option<ConnectedService>>,
    capture: Option<CaptureAdmission>,
    retained_io: Option<RetainedIoAdmission>,
    target_projection: Option<TargetProjectionAdmission>,
    target_connected: Mutex<Option<ConnectedTargetProjection>>,
    mount_namespace: Mutex<Option<File>>,
    ordinary_connected: Mutex<Option<crate::ordinary_namespace::ConnectedNamespace>>,
}
impl NamespaceOwner {
    pub(crate) fn for_ordinary(
        authority: Arc<dyn ServiceAuthority>,
        cancellation: ServiceCancellation,
        connected: crate::ordinary_namespace::ConnectedNamespace,
    ) -> io::Result<Self> {
        let owner = Self::new(authority, cancellation)?;
        *owner
            .ordinary_connected
            .lock()
            .map_err(|_| io::Error::other("ordinary namespace callback poisoned"))? =
            Some(connected);
        Ok(owner)
    }
    fn new(
        authority: Arc<dyn ServiceAuthority>,
        cancellation: ServiceCancellation,
    ) -> io::Result<Self> {
        let mut fds = [0; 2];
        // Each end is exclusively owned here; the launcher duplicates only the
        // writer into bwrap, which closes it before entering the provider.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let [read, write] = fds;
        Ok(Self {
            authority,
            cancellation,
            info_reader: Mutex::new(unsafe { File::from_raw_fd(read) }),
            info_writer: Mutex::new(Some(unsafe { File::from_raw_fd(write) })),
            quiescent: AtomicBool::new(false),
            shutdown: Arc::new(AtomicBool::new(false)),
            endpoint: PathBuf::new(),
            connected: Mutex::new(None),
            capture: None,
            retained_io: None,
            target_projection: None,
            target_connected: Mutex::new(None),
            mount_namespace: Mutex::new(None),
            ordinary_connected: Mutex::new(None),
        })
    }
    fn confirmed_shutdown(
        &self,
        key: &ServiceLeaseKey,
        data_home: &Path,
        package: &str,
    ) -> Option<ConfirmedShutdown> {
        self.shutdown
            .load(Ordering::Acquire)
            .then(|| ConfirmedShutdown {
                key: key.clone(),
                data_home: data_home.into(),
                package: package.into(),
            })
    }
    fn before_connection(&self) -> io::Result<()> {
        self.authority.check()?;
        if let Some(capture) = &self.capture {
            capture.check()?;
        }
        if let Some(io) = &self.retained_io {
            io.check()?;
        }
        if let Some(target) = &self.target_projection {
            target.check()?;
        }
        if self.cancellation.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok(())
    }
    fn init_handle(&self, child: &Child) -> io::Result<(OwnedFd, File, u32)> {
        let mut reader = self
            .info_reader
            .lock()
            .map_err(|_| io::Error::other("service info poisoned"))?;
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(5))
            .ok_or(io::ErrorKind::InvalidData)?;
        let mut bytes = Vec::new();
        let mut block = [0; 1024];
        loop {
            match reader.read(&mut block) {
                Ok(0) => break,
                Ok(count) if bytes.len().saturating_add(count) <= 16 * 1024 => {
                    bytes.extend_from_slice(block.get(..count).ok_or(io::ErrorKind::InvalidData)?);
                }
                Ok(_) => return Err(io::ErrorKind::InvalidData.into()),
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
            if self.cancellation.is_cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
        }
        drop(reader);
        let info: serde_json::Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        let pid = info
            .get("child-pid")
            .and_then(serde_json::Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .filter(|pid| *pid > 1)
            .ok_or(io::ErrorKind::InvalidData)?;
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        let fd = i32::try_from(fd).map_err(io::Error::other)?;
        if fd < 0 {
            let error = io::Error::last_os_error();
            return Err(io::Error::new(
                error.kind(),
                format!("private namespace pidfd_open({pid}): {error}"),
            ));
        }
        let handle = unsafe { OwnedFd::from_raw_fd(fd) };
        verify_init(pid, child.id())?;
        let namespace = File::open(format!("/proc/{pid}/ns/pid"))?;
        Ok((handle, namespace, pid.cast_unsigned()))
    }
}
impl NamespaceOwner {
    fn connect_services(
        &self,
        owned: &ServiceChild,
        namespace: File,
        init_pid: u32,
    ) -> io::Result<()> {
        let ordinary_connected = self
            .ordinary_connected
            .lock()
            .map_err(|_| io::Error::other("ordinary namespace callback poisoned"))?
            .take();
        if let Some(connected) = ordinary_connected {
            self.before_connection()?;
            let mount_namespace = File::open(format!("/proc/{init_pid}/ns/mnt"))?;
            let witness = crate::ordinary_namespace::NamespaceWitness::observed(
                owned
                    .init
                    .as_ref()
                    .ok_or(io::ErrorKind::InvalidData)?
                    .try_clone()?,
                namespace.try_clone()?,
                mount_namespace,
                init_pid,
                owned.child.id(),
            );
            // The fixed bounded handoff sends held descriptors to the protected
            // root owner. Returning never retires actual namespace custody.
            connected.send(witness, &self.cancellation)?;
            self.before_connection()?;
        }
        if let Some(target) = &self.target_projection {
            self.before_connection()?;
            let endpoint = ServiceEndpoint {
                retained_program: None,
                retained_gid: None,
                path: self.endpoint.clone(),
                namespace: namespace.try_clone()?,
                init: owned
                    .init
                    .as_ref()
                    .ok_or(io::ErrorKind::InvalidData)?
                    .try_clone()?,
                init_pid,
                expected_uid: target.identity().0,
                authority: self.authority.clone(),
                cancellation: self.cancellation.clone(),
            };
            let endpoint = TargetProjectionEndpoint::receive(endpoint, target)?;
            *self
                .mount_namespace
                .lock()
                .map_err(|_| io::Error::other("Target mount custody poisoned"))? =
                Some(endpoint.mount_namespace_clone()?);
            let callback = self
                .target_connected
                .lock()
                .map_err(|_| io::Error::other("Target callback poisoned"))?
                .take()
                .ok_or(io::ErrorKind::InvalidData)?;
            callback(endpoint)?;
        }
        let connected = self
            .connected
            .lock()
            .map_err(|_| io::Error::other("service connection poisoned"))?
            .take();
        if let Some(connected) = connected {
            self.before_connection()?;
            let init = owned
                .init
                .as_ref()
                .ok_or(io::ErrorKind::InvalidData)?
                .try_clone()?;
            connected(ServiceEndpoint {
                retained_program: self
                    .retained_io
                    .as_ref()
                    .and_then(|io| io.worker_program.clone()),
                retained_gid: self.retained_io.as_ref().map(|io| io.identity().1),
                path: self.endpoint.clone(),
                namespace,
                init,
                init_pid,
                expected_uid: self.retained_io.as_ref().map_or_else(
                    || {
                        self.capture.as_ref().map_or_else(
                            || rustix::process::geteuid().as_raw(),
                            CaptureAdmission::worker_uid,
                        )
                    },
                    |io| io.identity().0,
                ),
                authority: self.authority.clone(),
                cancellation: self.cancellation.clone(),
            })?;
        }

        Ok(())
    }
}
impl EnrolledProcessObserver for NamespaceOwner {
    fn reviewed_network_hosts(&self) -> io::Result<Option<Vec<String>>> {
        self.before_spawn()?;
        let hosts = self.authority.network_hosts()?;
        if (self.capture.is_some()
            || self.retained_io.is_some()
            || self.target_projection.is_some())
            && !hosts.is_empty()
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(Some(hosts))
    }
    fn retained_io_projection(&self) -> Option<&RetainedIoAdmission> {
        self.retained_io.as_ref()
    }
    fn target_projection(&self) -> Option<&TargetProjectionAdmission> {
        self.target_projection.as_ref()
    }
    fn capture_projection(&self) -> Option<&CaptureAdmission> {
        self.capture.as_ref()
    }
    fn namespace_init_fd(&self) -> Option<RawFd> {
        self.info_writer
            .lock()
            .ok()?
            .as_ref()
            .map(AsRawFd::as_raw_fd)
    }
    fn owns_process_group(&self) -> bool {
        true
    }
    fn before_spawn(&self) -> io::Result<()> {
        self.authority.check()?;
        if let Some(capture) = &self.capture {
            capture.check()?;
        }
        if let Some(io) = &self.retained_io {
            io.check()?;
        }
        if let Some(target) = &self.target_projection {
            target.check()?;
        }
        if self.cancellation.is_cancelled() || self.namespace_init_fd().is_none() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok(())
    }
    fn child_created(&self) {
        if let Ok(mut writer) = self.info_writer.lock() {
            writer.take();
        }
    }
    fn supervise(&self, child: Child) -> io::Result<ExitStatus> {
        let mut owned = ServiceChild {
            child,
            init: None,
            shutdown: self.shutdown.clone(),
        };
        let (init, namespace, init_pid) = self.init_handle(&owned.child)?;
        owned.init = Some(init);
        self.connect_services(&owned, namespace, init_pid)?;
        // Sleep on the outer child's exit and on cancellation; wake on a
        // timer only to re-check what cannot ring (AUTHORITY_RECHECK).
        let wake = Arc::new(bounded_command::WakeFd::new()?);
        self.cancellation.subscribe(&wake);
        let mut stopping = None;
        let mut forced = false;
        // The first wait only probes: the checks below run at once.
        let mut recheck = Instant::now();
        loop {
            let (ring, deadline) = match stopping {
                None => (Some(wake.fd()), Some(recheck)),
                Some(grace) if !forced => (None, Some(grace)),
                // Killed: nothing is left to do but observe the exit.
                Some(_) => (None, None),
            };
            match bounded_command::wait_child_or(&mut owned.child, ring, deadline)? {
                bounded_command::Wakeup::Exited(status) => {
                    let init = owned.init.as_ref().ok_or(io::ErrorKind::InvalidData)?;
                    if status.success() && !forced && exited(init)? {
                        if let Some(io) = &self.retained_io {
                            io.confirm_removal_retirement()?;
                        }
                        self.quiescent.store(true, Ordering::Release);
                        self.shutdown.store(true, Ordering::Release);
                        owned.init.take();
                    }
                    // An outer failure is not proof that the private namespace
                    // stopped. Retain the init pidfd so Drop kills only that init.
                    return Ok(status);
                }
                // Drain before checking, so a cancel racing the check rings again.
                bounded_command::Wakeup::Woken => wake.drain(),
                bounded_command::Wakeup::Deadline => {}
            }
            let now = Instant::now();
            if stopping.is_none() && now >= recheck {
                recheck = now
                    .checked_add(AUTHORITY_RECHECK)
                    .ok_or(io::ErrorKind::InvalidData)?;
            }
            if stopping.is_none()
                // The root-held clone must be detached after actual consumer
                // stop before this target's intentional teardown. Checks below
                // still fail and source/relay calls still deny immediately.
                && (self.cancellation.is_cancelled()
                    || self.authority.check().is_err()
                    || self
                        .capture
                        .as_ref()
                        .is_some_and(|capture| capture.check().is_err()))
                && self.target_projection.as_ref().is_none_or(TargetProjectionAdmission::begin_exposure_stop)
            {
                owned.signal(libc::SIGTERM)?;
                stopping = Some(
                    now.checked_add(STOP_GRACE)
                        .ok_or(io::ErrorKind::InvalidData)?,
                );
            }
            if stopping.is_some_and(|deadline| Instant::now() >= deadline) && !forced {
                owned.signal(libc::SIGKILL)?;
                forced = true;
            }
        }
    }
}
pub(crate) fn verify_init(pid: i32, parent: u32) -> io::Result<()> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).map_err(|error| {
        let own = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let visible = own
            .lines()
            .find(|line| line.starts_with("NSpid:"))
            .unwrap_or("NSpid: unavailable");
        io::Error::new(
            error.kind(),
            format!(
                "private namespace status pid={pid} parent={parent} self={} {visible}: {error}",
                std::process::id()
            ),
        )
    })?;
    let actual_parent = status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))
        .and_then(|value| value.trim().parse::<u32>().ok());
    let nspid = status
        .lines()
        .find_map(|line| line.strip_prefix("NSpid:"))
        .map(|value| value.split_whitespace().collect::<Vec<_>>());
    if actual_parent != Some(parent)
        || nspid
            .as_ref()
            .is_none_or(|ids| ids.len() < 2 || ids.last() != Some(&"1"))
        || std::fs::metadata(format!("/proc/{pid}/ns/pid"))?.ino()
            == std::fs::metadata("/proc/self/ns/pid")?.ino()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "private namespace witness differs: pid={pid} parent={parent} actual_parent={actual_parent:?} nspid={nspid:?}"
            ),
        ));
    }
    Ok(())
}
fn exited(handle: &OwnedFd) -> io::Result<bool> {
    let mut fd = libc::pollfd {
        fd: handle.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&raw mut fd, 1, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd.revents & libc::POLLIN != 0)
}
struct ServiceChild {
    child: Child,
    init: Option<OwnedFd>,
    shutdown: Arc<AtomicBool>,
}
impl ServiceChild {
    fn signal(&self, signal: i32) -> io::Result<()> {
        let fd = self
            .init
            .as_ref()
            .ok_or(io::ErrorKind::InvalidData)?
            .as_raw_fd();
        if unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd,
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        } != 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }
}
impl Drop for ServiceChild {
    fn drop(&mut self) {
        if self.init.is_some() {
            let _ = self.signal(libc::SIGKILL);
        }
        if self.child.try_wait().ok().flatten().is_none() {
            // The privately owned outer group covers setup failure before an
            // init pidfd could be acquired. It cannot mint a checkpoint witness.
            // The unreaped child owns this private group, so its number
            // cannot have been recycled into another session's group.
            bounded_command::terminate_tree(&mut self.child, Duration::ZERO);
        }
        if let Some(init) = &self.init {
            // This destructor runs on the canonical owning service worker.
            // Outer wait is not namespace quiescence. Keep the owner (including
            // original source/mount/store custody) alive until the exact init
            // pidfd becomes readable, even if killed tasks remain in kernel IO.
            // A deadline diagnoses incomplete cleanup; it cannot release leases.
            let started = Instant::now();
            let mut reported = false;
            loop {
                match exited(init) {
                    Ok(true) => {
                        self.shutdown.store(true, Ordering::Release);
                        break;
                    }
                    Err(error) if !reported => {
                        tracing::error!(%error, "owned namespace exit observation failed; retaining custody");
                        reported = true;
                    }
                    Ok(false) | Err(_) => {}
                }
                if !reported && started.elapsed() >= Duration::from_secs(3) {
                    tracing::error!(
                        "owned namespace cleanup timed out; retaining custody until kernel exit"
                    );
                    reported = true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stopping_enrollment_child_preserves_parent_but_parent_retirement_stops_new_children() {
        let owner = Arc::new(());
        let parent = ServiceCancellation::for_owner(Arc::downgrade(&owner));
        let child = parent.child();
        child.cancel();
        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
        let next = parent.child();
        assert!(!next.is_cancelled());
        parent.cancel();
        assert!(next.is_cancelled());
        let original = ServiceCancellation::for_owner(Arc::downgrade(&owner));
        let next = original.child();
        drop(owner);
        assert!(next.is_cancelled());
    }

    use std::os::unix::{fs::PermissionsExt as _, process::CommandExt as _};
    struct Authority(AtomicBool);
    impl ServiceAuthority for Authority {
        fn check(&self) -> io::Result<()> {
            if self.0.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(io::ErrorKind::PermissionDenied.into())
            }
        }
    }
    struct Running {
        root: tempfile::TempDir,
        owner: Arc<NamespaceOwner>,
        authority: Arc<Authority>,
        worker: Option<std::thread::JoinHandle<io::Result<ExitStatus>>>,
        /// Kernel thread id of the supervising worker.
        worker_tid: libc::pid_t,
        outer: u32,
        endpoint: ServiceEndpoint,
    }
    impl Running {
        fn start() -> Self {
            Self::with_script(
                "trap 'exit 0' TERM; @WRITER@ while [ ! -e /fixture/stop ]; do sleep 0.02; done; exit 0",
            )
        }
        fn with_script(script: &str) -> Self {
            Self::with_cancellation(script, ServiceCancellation::default())
        }
        fn with_cancellation(script: &str, cancellation: ServiceCancellation) -> Self {
            let root = tempfile::tempdir().unwrap();
            let authority = Arc::new(Authority(AtomicBool::new(true)));
            let mut owner = NamespaceOwner::new(authority.clone(), cancellation).unwrap();
            owner.endpoint = root.path().join("provider.sock");
            let (send, receive) = std::sync::mpsc::sync_channel(1);
            owner.connected = Mutex::new(Some(Box::new(move |endpoint| {
                send.send(endpoint)
                    .map_err(|_| io::ErrorKind::Interrupted.into())
            })));
            let owner = Arc::new(owner);
            assert_eq!(script.matches("@WRITER@").count(), 1);
            let script = script.replace(
                "@WRITER@",
                "python3 -c 'import socket,time; s=socket.socket(socket.AF_UNIX); s.bind(\"/fixture/provider.sock\"); s.listen(); open(\"/fixture/ready\",\"w\").close(); time.sleep(60)' & ",
            );
            let fd = owner.namespace_init_fd().unwrap();
            // Qualify the real kernel/production namespace owner without an
            // unsigned package or synthetic runtime becoming a trusted launch.
            let mut command = std::process::Command::new("/usr/bin/bwrap");
            command
                .args([
                    "--unshare-all",
                    "--as-pid-1",
                    "--die-with-parent",
                    "--info-fd",
                    &fd.to_string(),
                    "--ro-bind",
                    "/usr",
                    "/usr",
                    "--ro-bind",
                    "/lib",
                    "/lib",
                    "--ro-bind",
                    "/lib64",
                    "/lib64",
                    "--symlink",
                    "/usr/bin",
                    "/bin",
                    "--proc",
                    "/proc",
                    "--dev",
                    "/dev",
                    "--bind",
                ])
                .arg(root.path())
                .args(["/fixture", "/bin/sh", "-c", &script]);
            command.process_group(0);
            unsafe {
                command.pre_exec(move || {
                    libc::umask(0o077);
                    if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            owner.before_spawn().unwrap();
            let child = command.spawn().unwrap();
            let outer = child.id();
            owner.child_created();
            let observed = owner.clone();
            let (tid_send, tid_receive) = std::sync::mpsc::sync_channel(1);
            let worker = std::thread::spawn(move || {
                tid_send.send(unsafe { libc::gettid() }).unwrap();
                let result = observed.supervise(child);
                if let Err(error) = &result {
                    eprintln!("actual owned namespace supervision failed: {error}");
                }
                result
            });
            let running = Self {
                root,
                owner,
                authority,
                worker: Some(worker),
                worker_tid: tilo_test_env::recv("the supervisor thread", &tid_receive),
                outer,
                endpoint: tilo_test_env::recv("the provider endpoint", &receive),
            };
            tilo_test_env::wait_until("the fixture to observe its live subject", || {
                assert!(
                    !running.worker.as_ref().unwrap().is_finished(),
                    "actual private namespace must start"
                );
                running.root.path().join("ready").exists()
            });
            running
        }
        fn endpoint(&self) -> ServiceEndpoint {
            ServiceEndpoint {
                retained_program: self.endpoint.retained_program.clone(),
                retained_gid: self.endpoint.retained_gid,
                path: self.endpoint.path.clone(),
                namespace: self.endpoint.namespace.try_clone().unwrap(),
                init: self.endpoint.init.try_clone().unwrap(),
                init_pid: self.endpoint.init_pid,
                expected_uid: self.endpoint.expected_uid,
                authority: self.endpoint.authority.clone(),
                cancellation: self.endpoint.cancellation.clone(),
            }
        }
        fn orphan_handle(&self) -> OwnedFd {
            // Observe the production-authenticated writer directly. The
            // optional /proc/<pid>/task/<tid>/children file is not a witness.
            let stream = self.endpoint.connect().unwrap();
            let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
            let mut length = u32::try_from(std::mem::size_of::<libc::ucred>()).unwrap();
            assert_eq!(
                unsafe {
                    libc::getsockopt(
                        stream.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_PEERCRED,
                        std::ptr::from_mut(&mut peer).cast(),
                        &raw mut length,
                    )
                },
                0
            );
            assert_eq!(
                usize::try_from(length).unwrap(),
                std::mem::size_of::<libc::ucred>()
            );
            let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, peer.pid, 0) };
            assert!(raw >= 0, "the authenticated live writer must have a pidfd");
            let fd = unsafe { OwnedFd::from_raw_fd(i32::try_from(raw).unwrap()) };
            let status = std::fs::read_to_string(format!("/proc/{}/status", peer.pid)).unwrap();
            let inner_pid = status
                .lines()
                .find_map(|line| line.strip_prefix("NSpid:"))
                .and_then(|ids| ids.split_whitespace().last());
            assert!(
                inner_pid.is_some_and(|pid| pid != "1"),
                "the writer must be a namespace descendant"
            );
            assert!(
                !exited(&fd).unwrap(),
                "descendant must still be a live writer candidate"
            );
            fd
        }
    }
    impl Drop for Running {
        fn drop(&mut self) {
            self.owner.cancellation.cancel();
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
    #[test]
    fn stalled_ordinary_registration_cancels_exact_namespace_before_releasing_owner() {
        use std::io::Read as _;
        let cancellation = ServiceCancellation::default();
        let (sender, mut receiver) = UnixStream::pair().unwrap();
        receiver
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let owner = Arc::new(
            NamespaceOwner::for_ordinary(
                Arc::new(Authority(AtomicBool::new(true))),
                cancellation.clone(),
                crate::ordinary_namespace::ConnectedNamespace::new(sender),
            )
            .unwrap(),
        );
        let fd = owner.namespace_init_fd().unwrap();
        let mut command = std::process::Command::new("/usr/bin/bwrap");
        command.args([
            "--unshare-all",
            "--as-pid-1",
            "--die-with-parent",
            "--info-fd",
            &fd.to_string(),
            "--ro-bind",
            "/usr",
            "/usr",
            "--ro-bind",
            "/lib",
            "/lib",
            "--ro-bind",
            "/lib64",
            "/lib64",
            "--symlink",
            "/usr/bin",
            "/bin",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "/usr/bin/sleep",
            "30",
        ]);
        command.process_group(0);
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        owner.before_spawn().unwrap();
        let child = command.spawn().unwrap();
        owner.child_created();
        let observed = owner.clone();
        let worker = std::thread::spawn(move || observed.supervise(child));
        let mut numbers = [0; 12];
        receiver.read_exact(&mut numbers).unwrap();
        let (bytes, init) = runtime_portal::fd_transport::recv_with_fd(&receiver).unwrap();
        assert_eq!(bytes, [0]);
        let init = init.expect("canonical registration must send actual init pidfd");
        assert!(
            !exited(&init).unwrap(),
            "test must observe its live namespace subject"
        );
        // Retain actual root-side handle but withhold descriptor ACK. The
        // owner is still inside its real registration path, not its wait loop.
        // The ACK stays withheld: only the cancel can end the registration.
        cancellation.cancel();
        tilo_test_env::wait_until("the cancelled registration to return", || {
            worker.is_finished()
        });
        let error = worker
            .join()
            .unwrap()
            .expect_err("stalled registration must cancel");
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(
            exited(&init).unwrap(),
            "return requires exact namespace kernel stop"
        );
        assert!(owner.shutdown.load(Ordering::Acquire));
        assert!(
            !owner.quiescent.load(Ordering::Acquire),
            "cancelled registration cannot admit successful completion"
        );
        assert_eq!(
            unsafe { libc::fcntl(init.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            libc::FD_CLOEXEC,
            "root-side init custody remains owned after incomplete handoff"
        );
    }

    #[test]
    fn outer_process_wait_without_verified_init_never_mints_cleanup_receipt() {
        let child = std::process::Command::new("/bin/true").spawn().unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut service = ServiceChild {
            child,
            init: None,
            shutdown: shutdown.clone(),
        };
        service.child.wait().unwrap();
        drop(service);
        assert!(!shutdown.load(Ordering::Acquire));
        let mut owner = NamespaceOwner::new(
            Arc::new(Authority(AtomicBool::new(true))),
            ServiceCancellation::default(),
        )
        .unwrap();
        owner.shutdown = shutdown;
        assert!(
            owner
                .confirmed_shutdown(
                    &ServiceLeaseKey::default(),
                    Path::new("/fixture"),
                    "fixture.service"
                )
                .is_none()
        );
    }
    #[test]
    fn successful_namespace_exit_waits_for_the_actual_live_descendant() {
        let mut running = Running::start();
        let descendant = running.orphan_handle();
        std::fs::write(running.root.path().join("stop"), b"exit").unwrap();
        let status = running.worker.take().unwrap().join().unwrap().unwrap();
        assert!(status.success());
        assert!(running.owner.quiescent.load(Ordering::Acquire));
        assert!(
            exited(&descendant).unwrap(),
            "kernel namespace teardown must finish the descendant before checkpoint admission"
        );
    }
    #[test]
    fn outer_failure_stops_the_owned_namespace_without_admitting_a_checkpoint() {
        let mut running = Running::start();
        let descendant = running.orphan_handle();
        let outer = i32::try_from(running.outer).unwrap();
        assert_eq!(unsafe { libc::kill(outer, libc::SIGKILL) }, 0);
        let status = running.worker.take().unwrap().join().unwrap().unwrap();
        assert!(!status.success());
        assert!(!running.owner.quiescent.load(Ordering::Acquire));
        let key = ServiceLeaseKey::default();
        let cleanup = running
            .owner
            .confirmed_shutdown(&key, Path::new("/fixture"), "fixture.service")
            .expect("actual failed namespace teardown yields cleanup only");
        assert!(cleanup.matches(&key, Path::new("/fixture"), "fixture.service"));
        assert!(!cleanup.matches(
            &ServiceLeaseKey::default(),
            Path::new("/fixture"),
            "fixture.service"
        ));
        assert!(
            exited(&descendant).unwrap(),
            "outer failure must observe every owned writer stopped before returning"
        );
    }
    #[test]
    fn cancellation_finishes_descendants_before_a_clean_seal() {
        let mut running = Running::start();
        let descendant = running.orphan_handle();
        running.owner.cancellation.cancel();
        let status = running.worker.take().unwrap().join().unwrap().unwrap();
        assert!(status.success());
        assert!(running.owner.quiescent.load(Ordering::Acquire));
        assert!(exited(&descendant).unwrap());
    }
    use tilo_test_env::idle::context_switches;

    const WAITING_PROVIDER: &str =
        "trap 'touch /fixture/termed; exit 0' TERM; @WRITER@ while :; do sleep 0.02; done";

    #[test]
    fn owned_service_supervisor_sleeps_while_child_runs() {
        let mut running = Running::with_script(WAITING_PROVIDER);
        let tid = running.worker_tid.to_string();
        let before = context_switches(&tid);
        let started = Instant::now();
        std::thread::sleep(Duration::from_millis(400));
        let window = started.elapsed();
        let idle = context_switches(&tid).saturating_sub(before);
        // Only the authority recheck may wake it, however long the window
        // really lasted, and a woken thread may be preempted once while it
        // runs; the 10 ms poll this replaces switched about forty times in
        // 400 ms.
        let rechecks = window.as_millis() / AUTHORITY_RECHECK.as_millis() + 1;
        let allowed = u64::try_from(2 * rechecks).unwrap();
        assert!(
            idle <= allowed,
            "idle supervisor woke {idle} times in {window:?}"
        );
        running.owner.cancellation.cancel();
        let termed = running.root.path().join("termed");
        tilo_test_env::wait_until("the cancellation to reach the provider", || termed.exists());
        let status = running.worker.take().unwrap().join().unwrap().unwrap();
        assert!(status.success());
        assert!(running.owner.quiescent.load(Ordering::Acquire));
    }
    #[test]
    #[ignore = "measurement; run with --ignored --nocapture"]
    fn cancellation_and_revocation_latency() {
        let mut running = Running::with_script(WAITING_PROVIDER);
        let tid = running.worker_tid.to_string();
        // Cancel right after a recheck: only the cancellation ring, not the
        // next recheck, can then deliver SIGTERM well inside one period.
        let ticked = context_switches(&tid);
        let deadline = Instant::now().checked_add(Duration::from_secs(2)).unwrap();
        while context_switches(&tid) == ticked && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let cancelled = Instant::now();
        running.owner.cancellation.cancel();
        let termed = running.root.path().join("termed");
        while !termed.exists() && cancelled.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let latency = cancelled.elapsed();
        running.worker.take().unwrap().join().unwrap().unwrap();
        println!("cancellation reached the provider in {latency:?}");
        assert!(
            latency < Duration::from_millis(150),
            "cancellation took {latency:?} to reach the provider"
        );

        let mut running = Running::start();
        let revoked = Instant::now();
        running.authority.0.store(false, Ordering::Release);
        running.worker.take().unwrap().join().unwrap().unwrap();
        println!("revocation stopped the service in {:?}", revoked.elapsed());
        assert!(
            revoked.elapsed() < AUTHORITY_RECHECK + Duration::from_secs(2),
            "revocation took {:?}",
            revoked.elapsed()
        );
    }
    #[test]
    fn revoked_authority_stops_the_service() {
        let mut running = Running::start();
        let descendant = running.orphan_handle();
        running.authority.0.store(false, Ordering::Release);
        let worker = running.worker.take().unwrap();
        tilo_test_env::wait_until("the revoked service to stop", || worker.is_finished());
        let status = worker.join().unwrap().unwrap();
        assert!(status.success());
        assert!(running.owner.quiescent.load(Ordering::Acquire));
        assert!(exited(&descendant).unwrap());
    }
    #[test]
    fn dropping_original_host_lifetime_stops_only_its_private_namespace() {
        let host = Arc::new(());
        let mut running = Running::with_cancellation(
            "trap 'exit 0' TERM; @WRITER@ while :; do sleep 0.02; done",
            ServiceCancellation::for_owner(Arc::downgrade(&host)),
        );
        let descendant = running.orphan_handle();
        drop(host);
        let status = running.worker.take().unwrap().join().unwrap().unwrap();
        assert!(status.success());
        assert!(running.owner.quiescent.load(Ordering::Acquire));
        assert!(exited(&descendant).unwrap());
    }
    #[test]
    fn forced_namespace_shutdown_never_admits_a_checkpoint() {
        let mut running =
            Running::with_script("trap '' TERM; @WRITER@ while :; do sleep 0.02; done");
        let descendant = running.orphan_handle();
        running.owner.cancellation.cancel();
        let status = running.worker.take().unwrap().join().unwrap().unwrap();
        assert!(!status.success());
        assert!(!running.owner.quiescent.load(Ordering::Acquire));
        let key = ServiceLeaseKey::default();
        assert!(
            running
                .owner
                .confirmed_shutdown(&key, Path::new("/fixture"), "fixture.service")
                .is_some()
        );
        assert!(exited(&descendant).unwrap());
    }
    #[test]
    fn owned_endpoint_refuses_another_namespace_peer_and_retirement() {
        let running = Running::start();
        let mut endpoint = running.endpoint();
        let stream = endpoint.connect().unwrap();
        assert!(
            endpoint.connect_pending_worker().is_err(),
            "ordinary fixture has no actual signed retained program admission"
        );
        assert!(
            retained_worker::same_worker_peer(
                &stream,
                &endpoint.custody().unwrap(),
                rustix::process::geteuid().as_raw(),
                rustix::process::getegid().as_raw()
            )
            .is_err(),
            "actual descendant listener cannot substitute for exact fixed-entry init"
        );
        drop(stream);
        let alien_path = running.root.path().join("alien.sock");
        let listener = std::os::unix::net::UnixListener::bind(&alien_path).unwrap();
        std::fs::set_permissions(&alien_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        endpoint.path = alien_path;
        assert_eq!(
            endpoint.connect().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        assert_eq!(
            peer.read(&mut [0; 1]).unwrap(),
            0,
            "substituted peer receives no protocol bytes"
        );
        endpoint.path = running.root.path().join("provider.sock");
        running.owner.cancellation.cancel();
        assert_eq!(
            endpoint.connect().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }
    #[test]
    fn another_lease_cannot_reuse_a_namespace_checkpoint_witness() {
        let key = ServiceLeaseKey::default();
        let exit = QuiescentExit {
            key: key.clone(),
            data_home: "/fixture".into(),
            package: "fixture.service".into(),
        };
        assert!(exit.matches(&key, Path::new("/fixture"), "fixture.service"));
        assert!(!exit.matches(
            &ServiceLeaseKey::default(),
            Path::new("/fixture"),
            "fixture.service"
        ));
        assert!(!exit.matches(&key, Path::new("/other"), "fixture.service"));
        assert!(!exit.matches(&key, Path::new("/fixture"), "other.service"));
    }
}
