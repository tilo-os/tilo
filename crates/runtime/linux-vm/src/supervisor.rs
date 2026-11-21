//! The process that keeps one VM awake, and puts it to sleep. `tilo-linux-vm` starts
//! it on demand (`tilo-linux-vm serve`); it holds the VM's lock, owns the host halves
//! of its devices, carries out the [`Lifecycle`]'s actions and exits once the VM is
//! on disk.

use std::collections::BTreeMap;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use libvirt_domain::{Connection, DomainState, Virsh, VirshError, vsock_cid};
use linux_vm_protocol::client;
use linux_vm_protocol::link::AgentLink;
use linux_vm_protocol::{AGENT_PORT, GuestMessage, GuestShare, HostMessage};
use thiserror::Error;

use crate::config::VmConfig;
use crate::control::{ControlReply, ControlRequest, FRAMING, StatusReport};
use crate::domain::{
    attached_tags, domain_uuid, filesystem_xml, is_ours, render_domain_xml, with_uuid,
};
use crate::grants::{Grants, HostShare};
use crate::lifecycle::{Action, Lifecycle, Phase};
use crate::memory::actual_mib;
use crate::peer::same_user;
use crate::transport::{VsockBackend, hybrid_link, vhost_device_vsock_argv};
use crate::{image, virtiofs};

/// How often a running VM is asked what runs and how much memory it has.
const POLL: Duration = Duration::from_secs(5);
/// How long a first boot or restore may take before the agent answers.
const AGENT_PATIENCE: Duration = Duration::from_secs(300);
/// A supervisor nobody asked anything exits after this.
const UNUSED: Duration = Duration::from_secs(60);

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error(transparent)]
    Virsh(#[from] VirshError),
    #[error(transparent)]
    Image(#[from] image::ImageError),
    #[error(transparent)]
    Lifecycle(#[from] crate::lifecycle::LifecycleError),
    #[error(transparent)]
    Domain(#[from] crate::domain::DomainError),
    #[error("a libvirt domain named {0} exists and is not Tilo's; rename or remove it")]
    ForeignDomain(String),
    #[error("the VM has no vsock context id")]
    NoContextId,
    #[error("this host has no vsock for VMs: it needs /dev/vhost-vsock or vhost-device-vsock")]
    NoVsock,
    #[error("vhost-device-vsock did not come up")]
    VsockDaemon,
    #[error("the guest agent did not answer within {0:?}")]
    AgentTimeout(Duration),
    #[error("guest agent: {0}")]
    Agent(#[from] linux_vm_protocol::ProtocolError),
    #[error("virtiofsd for {0} did not come up")]
    ShareDaemon(PathBuf),
    #[error("the VM's disk does not exist and no guest image is available: {0}")]
    NoImage(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

struct ShareDaemon {
    share: HostShare,
    daemon: Child,
}

pub struct Supervisor {
    config: VmConfig,
    virsh: Virsh,
    life: Lifecycle,
    /// A local qcow2 to create the disk from instead of the pinned image.
    image_override: Option<PathBuf>,
    self_exe: PathBuf,
    backend: Option<VsockBackend>,
    link: Option<AgentLink>,
    /// The window forwarder's app keeper, and the keeper's directory in the keepers' registry.
    forwarder: Option<(Child, PathBuf)>,
    /// `vhost-device-vsock`, when the host has no kernel vhost-vsock.
    vsock_daemon: Option<Child>,
    shares: BTreeMap<String, ShareDaemon>,
    /// Directories a launch asked to share until the VM next sleeps.
    session_shares: Vec<HostShare>,
    memory_mib: Option<u64>,
    running: Option<u32>,
}

impl Supervisor {
    #[must_use]
    pub fn new(config: VmConfig, image_override: Option<PathBuf>, self_exe: PathBuf) -> Self {
        Self {
            virsh: Virsh::new(Connection::Session, config.domain_name.clone()),
            life: Lifecycle::new(config.idle_suspend.as_secs()),
            config,
            image_override,
            self_exe,
            backend: VsockBackend::detect(),
            link: None,
            forwarder: None,
            vsock_daemon: None,
            shares: BTreeMap::new(),
            session_shares: Vec::new(),
            memory_mib: None,
            running: None,
        }
    }

    /// Serve until the VM is asleep or gone. Returns at once when another
    /// supervisor holds the VM.
    pub fn run(mut self) -> Result<(), SupervisorError> {
        let paths = self.config.paths.clone();
        std::fs::create_dir_all(&paths.runtime)?;
        let Ok(_lock) = advisory_lock::FileLock::try_open(
            &paths.supervisor_lock(),
            advisory_lock::LockMode::Exclusive,
        ) else {
            return Ok(());
        };
        let socket = paths.supervisor_socket();
        let _ = std::fs::remove_file(&socket);
        let listener = tilo_files::native::socket::bind(&socket)?;
        let (requests, incoming) = mpsc::channel::<(ControlRequest, UnixStream)>();
        std::thread::spawn(move || accept_loop(&listener, &requests));

        self.observe()?;
        let started = Instant::now();
        let mut last_poll = Instant::now();
        let mut asked = false;
        loop {
            match incoming.recv_timeout(POLL) {
                Ok((request, mut stream)) => {
                    asked = true;
                    let reply = self.handle(request);
                    let _ = FRAMING.write_value(&mut stream, &reply);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if last_poll.elapsed() >= POLL {
                last_poll = Instant::now();
                self.poll();
            }
            match self.life.phase() {
                Phase::Suspended | Phase::Off | Phase::NotCreated
                    if asked || started.elapsed() >= UNUSED =>
                {
                    break;
                }
                _ => {}
            }
        }
        self.stop_devices();
        let _ = std::fs::remove_file(&socket);
        Ok(())
    }

    fn observe(&mut self) -> Result<Phase, SupervisorError> {
        let domain = self.virsh.state()?;
        if domain != DomainState::Absent && !is_ours(&self.virsh.dumpxml()?, &self.config) {
            return Err(SupervisorError::ForeignDomain(
                self.config.domain_name.clone(),
            ));
        }
        let managed = domain != DomainState::Absent && self.virsh.has_managed_save()?;
        Ok(self
            .life
            .observe(domain, managed, self.config.paths.disk().is_file())?)
    }

    fn handle(&mut self, request: ControlRequest) -> ControlReply {
        let result = match request {
            ControlRequest::Ensure { extra } => {
                self.ensure(&extra).map(|link| ControlReply::Ready {
                    link,
                    waypipe_port: self.config.waypipe_port,
                })
            }
            ControlRequest::Suspend => match self.life.suspend_now() {
                Some(actions) => self.execute(&actions).map(|()| ControlReply::Suspending),
                None => Ok(ControlReply::Suspending),
            },
            ControlRequest::Status => Ok(ControlReply::Status(StatusReport {
                phase: format!("{:?}", self.life.phase()),
                transport: self
                    .backend
                    .map_or("none", VsockBackend::as_str)
                    .to_string(),
                link: self.link.clone(),
                shares: self
                    .shares
                    .values()
                    .map(|share| share.share.path.display().to_string())
                    .collect(),
                memory_mib: self.memory_mib,
                running: self.running,
            })),
        };
        result.unwrap_or_else(|error| {
            // A half-done wake must not leave the lifecycle believing in a
            // phase libvirt does not show.
            let _ = self.observe();
            ControlReply::Error(error.to_string())
        })
    }

    fn ensure(&mut self, extra: &[PathBuf]) -> Result<AgentLink, SupervisorError> {
        for path in extra {
            let share = HostShare {
                path: path.clone(),
                read_only: false,
            };
            if linux_vm_protocol::share_path_allowed(path) && !self.session_shares.contains(&share)
            {
                self.session_shares.push(share);
            }
        }
        if matches!(self.life.phase(), Phase::Starting | Phase::Running) {
            // Someone may have stopped the VM behind our back.
            self.observe()?;
        }
        let actions = self.life.wake()?;
        self.execute(&actions)?;
        self.link.clone().ok_or(SupervisorError::NoContextId)
    }

    fn execute(&mut self, actions: &[Action]) -> Result<(), SupervisorError> {
        for action in actions {
            match action {
                Action::CreateDisk => self.create_disk()?,
                Action::Define => self.define()?,
                Action::Start => {
                    self.link = None;
                    self.start_vsock_daemon()?;
                    self.start_forwarder();
                    self.virsh.start()?;
                }
                Action::AwaitAgent => {
                    self.await_agent()?;
                    self.life.agent_ready();
                }
                Action::SyncShares => self.sync_shares()?,
                Action::DetachShares => self.detach_all(),
                Action::Suspend => {
                    if self.backend.is_some_and(VsockBackend::snapshots) {
                        self.virsh.managed_save()?;
                    } else {
                        self.power_off()?;
                    }
                    self.life.suspended();
                    self.link = None;
                    self.stop_devices();
                }
            }
        }
        Ok(())
    }

    fn create_disk(&self) -> Result<(), SupervisorError> {
        let base = if let Some(base) = &self.image_override {
            base.clone()
        } else {
            let reference = image::current_ref().ok_or_else(|| {
                SupervisorError::NoImage(image::ImageError::NotPublished.to_string())
            })?;
            image::ensure_base(&self.config.paths.images, &reference)?
        };
        image::create_disk(&base, &self.config.paths.disk())?;
        Ok(())
    }

    fn define(&self) -> Result<(), SupervisorError> {
        let backend = self.backend.ok_or(SupervisorError::NoVsock)?;
        let mut xml = render_domain_xml(&self.config, backend)?;
        if self.virsh.state()? != DomainState::Absent {
            let existing = self.virsh.dumpxml()?;
            if !is_ours(&existing, &self.config) {
                return Err(SupervisorError::ForeignDomain(
                    self.config.domain_name.clone(),
                ));
            }
            // Redefining keeps the domain's identity; libvirt refuses a new
            // definition under the same name with another UUID.
            if let Some(uuid) = domain_uuid(&existing) {
                xml = with_uuid(&xml, uuid);
            }
        }
        self.virsh.define(&xml)?;
        Ok(())
    }

    fn await_agent(&mut self) -> Result<(), SupervisorError> {
        let deadline = Instant::now().checked_add(AGENT_PATIENCE);
        loop {
            if self.link.is_none() {
                self.link = match self.backend.ok_or(SupervisorError::NoVsock)? {
                    VsockBackend::Kernel => {
                        vsock_cid(&self.virsh.dumpxml()?).map(|cid| AgentLink::Kernel { cid })
                    }
                    VsockBackend::Hybrid => Some(hybrid_link(&self.config.paths.vsock_uds())),
                };
            }
            if let Some(link) = &self.link
                && let Ok(mut stream) = link.connect(AGENT_PORT)
            {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                if client::handshake(&mut stream).is_ok() {
                    return Ok(());
                }
            }
            if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
                return Err(SupervisorError::AgentTimeout(AGENT_PATIENCE));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn agent(&self, message: &HostMessage) -> Result<GuestMessage, SupervisorError> {
        let link = self.link.as_ref().ok_or(SupervisorError::NoContextId)?;
        let mut stream = link.connect(AGENT_PORT)?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        Ok(client::request(&mut stream, message)?)
    }

    fn desired_shares(&self) -> Vec<HostShare> {
        let now = tilo_clock::unix_now();
        let mut desired = Grants::load(&self.config.paths.grants()).shares(Some(now));
        for share in &self.session_shares {
            if !desired.iter().any(|granted| granted.path == share.path) {
                desired.push(share.clone());
            }
        }
        desired.retain(|share| share.path.is_dir());
        desired.sort();
        desired
    }

    fn sync_shares(&mut self) -> Result<(), SupervisorError> {
        let desired = self.desired_shares();
        let live = attached_tags(&self.virsh.dumpxml()?);
        for share in &desired {
            let tag = share.tag();
            if self
                .shares
                .get(&tag)
                .is_some_and(|daemon| daemon.share == *share)
                && live.contains(&tag)
            {
                continue;
            }
            self.detach(&tag);
            self.attach(share)?;
        }
        let guest: Vec<GuestShare> = desired.iter().map(HostShare::guest).collect();
        if !matches!(
            self.agent(&HostMessage::Mount { shares: guest })?,
            GuestMessage::Mounted { .. }
        ) {
            return Err(linux_vm_protocol::ProtocolError::Unexpected.into());
        }
        let stale: Vec<String> = self
            .shares
            .keys()
            .filter(|tag| !desired.iter().any(|share| share.tag() == **tag))
            .cloned()
            .collect();
        for tag in stale {
            self.detach(&tag);
        }
        Ok(())
    }

    fn attach(&mut self, share: &HostShare) -> Result<(), SupervisorError> {
        let tag = share.tag();
        let socket = self.config.paths.share_socket(&tag);
        let _ = std::fs::remove_file(&socket);
        let daemon = virtiofs::virtiofsd_argv(
            virtiofs::VIRTIOFSD,
            share,
            &socket,
            self.config.guest_uid,
            self.config.host_uid,
            self.config.host_gid,
        );
        let argv = virtiofs::confined_argv(&self.self_exe, share, &socket, daemon);
        let (program, rest) = argv.split_first().ok_or(SupervisorError::NoContextId)?;
        let mut daemon = Command::new(program)
            .args(rest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?;
        if !wait_for_socket(&socket, &mut daemon) {
            let _ = daemon.kill();
            let _ = daemon.wait();
            return Err(SupervisorError::ShareDaemon(share.path.clone()));
        }
        if let Err(error) = self
            .virsh
            .attach_device(&filesystem_xml(share, &socket)?, &["--live"])
        {
            let _ = daemon.kill();
            let _ = daemon.wait();
            return Err(error.into());
        }
        self.shares.insert(
            tag,
            ShareDaemon {
                share: share.clone(),
                daemon,
            },
        );
        Ok(())
    }

    fn detach(&mut self, tag: &str) {
        let Some(mut daemon) = self.shares.remove(tag) else {
            return;
        };
        let socket = self.config.paths.share_socket(tag);
        if let Ok(xml) = filesystem_xml(&daemon.share, &socket) {
            let _ = self.virsh.detach_device(&xml, &["--live"]);
        }
        let _ = daemon.daemon.kill();
        let _ = daemon.daemon.wait();
        let _ = std::fs::remove_file(socket);
    }

    fn detach_all(&mut self) {
        let _ = self.agent(&HostMessage::Mount { shares: Vec::new() });
        let tags: Vec<String> = self.shares.keys().cloned().collect();
        for tag in tags {
            self.detach(&tag);
        }
        self.session_shares.clear();
    }

    /// One host `waypipe client` for every window this VM opens, behind an app keeper: the
    /// session finds the keeper in its registry and shows it, and the guest's windows stay open
    /// while the desktop restarts, named `vm.<guest>.<app id>`.
    #[cfg(target_os = "linux")]
    fn start_forwarder(&mut self) {
        if let Some((forwarder, dir)) = self.forwarder.as_mut() {
            if matches!(forwarder.try_wait(), Ok(None)) {
                return;
            }
            remove_keeper_dir(dir);
            self.forwarder = None;
        }
        let Some(keeper) = tilo_app_keeper::registry::find_keeper() else {
            eprintln!("tilo-linux-vm: no app keeper is installed; windows will not appear");
            return;
        };
        let port = self.config.waypipe_port;
        let waypipe = if self.backend == Some(VsockBackend::Hybrid) {
            let listener = AgentLink::hybrid_listener(&self.config.paths.vsock_uds(), port);
            let _ = std::fs::remove_file(&listener);
            linux_vm_protocol::waypipe::client_argv_unix("waypipe", &listener)
        } else {
            linux_vm_protocol::waypipe::client_argv("waypipe", port)
        };
        let launch = forwarder_launch(self.config.guest);
        let dir =
            match tilo_app_keeper::registry::new_dir(&tilo_app_keeper::registry::root(), &launch) {
                Ok(dir) => dir,
                Err(error) => {
                    eprintln!("tilo-linux-vm: no keeper directory for the windows: {error}");
                    return;
                }
            };
        let argv = forwarder_argv(&keeper, &dir, &launch, self.config.guest, &waypipe);
        let Some((program, rest)) = argv.split_first() else {
            return;
        };
        match Command::new(program)
            .args(rest)
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
        {
            Ok(child) => self.forwarder = Some((child, dir)),
            Err(error) => {
                eprintln!("tilo-linux-vm: could not start the window forwarder: {error}");
                remove_keeper_dir(&dir);
            }
        }
    }

    /// App keepers, and so a VM's windows, are Linux's.
    #[cfg(not(target_os = "linux"))]
    #[expect(clippy::unused_self, reason = "the Linux supervisor's signature")]
    fn start_forwarder(&mut self) {}

    fn poll(&mut self) {
        if self.life.phase() != Phase::Running {
            return;
        }
        if !matches!(self.virsh.state(), Ok(DomainState::Running)) {
            let _ = self.observe();
            return;
        }
        self.start_forwarder();
        let Ok(GuestMessage::Health(health)) = self.agent(&HostMessage::Health) else {
            return;
        };
        self.running = Some(health.running);
        if let Ok(stats) = self.virsh.memory_stats()
            && let Some(current) = actual_mib(&stats)
        {
            self.memory_mib = Some(current);
            if let Some(target) = self
                .config
                .memory
                .next_target(current, health.mem_available_kib / 1024)
            {
                let _ = self.virsh.set_memory_mib(target);
            }
        }
        if let Some(actions) = self.life.activity(health.running, tilo_clock::unix_now())
            && let Err(error) = self.execute(&actions)
        {
            eprintln!("tilo-linux-vm: could not suspend: {error}");
            let _ = self.observe();
        }
    }

    fn stop_devices(&mut self) {
        let tags: Vec<String> = self.shares.keys().cloned().collect();
        for tag in tags {
            if let Some(mut daemon) = self.shares.remove(&tag) {
                let _ = daemon.daemon.kill();
                let _ = daemon.daemon.wait();
            }
        }
        if let Some((mut forwarder, dir)) = self.forwarder.take() {
            let _ = forwarder.kill();
            let _ = forwarder.wait();
            remove_keeper_dir(&dir);
        }
        if let Some(mut daemon) = self.vsock_daemon.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
    }

    /// QEMU connects to the daemon, so start it first.
    fn start_vsock_daemon(&mut self) -> Result<(), SupervisorError> {
        if self.backend.ok_or(SupervisorError::NoVsock)? != VsockBackend::Hybrid {
            return Ok(());
        }
        if let Some(daemon) = self.vsock_daemon.as_mut()
            && matches!(daemon.try_wait(), Ok(None))
        {
            return Ok(());
        }
        let paths = &self.config.paths;
        let vhost_user = paths.vsock_vhost_user();
        let _ = std::fs::remove_file(&vhost_user);
        let _ = std::fs::remove_file(paths.vsock_uds());
        let argv = vhost_device_vsock_argv(&vhost_user, &paths.vsock_uds());
        let (program, rest) = argv.split_first().ok_or(SupervisorError::VsockDaemon)?;
        let mut daemon = Command::new(program)
            .args(rest)
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()?;
        if !wait_for_socket(&vhost_user, &mut daemon) {
            let _ = daemon.kill();
            let _ = daemon.wait();
            return Err(SupervisorError::VsockDaemon);
        }
        self.vsock_daemon = Some(daemon);
        Ok(())
    }

    /// Shut the guest down, cleanly when it lets us within a minute.
    fn power_off(&self) -> Result<(), SupervisorError> {
        if self.virsh.state()? != DomainState::Running {
            return Ok(());
        }
        self.virsh.shutdown()?;
        for _ in 0..120 {
            if self.virsh.state()? != DomainState::Running {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        self.virsh.destroy()?;
        Ok(())
    }
}

fn accept_loop(listener: &UnixListener, requests: &mpsc::Sender<(ControlRequest, UnixStream)>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        if !same_user(&stream) {
            continue;
        }
        if let Ok(Some(request)) = FRAMING.read_value::<_, ControlRequest>(&mut stream)
            && requests.send((request, stream)).is_err()
        {
            return;
        }
    }
}

fn wait_for_socket(socket: &Path, daemon: &mut Child) -> bool {
    for _ in 0..100 {
        if socket.exists() {
            return true;
        }
        if !matches!(daemon.try_wait(), Ok(None)) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// What the session is told of the VM's window forwarder: a service's launch, which the session
/// shows and never ends; the VM, not one of its apps.
#[cfg(target_os = "linux")]
fn forwarder_launch(guest: linux_vm_protocol::GuestOs) -> tilo_app_keeper::registry::KeptLaunch {
    tilo_app_keeper::registry::KeptLaunch {
        version: tilo_app_keeper::registry::LAUNCH_VERSION,
        app: format!("vm.{}", guest.as_str()),
        title: format!("{} apps", guest.label()),
        workspace: String::new(),
        asked: None,
        window_identities: Vec::new(),
        service: true,
    }
}

/// `keeper` keeping `waypipe` (the forwarder's own argv) in `dir`, every app id it shows prefixed
/// with the guest's.
#[cfg(target_os = "linux")]
fn forwarder_argv(
    keeper: &Path,
    dir: &Path,
    launch: &tilo_app_keeper::registry::KeptLaunch,
    guest: linux_vm_protocol::GuestOs,
    waypipe: &[String],
) -> Vec<std::ffi::OsString> {
    let mut argv: Vec<std::ffi::OsString> = vec![
        keeper.into(),
        "--dir".into(),
        dir.into(),
        "--launch".into(),
        launch.encode().into(),
        "--app-id-prefix".into(),
        linux_vm_protocol::window::host_app_id_prefix(guest).into(),
        "--".into(),
    ];
    argv.extend(waypipe.iter().map(Into::into));
    argv
}

/// A keeper directory this supervisor made and is done with.
fn remove_keeper_dir(dir: &Path) {
    if let Err(error) = std::fs::remove_dir_all(dir)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("tilo-linux-vm: {} stays: {error}", dir.display());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use linux_vm_protocol::GuestOs;

    tilo_test_env::scenario! {
        fn the_window_forwarder_runs_behind_an_app_keeper;

        phase the_keeper_keeps_waypipe_and_names_the_guests_windows;
        {
            let waypipe = linux_vm_protocol::waypipe::client_argv("waypipe", 4000);
            let launch = forwarder_launch(GuestOs::Linux);
            let argv = forwarder_argv(
                Path::new("/usr/libexec/tilo-app-keeper"),
                Path::new("/run/user/1000/tilo-session/kept/abc"),
                &launch,
                GuestOs::Linux,
                &waypipe,
            );
            let argv: Vec<String> = argv.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
            assert_eq!(
                argv.get(..8),
                Some(
                    &[
                        "/usr/libexec/tilo-app-keeper".to_owned(),
                        "--dir".to_owned(),
                        "/run/user/1000/tilo-session/kept/abc".to_owned(),
                        "--launch".to_owned(),
                        launch.encode(),
                        "--app-id-prefix".to_owned(),
                        "vm.linux.".to_owned(),
                        "--".to_owned(),
                    ][..]
                )
            );
            assert_eq!(argv.get(8..), Some(&waypipe[..]), "then waypipe's own command");
        }

        phase the_session_shows_it_and_leaves_it_to_the_supervisor;
        {
            let launch = forwarder_launch(GuestOs::Linux);
            assert!(launch.service, "the session never ends it");
            assert!(launch.workspace.is_empty(), "the VM serves every workspace");
            let read = tilo_app_keeper::registry::KeptLaunch::decode(&launch.encode());
            assert_eq!(read, Some(launch), "the session reads what the keeper is handed");
        }
    }
}
