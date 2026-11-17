//! The one process per host that owns every hosted VM. `tilo-legacy hosted serve`
//! runs it: it admits each wake against the account's plan, wakes the VM, polls the
//! awake ones and suspends the idle, and answers usage questions from the log.
//! Each VM is locked on its own, and admission is one lock. A host being taken back
//! takes no more wakes and suspends every VM at once onto its state directory,
//! which a service on another host takes over.

use std::collections::BTreeMap;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use legacy_agent_protocol::link::AgentLink;

use super::browser::{self, Browser, Kind, Launcher, Life, Limits, UP_PATIENCE};
use super::control::{
    ENVIRONMENT, FRAMING, HostLoad, OpenApp, OpenBrowser, Reply, Request, ShareRequest, VmRequest,
    VmStatus,
};
use super::files::Served;
use super::firecracker::VmFiles;
use super::memory::{self, Budget};
use super::meter::{Log, Meter, VmIdentity, summarize};
use super::plan::{AwakeVm, Plans, Refusal, admit};
use super::size::VmSize;
use super::valid_name;
use super::vm::{Clock, HostedVm, Settings, VmError};
use crate::lifecycle::Phase;
use tilo_actions::remote::host_account;

/// How often awake VMs are asked what runs in them.
pub const POLL: Duration = Duration::from_secs(1);

/// What a wake that waits for room is told, and its caller shows: a placer starts another machine
/// for the demand (about three minutes on Azure), or a VM going idle makes room sooner.
pub const BUSY: &str = "Starting another machine for it, about 3 minutes.";

/// How long a waiting wake counts as demand after it last asked: its
/// caller asks again every few seconds while it waits.
const WAITING_FOR_MS: u64 = 15_000;

/// How long a browser session is given to write its profile back and end
/// before it is killed.
const BROWSER_GRACE: Duration = Duration::from_secs(15);

/// How long after a template was last tried another try waits. A node that
/// could not spare the memory or whose boot failed gets another try while
/// whoever waited for the template is still waiting: ten minutes is long past
/// any check or placer that asked for it.
const TEMPLATE_RETRY_MS: u64 = 60 * 1000;

/// What a request for a VM that was deleted while it waited is told.
const DELETED: &str = "the workspace's VM was deleted; ask again for a new one";

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
    size: Mutex<super::size::VmSize>,
    /// Admitted and not yet seen asleep. Read without the VM's lock.
    awake: AtomicBool,
    vm: Mutex<HostedVm>,
    browser: Mutex<Option<Arc<Browser>>>,
    /// The apps session; the tenant's own, never the browser's.
    apps: Mutex<Option<Arc<Browser>>>,
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
    browser_limits: Limits,
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
    Refused(host_account::Refusal),
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
            EnsureError::Moving => Self::Session(EnsureError::Moving.to_string()),
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
    Refused(host_account::Refusal),
    Invalid(String),
    Vm(VmError),
    /// The host is being taken back; the VM continues on another one.
    Moving,
    /// Waking it would take the host's VMs past its memory budget: it
    /// waits for room ([`BUSY`]).
    HostFull,
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
                "the workspace's Linux VM is moving to another host, saved as it was; \
                 run the command again in a minute",
            ),
            Self::HostFull => f.write_str(BUSY),
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
            browser_limits: Limits::default(),
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
            browser: Mutex::new(None),
            apps: Mutex::new(None),
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

    /// Note that `id` waits for room: what a placer starts another machine
    /// for.
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
                Err(error) => eprintln!("tilo-legacy hosted: {}: {error}", entry.id()),
                // A VM that goes to sleep says what took it for idle: its own id and
                // what the host counted as running in it.
                Ok(()) if vm.phase() == Phase::Suspending => {
                    eprintln!(
                        "tilo-legacy hosted: {}: suspending (nothing ran in its guest, {held} session(s) held)",
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
                        "tilo-legacy hosted: {}: its Firecracker has no balloon device, so its \
                         memory is not the host's to reclaim",
                        entry.id()
                    );
                }
                Err(error) => eprintln!("tilo-legacy hosted: {}: balloon: {error}", entry.id()),
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

    /// Open `request`'s Browser: the session in the workspace's VM, started with the workspace's
    /// material, or the session already running. `Some(ms)` when this request handed it the
    /// workspace, `None` when it attached.
    pub fn open_browser(
        &self,
        request: &OpenBrowser,
    ) -> Result<(Option<u64>, String), BrowserError> {
        // Before anything wakes: a material the session could not join with
        // refuses the open.
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
                if !legacy_agent_protocol::share_path_allowed(store) {
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
        let browser = self.session_in(&entry, Kind::Browser, request.size, store)?;
        match browser.open(&material, UP_PATIENCE) {
            Ok(opened_ms) => {
                let provider = browser.life().provider().ok_or_else(|| {
                    BrowserError::Session("the session came up without a window".to_owned())
                })?;
                if let Some(opened_ms) = opened_ms {
                    eprintln!(
                        "tilo-legacy hosted: {}: Browser up {provider} {opened_ms}ms after hand-off",
                        entry.id()
                    );
                }
                Ok((opened_ms, provider))
            }
            Err(message) => {
                end_session(&entry, Kind::Browser, BROWSER_GRACE);
                Err(BrowserError::Session(message))
            }
        }
    }

    /// The line a workspace's session is handed: the server's material, with
    /// the server's address replaced by the one its VM dials
    /// ([`browser::hand_off`]).
    fn hand_off(&self, material: &str) -> Result<String, BrowserError> {
        let server = self
            .config
            .settings
            .server
            .ok_or(BrowserError::NotOffered)?;
        browser::hand_off(material, server.guest()).map_err(BrowserError::Invalid)
    }

    /// Start the Linux app `request` names in its workspace's VM: in the apps session there, or in
    /// one started now with the workspace's material. A Linux app is a Linux action, so the
    /// account's plan must allow it and a host that sells its VMs needs a card. `Some(ms)` when
    /// this request started the session.
    pub fn open_app(&self, request: &OpenApp) -> Result<Option<u64>, BrowserError> {
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
        let apps = self.session_in(&entry, Kind::Apps, request.size, None)?;
        let opened = match apps.open(&material, UP_PATIENCE) {
            Ok(opened) => opened,
            Err(message) => {
                end_session(&entry, Kind::Apps, BROWSER_GRACE);
                return Err(BrowserError::Session(message));
            }
        };
        apps.tell(&request.launch.line())
            .map_err(BrowserError::Session)?;
        eprintln!(
            "tilo-legacy hosted: {}: Linux app {} asked of its apps session{}",
            entry.id(),
            request.launch.desktop_id,
            opened.map_or_else(String::new, |ms| format!(", up {ms}ms after hand-off")),
        );
        Ok(opened)
    }

    pub fn stop_apps(&self, workspace: &str) {
        if let Some(entry) = self.entry(workspace, ENVIRONMENT) {
            end_session(&entry, Kind::Apps, BROWSER_GRACE);
        }
    }

    /// `kind`'s session in `entry`'s VM, woken at `size` with `store` mounted: the one running, or
    /// one that starts when the workspace's material is handed to it. A Linux apps session bills
    /// the VM's time as a Linux action does.
    fn session_in(
        &self,
        entry: &Entry,
        kind: Kind,
        size: VmSize,
        store: Option<PathBuf>,
    ) -> Result<Arc<Browser>, BrowserError> {
        let shares: Vec<Served> = store
            .iter()
            .map(|path| Served {
                path: path.clone(),
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
            vm.ensure(&shares, kind == Kind::Apps)
        })();
        entry.awake.store(vm.awake(), Ordering::SeqCst);
        drop(vm);
        let (link, _) = woke.map_err(BrowserError::Vm)?;
        let log = self
            .config
            .files(&entry.workspace, &entry.environment)
            .state
            .join(kind.log());
        let mut held = lock(session_slot(entry, kind));
        let running = held
            .as_ref()
            .filter(|browser| browser.life().ended_because().is_none())
            .cloned();
        let browser = running.unwrap_or_else(|| {
            let life = Arc::new(Life::new((self.clock)()));
            let starts = Launcher::new(
                link,
                kind,
                store,
                Arc::clone(&life),
                log,
                Arc::clone(&self.clock),
            );
            let browser = Arc::new(Browser::booted(life, Box::new(starts)));
            *held = Some(Arc::clone(&browser));
            browser
        });
        drop(held);
        Ok(browser)
    }

    pub fn stop_browser(&self, workspace: &str) {
        if let Some(entry) = self.entry(workspace, ENVIRONMENT) {
            end_session(&entry, Kind::Browser, BROWSER_GRACE);
        }
    }

    /// End the browser sessions at their limits, each on a thread of its own so it can write its
    /// profile, and make the template of each size in use that has none. Called between polls.
    pub fn maintain(self: &Arc<Self>) {
        let now = (self.clock)();
        let entries: Vec<Arc<Entry>> = lock(&self.vms).values().cloned().collect();
        let sizes: std::collections::BTreeSet<VmSize> =
            entries.iter().map(|entry| *lock(&entry.size)).collect();
        for size in sizes {
            self.keep_template(size, now);
        }
        for entry in entries {
            for kind in [Kind::Browser, Kind::Apps] {
                let end = lock(session_slot(&entry, kind))
                    .as_ref()
                    .and_then(|session| session.expired(now, self.browser_limits));
                if let Some(end) = end {
                    eprintln!(
                        "tilo-legacy hosted: {}: {kind:?} session ended ({end})",
                        entry.id()
                    );
                    let entry = Arc::clone(&entry);
                    std::thread::spawn(move || end_session(&entry, kind, BROWSER_GRACE));
                }
            }
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
            eprintln!("tilo-legacy hosted: making the {size} template");
            let made = service.make_template(size);
            service.building_mib.fetch_sub(counted, Ordering::SeqCst);
            let took = began.elapsed().as_secs();
            match made {
                Ok(()) => {
                    eprintln!("tilo-legacy hosted: the {size} template took {took}s");
                    eprintln!("tilo-legacy hosted: the {size} template is ready");
                }
                Err(error) => {
                    eprintln!("tilo-legacy hosted: the {size} template: {error} (after {took}s)");
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
                "tilo-legacy hosted: {}/{}: what a failed branch left: {error}",
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
            end_sessions(&entry, BROWSER_GRACE);
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
        let _ = remove_dir_if_present(&files.run);
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
        eprintln!("tilo-legacy hosted: {}: asked to suspend", entry.id());
        end_sessions(&entry, BROWSER_GRACE);
        let mut vm = lock(&entry.vm);
        let result = vm.suspend();
        entry.awake.store(vm.awake(), Ordering::SeqCst);
        result
    }

    pub fn stop(&self, workspace: &str, environment: &str) -> Result<(), VmError> {
        let Some(entry) = self.entry(workspace, environment) else {
            return Ok(());
        };
        end_sessions(&entry, BROWSER_GRACE);
        let mut vm = lock(&entry.vm);
        let result = vm.stop();
        entry.awake.store(vm.awake(), Ordering::SeqCst);
        result
    }

    pub fn suspend_all(&self) {
        let entries: Vec<Arc<Entry>> = lock(&self.vms).values().cloned().collect();
        for entry in entries {
            end_sessions(&entry, BROWSER_GRACE);
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
                    end_sessions(entry, Duration::from_secs(2));
                    let mut vm = lock(&entry.vm);
                    if vm.awake()
                        && let Err(error) = vm.suspend()
                    {
                        eprintln!("tilo-legacy hosted: {}: suspend: {error}", entry.id());
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
                Ok((opened_ms, provider)) => Reply::Opened {
                    opened_ms,
                    provider,
                },
                Err(BrowserError::Full(words)) => Reply::Busy(words),
                Err(error) => Reply::Error(error.to_string()),
            },
            Request::StopBrowser { workspace } => {
                self.stop_browser(&workspace);
                Reply::Done
            }
            Request::OpenApp { app } => match self.open_app(&app) {
                Ok(opened_ms) => Reply::Opened {
                    opened_ms,
                    provider: tilo_actions::remote::linux_apps::UP_PROVIDER.to_owned(),
                },
                Err(BrowserError::Refused(refusal)) => Reply::Refused(refusal),
                Err(BrowserError::Full(words)) => Reply::Busy(words),
                Err(error) => Reply::Error(error.to_string()),
            },
            Request::StopApps { workspace } => {
                self.stop_apps(&workspace);
                Reply::Done
            }
            Request::Place { .. } => {
                Reply::Error("a host runs VMs; a placer answers where".to_owned())
            }
            Request::Branch { vm, from } => self.answer_branch(&vm, &from),
            Request::Delete {
                workspace,
                environment,
            } => self.answer_delete(&workspace, &environment),
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
                    "tilo-legacy hosted: {} says this host is being taken back: suspending every VM",
                    notice.display()
                );
                self.evict();
                eprintln!("tilo-legacy hosted: every VM is saved");
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

impl Service {
    /// Answer [`Request::Status`] on `listener`, and nothing else: what a
    /// placer reads of this host over the network. Waking, stopping and usage
    /// stay on the service's own socket.
    pub fn serve_status(self: &Arc<Self>, listener: std::net::TcpListener) {
        let service = Arc::clone(self);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let service = Arc::clone(&service);
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    if let Ok(Some(request)) = FRAMING.read_value::<_, Request>(&mut stream) {
                        let reply = match request {
                            Request::Status => service.handle(Request::Status),
                            _ => Reply::Error("this listener answers Status only".to_owned()),
                        };
                        let _ = FRAMING.write_value(&mut stream, &reply);
                    }
                });
            }
        });
    }
}

fn end_sessions(entry: &Entry, grace: Duration) {
    end_session(entry, Kind::Apps, grace);
    end_session(entry, Kind::Browser, grace);
}

/// The sessions the host started in `entry`'s VM that are still running: what
/// keeps the VM awake between the commands run in it, as its guest's own
/// programs do.
fn held_sessions(entry: &Entry) -> u32 {
    [Kind::Browser, Kind::Apps]
        .into_iter()
        .filter(|kind| {
            lock(session_slot(entry, *kind))
                .as_ref()
                .is_some_and(|session| session.life().alive())
        })
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

fn session_slot(entry: &Entry, kind: Kind) -> &Mutex<Option<Arc<Browser>>> {
    match kind {
        Kind::Browser => &entry.browser,
        Kind::Apps => &entry.apps,
    }
}

fn end_session(entry: &Entry, kind: Kind, grace: Duration) {
    let session = lock(session_slot(entry, kind)).take();
    if let Some(browser) = session {
        browser.end(grace);
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

/// What a branch of `workspace`'s VM is told when it has none.
fn no_vm_yet(workspace: &str) -> String {
    format!(
        "workspace {workspace} has no Linux VM yet, so there is nothing to branch: a new \
         workspace's VM starts as every workspace's does, at its first Linux command"
    )
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
mod tests {
    use std::collections::BTreeSet;
    use std::io::{Read as _, Write as _};

    use super::super::firecracker::Machine;
    use super::super::memory::{BALLOON_STEP_MIB, CLONE_MIB};
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
                jail: None,
                server: None,
                templates: None,
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

    /// A host whose kernel measures nothing and has memory to spare: a test
    /// that is not about admission says so, and does not depend on the
    /// machine it runs on.
    fn open_at(config: &ServiceConfig, at: u64) -> Service {
        Service::open(config.clone(), clock(at))
            .unwrap()
            .with_measure(Arc::new(|_: &HostedVm| None), Arc::new(|| Some(1 << 20)))
    }

    /// Fake VMs, and the host's memory that follows them: what each one's guest holds, what its
    /// Firecracker API took for its balloon, and what the service asked for.
    #[derive(Default)]
    struct World {
        resident: Mutex<BTreeMap<String, u64>>,
        balloon: Mutex<BTreeMap<String, u64>>,
        asked: Mutex<Vec<(String, u64)>>,
        /// VMs whose Firecracker has no balloon device: their API answers as one booted or restored
        /// without the device does.
        no_balloon: Mutex<BTreeSet<String>>,
    }

    impl World {
        fn takes_mib(&self, id: &str) -> Option<u64> {
            let held = *self.resident.lock().unwrap().get(id)?;
            let back = *self.balloon.lock().unwrap().get(id).unwrap_or(&0);
            Some(held.saturating_sub(back))
        }

        fn used_mib(&self) -> u64 {
            let ids: Vec<String> = self.resident.lock().unwrap().keys().cloned().collect();
            ids.iter().filter_map(|id| self.takes_mib(id)).sum()
        }

        fn fill(&self, id: &str, mib: u64) {
            self.resident.lock().unwrap().insert(id.to_owned(), mib);
        }

        /// `id`'s VM sleeps: the host has its memory back (its snapshot is a file on disk).
        fn sleep(&self, id: &str) {
            self.resident.lock().unwrap().remove(id);
            self.balloon.lock().unwrap().remove(id);
        }

        fn asked(&self) -> Vec<(String, u64)> {
            self.asked.lock().unwrap().clone()
        }
    }

    fn fake_host(dir: &Path, node_mib: u64) -> (Service, Arc<World>) {
        let world = Arc::new(World::default());
        let weighed = Arc::clone(&world);
        let freed = Arc::clone(&world);
        let service = Service::open(config(dir), clock(0))
            .unwrap()
            .with_memory(Budget {
                node_mib: Some(node_mib),
                ..Budget::default()
            })
            .with_measure(
                Arc::new(move |vm: &HostedVm| weighed.takes_mib(&vm.identity().vm)),
                Arc::new(move || Some(node_mib.saturating_sub(freed.used_mib()))),
            );
        (service, world)
    }

    /// Admit `workspace`'s fake VM whose guest holds `mib`, with a Firecracker API at its socket.
    fn fake_vm(
        service: &Service,
        world: &Arc<World>,
        config: &ServiceConfig,
        workspace: &str,
        mib: u64,
        balloon: bool,
    ) -> String {
        let entry = service
            .admit(
                &request(workspace, &format!("acct-{workspace}"), VmSize::Small),
                true,
            )
            .unwrap();
        let id = entry.id();
        world.fill(&id, mib);
        if !balloon {
            world.no_balloon.lock().unwrap().insert(id.clone());
        }
        let files = config.files(workspace, "default");
        std::fs::create_dir_all(&files.run).unwrap();
        serve_api(&files.api_socket(), Arc::clone(world), id.clone());
        id
    }

    /// A fake Firecracker's API at `socket`, answering the balloon calls the service makes. One
    /// whole HTTP request: the client writes its head and its body in one go or not, so read until
    /// the body is all there.
    fn read_request(stream: &mut std::os::unix::net::UnixStream) -> String {
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = stream.read(&mut chunk).unwrap_or(0);
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            let text = String::from_utf8_lossy(&request).into_owned();
            let Some((head, body)) = text.split_once("\r\n\r\n") else {
                continue;
            };
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if body.len() >= length {
                return text;
            }
        }
        String::from_utf8_lossy(&request).into_owned()
    }

    fn serve_api(socket: &Path, world: Arc<World>, id: String) {
        let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let request = read_request(&mut stream);
                let reply = if request.starts_with("PATCH /balloon") {
                    let body = request
                        .split_once("\r\n\r\n")
                        .map(|(_, body)| body)
                        .unwrap_or_default();
                    let asked: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
                    let target = asked["amount_mib"].as_u64().unwrap_or(0);
                    world.asked.lock().unwrap().push((id.clone(), target));
                    if world.no_balloon.lock().unwrap().contains(&id) {
                        "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n"
                    } else {
                        world.balloon.lock().unwrap().insert(id.clone(), target);
                        "HTTP/1.1 204 No Content\r\n\r\n"
                    }
                } else {
                    "HTTP/1.1 204 No Content\r\n\r\n"
                };
                let _ = stream.write_all(reply.as_bytes());
            }
        });
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
                billed_from_ms: Some(1_500),
            }),
        );
        let service = open_at(&config, 90_000);
        let records = service.log.records("acct-1").unwrap();
        assert_eq!(records.len(), 1);
        let record = records.first().unwrap();
        assert_eq!(
            (record.kind, record.ended, record.end_ms, record.seconds),
            (UsageKind::Running, Some(RunEnd::Recovered), 61_500, 60)
        );
        drop(service);
        let again = open_at(&config, 95_000);
        assert_eq!(
            again.log.records("acct-1").unwrap().len(),
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
    fn a_host_being_taken_back_wakes_nothing_and_keeps_its_vms_for_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        existing(&config, "ws1", "acct-1", None);
        let service = open_at(&config, 0);
        assert!(!service.evicting());
        service.evict();
        assert!(service.evicting());
        let Reply::Error(message) = service.handle(Request::Ensure {
            vm: request("ws1", "acct-1", VmSize::Small),
            shares: Vec::new(),
        }) else {
            panic!("a host being taken back wakes nothing");
        };
        assert_eq!(
            message,
            "the workspace's Linux VM is moving to another host, saved as it was; \
             run the command again in a minute"
        );
        assert!(
            config.files("ws1", "default").meter().is_file(),
            "the VM stays where the next host finds it"
        );
        let next = ServiceConfig {
            run: dir.path().join("run-of-the-next-host"),
            ..config
        };
        let elsewhere = open_at(&next, 10_000);
        assert!(!elsewhere.evicting());
        assert_eq!(
            elsewhere
                .status()
                .iter()
                .map(|vm| (vm.vm.as_str(), vm.account.as_str()))
                .collect::<Vec<_>>(),
            [("ws1/default", "acct-1")]
        );
    }

    #[test]
    fn a_host_admits_by_what_its_vms_take_not_by_what_they_are_configured_at() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let (service, world) = fake_host(dir.path(), 4 * 1024);
        for n in 0..5 {
            fake_vm(
                &service,
                &world,
                &config,
                &format!("ws{n}"),
                CLONE_MIB,
                true,
            );
            service.tick();
        }
        assert_eq!(
            service.load(),
            HostLoad {
                budget_mib: Some(2 * 1024),
                awake_mib: 5 * CLONE_MIB,
                waiting_mib: 0,
            },
            "what the VMs take, not what they may grow into"
        );
        assert!(matches!(
            service.admit(&request("ws5", "acct-ws5", VmSize::Small), true),
            Err(EnsureError::HostFull)
        ));
        let Reply::Busy(words) = service.handle(Request::Ensure {
            vm: request("ws5", "acct-ws5", VmSize::Small),
            shares: Vec::new(),
        }) else {
            panic!("a full host makes the wake wait");
        };
        assert_eq!(words, "Starting another machine for it, about 3 minutes.");
        assert_eq!(
            service.load().waiting_mib,
            453,
            "what a VM is expected to take"
        );
        service
            .admit(&request("ws0", "acct-ws0", VmSize::Small), true)
            .unwrap();
        service
            .entry("ws1", "default")
            .unwrap()
            .awake
            .store(false, Ordering::SeqCst);
        world.sleep("ws1/default");
        service
            .admit(&request("ws5", "acct-ws5", VmSize::Small), true)
            .unwrap();
        assert_eq!(service.load().waiting_mib, 0, "a woken VM waits no more");
    }

    #[test]
    fn a_host_short_of_memory_balloons_its_vms_before_it_refuses_one_and_kills_none() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let (service, world) = fake_host(dir.path(), 4 * 1024);
        let mut ids = Vec::new();
        for n in 0..5 {
            ids.push(fake_vm(
                &service,
                &world,
                &config,
                &format!("ws{n}"),
                CLONE_MIB,
                true,
            ));
            service.tick();
        }
        assert_eq!(
            world.asked(),
            vec![],
            "room to spare asks no guest for memory"
        );
        for id in &ids {
            world.fill(id, 900);
        }
        service.tick();
        assert_eq!(
            world.used_mib(),
            5 * (900 - BALLOON_STEP_MIB),
            "each guest gave a step of its memory back"
        );
        for id in &ids {
            world.fill(id, 4096);
        }
        for _ in 0..10 {
            service.tick();
        }
        assert!(
            world
                .balloon
                .lock()
                .unwrap()
                .values()
                .all(|held| *held == 2 * 1024),
            "every balloon holds half a small VM, and no more"
        );
        assert!(matches!(
            service.admit(&request("ws9", "acct-ws9", VmSize::Small), true),
            Err(EnsureError::HostFull)
        ));
        assert_eq!(service.status().len(), 5, "every workspace keeps its VM");
        for n in 0..5 {
            assert!(
                service
                    .entry(&format!("ws{n}"), "default")
                    .unwrap()
                    .awake
                    .load(Ordering::SeqCst),
                "ws{n}'s VM still runs"
            );
        }
        for id in &ids {
            world.fill(id, 2 * 1024);
        }
        service.tick();
        assert!(
            world.used_mib() < CLONE_MIB,
            "the balloons hold what the guests no longer need: {} MiB in use",
            world.used_mib()
        );
        service
            .admit(&request("ws9", "acct-ws9", VmSize::Small), true)
            .unwrap();
    }

    #[test]
    fn a_template_is_counted_at_the_memory_its_guest_warms() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let (service, world) = fake_host(dir.path(), 4 * 1024);
        fake_vm(&service, &world, &config, "ws1", CLONE_MIB, true);
        service.tick();
        assert!(service.room_for(service.budget.expected_mib(), ""));
        assert!(!service.room_for(VmSize::Small.memory_mib(), ""));
        service
            .building_mib
            .fetch_add(VmSize::Small.memory_mib(), Ordering::SeqCst);
        assert_eq!(
            service.load().awake_mib,
            CLONE_MIB + VmSize::Small.memory_mib()
        );
        assert!(!service.room_for(service.budget.expected_mib(), ""));
    }

    #[test]
    fn a_vm_whose_firecracker_has_no_balloon_is_asked_once_and_never_killed_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let (service, world) = fake_host(dir.path(), 4 * 1024);
        let plain = fake_vm(&service, &world, &config, "ws1", CLONE_MIB, true);
        let bare = fake_vm(&service, &world, &config, "ws2", CLONE_MIB, false);
        world.fill(&plain, 4096);
        world.fill(&bare, 4096);
        service.tick();
        let asked = world.asked();
        assert!(
            asked
                .iter()
                .any(|(id, mib)| id == &plain && *mib == BALLOON_STEP_MIB),
            "{asked:?}"
        );
        assert!(
            asked.iter().any(|(id, _)| id == &bare),
            "the VM without a balloon was asked once: {asked:?}"
        );
        for _ in 0..5 {
            service.tick();
        }
        let bare_asks = world.asked().iter().filter(|(id, _)| id == &bare).count();
        assert_eq!(
            bare_asks, 1,
            "a balloon that is not there is not asked twice"
        );
        assert_eq!(service.status().len(), 2);
    }

    #[test]
    fn a_workspace_without_a_card_gets_its_vm_for_browser_and_no_linux_action() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        std::fs::write(
            &config.plans,
            r#"{"accounts":{"acct-card":{"max_running":2,"max_vms":2,"sizes":["small"]}},"sells":{"host":"tilo.sh","plans":[],
                "add_payment_method":"https://tilo.sh/card","browser_free_actions":300}}"#,
        )
        .unwrap();
        let service = open_at(&config, 0);
        service
            .admit(&request("ws1", "acct-free", VmSize::Small), false)
            .unwrap();
        let Reply::Refused(refusal) = service.handle(Request::Ensure {
            vm: request("ws1", "acct-free", VmSize::Small),
            shares: Vec::new(),
        }) else {
            panic!("a Linux action without a card is refused");
        };
        assert_eq!(refusal.reason, host_account::Reason::NoPaymentMethod);
        assert_eq!(
            service.status().len(),
            1,
            "one VM for the workspace, which Browser and Linux share"
        );
        service
            .admit(&request("ws2", "acct-card", VmSize::Small), true)
            .unwrap();
    }

    #[test]
    fn a_linux_app_without_a_card_is_refused_before_anything_starts() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        config.settings.server =
            Some(super::super::network::Endpoint::parse("127.0.0.1:7704").unwrap());
        std::fs::write(
            &config.plans,
            r#"{"accounts":{},"sells":{"host":"tilo.sh","plans":[],
                "add_payment_method":"https://tilo.sh/card","browser_free_actions":300}}"#,
        )
        .unwrap();
        let service = open_at(&config, 0);
        let Reply::Refused(refusal) = service.handle(Request::OpenApp {
            app: OpenApp {
                workspace: "ws1".into(),
                account: "acct-free".into(),
                size: VmSize::Small,
                material: browser::test_material("127.0.0.1:7704"),
                launch: tilo_actions::remote::linux_apps::AppLaunch::new("xterm").unwrap(),
            },
        }) else {
            panic!("a Linux app without a card is refused");
        };
        assert_eq!(refusal.reason, host_account::Reason::NoPaymentMethod);
        assert!(
            refusal
                .next
                .iter()
                .any(|next| next.kind == host_account::NextStepKind::AddPaymentMethod),
            "the refusal says what lets it through: {refusal:?}"
        );
        assert!(
            service.status().iter().all(|vm| vm.phase != "Running"),
            "nothing woke for it"
        );
    }

    #[test]
    fn a_workspace_is_handed_the_address_its_session_dials_the_server_at() {
        use super::super::network::Endpoint;

        let dir = tempfile::tempdir().unwrap();
        let with_server = |server: &str| {
            let mut config = config(dir.path());
            config.settings.server = Some(Endpoint::parse(server).unwrap());
            open_at(&config, 0)
        };
        let dialled = |line: &str| -> std::net::SocketAddr {
            let line: tilo_server_protocol::hosted::Material = serde_json::from_str(line).unwrap();
            line.server
                .addrs
                .into_iter()
                .find_map(|address| match address {
                    iroh_base::TransportAddr::Ip(socket) => Some(socket),
                    _ => None,
                })
                .unwrap()
        };
        let local = with_server("127.0.0.1:7704");
        let line = local
            .hand_off(&browser::test_material("127.0.0.1:7704"))
            .unwrap();
        assert_eq!(
            dialled(&line),
            "10.0.2.4:7704".parse().unwrap(),
            "the VM dials the mapped loopback, not the host's own"
        );
        let hosted = with_server("10.0.0.9:7704");
        let line = hosted
            .hand_off(&browser::test_material("203.0.113.9:7704"))
            .unwrap();
        assert_eq!(dialled(&line), "10.0.0.9:7704".parse().unwrap());
        // and a material the session could not join with refuses the open
        assert!(matches!(
            local.hand_off("{}"),
            Err(BrowserError::Invalid(_))
        ));
    }

    #[test]
    fn a_host_without_the_servers_endpoint_serves_no_browser() {
        let dir = tempfile::tempdir().unwrap();
        let service = open_at(&config(dir.path()), 0);
        let Reply::Error(message) = service.handle(Request::OpenBrowser {
            browser: OpenBrowser {
                workspace: "ws1".into(),
                account: "acct-1".into(),
                size: VmSize::Small,
                store: None,
                material: browser::test_material("127.0.0.1:7704"),
            },
        }) else {
            panic!("a host with no server endpoint opens nothing");
        };
        assert_eq!(
            message,
            "this host serves no Browser: it was started without the server's endpoint"
        );
        assert!(service.status().is_empty(), "and woke nothing");
        assert_eq!(
            service.handle(Request::StopBrowser {
                workspace: "ws1".into()
            }),
            Reply::Done,
            "stopping what is not open is nothing to do"
        );
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
        let service = open_at(&config, 0);
        let refused = |request: &VmRequest| match service.handle(Request::Ensure {
            vm: request.clone(),
            shares: Vec::new(),
        }) {
            Reply::Refused(refusal) => refusal.message,
            other => panic!("expected a refusal, got {other:?}"),
        };
        {
            let message = refused(&request("ws1", "acct-1", VmSize::Large));
            assert!(
                message.starts_with("This workspace's Linux VM is large"),
                "{message}"
            );
        }
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
        {
            let reply = service.handle(Request::Ensure {
                vm: request("../etc", "acct-1", VmSize::Small),
                shares: Vec::new(),
            });
            assert!(matches!(reply, Reply::Error(_)), "{reply:?}");
        }
    }

    /// A VM `account` keeps at `size`, off: its disk holds `disk`, and its Firecracker directory
    /// also holds each of `beside`.
    fn with_disk(
        config: &ServiceConfig,
        workspace: &str,
        account: &str,
        size: VmSize,
        disk: &[u8],
        beside: &[&str],
    ) -> VmFiles {
        let files = config.files(workspace, "default");
        Meter::new(VmIdentity {
            vm: format!("{workspace}/default"),
            workspace: workspace.into(),
            environment: "default".into(),
            account: account.into(),
            size,
        })
        .save(&files.meter())
        .unwrap();
        std::fs::create_dir_all(files.vmm()).unwrap();
        std::fs::write(files.disk(), disk).unwrap();
        for name in beside {
            std::fs::write(files.vmm().join(name), name.as_bytes()).unwrap();
        }
        files
    }

    fn branch(
        service: &Service,
        workspace: &str,
        account: &str,
        size: VmSize,
        from: &str,
    ) -> Reply {
        service.handle(Request::Branch {
            vm: request(workspace, account, size),
            from: from.into(),
        })
    }

    fn delete(service: &Service, workspace: &str) -> Reply {
        service.handle(Request::Delete {
            workspace: workspace.into(),
            environment: "default".into(),
        })
    }

    fn head(path: &Path, len: usize) -> Vec<u8> {
        use std::os::unix::fs::FileExt as _;
        let mut bytes = vec![0_u8; len];
        std::fs::File::open(path)
            .unwrap()
            .read_exact_at(&mut bytes, 0)
            .unwrap();
        bytes
    }

    fn held(service: &Service) -> Vec<(String, String, String)> {
        service
            .status()
            .into_iter()
            .map(|vm| (vm.vm, vm.account, vm.phase))
            .collect()
    }

    fn error(reply: Reply) -> String {
        match reply {
            Reply::Error(message) => message,
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn a_branch_is_its_sources_disk_in_a_vm_of_its_own_that_boots_cold() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let source = with_disk(
            &config,
            "ws1",
            "acct-1",
            VmSize::Small,
            b"the tenant's tools",
            &["vm.mem", "firecracker.json"],
        );
        let service = open_at(&config, 0);
        assert_eq!(
            branch(&service, "ws2", "acct-1", VmSize::Default, "ws1"),
            Reply::Done
        );

        let made = config.files("ws2", "default");
        assert_eq!(head(&made.disk(), 18), b"the tenant's tools");
        assert_eq!(
            std::fs::metadata(made.disk()).unwrap().len(),
            VmSize::Default.disk_bytes(),
            "grown to the size it was asked at"
        );
        assert!(!made.snapshot().memory.exists(), "no memory of the source");
        assert!(
            !made.snapshot().state.exists(),
            "no vCPU state of the source"
        );
        assert!(
            !made.config().exists(),
            "it boots from a configuration of its own"
        );
        assert!(
            made.branched().is_file(),
            "its guest forgets the source's sessions at its first wake"
        );
        let meter = Meter::load(&made.meter()).unwrap();
        assert_eq!(
            (meter.identity.account.as_str(), meter.identity.size),
            ("acct-1", VmSize::Default)
        );
        assert_eq!(
            held(&service),
            [
                ("ws1/default".into(), "acct-1".into(), "Off".into()),
                ("ws2/default".into(), "acct-1".into(), "Off".into()),
            ]
        );
        assert_eq!(std::fs::read(source.disk()).unwrap(), b"the tenant's tools");
        assert!(source.snapshot().memory.is_file() && source.config().is_file());
        drop(service);
        assert_eq!(
            held(&open_at(&config, 1_000)).len(),
            2,
            "the branch is a VM the host keeps"
        );
    }

    #[test]
    fn a_branch_of_a_workspace_without_a_vm_makes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        existing(&config, "ws-unbooted", "acct-1", None);
        let service = open_at(&config, 0);
        for from in ["ws-none", "ws-unbooted"] {
            assert_eq!(
                branch(&service, "ws2", "acct-1", VmSize::Small, from),
                Reply::NoVm {
                    workspace: from.into()
                },
                "said in a way the server tells apart"
            );
            assert!(
                !config.state.join("vms").join("ws2").exists(),
                "nothing was made"
            );
        }
        assert_eq!(
            held(&service),
            [(
                "ws-unbooted/default".into(),
                "acct-1".into(),
                "NotCreated".into()
            )]
        );
        let message = error(branch(&service, "ws2", "acct-1", VmSize::Small, "../ws1"));
        assert!(message.contains("is not a workspace id"), "{message}");
    }

    #[test]
    fn a_branch_is_refused_onto_a_vm_for_another_account_or_smaller_than_its_source() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        with_disk(&config, "ws1", "acct-1", VmSize::Small, b"one", &[]);
        let kept = with_disk(&config, "ws2", "acct-1", VmSize::Small, b"two", &[]);
        with_disk(&config, "ws9", "acct-2", VmSize::Default, b"nine", &[]);
        let service = open_at(&config, 0);
        let message = error(branch(&service, "ws2", "acct-1", VmSize::Small, "ws1"));
        assert!(message.contains("ws2 already has a VM"), "{message}");
        assert_eq!(std::fs::read(kept.disk()).unwrap(), b"two");
        assert!(!kept.branched().exists());

        let message = error(branch(&service, "ws3", "acct-1", VmSize::Default, "ws9"));
        assert!(message.contains("belongs to another account"), "{message}");
        assert!(!config.files("ws3", "default").state.exists());

        let message = error(branch(&service, "ws3", "acct-2", VmSize::Small, "ws9"));
        assert!(
            message.contains("is default, and a branch of it is no smaller"),
            "{message}"
        );
        assert!(!config.files("ws3", "default").state.exists());
        assert_eq!(service.status().len(), 3);
    }

    #[test]
    fn a_branch_counts_toward_the_accounts_vms_and_a_delete_gives_its_room_back() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        std::fs::write(
            &config.plans,
            r#"{"default":{"max_running":1,"max_vms":2,"sizes":["small","default"]}}"#,
        )
        .unwrap();
        with_disk(&config, "ws1", "acct-1", VmSize::Small, b"one", &[]);
        with_disk(&config, "ws2", "acct-1", VmSize::Small, b"two", &[]);
        let service = open_at(&config, 0);
        let Reply::Refused(refusal) = branch(&service, "ws3", "acct-1", VmSize::Small, "ws1")
        else {
            panic!("a branch past the plan's VMs is refused");
        };
        assert_eq!(
            refusal.reason,
            host_account::Reason::TooManyVms {
                limit: 2,
                existing: 2
            }
        );
        assert!(
            refusal
                .message
                .starts_with("Your plan keeps at most 2 Linux VMs"),
            "{}",
            refusal.message
        );
        assert!(!config.files("ws3", "default").state.exists());
        let Reply::Refused(refusal) = branch(&service, "ws3", "acct-1", VmSize::Large, "ws1")
        else {
            panic!("a size the plan does not include is refused");
        };
        assert!(matches!(
            refusal.reason,
            host_account::Reason::SizeNotInPlan { .. }
        ));

        assert_eq!(delete(&service, "ws2"), Reply::Done);
        assert_eq!(
            branch(&service, "ws3", "acct-1", VmSize::Small, "ws1"),
            Reply::Done
        );
        assert_eq!(
            held(&service)
                .into_iter()
                .map(|(vm, ..)| vm)
                .collect::<Vec<_>>(),
            ["ws1/default", "ws3/default"]
        );
    }

    #[test]
    fn a_branch_whose_copy_failed_leaves_no_vm() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let source = with_disk(&config, "ws1", "acct-1", VmSize::Small, b"one", &[]);
        let service = open_at(&config, 0);
        // The disk goes after the host saw it: the copy fails half-way.
        std::fs::remove_file(source.disk()).unwrap();
        let message = error(branch(&service, "ws2", "acct-1", VmSize::Small, "ws1"));
        assert!(
            message.starts_with("the VM of workspace ws1 could not be branched"),
            "{message}"
        );
        assert!(
            !config.state.join("vms").join("ws2").exists(),
            "no disk, no part of one and no meter for a later wake to take for a VM"
        );
        assert_eq!(service.status().len(), 1);
        assert!(
            lock(&service.branching).is_empty(),
            "nothing stays reserved"
        );
        std::fs::write(source.disk(), b"one").unwrap();
        assert_eq!(
            branch(&service, "ws2", "acct-1", VmSize::Small, "ws1"),
            Reply::Done
        );
    }

    #[test]
    fn a_deleted_vm_is_gone_from_the_host_and_deleting_it_again_is_done() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        config.settings.templates = Some(config.state.join("vms").join("template"));
        let files = with_disk(
            &config,
            "ws1",
            "acct-1",
            VmSize::Small,
            b"one",
            &["vm.mem", "vm.state"],
        );
        let template = with_disk(&config, "template", "", VmSize::Small, b"t", &[]);
        let service = open_at(&config, 0);
        assert_eq!(
            held(&service)
                .first()
                .map(|(vm, _, phase)| (vm.as_str(), phase.as_str())),
            Some(("template/default", "Off"))
        );
        assert_eq!(
            service
                .status()
                .iter()
                .find(|vm| vm.vm == "ws1/default")
                .map(|vm| vm.phase.as_str()),
            Some("Suspended")
        );
        assert_eq!(delete(&service, "ws1"), Reply::Done);
        assert!(
            !files.state.exists(),
            "its disk, snapshot and meter are gone"
        );
        assert!(
            !config.state.join("vms").join("ws1").exists(),
            "and its workspace's directory, which held no other VM"
        );
        assert!(service.entry("ws1", "default").is_none());
        assert_eq!(
            delete(&service, "ws1"),
            Reply::Done,
            "deleting it again is done"
        );
        assert_eq!(delete(&service, "ws-never"), Reply::Done);

        let message = error(delete(&service, "template"));
        assert!(message.contains("a template is the host's"), "{message}");
        assert!(template.disk().is_file());
        drop(service);
        assert!(
            open_at(&config, 1_000)
                .status()
                .iter()
                .all(|vm| vm.vm != "ws1/default"),
            "a restarted host does not find it"
        );
    }

    /// The status listener is the network's way to a host, and it reads: it changes no VM. Waking,
    /// stopping, branching and deleting reach the host only on its own socket.
    #[test]
    fn the_status_listener_answers_status_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let source = with_disk(&config, "ws1", "acct-1", VmSize::Small, b"one", &[]);
        let service = Arc::new(open_at(&config, 0));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        service.serve_status(listener);
        let ask = |request: &Request| {
            let mut stream = std::net::TcpStream::connect(&address).unwrap();
            FRAMING.write_value(&mut stream, request).unwrap();
            FRAMING.read_value::<_, Reply>(&mut stream).unwrap()
        };
        assert!(matches!(
            ask(&Request::Status),
            Some(Reply::Status { vms, .. }) if vms.len() == 1
        ));
        for request in [
            Request::Stop {
                workspace: "ws1".into(),
                environment: "default".into(),
            },
            Request::Branch {
                vm: request("ws2", "acct-1", VmSize::Small),
                from: "ws1".into(),
            },
            Request::Delete {
                workspace: "ws1".into(),
                environment: "default".into(),
            },
        ] {
            assert_eq!(
                ask(&request),
                Some(Reply::Error("this listener answers Status only".into())),
                "the network does not {request:?}"
            );
        }
        assert!(
            !config.state.join("vms").join("ws2").exists(),
            "nothing was branched"
        );
        assert!(source.disk().is_file(), "nothing was deleted");
        assert_eq!(
            held(&service),
            [("ws1/default".into(), "acct-1".into(), "Off".into())]
        );
    }

    /// The placer and a host as they run: the placer reads the host's Status and changes nothing on
    /// it, and whoever asked branches and deletes on the host's own socket.
    #[test]
    fn a_placer_places_a_branch_and_forgets_a_vm_its_host_no_longer_holds() {
        use super::super::placement::{FixedHosts, Placer, read_load};

        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        with_disk(&config, "ws1", "acct-1", VmSize::Small, b"one", &[]);
        let service = Arc::new(open_at(&config, 0));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        service.serve_status(listener);
        let placer = Placer::new(
            &dir.path().join("placements.json"),
            1,
            Box::new(FixedHosts::parse(&format!("vm-host-0={address}")).unwrap()),
            Box::new(read_load),
            Arc::new(|| 1_000),
        )
        .unwrap();
        let on_host = Reply::Placed {
            host: "vm-host-0".into(),
        };
        assert_eq!(
            placer.place(&request("ws1", "acct-1", VmSize::Small)),
            on_host
        );
        assert_eq!(
            placer.handle(Request::Branch {
                vm: request("ws2", "acct-1", VmSize::Small),
                from: "ws1".into(),
            }),
            on_host
        );
        assert!(
            !config.state.join("vms").join("ws2").exists(),
            "the placer made nothing on the host"
        );
        assert_eq!(
            branch(&service, "ws2", "acct-1", VmSize::Small, "ws1"),
            Reply::Done
        );
        let forget = || {
            placer.handle(Request::Delete {
                workspace: "ws2".into(),
                environment: "default".into(),
            })
        };
        let Reply::Error(message) = forget() else {
            panic!("a VM its host holds stays placed");
        };
        assert!(message.contains("vm-host-0 still holds"), "{message}");
        assert!(
            config.files("ws2", "default").disk().is_file(),
            "and is not deleted"
        );
        assert_eq!(delete(&service, "ws2"), Reply::Done);
        assert_eq!(forget(), Reply::Done);
    }
}
