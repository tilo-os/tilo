//! One hosted VM: woken for a command, snapshotted when idle, metered.
//!
//! The [`Lifecycle`] decides, as it does for the legacy VM; this carries
//! its actions out with Firecracker. A wake is a cold boot when the VM has
//! no valid snapshot and a restore when it has one. Every moment the
//! Firecracker process runs is metered, and every snapshot held on disk.

use std::os::fd::AsRawFd as _;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use legacy_agent_protocol::files::{FILES_PORT_BASE, FileShare};
use legacy_agent_protocol::link::AgentLink;
use legacy_agent_protocol::{AGENT_PORT, GuestMessage, HostMessage, client};
use libvirt_domain::DomainState;
use thiserror::Error;

use super::files::Served;
use super::firecracker::{self, Api, FirecrackerError, Machine, VmFiles};
use super::meter::{Ledger, Meter, RunEnd, UsageRecord, VmIdentity};
use super::size::VmSize;
use crate::lifecycle::{Action, Lifecycle, LifecycleError, Phase};
use crate::virtiofs::SYSTEM_READ_ONLY;

/// The most directories one VM mounts.
const MAX_SHARES: u32 = 64;

/// How long a cold boot or restore may take before the agent answers.
const AGENT_PATIENCE: Duration = Duration::from_secs(120);

/// Wall-clock milliseconds since the epoch.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

#[must_use]
pub fn system_clock() -> Clock {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            })
    })
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
    Agent(#[from] legacy_agent_protocol::ProtocolError),
    #[error("the VM is not running")]
    NotRunning,
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// What every VM of a host shares.
#[derive(Clone, Debug)]
pub struct Settings {
    pub machine: Machine,
    /// No command for this long suspends the VM.
    pub idle_suspend: Duration,
    /// `tilo-legacy` itself: it confines and serves the shares.
    pub self_exe: PathBuf,
}

/// How the last wake went: what an agent waited for its first command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Wake {
    pub restored: bool,
    /// From starting Firecracker to the agent's answer.
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
    ledger: Ledger,
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
    last_wake: Option<Wake>,
}

impl HostedVm {
    /// The VM whose files are `files`, as it was left. A run a crashed
    /// service left open is closed and recorded; a Firecracker it left
    /// behind is stopped.
    pub fn open(
        identity: VmIdentity,
        files: VmFiles,
        settings: Arc<Settings>,
        ledger: Ledger,
        clock: Clock,
    ) -> Result<Self, VmError> {
        std::fs::create_dir_all(&files.state)?;
        let mut meter = Meter::load(&files.meter()).unwrap_or_else(|| Meter::new(identity));
        kill_stray(&files);
        if let Some(record) = meter.recover() {
            ledger.append(&record)?;
        }
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
            ledger,
            clock,
            meter,
            life,
            process: None,
            link: None,
            desired: Vec::new(),
            servers: Vec::new(),
            wake_started: None,
            restoring: false,
            last_wake: None,
        })
    }

    #[must_use]
    pub const fn identity(&self) -> &VmIdentity {
        &self.meter.identity
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

    /// Wake the VM with `shares` mounted, and say how to reach its agent,
    /// and how the wake went when this call woke it.
    pub fn ensure(&mut self, shares: &[Served]) -> Result<(AgentLink, Option<Wake>), VmError> {
        self.reap();
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

    /// Look at the running VM: is it alive, what runs in it. Suspends it
    /// once it has been idle for the limit.
    pub fn poll(&mut self) -> Result<(), VmError> {
        if !self.reap() || self.life.phase() != Phase::Running {
            return Ok(());
        }
        let now = (self.clock)();
        let running = match self.agent(&HostMessage::Health) {
            Ok(GuestMessage::Health(health)) => health.running,
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

    /// Suspend now, whatever runs.
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
        if let Some(mut process) = self.process.take() {
            let _ = self.agent(&HostMessage::MountFiles { shares: Vec::new() });
            let _ = self.sync_guest();
            let _ = process.kill();
            let _ = process.wait();
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
                Action::CreateDisk => firecracker::create_disk(
                    &self.settings.machine,
                    &self.files.disk(),
                    self.meter.identity.size,
                )?,
                Action::Define => {}
                Action::Start => self.start()?,
                Action::AwaitAgent => {
                    self.await_agent()?;
                    self.life.agent_ready();
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

    fn start(&mut self) -> Result<(), VmError> {
        let settings = Arc::clone(&self.settings);
        let machine = &settings.machine;
        let snapshot = self.files.snapshot();
        std::fs::create_dir_all(&self.files.run)?;
        for socket in [self.files.api_socket(), self.files.vsock_socket()] {
            let _ = std::fs::remove_file(socket);
        }
        self.restoring = snapshot.valid();
        let argv = if self.restoring {
            firecracker::restore_argv(machine, &self.files)
        } else {
            // A cold boot: memory left from before is no base for anything.
            self.drop_snapshot()?;
            tilo_files::native::atomic::write_creating_parent(
                &self.files.config(),
                firecracker::config_json(machine, self.meter.identity.size, &self.files, None)
                    .as_bytes(),
            )?;
            firecracker::boot_argv(machine, &self.files)
        };
        let console = std::fs::File::create(self.files.console_log())?;
        let (program, rest) = argv.split_first().ok_or(VmError::NotRunning)?;
        self.wake_started = Some(Instant::now());
        let process = Command::new(program)
            .args(rest)
            .stdin(Stdio::null())
            .stdout(console.try_clone()?)
            .stderr(console)
            .process_group(0)
            .spawn()?;
        let _ = std::fs::write(self.pid_file(), process.id().to_string());
        self.process = Some(process);
        self.meter.started((self.clock)());
        self.meter.save(&self.files.meter())?;
        self.link = Some(AgentLink::Hybrid {
            socket: self.files.vsock_socket(),
        });
        if self.restoring {
            let api = Api::new(self.files.api_socket());
            api.wait(Duration::from_secs(10))?;
            api.restore(&snapshot.state, &snapshot.memory)?;
            // From here the VM writes its disk: this snapshot no longer
            // matches it.
            snapshot.in_use()?;
        }
        Ok(())
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
        Ok(())
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
            _ => Err(legacy_agent_protocol::ProtocolError::Unexpected.into()),
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
        let listener = UnixListener::bind(&listener_path)?;
        let argv = files_serve_argv(&self.settings.self_exe, share);
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
        api.snapshot(&snapshot.new_state, snapshot.next_memory_target())?;
        if let Some(mut process) = self.process.take() {
            let _ = process.kill();
            let _ = process.wait();
        }
        self.ended(RunEnd::Suspended)?;
        snapshot.complete()?;
        let now = (self.clock)();
        if let Some(record) = self.meter.snapshot_taken(now, snapshot.bytes_on_disk()) {
            self.ledger.append(&record)?;
        }
        self.meter.save(&self.files.meter())?;
        self.life.suspended();
        self.link = None;
        Ok(())
    }

    /// The process is gone: close its run.
    fn ended(&mut self, how: RunEnd) -> Result<Option<UsageRecord>, VmError> {
        let _ = std::fs::remove_file(self.pid_file());
        self.link = None;
        let record = self.meter.stopped((self.clock)(), how);
        if let Some(record) = &record {
            self.ledger.append(record)?;
        }
        self.meter.save(&self.files.meter())?;
        Ok(record)
    }

    fn drop_snapshot(&mut self) -> Result<(), VmError> {
        self.files.snapshot().discard()?;
        if let Some(record) = self.meter.snapshot_dropped((self.clock)()) {
            self.ledger.append(&record)?;
        }
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
        self.process = None;
        let _ = self.ended(RunEnd::Crashed);
        self.stop_servers();
        self.desired.clear();
        let _ = self.observe();
        false
    }

    /// A wake or suspend failed half-way: end the process and its run, and
    /// take the phase from what is left on disk.
    fn fail(&mut self) {
        if let Some(mut process) = self.process.take() {
            let _ = process.kill();
            let _ = process.wait();
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
        let link = self.link.as_ref().ok_or(VmError::NotRunning)?;
        let stream = link.connect(AGENT_PORT)?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let (mut out, mut err) = (std::io::sink(), std::io::sink());
        client::launch(
            stream,
            &legacy_agent_protocol::Launch {
                argv: vec!["sync".to_string()],
                cwd: Some("/".to_string()),
                env: Vec::new(),
                stdio: legacy_agent_protocol::Stdio::Null,
                display: legacy_agent_protocol::Display::None,
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
}

impl Drop for HostedVm {
    fn drop(&mut self) {
        self.stop_servers();
    }
}

/// The descriptor a share server gets its bound listener on.
pub const LISTEN_FD: i32 = 3;

/// `tilo-legacy confine-fs --ro … -- tilo-legacy hosted files-serve …`:
/// Landlock holds the server to its one share (read-only when the share
/// is) and what it needs to run.
#[must_use]
pub fn files_serve_argv(self_exe: &Path, share: &Served) -> Vec<String> {
    let exe = self_exe.display().to_string();
    let mut argv = vec![exe.clone(), "confine-fs".to_string()];
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
fn kill_stray(files: &VmFiles) {
    let pid_file = files.run.join("firecracker.pid");
    let Some(pid) = std::fs::read_to_string(&pid_file)
        .ok()
        .and_then(|pid| pid.trim().parse::<i32>().ok())
    else {
        return;
    };
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let api = files.api_socket().display().to_string();
    if String::from_utf8_lossy(&cmdline).contains(&api) {
        // SAFETY: `kill` has no memory preconditions; the pid's command
        // line names this VM's API socket.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    let _ = std::fs::remove_file(pid_file);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_share_server_is_confined_to_its_share() {
        let share = Served {
            path: "/srv/tilo/ws1/files".into(),
            read_only: false,
        };
        let argv = files_serve_argv(Path::new("/usr/libexec/tilo/tilo-legacy"), &share);
        assert_eq!(argv[..2], ["/usr/libexec/tilo/tilo-legacy", "confine-fs"]);
        let (ro, rw, program) = crate::virtiofs::parse_confined(&argv[2..]).unwrap();
        assert_eq!(rw, [PathBuf::from("/srv/tilo/ws1/files")]);
        assert!(ro.contains(&PathBuf::from("/usr/libexec/tilo")));
        assert_eq!(
            program,
            [
                "/usr/libexec/tilo/tilo-legacy",
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
        let argv = files_serve_argv(Path::new("/usr/libexec/tilo/tilo-legacy"), &read_only);
        let (ro, rw, program) = crate::virtiofs::parse_confined(&argv[2..]).unwrap();
        assert!(rw.is_empty());
        assert!(ro.contains(&PathBuf::from("/srv/tilo/ws1/files")));
        assert_eq!(program.last().map(String::as_str), Some("--read-only"));
    }
}
