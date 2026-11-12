//! The one process per host that owns every hosted VM.
//!
//! `tilo-legacy hosted serve` runs it. It admits each wake against the
//! account's plan ([`plan`](super::plan)), wakes the VM, polls the awake
//! ones and suspends the idle, and answers usage questions from the ledger.
//! Each VM is locked on its own, so one VM's boot never holds up another's
//! command; admission is one lock, so two wakes never both take an
//! account's last slot.
//!
//! A host can be taken back by its cloud at short notice (an Azure Spot VM
//! is evicted about 30 seconds after the notice). Given the notice, the
//! service takes no more wakes and suspends every awake VM at once onto its
//! state directory, which lives on storage that outlives the host; the next
//! service to open that directory, on another host, restores each VM on its
//! next command ([`Service::evict`]).

use std::collections::BTreeMap;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use legacy_agent_protocol::link::AgentLink;

use super::browser::{self, Browser, Launcher, Life, Limits, UP_PATIENCE};
use super::control::{
    ENVIRONMENT, FRAMING, HostLoad, OpenBrowser, Reply, Request, ShareRequest, VmRequest, VmStatus,
};
use super::files::Served;
use super::firecracker::VmFiles;
use super::memory::{self, Budget};
use super::meter::{Ledger, Meter, VmIdentity, summarize};
use super::plan::{AwakeVm, Plans, admit};
use super::size::VmSize;
use super::valid_name;
use super::vm::{Clock, HostedVm, Settings, VmError};
use tilo_actions::remote::host_account;

/// How often awake VMs are asked what runs in them.
pub const POLL: Duration = Duration::from_secs(1);

/// What a wake that waits for room is told, and its caller shows: a
/// placer starts another machine for the demand (about three minutes on
/// Azure), or a VM going idle makes room sooner.
pub const BUSY: &str = "Starting another machine for it, about 3 minutes.";

/// How long a waiting wake counts as demand after it last asked: its
/// caller asks again every few seconds while it waits.
const WAITING_FOR_MS: u64 = 15_000;

/// How long a browser session is given to write its profile back and end
/// before it is killed.
const BROWSER_GRACE: Duration = Duration::from_secs(15);

/// How long after a template was last tried another try waits.
const TEMPLATE_RETRY_MS: u64 = 10 * 60 * 1000;

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
    /// The browser session in this VM, once a browser action started one.
    browser: Mutex<Option<Arc<Browser>>>,
    /// What the VM takes of the host's memory, as the kernel last measured it.
    memory: Mutex<Measured>,
}

/// What a host knows of one VM's memory.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Measured {
    /// MiB the VM's process holds of this host's memory; `None` until it is
    /// measured since it woke, when one is counted at what a VM is expected
    /// to take ([`Budget::expected_mib`]).
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
    ledger: Ledger,
    clock: Clock,
    vms: Mutex<BTreeMap<String, Arc<Entry>>>,
    admission: Mutex<()>,
    /// The host is being taken back: no more wakes.
    evicting: AtomicBool,
    /// What the host may pack into its memory, and what it keeps free.
    budget: Budget,
    /// How it measures, and what it can still hand out.
    weigh: Weigh,
    available: Available,
    /// What the templates being made right now are counted at until each is
    /// a snapshot and no longer a VM.
    building_mib: AtomicU64,
    /// Wakes waiting for room: what each asks for, and when it last asked.
    waiting: Mutex<BTreeMap<String, (u64, u64)>>,
    /// How long a browser session may be idle, and live.
    browser_limits: Limits,
    /// When each size's template was last tried: building, or failed.
    templates: Mutex<BTreeMap<VmSize, u64>>,
}

/// Why a workspace's Browser did not open.
#[derive(Debug)]
pub enum BrowserError {
    /// This host serves no Browser: its service was started without the
    /// relay's endpoint, so no session in its VMs could join a workspace.
    NotOffered,
    Invalid(String),
    /// Not now: every machine is full ([`BUSY`]). The caller waits for
    /// room and asks again.
    Full(String),
    /// The session in the workspace's VM did not come up.
    Session(String),
    Vm(VmError),
}

impl From<EnsureError> for BrowserError {
    fn from(error: EnsureError) -> Self {
        match error {
            EnsureError::HostFull => Self::Full(BUSY.to_owned()),
            EnsureError::Vm(error) => Self::Vm(error),
            EnsureError::Invalid(message) => Self::Invalid(message),
            other => Self::Session(other.to_string()),
        }
    }
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotOffered => f.write_str(
                "this host serves no Browser: it was started without the relay's endpoint",
            ),
            Self::Invalid(message) => f.write_str(message),
            Self::Full(words) => f.write_str(words),
            Self::Session(message) => write!(f, "Browser did not open: {message}"),
            Self::Vm(error) => write!(f, "the workspace's VM could not start: {error}"),
        }
    }
}

/// Why a wake did not happen.
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
            evicting: AtomicBool::new(false),
            budget: Budget::default(),
            weigh: Arc::new(|vm: &HostedVm| vm.resident_mib()),
            available: Arc::new(memory::available_mib),
            building_mib: AtomicU64::new(0),
            waiting: Mutex::new(BTreeMap::new()),
            browser_limits: Limits::default(),
            templates: Mutex::new(BTreeMap::new()),
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

    /// Admit by the memory the VMs really take: what the kernel measures,
    /// with `budget`'s reserve kept free for the host itself, and a wake
    /// beyond that waits for room.
    #[must_use]
    pub const fn with_memory(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self
    }

    /// Measure with `weigh` and `available` instead of this host's kernel: a
    /// test's fake VMs, whose memory grows, and the free memory that follows.
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
        let ledger = Some(self.ledger.clone());
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
                ledger,
                Arc::clone(&self.clock),
            )?),
            browser: Mutex::new(None),
            memory: Mutex::new(Measured::default()),
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
        let result = (|| {
            vm.resize(request.size)?;
            *lock(&entry.size) = request.size;
            let (link, woke) = vm.ensure(&served, true)?;
            Ok((link, woke.map(|wake| wake.millis)))
        })();
        entry.awake.store(vm.awake(), Ordering::SeqCst);
        drop(vm);
        result.map_err(EnsureError::Vm)
    }

    /// The entry for `request`, reserved as awake, or why not. A Linux
    /// action (`linux`) needs the account's plan to allow it, and a card on
    /// a host that sells its VMs; a browser action needs neither, as its
    /// page actions are counted instead.
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
        if !runs && !self.room_for(&id) {
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

    /// What the awake VMs other than `except` take of the host's memory:
    /// what the kernel measured of each, or, for one admitted since the last
    /// measurement, what a VM is expected to take.
    fn used_mib(&self, except: &str) -> u64 {
        let expected = self.budget.expected_mib();
        lock(&self.vms)
            .values()
            .filter(|entry| entry.id() != except && entry.awake.load(Ordering::SeqCst))
            .map(|entry| lock(&entry.memory).resident_mib.unwrap_or(expected))
            .sum()
    }

    /// What the templates being made right now are counted at: each is a
    /// booted VM of its own until it is a snapshot.
    fn building_mib(&self) -> u64 {
        self.building_mib.load(Ordering::SeqCst)
    }

    /// Whether the host may start one more VM beside every awake one but the
    /// VM named `except` (one asking for room for itself): what the awake
    /// ones are measured at, what the new one is expected to take, and the
    /// host's reserve must fit its memory — and its kernel must have that
    /// much to hand out now.
    fn room_for(&self, except: &str) -> bool {
        let expected = self.budget.expected_mib();
        let used = self.used_mib(except).saturating_add(self.building_mib());
        if !self.budget.admits(used, expected) {
            return false;
        }
        // The arithmetic is what this host counted; this is what it really
        // has. The page cache of a hundred VMs' disks is available, but the
        // kernel may not have given it back yet.
        (self.available)()
            .is_none_or(|free| free >= self.budget.reserve_mib.saturating_add(expected))
    }

    /// Note that `id` waits for room: what a placer starts another machine
    /// for.
    fn waits(&self, id: &str) {
        lock(&self.waiting).insert(id.to_owned(), (self.budget.expected_mib(), (self.clock)()));
    }

    /// Poll every awake VM; suspend the idle ones; weigh the ones that run.
    /// A VM busy with a wake or a suspend keeps its last measurement, and the
    /// one the host counts it at until it is weighed again.
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
            let measured = (self.weigh)(&vm);
            // What holds memory runs: its process is the usual word on that,
            // and a host that weighs its VMs another way says so by weighing
            // one.
            let up = vm.awake() || measured.is_some();
            entry.awake.store(up, Ordering::SeqCst);
            lock(&entry.memory).resident_mib = measured;
        }
        self.reclaim();
    }

    /// Keep the host's own memory free: while its kernel says it has less
    /// than the reserve, ask every awake VM's guest for a step back, starting
    /// with the VM that holds the most; once the host has plenty, give one
    /// step back to one VM a tick, so that memory the guests then start using
    /// again arrives a step at a time and not as a spike the node cannot
    /// absorb.
    ///
    /// Nothing is ever killed here: a host that cannot reclaim refuses the
    /// next VM instead ([`Service::room_for`]), and a running workspace keeps
    /// its VM, its browser and its open page.
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
        // The VM holding the most is the one with the most to give back.
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
                // Not running, or the API is busy: the next tick asks again.
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

    /// Open `request`'s workspace's Browser: the session in the workspace's
    /// VM, started now with the workspace's material, the VM woken for it if
    /// it sleeps; or the session already running. `Some(ms)` when this
    /// request handed it the workspace; `None` when it attached to the
    /// session someone already opened. Either way, the provider token of the
    /// window it holds, which page actions on it name.
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
        let browser = self.browser_in(&entry, request.size, store)?;
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
                end_browser(&entry, BROWSER_GRACE);
                Err(BrowserError::Session(message))
            }
        }
    }

    /// The line a workspace's session is handed: the relay's material, with
    /// the relay's address replaced by the one its VM dials
    /// ([`browser::hand_off`]).
    fn hand_off(&self, material: &str) -> Result<String, BrowserError> {
        let relay = self.config.settings.relay.ok_or(BrowserError::NotOffered)?;
        browser::hand_off(material, relay.guest()).map_err(BrowserError::Invalid)
    }

    /// The browser session in `entry`'s VM, woken at `size` with `store`
    /// (where the session keeps its profile) mounted: the one running, or one
    /// that starts when the workspace's material is handed to it.
    fn browser_in(
        &self,
        entry: &Entry,
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
        let woke = (|| {
            vm.resize(size)?;
            *lock(&entry.size) = size;
            vm.ensure(&shares, false)
        })();
        entry.awake.store(vm.awake(), Ordering::SeqCst);
        drop(vm);
        let (link, _) = woke.map_err(BrowserError::Vm)?;
        let log = self
            .config
            .files(&entry.workspace, &entry.environment)
            .state
            .join("browser.log");
        let mut held = lock(&entry.browser);
        let running = held
            .as_ref()
            .filter(|browser| browser.life().ended_because().is_none())
            .cloned();
        let browser = running.unwrap_or_else(|| {
            let life = Arc::new(Life::new((self.clock)()));
            let starts =
                Launcher::new(link, store, Arc::clone(&life), log, Arc::clone(&self.clock));
            let browser = Arc::new(Browser::booted(life, Box::new(starts)));
            *held = Some(Arc::clone(&browser));
            browser
        });
        drop(held);
        Ok(browser)
    }

    /// End the browser session in `workspace`'s VM now; the VM sleeps like
    /// any idle VM.
    pub fn stop_browser(&self, workspace: &str) {
        if let Some(entry) = self.entry(workspace, ENVIRONMENT) {
            end_browser(&entry, BROWSER_GRACE);
        }
    }

    /// End the browser sessions at their limits, each on a thread of its
    /// own, as a session is given time to write its profile, and make the
    /// template of each size in use that has none. Called between polls.
    pub fn maintain(self: &Arc<Self>) {
        let now = (self.clock)();
        let entries: Vec<Arc<Entry>> = lock(&self.vms).values().cloned().collect();
        let sizes: std::collections::BTreeSet<VmSize> =
            entries.iter().map(|entry| *lock(&entry.size)).collect();
        for size in sizes {
            self.keep_template(size, now);
        }
        for entry in entries {
            let end = lock(&entry.browser)
                .as_ref()
                .and_then(|browser| browser.expired(now, self.browser_limits));
            if let Some(end) = end {
                eprintln!("tilo-legacy hosted: {}: Browser ended ({end})", entry.id());
                std::thread::spawn(move || end_browser(&entry, BROWSER_GRACE));
            }
        }
    }

    /// Start making `size`'s template when it has none, there is room for one
    /// more VM, and it was not tried lately. Until it is a snapshot it is a
    /// booted VM like any other, and is counted as one.
    fn keep_template(self: &Arc<Self>, size: VmSize, now: u64) {
        let settings = &self.config.settings;
        if super::vm::template_dir(settings, size).is_none()
            || super::vm::template(settings, size).is_some()
            || !self.room_for("")
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
        let expected = self.budget.expected_mib();
        self.building_mib.fetch_add(expected, Ordering::SeqCst);
        let service = Arc::clone(self);
        std::thread::spawn(move || {
            let made = service.make_template(size);
            service.building_mib.fetch_sub(expected, Ordering::SeqCst);
            match made {
                Ok(()) => eprintln!("tilo-legacy hosted: the {size} template is ready"),
                Err(error) => eprintln!("tilo-legacy hosted: the {size} template: {error}"),
            }
        });
    }

    /// Make `size`'s template: boot a VM of that size from the guest image,
    /// read the session's and Chromium's files into its page cache, snapshot
    /// it, and keep it read-only for new VMs to be cloned from. No workspace
    /// ever ran in it, so it holds no identity.
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

    pub fn suspend(&self, workspace: &str, environment: &str) -> Result<(), VmError> {
        let Some(entry) = self.entry(workspace, environment) else {
            return Ok(());
        };
        end_browser(&entry, BROWSER_GRACE);
        let mut vm = lock(&entry.vm);
        let result = vm.suspend();
        entry.awake.store(vm.awake(), Ordering::SeqCst);
        result
    }

    pub fn stop(&self, workspace: &str, environment: &str) -> Result<(), VmError> {
        let Some(entry) = self.entry(workspace, environment) else {
            return Ok(());
        };
        end_browser(&entry, BROWSER_GRACE);
        let mut vm = lock(&entry.vm);
        let result = vm.stop();
        entry.awake.store(vm.awake(), Ordering::SeqCst);
        result
    }

    /// Suspend every awake VM: the service is stopping.
    pub fn suspend_all(&self) {
        let entries: Vec<Arc<Entry>> = lock(&self.vms).values().cloned().collect();
        for entry in entries {
            end_browser(&entry, BROWSER_GRACE);
            let mut vm = lock(&entry.vm);
            if vm.awake() && vm.suspend().is_err() {
                let _ = vm.stop();
            }
            entry.awake.store(vm.awake(), Ordering::SeqCst);
        }
    }

    /// The host is being taken back: take no more wakes, and suspend every
    /// awake VM onto the state directory, all at once, since the notice is
    /// short and each suspend mostly waits on its disk. A VM whose suspend
    /// fails is stopped, so its disk is consistent and it boots afresh.
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
                    end_browser(entry, Duration::from_secs(2));
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

    /// Whether the host is being taken back.
    #[must_use]
    pub fn evicting(&self) -> bool {
        self.evicting.load(Ordering::SeqCst)
    }

    /// What the awake VMs and the templates being made take of the host's
    /// memory now, and what waiting wakes ask for.
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
            Request::Place { .. } => {
                Reply::Error("a host runs VMs; a placer answers where".to_owned())
            }
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

/// End the browser session in `entry`'s VM, if one runs, giving it `grace`
/// to write its profile to the VM's disk.
fn end_browser(entry: &Entry, grace: Duration) {
    let browser = lock(&entry.browser).take();
    if let Some(browser) = browser {
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
                relay: None,
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

    /// Fake VMs, and the host's memory that follows them: what each one's
    /// guest holds, what each one's Firecracker API took for its balloon, and
    /// what the service asked for.
    #[derive(Default)]
    struct World {
        resident: Mutex<BTreeMap<String, u64>>,
        balloon: Mutex<BTreeMap<String, u64>>,
        asked: Mutex<Vec<(String, u64)>>,
        /// VMs whose Firecracker has no balloon device: their API answers as
        /// one booted or restored without the device does.
        no_balloon: Mutex<BTreeSet<String>>,
    }

    impl World {
        /// What `id`'s VM takes of the host's memory now.
        fn takes_mib(&self, id: &str) -> Option<u64> {
            let held = *self.resident.lock().unwrap().get(id)?;
            let back = *self.balloon.lock().unwrap().get(id).unwrap_or(&0);
            Some(held.saturating_sub(back))
        }

        /// What the host's VMs take together.
        fn used_mib(&self) -> u64 {
            let ids: Vec<String> = self.resident.lock().unwrap().keys().cloned().collect();
            ids.iter().filter_map(|id| self.takes_mib(id)).sum()
        }

        /// `id`'s guest fills up to `mib`.
        fn fill(&self, id: &str, mib: u64) {
            self.resident.lock().unwrap().insert(id.to_owned(), mib);
        }

        /// `id`'s VM sleeps: the host has its memory back (its snapshot
        /// holds its pages as a file, which is disk and not this).
        fn sleep(&self, id: &str) {
            self.resident.lock().unwrap().remove(id);
            self.balloon.lock().unwrap().remove(id);
        }

        /// What the service asked each VM's balloon to hold, in order.
        fn asked(&self) -> Vec<(String, u64)> {
            self.asked.lock().unwrap().clone()
        }
    }

    /// A host whose node has `node_mib`, measuring the fake VMs of `world`.
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

    /// Admit `workspace`'s fake VM whose guest holds `mib`, with a Firecracker
    /// API at its socket: the one a VM booted without a balloon device would
    /// answer with when `balloon` is false.
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

    /// A fake Firecracker's API at `socket`, answering the balloon calls the
    /// service makes: a target the VM takes, or, without a balloon device, the
    /// client error a real one gives.
    /// One whole HTTP request: the client writes its head and its body in
    /// one go or not, so read until the body is all there.
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
        let records = service.ledger.records("acct-1").unwrap();
        assert_eq!(records.len(), 1);
        let record = records.first().unwrap();
        assert_eq!(
            (record.kind, record.ended, record.end_ms, record.seconds),
            (UsageKind::Running, Some(RunEnd::Recovered), 61_500, 60)
        );
        drop(service);
        let again = open_at(&config, 95_000);
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
        // phase: another host opens the same state and has the VM
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
        // A node of 4 GiB, which the host keeps 2 GiB of for itself: five
        // clones of 325 MiB fit where one VM of its configured 4 GiB would not.
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
        // phase: the sixth would pass the host's memory: it waits, and the
        // placer starts another machine for it
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
        // phase: a VM that already runs takes no more memory: its next
        // command runs on a host that is full
        service
            .admit(&request("ws0", "acct-ws0", VmSize::Small), true)
            .unwrap();
        // phase: once one sleeps there is room again, and nothing waits
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
        // phase: the guests fill up, and the host's free memory falls under
        // its reserve: each balloon gives a step back
        for id in &ids {
            world.fill(id, 900);
        }
        service.tick();
        assert_eq!(
            world.used_mib(),
            5 * (900 - BALLOON_STEP_MIB),
            "each guest gave a step of its memory back"
        );
        // phase: they fill up further, and the balloons take steps to half a
        // VM, which is as much as a guest may give
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
        // phase: what the guests took is past what the host has: it refuses a
        // new VM, and kills none
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
        // phase: the guests finish what they were doing, and the balloons hold
        // what they no longer need: the host has its memory back, and admits
        // the VM it refused while it had none
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
    fn a_vm_whose_firecracker_has_no_balloon_is_asked_once_and_never_killed_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let (service, world) = fake_host(dir.path(), 4 * 1024);
        let plain = fake_vm(&service, &world, &config, "ws1", CLONE_MIB, true);
        let bare = fake_vm(&service, &world, &config, "ws2", CLONE_MIB, false);
        // phase: pressure finds one balloon to ask, and one that answers with
        // no device at all
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
        // phase: it is not asked again, and it is not stopped either
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
        // phase: Browser wakes the workspace's VM, card or not
        service
            .admit(&request("ws1", "acct-free", VmSize::Small), false)
            .unwrap();
        // phase: a Linux action in that same VM needs the card
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
        // phase: with a card, the same VM takes Linux actions
        service
            .admit(&request("ws2", "acct-card", VmSize::Small), true)
            .unwrap();
    }

    #[test]
    fn a_workspace_is_handed_the_address_its_session_dials_the_relay_at() {
        use super::super::network::Endpoint;

        let dir = tempfile::tempdir().unwrap();
        let with_relay = |relay: &str| {
            let mut config = config(dir.path());
            config.settings.relay = Some(Endpoint::parse(relay).unwrap());
            open_at(&config, 0)
        };
        let dialled = |line: &str| -> std::net::SocketAddr {
            let line: tilo_relay_protocol::hosted::Material = serde_json::from_str(line).unwrap();
            line.relay
                .addrs
                .into_iter()
                .find_map(|address| match address {
                    iroh_base::TransportAddr::Ip(socket) => Some(socket),
                    _ => None,
                })
                .unwrap()
        };
        // phase: a relay on the host's own loopback is dialled at the address
        // `pasta` maps there
        let local = with_relay("127.0.0.1:7704");
        let line = local
            .hand_off(&browser::test_material("127.0.0.1:7704"))
            .unwrap();
        assert_eq!(
            dialled(&line),
            "10.0.2.4:7704".parse().unwrap(),
            "the VM dials the mapped loopback, not the host's own"
        );
        // phase: a relay at a cluster address (a hosted stack) is dialled as
        // it is
        let hosted = with_relay("10.0.0.9:7704");
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
    fn a_host_without_the_relays_endpoint_serves_no_browser() {
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
            panic!("a host with no relay endpoint opens nothing");
        };
        assert_eq!(
            message,
            "this host serves no Browser: it was started without the relay's endpoint"
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
