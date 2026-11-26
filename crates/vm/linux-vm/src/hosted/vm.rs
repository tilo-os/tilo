//! One hosted VM: woken for a command, snapshotted when idle, metered. The
//! [`Lifecycle`] decides, as it does for the Linux VM, and this carries its
//! actions out with Firecracker; every moment its process runs is metered.

use std::fs::File;
use std::os::fd::AsRawFd as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use libvirt_domain::DomainState;
use linux_vm_protocol::files::{FILES_PORT_BASE, FileShare};
use linux_vm_protocol::link::AgentLink;
use linux_vm_protocol::{AGENT_PORT, GuestMessage, HostMessage, client};
use thiserror::Error;

use super::files::Served;
use super::firecracker::{self, Api, FirecrackerError, Machine, VmFiles};
use super::jail::{Jail, JailDirs};
use super::memory;
use super::meter::{Log, Meter, RunEnd, UsageEntry, VmIdentity};
use super::network::Endpoint;
use crate::lifecycle::{Action, Lifecycle, LifecycleError, Phase};
use crate::virtiofs::SYSTEM_READ_ONLY;
use tilo_server_protocol::hosted_vm::VmSize;

/// The most directories one VM mounts.
const MAX_SHARES: u32 = 64;

/// How long a cold boot or restore may take before the agent answers.
const AGENT_PATIENCE: Duration = Duration::from_secs(120);
/// How long setting a guest's clock may take: one request to an agent that has
/// just answered.
const CLOCK_PATIENCE: Duration = Duration::from_secs(30);

/// Wall-clock milliseconds since the epoch.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

#[must_use]
pub fn system_clock() -> Clock {
    Arc::new(tilo_clock::unix_now_ms)
}

#[derive(Debug, Error)]
pub enum VmError {
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
    #[error(transparent)]
    Firecracker(#[from] FirecrackerError),
    #[error("the VM's agent did not answer within {0:?}")]
    AgentTimeout(Duration),
    #[error("the VM's agent: {0}")]
    Agent(#[from] linux_vm_protocol::ProtocolError),
    #[error("the VM is not running")]
    NotRunning,
    #[error(
        "every Linux VM this host runs at once is running; try again in a minute \
         (an idle VM goes to sleep after its idle limit)"
    )]
    HostFull,
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub machine: Machine,
    /// No command for this long suspends the VM.
    pub idle_suspend: Duration,
    /// `tilo-linux-vm` itself: it confines and serves the shares.
    pub self_exe: PathBuf,
    /// Every VM in a jail of its own, with a network when the jails have
    /// one ([`super::jail`]). Unjailed VMs (development only) have no
    /// network.
    pub jail: Option<Arc<Jail>>,
    /// Where the relay is, as the host reaches it: the one destination a VM may reach besides the
    /// internet. A relay on the host's own loopback (a local stack) is reached through
    /// [`super::network::HOST_LOOPBACK`]; anywhere else the VM dials it as it is. `None`: the host
    /// serves no Browser.
    pub server: Option<Endpoint>,
    /// Where the templates a new VM is cloned from are kept, one per size. Only jailed VMs are
    /// cloned: a jail names a VM's disk and sockets at the same path for every VM, which a snapshot
    /// records. `None`: every new VM boots cold.
    pub templates: Option<PathBuf>,
}

/// What a template's guest reads before it is snapshotted: the daemon's, the session's, Browser's
/// and Chromium's files, so each clone finds them in the page cache it shares with every other clone.
const WARM: &str = "find /usr/bin/tilo-session /usr/libexec/tilo /usr/lib/tilo-runtime \
    /usr/share/tilo-apps -type f -exec cat {} + >/dev/null 2>&1; sync";

/// What a template was made from: the guest image and the size. A template
/// of another image is not one.
fn template_stamp(settings: &Settings, size: VmSize) -> String {
    let machine = &settings.machine;
    format!(
        "{}\n{}\n{size}\n{WARM}\n",
        machine.kernel.display(),
        machine
            .base_disk
            .as_ref()
            .map_or_else(String::new, |disk| disk.display().to_string())
    )
}

#[must_use]
pub fn template_dir(settings: &Settings, size: VmSize) -> Option<PathBuf> {
    settings.jail.as_ref()?;
    Some(settings.templates.as_ref()?.join(size.as_str()))
}

#[must_use]
pub fn template(settings: &Settings, size: VmSize) -> Option<VmFiles> {
    let state = template_dir(settings, size)?;
    let files = VmFiles {
        run: state.clone(),
        state,
        jailed: true,
    };
    let stamp = std::fs::read_to_string(files.state.join("template")).ok()?;
    (stamp == template_stamp(settings, size) && files.snapshot().valid() && files.disk().is_file())
        .then_some(files)
}

/// Make the suspended VM at `files` `size`'s template: read-only, and
/// stamped with what it was made from.
pub fn freeze_template(settings: &Settings, size: VmSize, files: &VmFiles) -> Result<(), VmError> {
    use std::os::unix::fs::PermissionsExt as _;
    let snapshot = files.snapshot();
    for path in [&files.disk(), &snapshot.state, &snapshot.memory] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444))?;
    }
    tilo_files::native::atomic::write_creating_parent(
        &files.state.join("template"),
        template_stamp(settings, size).as_bytes(),
    )?;
    Ok(())
}

/// How the last wake went: what an agent waited for its first command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Wake {
    pub restored: bool,
    pub millis: u64,
}

struct ShareServer {
    share: Served,
    port: u32,
    process: Child,
}

pub struct HostedVm {
    files: VmFiles,
    settings: Arc<Settings>,
    log: Option<Log>,
    clock: Clock,
    meter: Meter,
    life: Lifecycle,
    process: Option<Child>,
    link: Option<AgentLink>,
    /// The shares asked for since the VM last woke.
    desired: Vec<Served>,
    servers: Vec<ShareServer>,
    wake_started: Option<Instant>,
    restoring: bool,
    /// The next run starts for a Linux action, and is billed from its start.
    bill_next: bool,
    last_wake: Option<Wake>,
    slot: Option<u32>,
}

impl HostedVm {
    /// The VM whose files are `files`, as it was left. A run a crashed
    /// service left open is closed and recorded; a Firecracker it left
    /// behind is stopped.
    pub fn open(
        identity: VmIdentity,
        files: VmFiles,
        settings: Arc<Settings>,
        log: Option<Log>,
        clock: Clock,
    ) -> Result<Self, VmError> {
        std::fs::create_dir_all(&files.state)?;
        let mut meter = Meter::load(&files.meter()).unwrap_or_else(|| Meter::new(identity));
        kill_stray(&files, settings.jail.as_deref());
        resource_in(log.as_ref(), meter.recover().as_ref())?;
        meter.save(&files.meter())?;
        let mut life = Lifecycle::new(settings.idle_suspend.as_secs());
        let disk = files.disk().is_file();
        life.observe(
            if disk {
                DomainState::ShutOff
            } else {
                DomainState::Absent
            },
            files.snapshot().valid(),
            disk,
        )?;
        Ok(Self {
            files,
            settings,
            log,
            clock,
            meter,
            life,
            process: None,
            link: None,
            desired: Vec::new(),
            servers: Vec::new(),
            wake_started: None,
            restoring: false,
            bill_next: false,
            last_wake: None,
            slot: None,
        })
    }

    #[must_use]
    pub const fn identity(&self) -> &VmIdentity {
        &self.meter.identity
    }

    #[must_use]
    pub fn link(&self) -> Option<AgentLink> {
        self.link.clone()
    }

    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.life.phase()
    }

    #[must_use]
    pub const fn awake(&self) -> bool {
        self.process.is_some()
    }

    #[must_use]
    pub const fn last_wake(&self) -> Option<Wake> {
        self.last_wake
    }

    #[must_use]
    pub fn snapshot_bytes(&self) -> Option<u64> {
        self.meter.snapshot.map(|held| held.bytes)
    }

    /// What the running VM takes of this host's memory, MiB, as its kernel accounts it: its
    /// Firecracker process and everything below it, which is what closing the VM would free. `None`
    /// when it is not running.
    #[must_use]
    pub fn resident_mib(&self) -> Option<u64> {
        memory::tree_mib(self.process.as_ref()?.id())
    }

    /// Ask the guest to give `target_mib` of its memory back: `Ok(true)` when its Firecracker took
    /// the target, `Ok(false)` when this VM has no balloon device, which no later call changes.
    pub fn balloon(&self, target_mib: u64) -> Result<bool, VmError> {
        match Api::new(self.files.api_socket()).balloon(target_mib) {
            Ok(()) => Ok(true),
            Err(FirecrackerError::Api { status, .. }) if (400..500).contains(&status) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Wake the VM with `shares` mounted, and say how to reach its agent and how the wake went. For
    /// a Linux action (`linux`) the VM's time is billed from now until it sleeps; the browser
    /// session alone bills none.
    pub fn ensure(
        &mut self,
        shares: &[Served],
        linux: bool,
    ) -> Result<(AgentLink, Option<Wake>), VmError> {
        if self.reap() && linux {
            self.meter.billed((self.clock)());
            self.meter.save(&self.files.meter())?;
        }
        self.bill_next = linux;
        for share in shares {
            if !self.desired.iter().any(|known| known.path == share.path) {
                self.desired.push(share.clone());
            }
        }
        let actions = self.life.wake()?;
        self.wake_started = None;
        let before = self.last_wake.take();
        if let Err(error) = self.execute(&actions) {
            self.last_wake = before;
            self.fail();
            return Err(error);
        }
        let woke = self.last_wake;
        if woke.is_none() {
            self.last_wake = before;
        }
        Ok((self.link.clone().ok_or(VmError::NotRunning)?, woke))
    }

    /// Change the VM's size. Only a VM that is not awake can: a snapshot
    /// of the old size is dropped (the next wake boots), and the disk grows
    /// to the new size (it never shrinks).
    pub fn resize(&mut self, size: VmSize) -> Result<(), VmError> {
        if self.meter.identity.size == size {
            return Ok(());
        }
        if self.awake() {
            return Err(VmError::Lifecycle(LifecycleError::Busy));
        }
        self.drop_snapshot()?;
        if self.files.disk().is_file() {
            firecracker::grow_disk(&self.files.disk(), size)?;
        }
        self.meter.identity.size = size;
        self.meter.save(&self.files.meter())?;
        self.observe()?;
        Ok(())
    }

    /// The size a command runs this VM at: `size` when the VM can take it — it
    /// has no disk yet, or it is asleep — and the size it has when it is awake,
    /// since a command does not resize a machine that is running. A host that
    /// places a workspace's VM fixes its size; the size a command names is what
    /// the VM would be made at.
    pub fn want_size(&mut self, size: VmSize) -> Result<(), VmError> {
        match self.resize(size) {
            Err(VmError::Lifecycle(LifecycleError::Busy)) if self.awake() => Ok(()),
            other => other,
        }
    }

    /// Look at the running VM: is it alive, what runs in it. Suspends it
    /// once it has been idle for the limit. `sessions` is how many sessions the
    /// host started in it that are still running (a browser's, the apps'): they
    /// count as running until their own idle limits end them, so a VM holding
    /// one is never taken for idle between the commands run in it.
    pub fn poll(&mut self, sessions: u32) -> Result<(), VmError> {
        if !self.reap() || self.life.phase() != Phase::Running {
            return Ok(());
        }
        let now = (self.clock)();
        let running = match self.agent(&HostMessage::Health) {
            Ok(GuestMessage::Health(health)) => health.running.saturating_add(sessions),
            _ => return Ok(()),
        };
        self.meter.seen(now);
        self.meter.save(&self.files.meter())?;
        if let Some(actions) = self.life.activity(running, now / 1000)
            && let Err(error) = self.execute(&actions)
        {
            self.fail();
            return Err(error);
        }
        Ok(())
    }

    pub fn suspend(&mut self) -> Result<(), VmError> {
        self.reap();
        if let Some(actions) = self.life.suspend_now()
            && let Err(error) = self.execute(&actions)
        {
            self.fail();
            return Err(error);
        }
        Ok(())
    }

    /// Stop the VM and drop its snapshot: the next command boots it from
    /// its disk. The guest flushes its disk first; it is not shut down
    /// beyond that, so stopping is never billed for a slow shutdown.
    pub fn stop(&mut self) -> Result<(), VmError> {
        if let Some(process) = self.process.take() {
            let _ = self.agent(&HostMessage::MountFiles { shares: Vec::new() });
            let _ = self.sync_guest();
            self.finish(process);
            self.ended(RunEnd::Stopped)?;
        }
        self.stop_servers();
        self.drop_snapshot()?;
        self.observe()?;
        Ok(())
    }

    fn execute(&mut self, actions: &[Action]) -> Result<(), VmError> {
        for action in actions {
            match action {
                Action::CreateDisk => match template(&self.settings, self.meter.identity.size) {
                    Some(template) => firecracker::clone_vm(&template, &self.files)?,
                    None => firecracker::create_disk(
                        &self.settings.machine,
                        &self.files.disk(),
                        self.meter.identity.size,
                    )?,
                },
                Action::Define => {}
                Action::Start => self.start()?,
                Action::AwaitAgent => {
                    self.await_agent()?;
                    self.life.agent_ready();
                    self.forget_branch_source()?;
                }
                Action::SyncShares => self.sync_shares()?,
                Action::DetachShares => {
                    let _ = self.agent(&HostMessage::MountFiles { shares: Vec::new() });
                    self.stop_servers();
                    self.desired.clear();
                }
                Action::Suspend => self.snapshot_and_exit()?,
            }
        }
        Ok(())
    }

    /// Copy this VM's disk to `to`, grown to `size`, for a branch ([`firecracker::branch_disk`]):
    /// the copy is a filesystem the branch boots from, holding everything a command in this VM had
    /// finished writing. A running VM syncs its guest and is paused for the copy, then runs on. A
    /// suspended one may hold in its saved memory what its disk does not, so it is woken for that
    /// and suspended again. One that is off is copied as it is: stopping synced it. `Ok(false)`:
    /// it has no disk, so nothing was copied.
    pub fn branch_disk(&mut self, to: &VmFiles, size: VmSize) -> Result<bool, VmError> {
        self.reap();
        match self.life.phase() {
            Phase::NotCreated => Ok(false),
            Phase::Off => {
                firecracker::branch_disk(&self.files, to, size)?;
                Ok(true)
            }
            Phase::Running => {
                self.copy_paused(to, size)?;
                Ok(true)
            }
            Phase::Suspended => {
                self.ensure(&[], false)?;
                let copied = self.copy_paused(to, size);
                let suspended = self.suspend();
                copied?;
                suspended?;
                Ok(true)
            }
            Phase::Starting | Phase::Suspending => Err(LifecycleError::Busy.into()),
        }
    }

    /// Sync the guest, pause it, copy its disk and let it run on. The sync puts on the disk what
    /// the guest had written, and the pause holds the disk still while it is copied: a write after
    /// the sync is either on the copy whole or a journal replays it away at the branch's boot.
    fn copy_paused(&mut self, to: &VmFiles, size: VmSize) -> Result<(), VmError> {
        self.sync_guest()?;
        let api = Api::new(self.files.api_socket());
        api.pause()?;
        let copied = firecracker::branch_disk(&self.files, to, size);
        if let Err(error) = api.resume().or_else(|_| api.resume()) {
            // A VM that does not run on is no VM: end it, as a wake that failed half-way is.
            self.fail();
            return Err(error.into());
        }
        Ok(copied?)
    }

    /// A branch's guest forgets the sessions of the VM it was copied from before anything here is
    /// told it is up: their device keys and certificates, their secrets, and Browser's whole
    /// profile, so no session that starts in it holds them. The tenant's own files stay; they are
    /// what a branch is for. Until the guest said it forgot them, the mark stays and the wake
    /// fails, so the next wake asks again.
    fn forget_branch_source(&self) -> Result<(), VmError> {
        if !self.files.branched().exists() {
            return Ok(());
        }
        let link = self.link.as_ref().ok_or(VmError::NotRunning)?;
        let stream = link.connect(AGENT_PORT)?;
        stream.set_read_timeout(Some(Duration::from_secs(60)))?;
        let (mut out, mut err) = (std::io::sink(), Vec::new());
        let status = client::launch(
            stream,
            &linux_vm_protocol::Launch {
                argv: forget_source_argv(),
                cwd: Some("/".to_string()),
                env: Vec::new(),
                stdio: linux_vm_protocol::Stdio::Null,
                display: linux_vm_protocol::Display::None,
            },
            client::LaunchIo {
                stdin: None,
                stdout: &mut out,
                stderr: &mut err,
                started: &mut |_| {},
            },
            |_| {},
        )?;
        if status.shell_code() != 0 {
            return Err(std::io::Error::other(format!(
                "the branch did not forget the sessions of the VM it was made from (status {})",
                status.shell_code()
            ))
            .into());
        }
        std::fs::remove_file(self.files.branched())?;
        Ok(())
    }

    fn start(&mut self) -> Result<(), VmError> {
        let settings = Arc::clone(&self.settings);
        let machine = &settings.machine;
        let snapshot = self.files.snapshot();
        std::fs::create_dir_all(&self.files.run)?;
        std::fs::create_dir_all(self.files.vmm())?;
        for socket in [self.files.api_socket(), self.files.vsock_socket()] {
            let _ = std::fs::remove_file(socket);
        }
        self.restoring = snapshot.valid();
        if !self.restoring {
            // A cold boot: memory left from before is no base for anything.
            self.drop_snapshot()?;
            let network = settings.jail.as_ref().and_then(|jail| jail.network_mbit());
            tilo_files::native::atomic::write_creating_parent(
                &self.files.config(),
                firecracker::config_json(machine, self.meter.identity.size, &self.files, network)
                    .as_bytes(),
            )?;
        }
        let console = std::fs::File::create(self.files.console_log())?;
        // Held until the spawn: the helper gets them as descriptors.
        let dirs = if let Some(jail) = &settings.jail {
            let slot = jail.take().ok_or(VmError::HostFull)?;
            self.slot = Some(slot);
            match self.jail_dirs(machine) {
                Ok(dirs) => Some((jail, slot, dirs)),
                Err(error) => {
                    self.release();
                    return Err(error.into());
                }
            }
        } else {
            None
        };
        let mut command = if let Some((jail, slot, (vmm, sockets, guest))) = &dirs {
            jail.helper.start(
                *slot,
                self.meter.identity.size,
                !self.restoring,
                &JailDirs {
                    vmm,
                    sockets,
                    guest: guest.as_ref(),
                },
                settings.server,
            )
        } else {
            let argv = if self.restoring {
                firecracker::restore_argv(machine, &self.files)
            } else {
                firecracker::boot_argv(machine, &self.files)
            };
            let (program, rest) = argv.split_first().ok_or(VmError::NotRunning)?;
            let mut command = Command::new(program);
            command.args(rest);
            command
        };
        self.wake_started = Some(Instant::now());
        let spawned = command
            .stdin(Stdio::null())
            .stdout(console.try_clone()?)
            .stderr(console)
            .process_group(0)
            .spawn();
        drop(command);
        drop(dirs);
        let process = match spawned {
            Ok(process) => process,
            Err(error) => {
                self.release();
                return Err(error.into());
            }
        };
        let _ = std::fs::write(
            self.pid_file(),
            match self.slot {
                Some(slot) => format!("{} {slot}", process.id()),
                None => process.id().to_string(),
            },
        );
        self.process = Some(process);
        self.meter
            .started((self.clock)(), std::mem::take(&mut self.bill_next));
        self.meter.save(&self.files.meter())?;
        self.link = Some(AgentLink::Hybrid {
            socket: self.files.vsock_socket(),
        });
        if self.restoring {
            let api = Api::new(self.files.api_socket());
            api.wait(Duration::from_secs(10))?;
            api.restore(
                &self.files.for_vmm(&snapshot.state),
                &self.files.for_vmm(&snapshot.memory),
            )?;
            // From here the VM writes its disk: this snapshot no longer
            // matches it.
            snapshot.in_use()?;
        }
        Ok(())
    }

    /// The directories a jail is made of: the VM's state, its sockets, and
    /// the guest kernel's (absent for a kernel at the filesystem root).
    fn jail_dirs(&self, machine: &Machine) -> std::io::Result<(File, File, Option<File>)> {
        let guest = machine.kernel.parent().map(File::open).transpose()?;
        Ok((
            File::open(self.files.vmm())?,
            File::open(&self.files.run)?,
            guest,
        ))
    }

    /// End `process` and wait for it: through the helper when jailed (the
    /// VM's uid is not the service's to signal), then release its jail.
    fn finish(&mut self, mut process: Child) {
        if let (Some(jail), Some(slot)) = (&self.settings.jail, self.slot) {
            if let Err(error) = jail.helper.stop(slot) {
                eprintln!("tilo-linux-vm hosted: stopping slot {slot}: {error}");
            }
        } else {
            let _ = process.kill();
        }
        let _ = process.wait();
        self.release();
    }

    /// The VM is gone: give its jail back. A slot whose release failed is
    /// not used again, so no later VM inherits what is left in it.
    fn release(&mut self) {
        let Some(slot) = self.slot.take() else {
            return;
        };
        let Some(jail) = self.settings.jail.clone() else {
            return;
        };
        let released = self
            .jail_dirs(&self.settings.machine)
            .and_then(|(vmm, sockets, _)| {
                jail.helper.release(
                    slot,
                    &JailDirs {
                        vmm: &vmm,
                        sockets: &sockets,
                        guest: None,
                    },
                )
            });
        match released {
            Ok(()) => jail.give_back(slot),
            Err(error) => eprintln!("tilo-linux-vm hosted: releasing slot {slot}: {error}"),
        }
    }

    fn await_agent(&mut self) -> Result<(), VmError> {
        let link = self.link.clone().ok_or(VmError::NotRunning)?;
        let started = Instant::now();
        loop {
            if let Ok(mut stream) = link.connect(AGENT_PORT) {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                if client::handshake(&mut stream).is_ok() {
                    break;
                }
            }
            if !self.reap() {
                return Err(VmError::NotRunning);
            }
            if started.elapsed() >= AGENT_PATIENCE {
                return Err(VmError::AgentTimeout(AGENT_PATIENCE));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if let Some(wake) = self.wake_started.take() {
            self.last_wake = Some(Wake {
                restored: self.restoring,
                millis: u64::try_from(wake.elapsed().as_millis()).unwrap_or(u64::MAX),
            });
        }
        self.set_guest_clock()?;
        Ok(())
    }

    /// Set the guest's wall clock from the host's, before the VM is handed out. A VM restored from
    /// a snapshot resumes with the clock its snapshot was taken at, which is behind the host's by
    /// however long that snapshot was kept: a certificate the server issued this moment is then
    /// "not yet valid" inside the VM, and the session that scans it refuses its own identity. The
    /// guest agent runs as root and moves `CLOCK_REALTIME` for the whole guest, which a program of
    /// the tenant's could not. A cold boot reads the host's time already; setting it again costs
    /// one round trip and keeps boot and restore one path.
    fn set_guest_clock(&self) -> Result<(), VmError> {
        // The host's time, plus a millisecond so a certificate issued just before this request is
        // never ahead of the guest's clock when the guest reads it.
        let unix_millis = (self.clock)().saturating_add(1);
        let link = self.link.as_ref().ok_or(VmError::NotRunning)?;
        let mut stream = link.connect(AGENT_PORT)?;
        stream.set_read_timeout(Some(CLOCK_PATIENCE))?;
        Ok(set_guest_clock_through(&mut stream, unix_millis)?)
    }

    fn sync_shares(&mut self) -> Result<(), VmError> {
        let desired = self.desired.clone();
        for share in &desired {
            if !self.servers.iter().any(|server| server.share == *share) {
                self.serve(share)?;
            }
        }
        let shares: Vec<FileShare> = self
            .servers
            .iter()
            .map(|server| FileShare {
                path: server.share.path.display().to_string(),
                read_only: server.share.read_only,
                port: server.port,
            })
            .collect();
        match self.agent(&HostMessage::MountFiles { shares })? {
            GuestMessage::FilesMounted { .. } => Ok(()),
            _ => Err(linux_vm_protocol::ProtocolError::Unexpected.into()),
        }
    }

    /// One confined server for `share`, listening where the guest's vsock
    /// connections to its port arrive.
    fn serve(&mut self, share: &Served) -> Result<(), VmError> {
        let port = (FILES_PORT_BASE..FILES_PORT_BASE.saturating_add(MAX_SHARES))
            .find(|port| !self.servers.iter().any(|server| server.port == *port))
            .ok_or_else(|| std::io::Error::other("too many shares for one VM"))?;
        let listener_path = AgentLink::hybrid_listener(&self.files.vsock_socket(), port);
        let _ = std::fs::remove_file(&listener_path);
        let listener = tilo_files::native::socket::bind(&listener_path)?;
        // A jailed Firecracker runs as the VM's uid: it may connect. The
        // directory holds only this VM's sockets and is closed to others.
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&listener_path, std::fs::Permissions::from_mode(0o777))?;
        }
        let argv = files_serve_argv(&self.settings.self_exe, share, &self.hidden());
        let (program, rest) = argv.split_first().ok_or(VmError::NotRunning)?;
        let fd = listener.as_raw_fd();
        let mut command = Command::new(program);
        command
            .args(rest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .process_group(0);
        // SAFETY: `dup2` and `fcntl` are async-signal-safe; they give the
        // server the bound listener as descriptor 3, without close-on-exec.
        // When it already is 3, `dup2` would keep the flag: clear it.
        unsafe {
            command.pre_exec(move || {
                let result = if fd == LISTEN_FD {
                    libc::fcntl(fd, libc::F_SETFD, 0)
                } else {
                    libc::dup2(fd, LISTEN_FD)
                };
                if result < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let process = command.spawn()?;
        drop(listener);
        self.servers.push(ShareServer {
            share: share.clone(),
            port,
            process,
        });
        Ok(())
    }

    fn stop_servers(&mut self) {
        for mut server in self.servers.drain(..) {
            let _ = server.process.kill();
            let _ = server.process.wait();
            let _ = std::fs::remove_file(AgentLink::hybrid_listener(
                &self.files.vsock_socket(),
                server.port,
            ));
        }
    }

    fn snapshot_and_exit(&mut self) -> Result<(), VmError> {
        let snapshot = self.files.snapshot();
        let api = Api::new(self.files.api_socket());
        api.pause()?;
        let _ = std::fs::remove_file(&snapshot.diff);
        api.snapshot(
            &self.files.for_vmm(&snapshot.new_state),
            &self.files.for_vmm(snapshot.next_memory_target()),
        )?;
        if let Some(process) = self.process.take() {
            self.finish(process);
        }
        self.ended(RunEnd::Suspended)?;
        snapshot.complete()?;
        let now = (self.clock)();
        let record = self.meter.snapshot_taken(now, snapshot.bytes_on_disk());
        resource_in(self.log.as_ref(), record.as_ref())?;
        self.meter.save(&self.files.meter())?;
        self.life.suspended();
        self.link = None;
        Ok(())
    }

    fn ended(&mut self, how: RunEnd) -> Result<Option<UsageEntry>, VmError> {
        let _ = std::fs::remove_file(self.pid_file());
        self.link = None;
        let record = self.meter.stopped((self.clock)(), how);
        resource_in(self.log.as_ref(), record.as_ref())?;
        self.meter.save(&self.files.meter())?;
        Ok(record)
    }

    fn drop_snapshot(&mut self) -> Result<(), VmError> {
        self.files.snapshot().discard()?;
        let record = self.meter.snapshot_dropped((self.clock)());
        resource_in(self.log.as_ref(), record.as_ref())?;
        self.meter.save(&self.files.meter())?;
        Ok(())
    }

    /// Whether the process is alive; a dead one is recorded as crashed.
    fn reap(&mut self) -> bool {
        let Some(process) = self.process.as_mut() else {
            return false;
        };
        if matches!(process.try_wait(), Ok(None)) {
            return true;
        }
        if let Some(process) = self.process.take() {
            self.finish(process);
        }
        let _ = self.ended(RunEnd::Crashed);
        self.stop_servers();
        self.desired.clear();
        let _ = self.observe();
        false
    }

    /// A wake or suspend failed half-way: end the process and its run, and
    /// take the phase from what is left on disk.
    fn fail(&mut self) {
        if let Some(process) = self.process.take() {
            self.finish(process);
            let _ = self.ended(RunEnd::Crashed);
        }
        self.stop_servers();
        self.desired.clear();
        let _ = self.observe();
    }

    fn observe(&mut self) -> Result<Phase, VmError> {
        let disk = self.files.disk().is_file();
        let state = match (&self.process, disk) {
            (Some(_), _) => DomainState::Running,
            (None, true) => DomainState::ShutOff,
            (None, false) => DomainState::Absent,
        };
        Ok(self
            .life
            .observe(state, self.files.snapshot().valid(), disk)?)
    }

    /// `sync` in the guest: what it wrote is on its disk.
    fn sync_guest(&self) -> Result<(), VmError> {
        self.run_in_guest(&["sync"], Duration::from_secs(30))
    }

    pub fn warm(&self) -> Result<(), VmError> {
        self.run_in_guest(&["sh", "-c", WARM], Duration::from_secs(300))
    }

    /// Run `argv` in the guest and wait for it to end.
    fn run_in_guest(&self, argv: &[&str], patience: Duration) -> Result<(), VmError> {
        let link = self.link.as_ref().ok_or(VmError::NotRunning)?;
        let stream = link.connect(AGENT_PORT)?;
        stream.set_read_timeout(Some(patience))?;
        let (mut out, mut err) = (std::io::sink(), std::io::sink());
        client::launch(
            stream,
            &linux_vm_protocol::Launch {
                argv: argv.iter().map(ToString::to_string).collect(),
                cwd: Some("/".to_string()),
                env: Vec::new(),
                stdio: linux_vm_protocol::Stdio::Null,
                display: linux_vm_protocol::Display::None,
            },
            client::LaunchIo {
                stdin: None,
                stdout: &mut out,
                stderr: &mut err,
                started: &mut |_| {},
            },
            |_| {},
        )?;
        Ok(())
    }

    fn agent(&self, message: &HostMessage) -> Result<GuestMessage, VmError> {
        let link = self.link.as_ref().ok_or(VmError::NotRunning)?;
        let mut stream = link.connect(AGENT_PORT)?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        Ok(client::request(&mut stream, message)?)
    }

    fn pid_file(&self) -> PathBuf {
        self.files.run.join("firecracker.pid")
    }

    /// What a share server must not see: every VM's sockets and the
    /// service's own (the service's run directory), and the jails.
    fn hidden(&self) -> Vec<PathBuf> {
        let mut hidden: Vec<PathBuf> = self
            .files
            .run
            .parent()
            .map(Path::to_path_buf)
            .into_iter()
            .collect();
        if let Some(jail) = &self.settings.jail {
            hidden.push(jail.helper.config.chroot_base.clone());
        }
        hidden
    }
}

impl Drop for HostedVm {
    fn drop(&mut self) {
        self.stop_servers();
    }
}

/// Ask the agent on `stream` to make the guest's whole wall clock read `unix_millis`. The agent
/// runs as root in the guest and moves `CLOCK_REALTIME`, which a program of the tenant's could not.
/// The answer must name the time that was set: an agent that set another time, or refused, is an
/// error the wake reports rather than a VM handed out with a clock behind the host's.
fn set_guest_clock_through<S: std::io::Read + std::io::Write>(
    stream: &mut S,
    unix_millis: u64,
) -> Result<(), linux_vm_protocol::ProtocolError> {
    match client::request(stream, &HostMessage::SetClock { unix_millis })? {
        GuestMessage::ClockSet { unix_millis: set } if set == unix_millis => Ok(()),
        _ => Err(linux_vm_protocol::ProtocolError::Unexpected),
    }
}

/// The descriptor a share server gets its bound listener on.
pub const LISTEN_FD: i32 = 3;

/// What a branch's guest runs, as root through the tenant's `sudo`, before its first session: the
/// sessions' own directories in the tenant's home (`~/.config/tilo` holds the device's key and
/// certificate, `~/.local/share/tilo` its secrets and runtime, `~/.local/state/tilo` and
/// `~/.cache/tilo` what the session keeps), and Browser's account's whole home, which is its
/// profile, made again as the image makes it. It fails unless every one of them is gone.
const FORGET_SOURCE: &str = "set -eu
tenant=$(getent passwd tilo | cut -d: -f6)
browser=$(getent passwd tilo-browser | cut -d: -f6)
[ -n \"$tenant\" ]
[ -n \"$browser\" ]
for gone in \"$tenant/.config/tilo\" \"$tenant/.local/share/tilo\" \"$tenant/.local/state/tilo\" \"$tenant/.cache/tilo\"; do
  rm -rf -- \"$gone\"
  [ ! -e \"$gone\" ]
done
rm -rf -- \"$browser\"
install -d -m 0700 -o tilo-browser -g tilo-browser \"$browser\"
[ -z \"$(ls -A \"$browser\")\" ]
sync
";

#[must_use]
pub fn forget_source_argv() -> Vec<String> {
    ["/usr/bin/sudo", "-n", "/bin/sh", "-c", FORGET_SOURCE]
        .map(str::to_owned)
        .to_vec()
}

fn resource_in(log: Option<&Log>, record: Option<&UsageEntry>) -> Result<(), VmError> {
    match (log, record) {
        (Some(log), Some(record)) => Ok(log.append(record)?),
        _ => Ok(()),
    }
}

/// `tilo-linux-vm confine-fs --isolate --hide … --ro … -- tilo-linux-vm hosted files-serve …`: Landlock
/// holds the server to its one share (read-only when the share is), and namespaces of its own keep
/// it from every socket in `hidden` and from the network.
#[must_use]
pub fn files_serve_argv(self_exe: &Path, share: &Served, hidden: &[PathBuf]) -> Vec<String> {
    let exe = self_exe.display().to_string();
    let mut argv = vec![exe.clone(), "confine-fs".to_string()];
    let hidden: Vec<&Path> = hidden.iter().map(PathBuf::as_path).collect();
    argv.extend(super::jail::hide_args(&hidden));
    for path in SYSTEM_READ_ONLY {
        argv.extend(["--ro".to_string(), (*path).to_string()]);
    }
    if let Some(dir) = self_exe.parent() {
        argv.extend(["--ro".to_string(), dir.display().to_string()]);
    }
    let access = if share.read_only { "--ro" } else { "--rw" };
    argv.extend([access.to_string(), share.path.display().to_string()]);
    argv.extend([
        "--".to_string(),
        exe,
        "hosted".to_string(),
        "files-serve".to_string(),
        "--listen-fd".to_string(),
        LISTEN_FD.to_string(),
        "--share".to_string(),
        share.path.display().to_string(),
    ]);
    if share.read_only {
        argv.push("--read-only".to_string());
    }
    argv
}

/// A Firecracker a previous service left running for this VM: its run was
/// closed at the last moment it was seen, so it must not go on unmetered.
/// A jailed one is stopped and its jail released through the helper.
fn kill_stray(files: &VmFiles, jail: Option<&Jail>) {
    let pid_file = files.run.join("firecracker.pid");
    let Ok(text) = std::fs::read_to_string(&pid_file) else {
        return;
    };
    let mut words = text.split_whitespace();
    let pid = words.next().and_then(|pid| pid.parse::<i32>().ok());
    let slot = words.next().and_then(|slot| slot.parse::<u32>().ok());
    match (jail, slot) {
        (Some(jail), Some(slot)) => {
            jail.claim(slot);
            let released = jail.helper.stop(slot).and_then(|()| {
                let (vmm, sockets) = (File::open(files.vmm())?, File::open(&files.run)?);
                jail.helper.release(
                    slot,
                    &JailDirs {
                        vmm: &vmm,
                        sockets: &sockets,
                        guest: None,
                    },
                )
            });
            match released {
                Ok(()) => jail.give_back(slot),
                Err(error) => eprintln!("tilo-linux-vm hosted: releasing slot {slot}: {error}"),
            }
        }
        _ => {
            if let Some(pid) = pid {
                let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
                let api = files.api_socket().display().to_string();
                if String::from_utf8_lossy(&cmdline).contains(&api) {
                    // SAFETY: `kill` has no memory preconditions; the pid's
                    // command line names this VM's API socket.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
        }
    }
    let _ = std::fs::remove_file(pid_file);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake agent on `guest`: it shakes the host's hand, expects one clock
    /// request, and answers it with `reply`. Assertions live here, not in a
    /// function returning a result.
    fn answer_clock_request(
        mut guest: std::os::unix::net::UnixStream,
        reply: impl FnOnce(u64) -> GuestMessage,
    ) {
        use linux_vm_protocol::{AgentHello, GuestOs, PROTOCOL_MAJOR, recv, send};

        assert!(matches!(
            recv::<_, HostMessage>(&mut guest).unwrap(),
            Some(HostMessage::Hello { .. })
        ));
        send(
            &mut guest,
            &GuestMessage::Hello(AgentHello {
                protocol_major: PROTOCOL_MAJOR,
                os: GuestOs::Linux,
                boot_id: "boot".into(),
                agent_version: "test".into(),
            }),
        )
        .unwrap();
        let Some(HostMessage::SetClock { unix_millis }) = recv(&mut guest).unwrap() else {
            panic!("expected a clock request");
        };
        send(&mut guest, &reply(unix_millis)).unwrap();
    }

    /// One clock request to a fake agent on a socket pair, which answers `reply`.
    fn clock_exchange(
        unix_millis: u64,
        reply: impl FnOnce(u64) -> GuestMessage + Send + 'static,
    ) -> Result<(), linux_vm_protocol::ProtocolError> {
        let (mut host, guest) = std::os::unix::net::UnixStream::pair().unwrap();
        let agent = std::thread::spawn(move || answer_clock_request(guest, reply));
        let result = set_guest_clock_through(&mut host, unix_millis);
        agent.join().unwrap();
        result
    }

    #[test]
    fn a_clock_request_sends_the_hosts_time_and_needs_it_back() {
        let at = 1_782_816_120_123;
        assert!(
            clock_exchange(at, move |sent| {
                assert_eq!(sent, at, "the host sent its time");
                GuestMessage::ClockSet { unix_millis: sent }
            })
            .is_ok(),
            "the agent set the time the host asked for"
        );
        assert!(
            matches!(
                clock_exchange(at, move |_| GuestMessage::ClockSet {
                    unix_millis: at.saturating_sub(1)
                }),
                Err(linux_vm_protocol::ProtocolError::Unexpected)
            ),
            "an agent that set another time is not believed"
        );
        assert!(
            matches!(
                clock_exchange(at, |_| GuestMessage::Error {
                    message: "Operation not permitted".into()
                }),
                Err(linux_vm_protocol::ProtocolError::Refused(_))
            ),
            "a refusal fails the wake instead of handing the VM out"
        );
    }

    #[test]
    fn a_branch_forgets_every_session_of_its_source_as_root_and_keeps_the_tenants_files() {
        let argv = forget_source_argv();
        assert_eq!(argv[..4], ["/usr/bin/sudo", "-n", "/bin/sh", "-c"]);
        let script = &argv[4];
        assert!(
            script.starts_with("set -eu\n"),
            "a step that fails fails it"
        );
        for gone in [
            "$tenant/.config/tilo",
            "$tenant/.local/share/tilo",
            "$tenant/.local/state/tilo",
        ] {
            assert!(script.contains(&format!("\"{gone}\"")), "{gone}");
        }
        assert!(script.contains("rm -rf -- \"$browser\"\n"));
        for kept in [".ssh", ".gitconfig", ".cargo", ".local/bin"] {
            assert!(!script.contains(kept), "{kept} is the tenant's");
        }
    }

    #[test]
    fn a_share_server_is_confined_to_its_share() {
        let share = Served {
            path: "/srv/tilo/ws1/files".into(),
            read_only: false,
        };
        let argv = files_serve_argv(
            Path::new("/usr/libexec/tilo/tilo-linux-vm"),
            &share,
            &[PathBuf::from("/run/tilo-hosted")],
        );
        assert_eq!(argv[..2], ["/usr/libexec/tilo/tilo-linux-vm", "confine-fs"]);
        let crate::virtiofs::Confinement {
            ro,
            rw,
            isolate,
            hide,
            program,
        } = crate::virtiofs::parse_confined(&argv[2..]).unwrap();
        assert!(isolate, "a share server gets namespaces of its own");
        assert_eq!(hide, [PathBuf::from("/run/tilo-hosted")]);
        assert_eq!(rw, [PathBuf::from("/srv/tilo/ws1/files")]);
        assert!(ro.contains(&PathBuf::from("/usr/libexec/tilo")));
        assert_eq!(
            program,
            [
                "/usr/libexec/tilo/tilo-linux-vm",
                "hosted",
                "files-serve",
                "--listen-fd",
                "3",
                "--share",
                "/srv/tilo/ws1/files"
            ]
        );
        let read_only = Served {
            read_only: true,
            ..share
        };
        let argv = files_serve_argv(
            Path::new("/usr/libexec/tilo/tilo-linux-vm"),
            &read_only,
            &[],
        );
        let crate::virtiofs::Confinement {
            ro, rw, program, ..
        } = crate::virtiofs::parse_confined(&argv[2..]).unwrap();
        assert!(rw.is_empty());
        assert!(ro.contains(&PathBuf::from("/srv/tilo/ws1/files")));
        assert_eq!(program.last().map(String::as_str), Some("--read-only"));
    }
}
