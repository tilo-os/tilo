//! The one process per host that owns every hosted VM.
//!
//! `tilo-legacy hosted serve` runs it. It admits each wake against the
//! account's plan ([`plan`](super::plan)), wakes the VM, polls the awake
//! ones and suspends the idle, and answers usage questions from the ledger.
//! Each VM is locked on its own, so one VM's boot never holds up another's
//! command; admission is one lock, so two wakes never both take an
//! account's last slot.

use std::collections::BTreeMap;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use legacy_agent_protocol::link::AgentLink;

use super::control::{FRAMING, Reply, Request, ShareRequest, VmRequest, VmStatus};
use super::files::Served;
use super::firecracker::VmFiles;
use super::meter::{Ledger, Meter, VmIdentity, summarize};
use super::plan::{AwakeVm, Plans, Refusal, admit};
use super::valid_name;
use super::vm::{Clock, HostedVm, Settings, VmError};

/// How often awake VMs are asked what runs in them.
pub const POLL: Duration = Duration::from_secs(1);

/// Where a host keeps its VMs and what it boots them with.
#[derive(Clone, Debug)]
pub struct ServiceConfig {
    /// Durable: each VM's disk, snapshot and meter; the usage ledger.
    pub state: PathBuf,
    /// Sockets: the service's own, and each VM's.
    pub run: PathBuf,
    /// The plans file ([`Plans`]), read at every admission.
    pub plans: PathBuf,
    pub settings: Settings,
}

impl ServiceConfig {
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.run.join("hosted.sock")
    }

    #[must_use]
    pub fn ledger(&self) -> Ledger {
        Ledger::new(self.state.join("usage"))
    }

    fn files(&self, workspace: &str, environment: &str) -> VmFiles {
        VmFiles {
            state: self.state.join("vms").join(workspace).join(environment),
            // Short: a Unix socket path has at most 107 bytes.
            run: self.run.join(format!(
                "{:016x}",
                fnv_hash(&format!("{workspace}/{environment}"))
            )),
        }
    }
}

fn fnv_hash(text: &str) -> u64 {
    use std::hash::Hasher as _;
    let mut hasher = fnv::FnvHasher::default();
    hasher.write(text.as_bytes());
    hasher.finish()
}

struct Entry {
    workspace: String,
    environment: String,
    account: String,
    size: Mutex<super::size::VmSize>,
    /// Admitted and not yet seen asleep. Read without the VM's lock.
    awake: AtomicBool,
    vm: Mutex<HostedVm>,
}

impl Entry {
    fn id(&self) -> String {
        format!("{}/{}", self.workspace, self.environment)
    }
}

pub struct Service {
    config: ServiceConfig,
    ledger: Ledger,
    clock: Clock,
    vms: Mutex<BTreeMap<String, Arc<Entry>>>,
    admission: Mutex<()>,
}

/// Why a wake did not happen.
#[derive(Debug)]
pub enum EnsureError {
    Refused(Refusal),
    Invalid(String),
    Vm(VmError),
}

impl std::fmt::Display for EnsureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(f),
            Self::Invalid(message) => f.write_str(message),
            Self::Vm(error) => write!(f, "the workspace's Linux VM could not start: {error}"),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Service {
    /// Open every VM the host has, closing runs a previous service left
    /// open.
    pub fn open(config: ServiceConfig, clock: Clock) -> Result<Self, VmError> {
        std::fs::create_dir_all(&config.state)?;
        std::fs::create_dir_all(&config.run)?;
        let ledger = config.ledger();
        let service = Self {
            ledger,
            clock,
            vms: Mutex::new(BTreeMap::new()),
            admission: Mutex::new(()),
            config,
        };
        let vms_dir = service.config.state.join("vms");
        for workspace in read_names(&vms_dir) {
            for environment in read_names(&vms_dir.join(&workspace)) {
                let files = service.config.files(&workspace, &environment);
                let Some(meter) = Meter::load(&files.meter()) else {
                    continue;
                };
                service.insert(meter.identity, files)?;
            }
        }
        Ok(service)
    }

    #[must_use]
    pub const fn config(&self) -> &ServiceConfig {
        &self.config
    }

    fn insert(&self, identity: VmIdentity, files: VmFiles) -> Result<Arc<Entry>, VmError> {
        let entry = Arc::new(Entry {
            workspace: identity.workspace.clone(),
            environment: identity.environment.clone(),
            account: identity.account.clone(),
            size: Mutex::new(identity.size),
            awake: AtomicBool::new(false),
            vm: Mutex::new(HostedVm::open(
                identity,
                files,
                Arc::new(self.config.settings.clone()),
                self.ledger.clone(),
                Arc::clone(&self.clock),
            )?),
        });
        lock(&self.vms).insert(entry.id(), Arc::clone(&entry));
        Ok(entry)
    }

    /// Wake `request`'s VM with `shares`, if the account's plan allows it.
    /// Returns the agent's link, and what waking took when this woke it.
    pub fn ensure(
        &self,
        request: &VmRequest,
        shares: &[ShareRequest],
    ) -> Result<(AgentLink, Option<u64>), EnsureError> {
        for (what, name) in [
            ("workspace", &request.workspace),
            ("environment", &request.environment),
            ("account", &request.account),
        ] {
            if !valid_name(name) {
                return Err(EnsureError::Invalid(format!("{name:?} is not a {what} id")));
            }
        }
        let served: Vec<Served> = shares
            .iter()
            .map(|share| {
                if legacy_agent_protocol::share_path_allowed(&share.path) && share.path.is_dir() {
                    Ok(Served {
                        path: share.path.clone(),
                        read_only: share.read_only,
                    })
                } else {
                    Err(EnsureError::Invalid(format!(
                        "{} cannot be shared with a VM",
                        share.path.display()
                    )))
                }
            })
            .collect::<Result<_, _>>()?;
        let entry = self.admit(request)?;
        let mut vm = lock(&entry.vm);
        let result = (|| {
            vm.resize(request.size)?;
            *lock(&entry.size) = request.size;
            let (link, woke) = vm.ensure(&served)?;
            Ok((link, woke.map(|wake| wake.millis)))
        })();
        entry.awake.store(vm.awake(), Ordering::SeqCst);
        drop(vm);
        result.map_err(EnsureError::Vm)
    }

    /// The entry for `request`, reserved as awake, or why not.
    fn admit(&self, request: &VmRequest) -> Result<Arc<Entry>, EnsureError> {
        let _admission = lock(&self.admission);
        let plans = Plans::load(&self.config.plans).map_err(EnsureError::Invalid)?;
        let plan = plans.for_account(&request.account);
        let id = format!("{}/{}", request.workspace, request.environment);
        let (existing_entry, awake, existing) = {
            let vms = lock(&self.vms);
            let mine: Vec<&Arc<Entry>> = vms
                .values()
                .filter(|entry| entry.account == request.account)
                .collect();
            let awake: Vec<AwakeVm> = mine
                .iter()
                .filter(|entry| entry.awake.load(Ordering::SeqCst))
                .map(|entry| AwakeVm {
                    vm: entry.id(),
                    workspace: entry.workspace.clone(),
                    size: *lock(&entry.size),
                })
                .collect();
            (
                vms.get(&id).cloned(),
                awake,
                u32::try_from(mine.len()).unwrap_or(u32::MAX),
            )
        };
        if let Some(entry) = &existing_entry
            && entry.account != request.account
        {
            return Err(EnsureError::Invalid(format!(
                "the VM of workspace {} belongs to another account",
                request.workspace
            )));
        }
        admit(
            plan,
            &id,
            request.size,
            &awake,
            existing,
            existing_entry.is_some(),
        )
        .map_err(EnsureError::Refused)?;
        let entry = match existing_entry {
            Some(entry) => entry,
            None => self
                .insert(
                    VmIdentity {
                        vm: id,
                        workspace: request.workspace.clone(),
                        environment: request.environment.clone(),
                        account: request.account.clone(),
                        size: request.size,
                    },
                    self.config.files(&request.workspace, &request.environment),
                )
                .map_err(EnsureError::Vm)?,
        };
        entry.awake.store(true, Ordering::SeqCst);
        Ok(entry)
    }

    /// Poll every awake VM; suspend the idle ones. A VM busy with a wake
    /// or a suspend is left for the next tick.
    pub fn tick(&self) {
        let awake: Vec<Arc<Entry>> = lock(&self.vms)
            .values()
            .filter(|entry| entry.awake.load(Ordering::SeqCst))
            .cloned()
            .collect();
        for entry in awake {
            let Ok(mut vm) = entry.vm.try_lock() else {
                continue;
            };
            if let Err(error) = vm.poll() {
                eprintln!("tilo-legacy hosted: {}: {error}", entry.id());
            }
            entry.awake.store(vm.awake(), Ordering::SeqCst);
        }
    }

    fn entry(&self, workspace: &str, environment: &str) -> Option<Arc<Entry>> {
        lock(&self.vms)
            .get(&format!("{workspace}/{environment}"))
            .cloned()
    }

    pub fn suspend(&self, workspace: &str, environment: &str) -> Result<(), VmError> {
        let Some(entry) = self.entry(workspace, environment) else {
            return Ok(());
        };
        let mut vm = lock(&entry.vm);
        let result = vm.suspend();
        entry.awake.store(vm.awake(), Ordering::SeqCst);
        result
    }

    pub fn stop(&self, workspace: &str, environment: &str) -> Result<(), VmError> {
        let Some(entry) = self.entry(workspace, environment) else {
            return Ok(());
        };
        let mut vm = lock(&entry.vm);
        let result = vm.stop();
        entry.awake.store(vm.awake(), Ordering::SeqCst);
        result
    }

    /// Suspend every awake VM: the service is stopping.
    pub fn suspend_all(&self) {
        let entries: Vec<Arc<Entry>> = lock(&self.vms).values().cloned().collect();
        for entry in entries {
            let mut vm = lock(&entry.vm);
            if vm.awake() && vm.suspend().is_err() {
                let _ = vm.stop();
            }
            entry.awake.store(vm.awake(), Ordering::SeqCst);
        }
    }

    #[must_use]
    pub fn status(&self) -> Vec<VmStatus> {
        let entries: Vec<Arc<Entry>> = lock(&self.vms).values().cloned().collect();
        entries
            .iter()
            .map(|entry| {
                let vm = lock(&entry.vm);
                let wake = vm.last_wake();
                VmStatus {
                    vm: entry.id(),
                    account: entry.account.clone(),
                    size: vm.identity().size,
                    phase: format!("{:?}", vm.phase()),
                    last_wake_ms: wake.map(|wake| wake.millis),
                    last_wake_restored: wake.map(|wake| wake.restored),
                    snapshot_bytes: vm.snapshot_bytes(),
                }
            })
            .collect()
    }

    pub fn handle(&self, request: Request) -> Reply {
        match request {
            Request::Ensure { vm, shares } => match self.ensure(&vm, &shares) {
                Ok((link, woke_ms)) => Reply::Ready { link, woke_ms },
                Err(EnsureError::Refused(refusal)) => Reply::Refused(refusal.to_string()),
                Err(error) => Reply::Error(error.to_string()),
            },
            Request::Suspend {
                workspace,
                environment,
            } => self
                .suspend(&workspace, &environment)
                .map_or_else(|error| Reply::Error(error.to_string()), |()| Reply::Done),
            Request::Stop {
                workspace,
                environment,
            } => self
                .stop(&workspace, &environment)
                .map_or_else(|error| Reply::Error(error.to_string()), |()| Reply::Done),
            Request::Status => Reply::Status(self.status()),
            Request::Usage {
                account,
                from_ms,
                to_ms,
            } => match self.ledger.records(&account) {
                Ok(records) => {
                    let records: Vec<_> = records
                        .into_iter()
                        .filter(|record| (from_ms..to_ms).contains(&record.end_ms))
                        .collect();
                    let by_size = summarize(&records, from_ms, to_ms);
                    Reply::Usage { records, by_size }
                }
                Err(error) => Reply::Error(error.to_string()),
            },
        }
    }

    /// Serve the socket until `stop` is set, polling awake VMs between.
    pub fn serve(self: &Arc<Self>, stop: &AtomicBool) -> std::io::Result<()> {
        let socket = self.config.socket();
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket)?;
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        }
        let accepting = Arc::clone(self);
        std::thread::spawn(move || accept_loop(&listener, &accepting));
        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(POLL);
            self.tick();
        }
        self.suspend_all();
        let _ = std::fs::remove_file(&socket);
        Ok(())
    }
}

fn accept_loop(listener: &UnixListener, service: &Arc<Service>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        if !crate::supervisor::same_user(&stream) {
            continue;
        }
        let service = Arc::clone(service);
        std::thread::spawn(move || answer(stream, &service));
    }
}

fn answer(mut stream: UnixStream, service: &Service) {
    if let Ok(Some(request)) = FRAMING.read_value::<_, Request>(&mut stream) {
        let reply = service.handle(request);
        let _ = FRAMING.write_value(&mut stream, &reply);
    }
}

fn read_names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| valid_name(name))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::firecracker::Machine;
    use super::super::meter::{OpenRun, RunEnd, UsageKind};
    use super::super::size::VmSize;
    use super::*;

    fn config(dir: &Path) -> ServiceConfig {
        ServiceConfig {
            state: dir.join("state"),
            run: dir.join("run"),
            plans: dir.join("plans.json"),
            settings: Settings {
                machine: Machine {
                    // Never reached: every wake here is refused first.
                    firecracker: dir.join("no-firecracker"),
                    kernel: dir.join("no-kernel"),
                    initramfs: None,
                    cmdline: Machine::DEFAULT_CMDLINE.into(),
                    base_disk: None,
                },
                idle_suspend: Duration::from_secs(30),
                self_exe: dir.join("tilo-legacy"),
            },
        }
    }

    fn request(workspace: &str, account: &str, size: VmSize) -> VmRequest {
        VmRequest {
            workspace: workspace.into(),
            environment: "default".into(),
            account: account.into(),
            size,
        }
    }

    fn clock(at: u64) -> Clock {
        Arc::new(move || at)
    }

    /// A VM the account already has, as a meter on disk.
    fn existing(config: &ServiceConfig, workspace: &str, account: &str, open: Option<OpenRun>) {
        let files = config.files(workspace, "default");
        let mut meter = Meter::new(VmIdentity {
            vm: format!("{workspace}/default"),
            workspace: workspace.into(),
            environment: "default".into(),
            account: account.into(),
            size: VmSize::Small,
        });
        meter.open = open;
        meter.save(&files.meter()).unwrap();
    }

    #[test]
    fn a_restarted_service_closes_the_runs_it_lost() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        existing(
            &config,
            "ws1",
            "acct-1",
            Some(OpenRun {
                start_ms: 1_000,
                last_seen_ms: 61_500,
            }),
        );
        let service = Service::open(config.clone(), clock(90_000)).unwrap();
        let records = service.ledger.records("acct-1").unwrap();
        assert_eq!(records.len(), 1);
        let record = records.first().unwrap();
        assert_eq!(
            (record.kind, record.ended, record.end_ms, record.seconds),
            (UsageKind::Running, Some(RunEnd::Recovered), 61_500, 60)
        );
        drop(service);
        let again = Service::open(config, clock(95_000)).unwrap();
        assert_eq!(
            again.ledger.records("acct-1").unwrap().len(),
            1,
            "a recovered run is closed once"
        );
        let Reply::Usage { by_size, .. } = again.handle(Request::Usage {
            account: "acct-1".into(),
            from_ms: 0,
            to_ms: u64::MAX,
        }) else {
            panic!("usage");
        };
        assert_eq!(by_size.first().map(|usage| usage.running_seconds), Some(60));
    }

    #[test]
    fn wakes_beyond_the_plan_are_refused_in_plain_words() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        std::fs::write(
            &config.plans,
            r#"{"default":{"max_running":1,"max_vms":2,"sizes":["small","default"]}}"#,
        )
        .unwrap();
        existing(&config, "ws1", "acct-1", None);
        existing(&config, "ws2", "acct-1", None);
        existing(&config, "ws9", "acct-2", None);
        let service = Service::open(config, clock(0)).unwrap();
        let refused = |request: &VmRequest| match service.handle(Request::Ensure {
            vm: request.clone(),
            shares: Vec::new(),
        }) {
            Reply::Refused(message) => message,
            other => panic!("expected a refusal, got {other:?}"),
        };
        // phase: a size outside the plan
        {
            let message = refused(&request("ws1", "acct-1", VmSize::Large));
            assert!(
                message.starts_with("This workspace's Linux VM is large"),
                "{message}"
            );
        }
        // phase: a third disk
        {
            let message = refused(&request("ws3", "acct-1", VmSize::Small));
            assert!(
                message.starts_with("Your plan keeps at most 2 Linux VMs"),
                "{message}"
            );
            assert!(
                !dir.path().join("state/vms/ws3").exists(),
                "nothing was created"
            );
        }
        // phase: a second VM running at once
        {
            lock(&service.vms)
                .get("ws1/default")
                .unwrap()
                .awake
                .store(true, Ordering::SeqCst);
            let message = refused(&request("ws2", "acct-1", VmSize::Small));
            assert!(
                message.starts_with("Your plan runs at most 1 Linux VM at a time, and 1 is running now: ws1 (small)."),
                "{message}"
            );
        }
        // phase: another account's workspace is not reachable by naming it
        {
            let reply = service.handle(Request::Ensure {
                vm: request("ws9", "acct-1", VmSize::Small),
                shares: Vec::new(),
            });
            assert!(
                matches!(&reply, Reply::Error(message) if message.contains("belongs to another account")),
                "{reply:?}"
            );
        }
        // phase: ids that are not file names never reach the disk
        {
            let reply = service.handle(Request::Ensure {
                vm: request("../etc", "acct-1", VmSize::Small),
                shares: Vec::new(),
            });
            assert!(matches!(reply, Reply::Error(_)), "{reply:?}");
        }
    }
}
