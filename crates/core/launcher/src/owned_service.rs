//! Signed services whose successful exit proves their private PID namespace has
//! finished. Ordinary bwrap leader exit is deliberately not this proof.
use crate::run::{EnrolledProcessObserver, LaunchError, LaunchOptions};
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

/// Original host admission/ownership, checked before spawn and while running.
pub trait ServiceAuthority: Send + Sync {
    /// Temporary protected network decision of the original native host.
    fn network_hosts(&self) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Refuses retired original authority.
    fn check(&self) -> io::Result<()>;
}
/// How often a running service's supervisor re-checks [`ServiceAuthority`] and the host lifetime of
/// [`ServiceCancellation::for_owner`].
const AUTHORITY_RECHECK: Duration = Duration::from_millis(250);
/// SIGTERM grace before the private namespace init is killed.
const STOP_GRACE: Duration = Duration::from_secs(3);

/// What woke a supervisor, counted in a test: how it learned to stop is the proof that a
/// cancellation rings it awake and only what cannot ring waits for a recheck, where a time would
/// vary with everything else the machine runs.
#[cfg(test)]
mod woke {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Rechecks: one per [`super::AUTHORITY_RECHECK`], and the first wait's probe.
    pub(super) static RECHECKS: AtomicUsize = AtomicUsize::new(0);
    /// Whether a wait that began with the cancellation already in ran to its timer: the ring
    /// did not wake it.
    pub(super) static CANCEL_WAITED_OUT: AtomicBool = AtomicBool::new(false);

    pub(super) fn recheck() {
        RECHECKS.fetch_add(1, Ordering::AcqRel);
    }

    pub(super) fn waited(cancelled_before: bool, rang: bool) {
        CANCEL_WAITED_OUT.fetch_or(cancelled_before && !rang, Ordering::AcqRel);
    }
}
#[cfg(not(test))]
mod woke {
    pub(super) fn recheck() {}
    pub(super) fn waited(_: bool, _: bool) {}
}
#[derive(Default)]
struct CancelState {
    cancelled: AtomicBool,
    /// Supervisors to ring on [`ServiceCancellation::cancel`]; dead entries are pruned whenever another
    /// supervisor subscribes.
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
    /// Ring `wake` when this cancellation or any parent is cancelled.
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
/// Created only after actual signed launch, successful wait and PID-namespace init exit.
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
/// The installed package selected by the native host, never by an app request.
pub struct ServiceLaunch {
    pub manifest: PathBuf,
    pub payload: PathBuf,
    pub options: LaunchOptions,
}
/// Use the normal signed sandbox pipeline, with the actual provider as PID 1.
pub fn run(
    launch: ServiceLaunch,
    authority: Arc<dyn ServiceAuthority>,
    cancellation: ServiceCancellation,
    key: ServiceLeaseKey,
) -> Result<QuiescentExit, LaunchError> {
    run_connected(launch, authority, cancellation, key, None)
}

/// Connect only to a peer in the verified private namespace of this exact signed launch.
pub fn run_connected(
    launch: ServiceLaunch,
    authority: Arc<dyn ServiceAuthority>,
    cancellation: ServiceCancellation,
    key: ServiceLeaseKey,
    connected: Option<ConnectedService>,
) -> Result<QuiescentExit, LaunchError> {
    let mut options = launch.options;
    configure_service_options(&mut options)?;
    let expected = options
        .expected_package_hash
        .as_deref()
        .ok_or_else(|| LaunchError::InvalidServiceLaunch("original installation missing".into()))?;
    let manifest = crate::run::verified_original_manifest(
        &launch.manifest,
        &launch.payload,
        &options,
        expected,
    )?;
    validate_service_manifest(&options, &manifest)?;
    let data_home = options
        .data_home
        .clone()
        .ok_or_else(|| LaunchError::InvalidServiceLaunch("private working lease missing".into()))?;
    let mut owner = NamespaceOwner::new(authority, cancellation)?;
    owner.endpoint = crate::appdir::host_app_dir(
        &data_home,
        crate::appdir::PERSONAL_WORKSPACE,
        &manifest.package_id,
    )
    .join("provider.sock");
    owner.connected = Mutex::new(connected);
    // Even errors return only after canonical ServiceChild teardown.
    let status =
        crate::run::launch_with_context(&manifest, &launch.payload, &options, Some(&owner))?;
    if !status.success() || !owner.quiescent.load(Ordering::Acquire) {
        return Err(LaunchError::InvalidServiceLaunch(
            "provider exit was not a clean checkpoint".into(),
        ));
    }
    Ok(QuiescentExit {
        key,
        data_home,
        package: manifest.package_id,
    })
}

fn configure_service_options(options: &mut LaunchOptions) -> Result<(), LaunchError> {
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
        return Err(LaunchError::InvalidServiceLaunch(
            "service ownership options changed".into(),
        ));
    }
    options.bwrap_bin = Some("/usr/bin/bwrap".into());
    // This API is called in-process by the shell, whose executable deliberately refuses shim mode.
    options.sandbox_shim_bin = Some("/usr/libexec/tilo/tilo-launcher".into());
    Ok(())
}

fn validate_service_manifest(
    options: &LaunchOptions,
    manifest: &crate::manifest::ManifestView,
) -> Result<(), LaunchError> {
    if options.app_id.as_deref() != Some(manifest.package_id.as_str()) {
        return Err(LaunchError::InvalidServiceLaunch(
            "owned service identity changed".into(),
        ));
    }
    if manifest.sandbox.net.mode != "off" || !manifest.sandbox.net.hosts.is_empty() {
        return Err(LaunchError::InvalidServiceLaunch(
            "owned service egress requires protected native review".into(),
        ));
    }
    Ok(())
}

pub type ConnectedService = Box<dyn FnOnce(ServiceEndpoint) -> io::Result<()> + Send>;

/// Host-only endpoint pinned to the actual private namespace and lease path.
pub struct ServiceEndpoint {
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
pub struct ServiceOwnership {
    pub init_pid: u32,
    pub start_ticks: u64,
    pub namespace_dev: u64,
    pub namespace_ino: u64,
}
impl ServiceEndpoint {
    /// Observe exact retained init pidfd readiness even after cancellation or authority revocation.
    pub fn observed_stopped(&self) -> io::Result<bool> {
        if !rustix::process::getuid().is_root() || !rustix::process::geteuid().is_root() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        exited(&self.init)
    }
    /// Observe the exact live namespace before issuing any provider command.
    pub fn ownership(&self) -> io::Result<ServiceOwnership> {
        self.authority.check()?;
        if self.cancellation.is_cancelled() || exited(&self.init)? {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let held = self.namespace.metadata()?;
        // PID namespace membership belongs to this exact process for its lifetime.
        let start_ticks =
            tilo_process::start_time(self.init_pid).ok_or(io::ErrorKind::InvalidData)?;
        self.authority.check()?;
        if self.cancellation.is_cancelled() || exited(&self.init)? {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok(ServiceOwnership {
            init_pid: self.init_pid,
            start_ticks,
            namespace_dev: held.dev(),
            namespace_ino: held.ino(),
        })
    }
    /// Open a separate authenticated stream to this one provider.
    pub fn connect(&self) -> io::Result<UnixStream> {
        self.authority.check()?;
        if self.cancellation.is_cancelled() || exited(&self.init)? {
            return Err(io::ErrorKind::Interrupted.into());
        }
        // AF_UNIX has a tiny address field.
        let parent = self.path.parent().ok_or(io::ErrorKind::InvalidInput)?;
        let name = self.path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        let directory = File::options()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(parent)?;
        let socket = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(name);
        let metadata = std::fs::symlink_metadata(&socket)?;
        // Privacy is checked against the exact worker owner.
        if !metadata.file_type().is_socket()
            || metadata.uid() != self.expected_uid
            || !tilo_files::native::private::is_owner_only(&metadata)
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let stream = tilo_files::native::socket::connect(&socket)?;
        let facts = peer_facts::facts(&stream)?;
        let Some(pid) = facts.pid.filter(|pid| *pid > 1) else {
            return Err(io::ErrorKind::PermissionDenied.into());
        };
        if facts.uid != self.expected_uid {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        // The live init pidfd the endpoint keeps prevents reuse of this exact PID.
        if pid != self.init_pid {
            let peer = File::open(format!("/proc/{pid}/ns/pid"))?.metadata()?;
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
}
impl NamespaceOwner {
    fn new(
        authority: Arc<dyn ServiceAuthority>,
        cancellation: ServiceCancellation,
    ) -> io::Result<Self> {
        let mut fds = [0; 2];
        // Each end is exclusively owned here; the launcher duplicates only the writer into bwrap,
        // which closes it before entering the provider.
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
        })
    }
    fn before_connection(&self) -> io::Result<()> {
        self.authority.check()?;
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
                path: self.endpoint.clone(),
                namespace,
                init,
                init_pid,
                expected_uid: rustix::process::geteuid().as_raw(),
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
        Ok(Some(self.authority.network_hosts()?))
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
        // Sleep on the outer child's exit and on cancellation; wake on a timer only to re-check what
        // cannot ring (AUTHORITY_RECHECK).
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
            let cancelled_before = ring.is_some() && self.cancellation.is_cancelled();
            let woken_by_ring =
                match bounded_command::wait_child_or(&mut owned.child, ring, deadline)? {
                    bounded_command::Wakeup::Exited(status) => {
                        let init = owned.init.as_ref().ok_or(io::ErrorKind::InvalidData)?;
                        if status.success() && !forced && exited(init)? {
                            self.quiescent.store(true, Ordering::Release);
                            self.shutdown.store(true, Ordering::Release);
                            owned.init.take();
                        }
                        // An outer failure is not proof that the private namespace stopped.
                        return Ok(status);
                    }
                    // Drain before checking, so a cancel racing the check rings again.
                    bounded_command::Wakeup::Woken => {
                        wake.drain();
                        true
                    }
                    bounded_command::Wakeup::Deadline => false,
                };
            woke::waited(cancelled_before, woken_by_ring);
            let now = Instant::now();
            if stopping.is_none() && now >= recheck {
                woke::recheck();
                recheck = now
                    .checked_add(AUTHORITY_RECHECK)
                    .ok_or(io::ErrorKind::InvalidData)?;
            }
            if stopping.is_none()
                && (self.cancellation.is_cancelled() || self.authority.check().is_err())
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
                "private namespace proof differs: pid={pid} parent={parent} actual_parent={actual_parent:?} nspid={nspid:?}"
            ),
        ));
    }
    Ok(())
}
fn exited(handle: &OwnedFd) -> io::Result<bool> {
    crate::fd::pidfd_exited(handle)
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
            // The privately owned outer group covers setup failure before an init pidfd could be
            // acquired.
            bounded_command::terminate_tree(&mut self.child, Duration::ZERO);
        }
        if let Some(init) = &self.init {
            // This destructor runs on the canonical owning service worker.
            let started = Instant::now();
            let mut reported = false;
            loop {
                match exited(init) {
                    Ok(true) => {
                        self.shutdown.store(true, Ordering::Release);
                        break;
                    }
                    Err(error) if !reported => {
                        tracing::error!(%error, "owned namespace exit observation failed; retaining ownership");
                        reported = true;
                    }
                    Ok(false) | Err(_) => {}
                }
                if !reported && started.elapsed() >= Duration::from_secs(3) {
                    tracing::error!(
                        "owned namespace cleanup timed out; retaining ownership until kernel exit"
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
    use tilo_test_env::SpawnUnderEnvLock as _;
    tilo_test_env::scenario! {
        fn registration_and_cancellation_bind_the_exact_namespace;

        phase stopping_enrollment_child_preserves_parent_but_parent_retirement_stops_new_children;
        {
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

        phase outer_process_wait_without_verified_init_never_mints_cleanup_receipt;
        {
            let child = std::process::Command::new("/bin/true")
                .spawn_locked()
                .unwrap();
            let shutdown = Arc::new(AtomicBool::new(false));
            let mut service = ServiceChild {
                child,
                init: None,
                shutdown: shutdown.clone(),
            };
            service.child.wait().unwrap();
            drop(service);
            assert!(!shutdown.load(Ordering::Acquire));
        }
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
            // Qualify the real kernel/production namespace owner without an unsigned package or
            // synthetic runtime becoming a trusted launch.
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
            let child = command.spawn_locked().unwrap();
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
            // Observe the production-authenticated writer directly.
            let stream = self.endpoint.connect().unwrap();
            let writer = peer_facts::facts(&stream)
                .unwrap()
                .pid
                .expect("the kernel names the authenticated writer");
            // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor or -1.
            let raw = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_open,
                    libc::pid_t::try_from(writer).unwrap(),
                    0,
                )
            };
            assert!(raw >= 0, "the authenticated live writer must have a pidfd");
            let fd = unsafe { OwnedFd::from_raw_fd(i32::try_from(raw).unwrap()) };
            let status = std::fs::read_to_string(format!("/proc/{writer}/status")).unwrap();
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
    use tilo_test_env::idle::{context_switches, voluntary_switches};

    const WAITING_PROVIDER: &str =
        "trap 'touch /fixture/termed; exit 0' TERM; @WRITER@ while :; do sleep 0.02; done";

    #[test]
    fn owned_service_supervisor_sleeps_while_child_runs() {
        let mut running = Running::with_script(WAITING_PROVIDER);
        let tid = running.worker_tid.to_string();
        let before = voluntary_switches(&tid);
        let started = Instant::now();
        std::thread::sleep(Duration::from_millis(400));
        let window = started.elapsed();
        // A wake the supervisor asked for is a voluntary switch; a preemption a loaded node
        // imposes is not a wake. Only the authority recheck may wake it, however long the window
        // really lasted, and a woken thread may be preempted once while it runs; the 10 ms poll
        // this replaces switched about forty times in 400 ms.
        let idle = voluntary_switches(&tid).saturating_sub(before);
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
    /// A cancellation rings the supervisor awake and reaches the provider without waiting for the
    /// next recheck; a revocation, which cannot ring, stops the service at the recheck after it.
    /// Counted by what woke the supervisor, not timed.
    #[test]
    fn cancellation_rings_and_revocation_waits_one_recheck() {
        let mut running = Running::with_script(WAITING_PROVIDER);
        let tid = running.worker_tid.to_string();
        // Cancel right after a recheck, so the ring and the next recheck cannot coincide.
        let ticked = context_switches(&tid);
        let deadline = Instant::now().checked_add(Duration::from_secs(2)).unwrap();
        while context_switches(&tid) == ticked && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        running.owner.cancellation.cancel();
        let termed = running.root.path().join("termed");
        tilo_test_env::wait_until("the cancellation to reach the provider", || termed.exists());
        running.worker.take().unwrap().join().unwrap().unwrap();
        // Seen in the pass under way, or rung awake: never slept out to a timer.
        assert!(
            !woke::CANCEL_WAITED_OUT.load(Ordering::Acquire),
            "the cancellation waited for a recheck to reach the provider"
        );

        let mut running = Running::start();
        let before = woke::RECHECKS.load(Ordering::Acquire);
        running.authority.0.store(false, Ordering::Release);
        running.worker.take().unwrap().join().unwrap().unwrap();
        let rechecks = woke::RECHECKS
            .load(Ordering::Acquire)
            .saturating_sub(before);
        // The recheck under way when it was revoked, and the one after it.
        assert!(
            rechecks <= 2,
            "the revocation took {rechecks} rechecks to stop the service"
        );
    }
    tilo_test_env::scenario! {
        fn an_owned_namespace_stops_with_its_owner_and_seals_only_when_clean;

        phase successful_namespace_exit_waits_for_the_actual_live_descendant;
        {
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

        phase outer_failure_stops_the_owned_namespace_without_admitting_a_checkpoint;
        {
            let mut running = Running::start();
            let descendant = running.orphan_handle();
            let outer = i32::try_from(running.outer).unwrap();
            assert_eq!(unsafe { libc::kill(outer, libc::SIGKILL) }, 0);
            let status = running.worker.take().unwrap().join().unwrap().unwrap();
            assert!(!status.success());
            assert!(!running.owner.quiescent.load(Ordering::Acquire));
            assert!(
                running.owner.shutdown.load(Ordering::Acquire),
                "actual failed namespace teardown is observed"
            );
            assert!(
                exited(&descendant).unwrap(),
                "outer failure must observe every owned writer stopped before returning"
            );
        }

        phase cancellation_finishes_descendants_before_a_clean_seal;
        {
            let mut running = Running::start();
            let descendant = running.orphan_handle();
            running.owner.cancellation.cancel();
            let status = running.worker.take().unwrap().join().unwrap().unwrap();
            assert!(status.success());
            assert!(running.owner.quiescent.load(Ordering::Acquire));
            assert!(exited(&descendant).unwrap());
        }

        phase revoked_authority_stops_the_service;
        {
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

        phase dropping_original_host_lifetime_stops_only_its_private_namespace;
        {
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
        assert!(running.owner.shutdown.load(Ordering::Acquire));
        assert!(exited(&descendant).unwrap());
    }
    tilo_test_env::scenario! {
        fn an_owned_endpoint_and_its_checkpoint_belong_to_one_lease;

        phase owned_endpoint_refuses_another_namespace_peer_and_retirement;
        {
            let running = Running::start();
            let mut endpoint = running.endpoint();
            let stream = endpoint.connect().unwrap();
            endpoint.ownership().unwrap();
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

        phase another_lease_cannot_reuse_a_namespace_checkpoint_proof;
        {
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
}
