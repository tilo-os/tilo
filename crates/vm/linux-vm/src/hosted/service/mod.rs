//! The one process per host that owns every hosted VM. `tilo-linux-vm hosted serve`
//! runs it: it admits each wake against the account's plan, wakes the VM, polls the
//! awake ones and suspends the idle, and answers usage questions from the log.
//! Each VM is locked on its own, and admission is one lock. A host being taken back
//! takes no more wakes and suspends every VM at once onto its state directory,
//! which a service on another machine takes over.

use std::collections::BTreeMap;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use linux_vm_protocol::link::AgentLink;

use super::browser::{self, Daemon, Launcher, UP_PATIENCE};
use super::control::{
    FRAMING, HostLoad, OpenApp, OpenBrowser, Reply, Request, ShareRequest, VmRequest, VmStatus,
};
use super::files::Served;
use super::firecracker::VmFiles;
use super::memory::{self, Budget};
use super::meter::{Log, Meter, VmIdentity, summarize};
use super::move_vm::{self, MoveError, Part};
use super::plan::{AwakeVm, Plans, Refusal, admit};
use super::valid_name;
use super::vm::{Clock, HostedVm, Settings, VmError};
use crate::lifecycle::Phase;
use tilo_actions::remote::server_account;
use tilo_server_protocol::hosted_vm::{ENVIRONMENT, VmSize};

/// How often awake VMs are asked what runs in them.
pub const POLL: Duration = Duration::from_secs(1);

/// What a wake that waits for room is told, and its caller shows: a VM going idle makes room.
pub const BUSY: &str = "The machine is full until one of its VMs goes idle.";

/// How long a waiting wake counts as demand after it last asked: its
/// caller asks again every few seconds while it waits.
const WAITING_FOR_MS: u64 = 15_000;

/// How long a VM's daemon is given to end its sessions (Browser writes its profile back) and end
/// before it is killed.
const DAEMON_GRACE: Duration = Duration::from_secs(20);

/// How long after a template was last tried another try waits. A node that
/// could not spare the memory or whose boot failed gets another try while
/// whoever waited for the template is still waiting: ten minutes is long past
/// any check that asked for it.
const TEMPLATE_RETRY_MS: u64 = 60 * 1000;

/// What a request for a VM that was deleted while it waited is told.
const DELETED: &str = "the workspace's VM was deleted; ask again for a new one";

/// What a wake placed on a machine the VM has left is told.
pub const MOVED: &str =
    "the workspace's Linux VM moved to another machine, saved as it was; ask again in a minute";

/// How long one read or write of a move may take: a block is 4 MiB, so a link
/// that is there carries it in seconds.
const LINK_PATIENCE: Duration = Duration::from_secs(120);

#[derive(Clone, Debug)]
pub struct ServiceConfig {
    /// Durable: each VM's disk, snapshot and meter; the usage log.
    pub state: PathBuf,
    /// Sockets: the service's own, and each VM's.
    pub run: PathBuf,
    pub plans: PathBuf,
    pub settings: Settings,
}

impl ServiceConfig {
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.run.join("hosted.sock")
    }

    #[must_use]
    pub fn log(&self) -> Log {
        Log::new(self.state.join("usage"))
    }

    fn files(&self, workspace: &str, environment: &str) -> VmFiles {
        VmFiles {
            state: self.state.join("vms").join(workspace).join(environment),
            // Short: a Unix socket path has at most 107 bytes.
            run: self.run.join(format!(
                "{:016x}",
                fnv_hash(&format!("{workspace}/{environment}"))
            )),
            jailed: self.settings.jail.is_some(),
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
    size: Mutex<tilo_server_protocol::hosted_vm::VmSize>,
    /// Admitted and not yet seen asleep. Read without the VM's lock.
    awake: AtomicBool,
    vm: Mutex<HostedVm>,
    /// The VM's daemon, which runs its Browser and Linux apps sessions.
    daemon: Mutex<Option<Arc<Daemon>>>,
    memory: Mutex<Measured>,
    /// Deleted: its files are gone, and whoever still holds it wakes nothing.
    gone: AtomicBool,
}

/// MiB the VM's process holds of this host's memory; `None` until it is measured since it woke.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Measured {
    resident_mib: Option<u64>,
    /// MiB the VM's balloon holds back from its guest, which the host has.
    balloon_mib: u64,
    /// Whether the VM's balloon took a target: `None` until one is set,
    /// `Some(false)` when its Firecracker has no balloon device at all.
    balloon: Option<bool>,
}

/// What the host measures: what a running VM's process takes, and what the
/// host can still hand out. A test replaces them with fake VMs whose memory
/// grows; a host reads its kernel.
type Weigh = Arc<dyn Fn(&HostedVm) -> Option<u64> + Send + Sync>;
type Available = Arc<dyn Fn() -> Option<u64> + Send + Sync>;

impl Entry {
    fn id(&self) -> String {
        format!("{}/{}", self.workspace, self.environment)
    }
}

pub struct Service {
    config: ServiceConfig,
    log: Log,
    clock: Clock,
    vms: Mutex<BTreeMap<String, Arc<Entry>>>,
    admission: Mutex<()>,
    /// The host is being taken back: no more wakes.
    evicting: AtomicBool,
    budget: Budget,
    weigh: Weigh,
    available: Available,
    /// What the templates being made right now are counted at until each is
    /// a snapshot and no longer a VM.
    building_mib: AtomicU64,
    /// Wakes waiting for room: what each asks for, and when it last asked.
    waiting: Mutex<BTreeMap<String, (u64, u64)>>,
    /// When each size's template was last tried: building, or failed.
    templates: Mutex<BTreeMap<VmSize, u64>>,
    /// The VMs being made as branches of others, each with its account: no wake takes one, and
    /// each counts toward its account's VMs.
    branching: Mutex<BTreeMap<String, String>>,
}

#[derive(Debug)]
pub enum BrowserError {
    /// This host serves no Browser: its service was started without the
    /// server's endpoint, so no session in its VMs could join a workspace.
    NotOffered,
    Invalid(String),
    /// The account's plan does not allow it: a Linux app is a Linux action.
    Refused(server_account::Refusal),
    /// Not now: every machine is full ([`BUSY`]). The caller waits for
    /// room and asks again.
    Full(String),
    Session(String),
    Vm(VmError),
}

impl From<EnsureError> for BrowserError {
    fn from(error: EnsureError) -> Self {
        match error {
            EnsureError::HostFull => Self::Full(BUSY.to_owned()),
            EnsureError::Vm(error) => Self::Vm(error),
            EnsureError::Invalid(message) => Self::Invalid(message),
            EnsureError::Refused(refusal) => Self::Refused(refusal),
            EnsureError::Moving | EnsureError::Moved => {
                Self::Session(EnsureError::Moving.to_string())
            }
            EnsureError::NoVm(workspace) => Self::Invalid(no_vm_yet(&workspace)),
        }
    }
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotOffered => f.write_str(
                "this host serves no Browser: it was started without the server's endpoint",
            ),
            Self::Invalid(message) => f.write_str(message),
            Self::Refused(refusal) => f.write_str(&refusal.message),
            Self::Full(words) => f.write_str(words),
            Self::Session(message) => write!(f, "Browser did not open: {message}"),
            Self::Vm(error) => write!(f, "the workspace's VM could not start: {error}"),
        }
    }
}

#[derive(Debug)]
pub enum EnsureError {
    Refused(server_account::Refusal),
    Invalid(String),
    Vm(VmError),
    /// The host is being taken back; the VM continues on another one.
    Moving,
    /// Waking it would take the host's VMs past its memory budget: it
    /// waits for room ([`BUSY`]).
    HostFull,
    /// The VM moved to another machine: this machine holds no disk of it, so a wake here would make an
    /// empty machine in its place.
    Moved,
    /// The workspace named has no VM, so there is nothing to branch from.
    NoVm(String),
}

impl std::fmt::Display for EnsureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(f),
            Self::Invalid(message) => f.write_str(message),
            Self::Vm(error) => write!(f, "the workspace's Linux VM could not start: {error}"),
            Self::Moving => f.write_str(
                "the workspace's Linux VM is moving to another machine, saved as it was; \
                 run the command again in a minute",
            ),
            Self::HostFull => f.write_str(BUSY),
            Self::Moved => f.write_str(MOVED),
            Self::NoVm(workspace) => f.write_str(&no_vm_yet(workspace)),
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
        let log = config.log();
        let service = Self {
            log,
            clock,
            vms: Mutex::new(BTreeMap::new()),
            admission: Mutex::new(()),
            evicting: AtomicBool::new(false),
            budget: Budget::default(),
            weigh: Arc::new(|vm: &HostedVm| vm.resident_mib()),
            available: Arc::new(memory::available_mib),
            building_mib: AtomicU64::new(0),
            waiting: Mutex::new(BTreeMap::new()),
            templates: Mutex::new(BTreeMap::new()),
            branching: Mutex::new(BTreeMap::new()),
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

    /// Admit by what the kernel measures, with `budget`'s reserve kept free; a wake beyond that
    /// waits for room.
    #[must_use]
    pub const fn with_memory(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self
    }

    #[must_use]
    pub fn with_measure(mut self, weigh: Weigh, available: Available) -> Self {
        self.weigh = weigh;
        self.available = available;
        self
    }

    #[must_use]
    pub const fn config(&self) -> &ServiceConfig {
        &self.config
    }

    fn insert(&self, identity: VmIdentity, files: VmFiles) -> Result<Arc<Entry>, VmError> {
        let log = Some(self.log.clone());
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
                log,
                Arc::clone(&self.clock),
            )?),
            daemon: Mutex::new(None),
            memory: Mutex::new(Measured::default()),
            gone: AtomicBool::new(false),
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
                if linux_vm_protocol::share_path_allowed(&share.path) && share.path.is_dir() {
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
        let entry = self.admit(request, true)?;
        let mut vm = lock(&entry.vm);
        if entry.gone.load(Ordering::SeqCst) {
            return Err(EnsureError::Invalid(DELETED.to_owned()));
        }
        let result = (|| {
            // The size named is what a VM that is not running would be made at: an
            // awake machine runs the command at the size it has.
            vm.want_size(request.size)?;
            *lock(&entry.size) = vm.identity().size;
            let (link, woke) = vm.ensure(&served, true)?;
            Ok((link, woke.map(|wake| wake.millis)))
        })();
        entry.awake.store(vm.awake(), Ordering::SeqCst);
        drop(vm);
        result.map_err(EnsureError::Vm)
    }

    /// The entry for `request`, reserved as awake, or why not. A Linux action (`linux`) needs the
    /// account's plan to allow it, and a card on a host that sells its VMs; a browser action needs
    /// neither.
    fn admit(&self, request: &VmRequest, linux: bool) -> Result<Arc<Entry>, EnsureError> {
        let _admission = lock(&self.admission);
        // Under the admission lock, so no wake slips in after the eviction's
        // suspends began.
        if self.evicting.load(Ordering::SeqCst) {
            return Err(EnsureError::Moving);
        }
        // The VM moved to another machine, and this one holds no disk of it: a wake here would make
        // an empty machine in its place.
        if self
            .config
            .files(&request.workspace, &request.environment)
            .state
            .join("moved")
            .is_file()
        {
            return Err(EnsureError::Moved);
        }
        let plans = Plans::load(&self.config.plans).map_err(EnsureError::Invalid)?;
        let id = format!("{}/{}", request.workspace, request.environment);
        let (existing_entry, awake, existing) = {
            let vms = lock(&self.vms);
            lock(&self.waiting).remove(&id);
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
        let existing = existing.saturating_add(self.branches_of(&request.account));
        if lock(&self.branching).contains_key(&id) {
            return Err(EnsureError::Invalid(format!(
                "the VM of workspace {} is being made from another workspace's; ask again in a \
                 moment",
                request.workspace
            )));
        }
        if let Some(entry) = &existing_entry
            && entry.account != request.account
        {
            return Err(EnsureError::Invalid(format!(
                "the VM of workspace {} belongs to another account",
                request.workspace
            )));
        }
        // Under the admission lock, so the room it finds is the room the wake
        // takes. A VM that already runs takes no more of the host's memory:
        // its next command runs even where the host is full, so a workspace is
        // never refused its own machine.
        let runs = existing_entry
            .as_ref()
            .is_some_and(|entry| entry.awake.load(Ordering::SeqCst));
        if !runs && !self.room_for(self.budget.expected_mib(), &id) {
            self.waits(&id);
            return Err(EnsureError::HostFull);
        }
        if linux {
            admit(
                &plans,
                &request.account,
                &id,
                request.size,
                &awake,
                existing,
                existing_entry.is_some(),
            )
            .map_err(|refusal| {
                EnsureError::Refused(refusal.typed(plans.sells.as_ref(), &request.account))
            })?;
        }
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

    fn used_mib(&self, except: &str) -> u64 {
        let expected = self.budget.expected_mib();
        lock(&self.vms)
            .values()
            .filter(|entry| entry.id() != except && entry.awake.load(Ordering::SeqCst))
            .map(|entry| lock(&entry.memory).resident_mib.unwrap_or(expected))
            .sum()
    }

    fn building_mib(&self) -> u64 {
        self.building_mib.load(Ordering::SeqCst)
    }

    fn room_for(&self, expected_mib: u64, except: &str) -> bool {
        let used = self.used_mib(except).saturating_add(self.building_mib());
        self.budget.admits(used, expected_mib) && self.free_for(expected_mib)
    }

    /// Whether the machine really has `expected_mib` to hand out over the host's own reserve: the
    /// arithmetic is what this host counted, this is what it has. The page cache of a hundred VMs'
    /// disks is available, but the kernel may not have given it back yet.
    fn free_for(&self, expected_mib: u64) -> bool {
        (self.available)()
            .is_none_or(|free| free >= self.budget.reserve_mib.saturating_add(expected_mib))
    }

    /// Note that `id` waits for room: the demand the host's load reports.
    fn waits(&self, id: &str) {
        lock(&self.waiting).insert(id.to_owned(), (self.budget.expected_mib(), (self.clock)()));
    }

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
            let held = held_sessions(&entry);
            match vm.poll(held) {
                Err(error) => eprintln!("tilo-linux-vm hosted: {}: {error}", entry.id()),
                // A VM that goes to sleep says what took it for idle: its own id and
                // what the host counted as running in it.
                Ok(()) if vm.phase() == Phase::Suspending => {
                    eprintln!(
                        "tilo-linux-vm hosted: {}: suspending (nothing ran in its guest, {held} session(s) held)",
                        entry.id()
                    );
                }
                Ok(()) => {}
            }
            let measured = (self.weigh)(&vm);
            // What holds memory runs: its process is what the host weighs.
            let up = vm.awake() || measured.is_some();
            entry.awake.store(up, Ordering::SeqCst);
            lock(&entry.memory).resident_mib = measured;
        }
        self.reclaim();
    }

    /// Keep the host's own memory free: while it has less than the reserve, ask the awake VMs'
    /// guests for a step back, the biggest first; once it has plenty, give one step back a tick.
    /// Nothing is ever killed here: a host that cannot reclaim refuses the next VM instead, and a
    /// running workspace keeps its VM and its open page.
    fn reclaim(&self) {
        let Some(free) = (self.available)() else {
            return;
        };
        let reserve = self.budget.reserve_mib;
        let short = free < reserve;
        let plenty = free > reserve.saturating_add(memory::BALLOON_STEP_MIB.saturating_mul(2));
        if !short && !plenty {
            return;
        }
        let mut awake: Vec<Arc<Entry>> = lock(&self.vms)
            .values()
            .filter(|entry| entry.awake.load(Ordering::SeqCst))
            .cloned()
            .collect();
        awake.sort_by_key(|entry| std::cmp::Reverse(lock(&entry.memory).resident_mib.unwrap_or(0)));
        for entry in awake {
            let Ok(vm) = entry.vm.try_lock() else {
                continue;
            };
            let size_mib = lock(&entry.size).memory_mib();
            let (current_mib, balloon) = {
                let measured = lock(&entry.memory);
                (measured.balloon_mib, measured.balloon)
            };
            // A VM whose Firecracker has no balloon device has no memory to
            // give back, and no later call changes that.
            if balloon == Some(false) {
                continue;
            }
            let target = if short {
                memory::next_inflate_mib(current_mib, size_mib)
            } else {
                memory::next_deflate_mib(current_mib)
            };
            if target == current_mib {
                continue;
            }
            match vm.balloon(target) {
                Ok(true) => {
                    let mut measured = lock(&entry.memory);
                    measured.balloon_mib = target;
                    measured.balloon = Some(true);
                }
                Ok(false) => {
                    lock(&entry.memory).balloon = Some(false);
                    eprintln!(
                        "tilo-linux-vm hosted: {}: its Firecracker has no balloon device, so its \
                         memory is not the host's to reclaim",
                        entry.id()
                    );
                }
                Err(error) => eprintln!("tilo-linux-vm hosted: {}: balloon: {error}", entry.id()),
            }
            if plenty {
                return;
            }
        }
    }

    fn entry(&self, workspace: &str, environment: &str) -> Option<Arc<Entry>> {
        lock(&self.vms)
            .get(&format!("{workspace}/{environment}"))
            .cloned()
    }

    /// Start `request`'s workspace VM for its Browser: its daemon logged in with the workspace's
    /// placement, started now or running already, with the folder Browser keeps its profile in
    /// shared with the VM. The device the daemon logged in as, and what starting it took when this
    /// request started it.
    pub fn open_browser(
        &self,
        request: &OpenBrowser,
    ) -> Result<(Option<u64>, [u8; 32]), BrowserError> {
        // Before anything wakes: a placement the daemon could not log in with refuses the open.
        let material = self.hand_off(&request.material)?;
        let vm = VmRequest {
            workspace: request.workspace.clone(),
            environment: ENVIRONMENT.to_owned(),
            account: request.account.clone(),
            size: request.size,
        };
        for (what, name) in [("workspace", &vm.workspace), ("account", &vm.account)] {
            if !valid_name(name) {
                return Err(BrowserError::Invalid(format!(
                    "{name:?} is not a {what} id"
                )));
            }
        }
        let store = request
            .store
            .as_ref()
            .map(|store| {
                if !linux_vm_protocol::share_path_allowed(store) {
                    return Err(BrowserError::Invalid(format!(
                        "{} cannot be shared with a VM",
                        store.display()
                    )));
                }
                std::fs::create_dir_all(store)
                    .map(|()| store.clone())
                    .map_err(|error| BrowserError::Vm(error.into()))
            })
            .transpose()?;
        let entry = self.admit(&vm, false)?;
        let daemon = self.daemon_in(&entry, false, request.size, store.as_deref())?;
        logged_in(&entry, &daemon, &material, store.as_deref())
    }

    /// The placement a workspace's daemon is handed: the server's material, with the address its
    /// ticket names the server at replaced by the one its VM dials ([`browser::hand_off`]).
    fn hand_off(
        &self,
        material: &str,
    ) -> Result<tilo_server_protocol::hosted::Material, BrowserError> {
        let server = self
            .config
            .settings
            .server
            .ok_or(BrowserError::NotOffered)?;
        browser::hand_off(material, server.guest()).map_err(BrowserError::Invalid)
    }

    /// Start `request`'s workspace VM for its Linux apps, as [`Self::open_browser`] does for its
    /// Browser. A Linux app is a Linux action, so the account's plan must allow it and a host that
    /// sells its VMs needs a card.
    pub fn open_app(&self, request: &OpenApp) -> Result<(Option<u64>, [u8; 32]), BrowserError> {
        let material = self.hand_off(&request.material)?;
        let vm = VmRequest {
            workspace: request.workspace.clone(),
            environment: ENVIRONMENT.to_owned(),
            account: request.account.clone(),
            size: request.size,
        };
        for (what, name) in [("workspace", &vm.workspace), ("account", &vm.account)] {
            if !valid_name(name) {
                return Err(BrowserError::Invalid(format!(
                    "{name:?} is not a {what} id"
                )));
            }
        }
        let entry = self.admit(&vm, true)?;
        let daemon = self.daemon_in(&entry, true, request.size, None)?;
        logged_in(&entry, &daemon, &material, None)
    }

    /// `entry`'s VM's daemon, the VM woken at `size` with `store` mounted and an X display when
    /// `x11`: the one running, or one that starts when the workspace's placement is handed to it. A
    /// VM woken for its Linux apps bills its time as a Linux action does.
    fn daemon_in(
        &self,
        entry: &Entry,
        x11: bool,
        size: VmSize,
        store: Option<&Path>,
    ) -> Result<Arc<Daemon>, BrowserError> {
        let shares: Vec<Served> = store
            .iter()
            .map(|path| Served {
                path: path.to_path_buf(),
                read_only: false,
            })
            .collect();
        let mut vm = lock(&entry.vm);
        if entry.gone.load(Ordering::SeqCst) {
            return Err(BrowserError::Session(DELETED.to_owned()));
        }
        let woke = (|| {
            // A command runs the VM at the size it has when it is awake: the size
            // named is what a VM that is not running here would be made at.
            vm.want_size(size)?;
            *lock(&entry.size) = vm.identity().size;
            vm.ensure(&shares, x11)
        })();
        entry.awake.store(vm.awake(), Ordering::SeqCst);
        drop(vm);
        let (link, _) = woke.map_err(BrowserError::Vm)?;
        let log = self
            .config
            .files(&entry.workspace, &entry.environment)
            .state
            .join(browser::LOG);
        let mut held = lock(&entry.daemon);
        let running = held.as_ref().filter(|daemon| daemon.alive()).cloned();
        let daemon = running.unwrap_or_else(|| {
            let daemon = Arc::new(Daemon::booted(Box::new(Launcher::new(link, log))));
            *held = Some(Arc::clone(&daemon));
            daemon
        });
        drop(held);
        Ok(daemon)
    }

    /// End the daemon in `workspace`'s VM, and its sessions.
    pub fn stop_daemon(&self, workspace: &str) {
        if let Some(entry) = self.entry(workspace, ENVIRONMENT) {
            end_daemon(&entry, DAEMON_GRACE);
        }
    }

    /// Make the template of each size in use that has none. Called between polls. The VMs'
    /// sessions end at their limits in their own daemons.
    pub fn maintain(self: &Arc<Self>) {
        let now = (self.clock)();
        let entries: Vec<Arc<Entry>> = lock(&self.vms).values().cloned().collect();
        let sizes: std::collections::BTreeSet<VmSize> =
            entries.iter().map(|entry| *lock(&entry.size)).collect();
        for size in sizes {
            self.keep_template(size, now);
        }
    }

    /// Start making `size`'s template when it has none, there is room for it, and it was not tried
    /// lately. Admission counts a template at its guest's whole configured memory, so no VM is
    /// admitted on room the template may take; what the machine must have *free* for it is less.
    /// Warming reads a fixed set of the guest's files — the session, its runtime and its apps — and
    /// the pages it keeps are file-backed and shared with every clone of it, so a machine that can
    /// hold a clone can make the template. Asking it for the whole size there made such a machine
    /// make none and boot every VM cold.
    fn keep_template(self: &Arc<Self>, size: VmSize, now: u64) {
        let settings = &self.config.settings;
        if super::vm::template_dir(settings, size).is_none()
            || super::vm::template(settings, size).is_some()
            || !self.budget.admits(
                self.used_mib("").saturating_add(self.building_mib()),
                size.memory_mib(),
            )
            || !self.free_for(self.budget.expected_mib())
        {
            return;
        }
        {
            let mut tried = lock(&self.templates);
            if tried
                .get(&size)
                .is_some_and(|at| now.saturating_sub(*at) < TEMPLATE_RETRY_MS)
            {
                return;
            }
            tried.insert(size, now);
        }
        let counted = size.memory_mib();
        self.building_mib.fetch_add(counted, Ordering::SeqCst);
        let service = Arc::clone(self);
        std::thread::spawn(move || {
            let began = std::time::Instant::now();
            eprintln!("tilo-linux-vm hosted: making the {size} template");
            let made = service.make_template(size);
            service.building_mib.fetch_sub(counted, Ordering::SeqCst);
            let took = began.elapsed().as_secs();
            match made {
                Ok(()) => {
                    eprintln!("tilo-linux-vm hosted: the {size} template took {took}s");
                    eprintln!("tilo-linux-vm hosted: the {size} template is ready");
                }
                Err(error) => {
                    eprintln!("tilo-linux-vm hosted: the {size} template: {error} (after {took}s)");
                }
            }
        });
    }

    /// Make `size`'s template: boot a VM of that size from the guest image, warm its page cache,
    /// snapshot it, and keep it read-only for new VMs to be cloned from.
    pub fn make_template(&self, size: VmSize) -> Result<(), VmError> {
        let settings = &self.config.settings;
        let Some(state) = super::vm::template_dir(settings, size) else {
            return Ok(());
        };
        if state.exists() {
            std::fs::remove_dir_all(&state)?;
        }
        let files = VmFiles {
            state,
            run: self.config.run.join(format!("template-{size}")),
            jailed: settings.jail.is_some(),
        };
        let mut vm = HostedVm::open(
            VmIdentity {
                vm: format!("template/{size}"),
                workspace: "template".to_owned(),
                environment: size.as_str().to_owned(),
                account: String::new(),
                size,
            },
            files.clone(),
            Arc::new(settings.clone()),
            None,
            Arc::clone(&self.clock),
        )?;
        let made = (|| {
            vm.ensure(&[], false)?;
            vm.warm()?;
            vm.suspend()?;
            super::vm::freeze_template(settings, size, &files)
        })();
        if made.is_err() {
            let _ = vm.stop();
        }
        made
    }

    /// The VMs being made as branches for `account`.
    fn branches_of(&self, account: &str) -> u32 {
        let count = lock(&self.branching)
            .values()
            .filter(|of| *of == account)
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    /// Make `request`'s VM, which must not exist, a branch of workspace `from`'s VM in the same
    /// environment ([`Request::Branch`]): a copy of its disk as it is now ([`HostedVm::branch_disk`]),
    /// which boots cold and whose guest forgets `from`'s sessions before its first one. It is the
    /// account's whose `from`'s VM is, as this host recorded it, and the account's plan admits it
    /// as any new VM; it is at least `from`'s size. A branch that could not be made leaves no VM.
    pub fn branch(&self, request: &VmRequest, from: &str) -> Result<(), EnsureError> {
        for (what, name) in [
            ("workspace", request.workspace.as_str()),
            ("environment", request.environment.as_str()),
            ("account", request.account.as_str()),
            ("workspace", from),
        ] {
            if !valid_name(name) {
                return Err(EnsureError::Invalid(format!("{name:?} is not a {what} id")));
            }
        }
        if from == request.workspace {
            return Err(EnsureError::Invalid(format!(
                "the VM of workspace {from} is not branched into itself"
            )));
        }
        let id = format!("{}/{}", request.workspace, request.environment);
        let source = self.reserve_branch(request, from, &id)?;
        let made = self.make_branch(&source, request);
        lock(&self.branching).remove(&id);
        made
    }

    /// `from`'s VM, with `id` reserved for its branch, or why not.
    fn reserve_branch(
        &self,
        request: &VmRequest,
        from: &str,
        id: &str,
    ) -> Result<Arc<Entry>, EnsureError> {
        let _admission = lock(&self.admission);
        if self.evicting.load(Ordering::SeqCst) {
            return Err(EnsureError::Moving);
        }
        let plans = Plans::load(&self.config.plans).map_err(EnsureError::Invalid)?;
        let vms = lock(&self.vms);
        let source = vms
            .get(&format!("{from}/{}", request.environment))
            .cloned()
            .ok_or_else(|| EnsureError::NoVm(from.to_owned()))?;
        if source.account != request.account {
            return Err(EnsureError::Invalid(format!(
                "the VM of workspace {from} belongs to another account"
            )));
        }
        let target = self.config.files(&request.workspace, &request.environment);
        let mut branching = lock(&self.branching);
        if vms.contains_key(id) || branching.contains_key(id) || target.disk().exists() {
            return Err(EnsureError::Invalid(format!(
                "workspace {} already has a VM; a branch is made as a new one",
                request.workspace
            )));
        }
        let at_least = *lock(&source.size);
        if request.size < at_least {
            return Err(EnsureError::Invalid(format!(
                "the VM of workspace {from} is {at_least}, and a branch of it is no smaller"
            )));
        }
        let existing = vms
            .values()
            .filter(|entry| entry.account == request.account)
            .count()
            .saturating_add(
                branching
                    .values()
                    .filter(|account| **account == request.account)
                    .count(),
            );
        drop(vms);
        // A branch is made asleep: what runs is not its plan's question until it wakes.
        match admit(
            &plans,
            &request.account,
            id,
            request.size,
            &[],
            u32::try_from(existing).unwrap_or(u32::MAX),
            false,
        ) {
            Ok(()) | Err(Refusal::TooManyRunning { .. }) => {}
            Err(refusal) => {
                return Err(EnsureError::Refused(
                    refusal.typed(plans.sells.as_ref(), &request.account),
                ));
            }
        }
        branching.insert(id.to_owned(), request.account.clone());
        drop(branching);
        Ok(source)
    }

    fn make_branch(&self, source: &Entry, request: &VmRequest) -> Result<(), EnsureError> {
        let target = self.config.files(&request.workspace, &request.environment);
        let from = source.workspace.clone();
        // What an earlier branch cut short left holds no disk, so it is no VM.
        remove_dir_if_present(&target.state).map_err(|error| EnsureError::Vm(error.into()))?;
        let made = (|| {
            // First, so a host that stops between the copy and the meter still has its guest
            // forget `from`'s sessions at the branch's first wake.
            std::fs::create_dir_all(&target.state).map_err(|e| EnsureError::Vm(e.into()))?;
            std::fs::write(target.branched(), format!("{}\n", source.id()))
                .map_err(|e| EnsureError::Vm(e.into()))?;
            let mut vm = lock(&source.vm);
            if source.gone.load(Ordering::SeqCst) {
                return Ok(false);
            }
            if vm.phase() == Phase::Suspended {
                // It wakes for the copy, and takes this host's memory while it is awake.
                let _admission = lock(&self.admission);
                if self.evicting.load(Ordering::SeqCst) {
                    return Err(EnsureError::Moving);
                }
                if !self.room_for(self.budget.expected_mib(), &source.id()) {
                    return Err(EnsureError::HostFull);
                }
                source.awake.store(true, Ordering::SeqCst);
            }
            let copied = vm.branch_disk(&target, request.size);
            source.awake.store(vm.awake(), Ordering::SeqCst);
            drop(vm);
            copied.map_err(EnsureError::Vm)
        })();
        let made = match made {
            Ok(true) => self
                .insert(
                    VmIdentity {
                        vm: format!("{}/{}", request.workspace, request.environment),
                        workspace: request.workspace.clone(),
                        environment: request.environment.clone(),
                        account: source.account.clone(),
                        size: request.size,
                    },
                    target.clone(),
                )
                .map(drop)
                .map_err(EnsureError::Vm),
            Ok(false) => Err(EnsureError::NoVm(from)),
            Err(error) => Err(error),
        };
        if made.is_err()
            && let Err(error) = remove_vm_dir(&target.state)
        {
            eprintln!(
                "tilo-linux-vm hosted: {}/{}: what a failed branch left: {error}",
                request.workspace, request.environment
            );
        }
        made
    }

    /// Delete the VM ([`Request::Delete`]): stop it as [`Self::stop`] does, closing its run and its
    /// snapshot in the log, then remove its disk, snapshot and meter. The account's records stay
    /// in the log. One there is not is deleted already.
    pub fn delete(&self, workspace: &str, environment: &str) -> Result<(), EnsureError> {
        for (what, name) in [("workspace", workspace), ("environment", environment)] {
            if !valid_name(name) {
                return Err(EnsureError::Invalid(format!("{name:?} is not a {what} id")));
            }
        }
        let id = format!("{workspace}/{environment}");
        let files = self.config.files(workspace, environment);
        if self
            .config
            .settings
            .templates
            .as_ref()
            .is_some_and(|templates| files.state.starts_with(templates))
        {
            return Err(EnsureError::Invalid(
                "a template is the host's, not a workspace's VM".to_owned(),
            ));
        }
        if lock(&self.branching).contains_key(&id) {
            return Err(EnsureError::Invalid(format!(
                "the VM of workspace {workspace} is being made; ask again in a moment"
            )));
        }
        if let Some(entry) = self.entry(workspace, environment) {
            end_daemon(&entry, DAEMON_GRACE);
            // Held until its files are gone: a wake that waited for it finds it gone, never a VM
            // half removed.
            let mut vm = lock(&entry.vm);
            let stopped = vm.stop();
            entry.awake.store(vm.awake(), Ordering::SeqCst);
            stopped.map_err(EnsureError::Vm)?;
            entry.gone.store(true, Ordering::SeqCst);
            self.forget(&id, &entry);
            let removed = remove_vm_dir(&files.state);
            drop(vm);
            removed.map_err(|error| EnsureError::Vm(error.into()))?;
        } else {
            remove_vm_dir(&files.state).map_err(|error| EnsureError::Vm(error.into()))?;
        }
        Ok(())
    }

    /// No longer hold `entry` as `id`'s VM.
    fn forget(&self, id: &str, entry: &Arc<Entry>) {
        let _admission = lock(&self.admission);
        let mut vms = lock(&self.vms);
        if vms.get(id).is_some_and(|held| Arc::ptr_eq(held, entry)) {
            vms.remove(id);
        }
        drop(vms);
        lock(&self.waiting).remove(id);
    }

    pub fn suspend(&self, workspace: &str, environment: &str) -> Result<(), VmError> {
        let Some(entry) = self.entry(workspace, environment) else {
            return Ok(());
        };
        eprintln!("tilo-linux-vm hosted: {}: asked to suspend", entry.id());
        end_daemon(&entry, DAEMON_GRACE);
        let mut vm = lock(&entry.vm);
        let result = vm.suspend();
        entry.awake.store(vm.awake(), Ordering::SeqCst);
        result
    }

    pub fn stop(&self, workspace: &str, environment: &str) -> Result<(), VmError> {
        let Some(entry) = self.entry(workspace, environment) else {
            return Ok(());
        };
        end_daemon(&entry, DAEMON_GRACE);
        let mut vm = lock(&entry.vm);
        let result = vm.stop();
        entry.awake.store(vm.awake(), Ordering::SeqCst);
        result
    }

    pub fn suspend_all(&self) {
        let entries: Vec<Arc<Entry>> = lock(&self.vms).values().cloned().collect();
        for entry in entries {
            end_daemon(&entry, DAEMON_GRACE);
            let mut vm = lock(&entry.vm);
            if vm.awake() && vm.suspend().is_err() {
                let _ = vm.stop();
            }
            entry.awake.store(vm.awake(), Ordering::SeqCst);
        }
    }

    /// The host is being taken back: take no more wakes, and suspend every awake VM onto the state
    /// directory at once, since the notice is short and each suspend mostly waits on its disk. A VM
    /// whose suspend fails is stopped, so its disk is consistent and it boots afresh.
    pub fn evict(&self) {
        {
            let _admission = lock(&self.admission);
            self.evicting.store(true, Ordering::SeqCst);
        }
        let entries: Vec<Arc<Entry>> = lock(&self.vms).values().cloned().collect();
        std::thread::scope(|scope| {
            for entry in &entries {
                scope.spawn(move || {
                    // A short grace: the notice is about 30 seconds.
                    end_daemon(entry, Duration::from_secs(2));
                    let mut vm = lock(&entry.vm);
                    if vm.awake()
                        && let Err(error) = vm.suspend()
                    {
                        eprintln!("tilo-linux-vm hosted: {}: suspend: {error}", entry.id());
                        let _ = vm.stop();
                    }
                    entry.awake.store(vm.awake(), Ordering::SeqCst);
                });
            }
        });
    }

    #[must_use]
    pub fn evicting(&self) -> bool {
        self.evicting.load(Ordering::SeqCst)
    }

    /// Move `workspace`'s VM to the machine at the other end of `link`, which the lease now names:
    /// suspend it, stream its disk and snapshot there, and forget it here once that machine has all
    /// of it. This is the one hand-off a lease calls; the link is the connection the machines keep.
    pub fn move_vm<L: move_vm::Link + ?Sized>(
        &self,
        workspace: &str,
        environment: &str,
        link: &mut L,
    ) -> Result<u64, MoveError> {
        for (what, name) in [("workspace", workspace), ("environment", environment)] {
            if !valid_name(name) {
                return Err(MoveError::Invalid(format!("{name:?} is not a {what} id")));
            }
        }
        let Some(entry) = self.entry(workspace, environment) else {
            return Err(MoveError::Invalid(format!(
                "this machine holds no VM of workspace {workspace}"
            )));
        };
        if entry.gone.load(Ordering::SeqCst) {
            return Err(MoveError::Invalid(DELETED.to_owned()));
        }
        let id = entry.id();
        let files = self.config.files(workspace, environment);
        end_daemon(&entry, DAEMON_GRACE);
        // Held for the transfer: a wake that waited for it finds it gone, never the disk half sent.
        let mut vm = lock(&entry.vm);
        if vm.awake()
            && let Err(error) = vm.suspend()
        {
            // A VM that could not be saved is stopped, so what moves is a disk that is whole and
            // boots afresh at the other end.
            eprintln!("tilo-linux-vm hosted: {id}: suspend for the move: {error}");
            let _ = vm.stop();
        }
        entry.awake.store(vm.awake(), Ordering::SeqCst);
        let sleeping = vm.phase() == Phase::Suspended;
        let parts: Vec<Part> = if sleeping {
            Part::ALL.to_vec()
        } else {
            vec![Part::Disk, Part::Config, Part::Meter]
        };
        let sent = move_vm::send(&files, &id, &parts, move_vm::Kind::Take, link)?;
        entry.gone.store(true, Ordering::SeqCst);
        self.forget(&id, &entry);
        drop(vm);
        forget_files(&files, &id);
        Ok(sent)
    }

    /// Take a VM over `link` from the machine that had it: it arrives asleep, and its next command
    /// restores it here. A replica copy stays the other machine's and is kept there.
    pub fn receive_vm<L: move_vm::Link + ?Sized>(&self, link: &mut L) -> Result<String, MoveError> {
        let offer = move_vm::hello(link)?;
        let (workspace, environment) = offer.parts()?;
        let id = offer.vm.clone();
        if lock(&self.vms).contains_key(&id) {
            return Err(move_vm::decline(
                link,
                format!("this machine already has the VM of workspace {workspace}"),
            ));
        }
        let files = self.config.files(&workspace, &environment);
        if offer.kind == move_vm::Kind::Take && files.disk().is_file() {
            return Err(move_vm::decline(
                link,
                format!("this machine already has a VM of workspace {workspace}"),
            ));
        }
        move_vm::receive(&offer, &files, link)?;
        if offer.kind == move_vm::Kind::Replica {
            // Kept for the day the machine holding it is lost; it lives there, not here.
            eprintln!("tilo-linux-vm hosted: copied the VM of workspace {workspace} as a replica");
            return Ok(id);
        }
        let Some(meter) = Meter::load(&files.meter()) else {
            let _ = remove_vm_dir(&files.state);
            return Err(MoveError::Invalid(format!(
                "the VM of workspace {workspace} arrived without its meter"
            )));
        };
        if meter.identity.vm != id
            || meter.identity.workspace != workspace
            || meter.identity.environment != environment
        {
            let _ = remove_vm_dir(&files.state);
            return Err(MoveError::Invalid(format!(
                "what arrived as the VM of workspace {workspace} is another's: {}",
                meter.identity.vm
            )));
        }
        let _admission = lock(&self.admission);
        self.insert(meter.identity, files).map_err(|error| {
            MoveError::Invalid(format!(
                "the VM of workspace {workspace} did not take here: {error}"
            ))
        })?;
        Ok(id)
    }

    /// Copy `workspace`'s VM to the replica machine at the other end of `link`, incrementally: only
    /// the blocks that changed since its last copy. A VM that is awake is paused for a consistent
    /// disk and goes on; only a suspended VM's snapshot goes with it. `Ok(bytes)`: the block bytes
    /// written.
    pub fn replicate_vm<L: move_vm::Link + ?Sized>(
        &self,
        workspace: &str,
        environment: &str,
        link: &mut L,
    ) -> Result<u64, MoveError> {
        for (what, name) in [("workspace", workspace), ("environment", environment)] {
            if !valid_name(name) {
                return Err(MoveError::Invalid(format!("{name:?} is not a {what} id")));
            }
        }
        let Some(entry) = self.entry(workspace, environment) else {
            return Err(MoveError::Invalid(format!(
                "this machine holds no VM of workspace {workspace}"
            )));
        };
        if entry.gone.load(Ordering::SeqCst) {
            return Err(MoveError::Invalid(DELETED.to_owned()));
        }
        let id = entry.id();
        let files = self.config.files(workspace, environment);
        let mut vm = lock(&entry.vm);
        if vm.awake() {
            // A running VM's disk moves under the copy: the guest flushes and the VM is paused
            // for it, and no snapshot goes (what it holds is in its memory, not on its disk).
            let parts = [Part::Disk, Part::Config, Part::Meter];
            Ok(vm.holding_disk(|| {
                move_vm::send(&files, &id, &parts, move_vm::Kind::Replica, link)
            })??)
        } else {
            let parts: Vec<Part> = if files.snapshot().valid() {
                Part::ALL.to_vec()
            } else {
                vec![Part::Disk, Part::Config, Part::Meter]
            };
            Ok(move_vm::send(
                &files,
                &id,
                &parts,
                move_vm::Kind::Replica,
                link,
            )?)
        }
    }

    /// Dial the machine at `to` and move `workspace`'s VM there.
    pub fn move_to(&self, workspace: &str, environment: &str, to: &Path) -> Result<u64, MoveError> {
        let mut link = ask_link(to)?;
        self.move_vm(workspace, environment, &mut link)
    }

    /// Dial the replica machine at `to` and copy `workspace`'s VM there.
    pub fn replicate_to(
        &self,
        workspace: &str,
        environment: &str,
        to: &Path,
    ) -> Result<u64, MoveError> {
        let mut link = ask_link(to)?;
        self.replicate_vm(workspace, environment, &mut link)
    }

    /// Serve the socket between machines at `socket` until the process ends: each connection carries
    /// what the other machine sends, a VM that moves or a replica copy. Same user only, as the
    /// service's own socket is.
    pub fn serve_moves(self: &Arc<Self>, socket: &Path) -> std::io::Result<()> {
        let _ = std::fs::remove_file(socket);
        let listener = tilo_files::native::socket::bind(socket)?;
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
        }
        let service = Arc::clone(self);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                if !crate::peer::same_user(&stream) {
                    continue;
                }
                let service = Arc::clone(&service);
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(LINK_PATIENCE));
                    let _ = stream.set_write_timeout(Some(LINK_PATIENCE));
                    match service.receive_vm(&mut stream) {
                        Ok(vm) => eprintln!("tilo-linux-vm hosted: took the VM of {vm} over"),
                        Err(error) => eprintln!("tilo-linux-vm hosted: taking a VM: {error}"),
                    }
                });
            }
        });
        Ok(())
    }

    #[must_use]
    pub fn load(&self) -> HostLoad {
        let used_mib = self.used_mib("").saturating_add(self.building_mib());
        let now = (self.clock)();
        let mut waiting = lock(&self.waiting);
        waiting.retain(|_, (_, at)| now.saturating_sub(*at) <= WAITING_FOR_MS);
        HostLoad {
            budget_mib: self.budget.for_vms_mib(),
            awake_mib: used_mib,
            waiting_mib: waiting.values().map(|(mib, _)| mib).sum(),
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
                let measured = *lock(&entry.memory);
                VmStatus {
                    vm: entry.id(),
                    account: entry.account.clone(),
                    size: vm.identity().size,
                    phase: format!("{:?}", vm.phase()),
                    last_wake_ms: wake.map(|wake| wake.millis),
                    last_wake_restored: wake.map(|wake| wake.restored),
                    snapshot_bytes: vm.snapshot_bytes(),
                    resident_mib: measured.resident_mib,
                    balloon_mib: measured.balloon_mib,
                }
            })
            .collect()
    }

    pub fn handle(&self, request: Request) -> Reply {
        match request {
            Request::Ensure { vm, shares } => match self.ensure(&vm, &shares) {
                Ok((link, woke_ms)) => Reply::Ready { link, woke_ms },
                Err(EnsureError::Refused(refusal)) => Reply::Refused(refusal),
                Err(EnsureError::HostFull) => Reply::Busy(BUSY.to_owned()),
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
            Request::Status => Reply::Status {
                vms: self.status(),
                load: self.load(),
            },
            Request::OpenBrowser { browser } => match self.open_browser(&browser) {
                Ok((opened_ms, device)) => Reply::Opened {
                    opened_ms,
                    device: hex::encode(device),
                },
                Err(BrowserError::Full(words)) => Reply::Busy(words),
                Err(error) => Reply::Error(error.to_string()),
            },
            Request::OpenApp { app } => match self.open_app(&app) {
                Ok((opened_ms, device)) => Reply::Opened {
                    opened_ms,
                    device: hex::encode(device),
                },
                Err(BrowserError::Refused(refusal)) => Reply::Refused(refusal),
                Err(BrowserError::Full(words)) => Reply::Busy(words),
                Err(error) => Reply::Error(error.to_string()),
            },
            Request::StopBrowser { workspace } | Request::StopApps { workspace } => {
                self.stop_daemon(&workspace);
                Reply::Done
            }
            Request::Branch { vm, from } => self.answer_branch(&vm, &from),
            Request::Delete {
                workspace,
                environment,
            } => self.answer_delete(&workspace, &environment),
            Request::MoveVm {
                workspace,
                environment,
                to,
            } => match self.move_to(&workspace, &environment, &to) {
                Ok(bytes) => Reply::Moved {
                    vm: format!("{workspace}/{environment}"),
                    bytes,
                },
                Err(error) => Reply::Error(error.to_string()),
            },
            Request::ReplicateVm {
                workspace,
                environment,
                to,
            } => match self.replicate_to(&workspace, &environment, &to) {
                Ok(bytes) => Reply::Moved {
                    vm: format!("{workspace}/{environment}"),
                    bytes,
                },
                Err(error) => Reply::Error(error.to_string()),
            },
            Request::Drain => {
                self.evict();
                Reply::Done
            }
            Request::Usage {
                account,
                from_ms,
                to_ms,
            } => match self.log.records(&account) {
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

    fn answer_branch(&self, vm: &VmRequest, from: &str) -> Reply {
        match self.branch(vm, from) {
            Ok(()) => Reply::Done,
            Err(EnsureError::NoVm(workspace)) => Reply::NoVm { workspace },
            Err(EnsureError::Refused(refusal)) => Reply::Refused(refusal),
            Err(EnsureError::HostFull) => Reply::Busy(format!(
                "the VM of workspace {from} is asleep, and this host has no room to wake it for \
                 the copy now; ask again in a minute"
            )),
            Err(EnsureError::Vm(error)) => Reply::Error(format!(
                "the VM of workspace {from} could not be branched: {error}"
            )),
            Err(error) => Reply::Error(error.to_string()),
        }
    }

    fn answer_delete(&self, workspace: &str, environment: &str) -> Reply {
        match self.delete(workspace, environment) {
            Ok(()) => Reply::Done,
            Err(EnsureError::Vm(error)) => Reply::Error(format!(
                "the VM of workspace {workspace} could not be deleted: {error}"
            )),
            Err(error) => Reply::Error(error.to_string()),
        }
    }

    /// Serve the socket until `stop` is set, polling awake VMs between.
    /// When `evict_notice` names a file, its appearing is the notice that
    /// the host is being taken back ([`Service::evict`]); whatever watches
    /// the cloud for that notice writes it.
    pub fn serve(
        self: &Arc<Self>,
        stop: &AtomicBool,
        evict_notice: Option<&Path>,
    ) -> std::io::Result<()> {
        let socket = self.config.socket();
        let _ = std::fs::remove_file(&socket);
        let listener = tilo_files::native::socket::bind(&socket)?;
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        }
        let accepting = Arc::clone(self);
        std::thread::spawn(move || accept_loop(&listener, &accepting));
        while !stop.load(Ordering::SeqCst) {
            if let Some(notice) = evict_notice
                && !self.evicting()
                && notice.exists()
            {
                eprintln!(
                    "tilo-linux-vm hosted: {} says this host is being taken back: suspending every VM",
                    notice.display()
                );
                self.evict();
                eprintln!("tilo-linux-vm hosted: every VM is saved");
            }
            std::thread::sleep(POLL);
            self.tick();
            self.maintain();
        }
        self.suspend_all();
        let _ = std::fs::remove_file(&socket);
        Ok(())
    }
}

/// `daemon` logged in with `material`, `store` named as Browser's: ended again when it does not.
fn logged_in(
    entry: &Entry,
    daemon: &Daemon,
    material: &tilo_server_protocol::hosted::Material,
    store: Option<&Path>,
) -> Result<(Option<u64>, [u8; 32]), BrowserError> {
    match daemon.open(material, store, UP_PATIENCE) {
        Ok((started_ms, device)) => {
            if let Some(started_ms) = started_ms {
                eprintln!(
                    "tilo-linux-vm hosted: {}: its daemon logged in {started_ms}ms after it \
                     started",
                    entry.id()
                );
            }
            Ok((started_ms, device))
        }
        Err(message) => {
            end_daemon(entry, DAEMON_GRACE);
            Err(BrowserError::Session(message))
        }
    }
}

/// Whether `entry`'s VM's daemon runs: what keeps the VM awake between the commands run in it,
/// as its guest's own programs do.
fn held_sessions(entry: &Entry) -> u32 {
    u32::from(
        lock(&entry.daemon)
            .as_ref()
            .is_some_and(|daemon| daemon.alive()),
    )
}

fn end_daemon(entry: &Entry, grace: Duration) {
    let daemon = lock(&entry.daemon).take();
    if let Some(daemon) = daemon {
        daemon.end(grace);
    }
}

fn accept_loop(listener: &UnixListener, service: &Arc<Service>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        if !crate::peer::same_user(&stream) {
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

/// What a branch of `workspace`'s VM is told when it has none.
fn no_vm_yet(workspace: &str) -> String {
    format!(
        "workspace {workspace} has no Linux VM yet, so there is nothing to branch: a new \
         workspace's VM starts as every workspace's does, at its first Linux command"
    )
}

/// What is left of a VM that moved: the marker a wake here reads, never a VM. The other machine has
/// its disk, its snapshot and its meter.
fn forget_files(files: &VmFiles, id: &str) {
    if let Err(error) = std::fs::write(
        files.state.join("moved"),
        format!("{id} left this machine; its disk is on another\n"),
    ) {
        eprintln!("tilo-linux-vm hosted: {id}: marking where it moved: {error}");
    }
    let removed = remove_dir_if_present(&files.vmm()).and_then(|()| {
        match std::fs::remove_file(files.meter()) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    });
    if let Err(error) = removed {
        eprintln!("tilo-linux-vm hosted: {id}: what the move left: {error}");
    }
    let _ = remove_dir_if_present(&files.run);
}

/// The connection to the machine at `to`: the socket the other machine serves what it is sent on.
fn ask_link(to: &Path) -> Result<UnixStream, MoveError> {
    let stream = tilo_files::native::socket::connect(to)?;
    stream.set_read_timeout(Some(LINK_PATIENCE))?;
    stream.set_write_timeout(Some(LINK_PATIENCE))?;
    Ok(stream)
}

/// Remove a VM's state directory, and its workspace's directory once it holds no other VM.
fn remove_vm_dir(state: &Path) -> std::io::Result<()> {
    remove_dir_if_present(state)?;
    if let Some(workspace) = state.parent() {
        // Only an empty one goes: another environment's VM keeps it.
        let _ = std::fs::remove_dir(workspace);
    }
    Ok(())
}

fn remove_dir_if_present(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
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
mod tests;
