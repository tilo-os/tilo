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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use legacy_agent_protocol::link::AgentLink;

use super::browser::{self, Browser, Named, Pool, Session, UP_PATIENCE};
use super::control::{
    FRAMING, HostLoad, OpenBrowser, Reply, Request, ShareRequest, VmRequest, VmStatus,
};
use super::files::Served;
use super::firecracker::VmFiles;
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
    /// The host is being taken back: no more wakes.
    evicting: AtomicBool,
    /// The most memory the VMs awake on this host may take together, by
    /// their sizes; `None` bounds them by the slots alone.
    memory_budget_mib: Option<u64>,
    /// Wakes waiting for room: what each asks for, and when it last asked.
    waiting: Mutex<BTreeMap<String, (u64, u64)>>,
    /// The browser environments it runs, and the spares kept for the next
    /// workspace.
    browsers: Mutex<Pool>,
}

/// Why a workspace's browser environment did not open.
#[derive(Debug)]
pub enum BrowserError {
    /// This host runs no browser environments: its service was started
    /// without the browser guest image.
    NotOffered,
    Invalid(String),
    /// Not now: every machine is full ([`BUSY`]). The caller waits for
    /// room and asks again.
    Full(String),
    /// The environment's session did not come up.
    Session(String),
    Vm(VmError),
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotOffered => {
                f.write_str("this host runs no browser environments: it has no browser guest image")
            }
            Self::Invalid(message) => f.write_str(message),
            Self::Full(words) => f.write_str(words),
            Self::Session(message) => write!(f, "the browser environment did not open: {message}"),
            Self::Vm(error) => write!(f, "the browser environment could not boot: {error}"),
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
    /// open. A browser environment keeps nothing, so what a service that
    /// stopped mid-life left of one is deleted, not reopened.
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
            memory_budget_mib: None,
            waiting: Mutex::new(BTreeMap::new()),
            browsers: Mutex::new(Pool::default()),
            config,
        };
        let vms_dir = service.config.state.join("vms");
        for workspace in read_names(&vms_dir) {
            for environment in read_names(&vms_dir.join(&workspace)) {
                let files = service.config.files(&workspace, &environment);
                let Some(meter) = Meter::load(&files.meter()) else {
                    continue;
                };
                if meter.identity.size == VmSize::Browser {
                    let left = HostedVm::open(
                        meter.identity,
                        files,
                        Arc::new(service.config.settings.clone()),
                        None,
                        Arc::clone(&service.clock),
                    );
                    if let Ok(mut vm) = left {
                        let _ = vm.stop();
                    }
                    continue;
                }
                service.insert(meter.identity, files)?;
            }
        }
        Ok(service)
    }

    /// Bound the VMs awake at once by the memory their sizes take
    /// together: a wake beyond it is refused in plain words, so a small
    /// host never runs out of memory under its VMs.
    #[must_use]
    pub const fn with_memory_budget(mut self, mib: u64) -> Self {
        self.memory_budget_mib = Some(mib);
        self
    }

    #[must_use]
    pub const fn config(&self) -> &ServiceConfig {
        &self.config
    }

    fn insert(&self, identity: VmIdentity, files: VmFiles) -> Result<Arc<Entry>, VmError> {
        // A browser environment is not billed to an account: the relay counts
        // the page actions, and the machine it runs on is the host's cost.
        let ledger = (identity.size != VmSize::Browser).then(|| self.ledger.clone());
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
        if request.size == VmSize::Browser {
            return Err(EnsureError::Invalid(
                "a browser environment is opened with its own request, not by a shell run"
                    .to_owned(),
            ));
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
        // Under the admission lock, so no wake slips in after the eviction's
        // suspends began.
        if self.evicting.load(Ordering::SeqCst) {
            return Err(EnsureError::Moving);
        }
        let plans = Plans::load(&self.config.plans).map_err(EnsureError::Invalid)?;
        let id = format!("{}/{}", request.workspace, request.environment);
        // Under the admission lock, so the room it finds is the room the
        // entry takes.
        if !self.room_for(request.size, &id) {
            self.waits(&id, request.size);
            return Err(EnsureError::HostFull);
        }
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

    /// What the awake VMs other than `except` take of the memory budget.
    fn awake_mib(&self, except: &str) -> u64 {
        lock(&self.vms)
            .values()
            .filter(|entry| entry.id() != except && entry.awake.load(Ordering::SeqCst))
            .map(|entry| lock(&entry.size).memory_mib())
            .sum()
    }

    /// Whether one more VM of `size` fits in the memory budget, beside every
    /// awake VM but the one named `except` (a VM asking for room for itself).
    fn room_for(&self, size: VmSize, except: &str) -> bool {
        self.memory_budget_mib.is_none_or(|budget_mib| {
            self.awake_mib(except).saturating_add(size.memory_mib()) <= budget_mib
        })
    }

    /// Note that `id` waits for room: what a placer starts another machine
    /// for.
    fn waits(&self, id: &str, size: VmSize) {
        lock(&self.waiting).insert(id.to_owned(), (size.memory_mib(), (self.clock)()));
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

    /// Open `request`'s workspace's browser environment: the one already
    /// serving it, a spare, or a freshly booted one. `Some(ms)` when this
    /// request handed it the workspace; `None` when it attached to the
    /// environment a person already opened. Either way, the provider token of
    /// the window it holds, which page actions on it name.
    pub fn open_browser(
        &self,
        request: &OpenBrowser,
    ) -> Result<(Option<u64>, String), BrowserError> {
        if !valid_name(&request.workspace) {
            return Err(BrowserError::Invalid(format!(
                "{:?} is not a workspace id",
                request.workspace
            )));
        }
        self.config
            .settings
            .browser
            .as_ref()
            .ok_or(BrowserError::NotOffered)?;
        // Before anything boots: a material the session could not join with
        // refuses the open, and no spare is spent on it.
        let material = self.hand_off(&request.material)?;
        let named = self.browser_for(&request.workspace, &request.account)?;
        match named.browser.open(&material, UP_PATIENCE) {
            Ok(opened_ms) => {
                lock(&self.waiting).remove(&browser::environment(&request.workspace));
                let provider = named.browser.life().provider().ok_or_else(|| {
                    BrowserError::Session(
                        "the browser environment came up without a window".to_owned(),
                    )
                })?;
                Ok((opened_ms, provider))
            }
            Err(message) => {
                // A spare or a fresh environment that did not come up is of
                // no use to this workspace: end it, and say why.
                let _ = self.stop_browser(&request.workspace);
                Err(BrowserError::Session(message))
            }
        }
    }

    /// The line a workspace's session is handed: the relay's material, with
    /// the relay's address replaced by the one this VM dials
    /// ([`browser::hand_off`]).
    fn hand_off(&self, material: &str) -> Result<String, BrowserError> {
        let guest = self
            .config
            .settings
            .browser
            .as_ref()
            .ok_or(BrowserError::NotOffered)?;
        browser::hand_off(material, guest.relay.guest()).map_err(BrowserError::Invalid)
    }

    /// The environment serving `workspace`: the one it has, a spare it takes,
    /// or one booted for it.
    fn browser_for(&self, workspace: &str, account: &str) -> Result<Named, BrowserError> {
        let claimed = lock(&self.browsers).claim(workspace);
        if let Some(named) = claimed {
            return Ok(named);
        }
        let environment = browser::environment(workspace);
        if !self.room_for(VmSize::Browser, &environment) {
            self.waits(&environment, VmSize::Browser);
            return Err(BrowserError::Full(BUSY.to_owned()));
        }
        let named = self.boot_browser(&environment, account)?;
        if lock(&self.browsers).serve(workspace, named.clone()) {
            return Ok(named);
        }
        // Another request served the workspace while this one booted it: this
        // environment waits for the next workspace, and this call follows the
        // one that got there first.
        lock(&self.browsers).spare_booted(named);
        lock(&self.browsers).serving(workspace).ok_or_else(|| {
            BrowserError::Session("the workspace's browser environment went away".to_owned())
        })
    }

    /// End `workspace`'s browser environment now, with everything it wrote.
    pub fn stop_browser(&self, workspace: &str) -> Result<(), VmError> {
        let Some(named) = lock(&self.browsers).close(workspace) else {
            return Ok(());
        };
        self.end_environment(&named.environment, "the workspace closed it")
    }

    /// Stop and delete one browser environment's VM.
    fn end_environment(&self, environment: &str, why: &str) -> Result<(), VmError> {
        let entry = lock(&self.vms).remove(environment);
        let Some(entry) = entry else {
            return Ok(());
        };
        eprintln!("tilo-legacy hosted: {environment}: ended ({why})");
        let mut vm = lock(&entry.vm);
        let stopped = vm.stop();
        entry.awake.store(false, Ordering::SeqCst);
        drop(vm);
        stopped
    }

    /// Boot a browser environment named `environment` (a workspace's
    /// `<workspace>/browser`, or a spare's): the VM boots from the browser
    /// guest image on a fresh disk, and its session starts and waits for the
    /// workspace's material.
    fn boot_browser(&self, environment: &str, account: &str) -> Result<Named, BrowserError> {
        let (workspace, name) = match environment.split_once('/') {
            Some((workspace, name)) => (workspace.to_owned(), name.to_owned()),
            None => (environment.to_owned(), browser::BROWSER.to_owned()),
        };
        let files = self.config.files(&workspace, &name);
        let entry = self
            .insert(
                VmIdentity {
                    vm: environment.to_owned(),
                    workspace,
                    environment: name,
                    account: account.to_owned(),
                    size: VmSize::Browser,
                },
                files,
            )
            .map_err(BrowserError::Vm)?;
        let browser = Arc::new(Browser::new((self.clock)()));
        let booted = (|| {
            let link = {
                let mut vm = lock(&entry.vm);
                let (link, _) = vm.ensure(&[]).map_err(BrowserError::Vm)?;
                link
            };
            entry.awake.store(true, Ordering::SeqCst);
            let log = self
                .config
                .files(&entry.workspace, &entry.environment)
                .state
                .join("session.log");
            let session = Session::start(
                &link,
                Arc::clone(browser.life()),
                log,
                Arc::clone(&self.clock),
            )
            .map_err(|error| BrowserError::Session(error.to_string()))?;
            browser.attach(session);
            browser
                .wait_ready(browser::READY_PATIENCE)
                .map_err(BrowserError::Session)
        })();
        if let Err(error) = booted {
            let _ = self.end_environment(environment, "it did not come up");
            return Err(error);
        }
        Ok(Named {
            environment: environment.to_owned(),
            browser,
        })
    }

    /// End the browser environments that are at their end, and keep the
    /// spares the host wants. Called between polls.
    pub fn maintain(self: &Arc<Self>) {
        let Some(guest) = self.config.settings.browser.clone() else {
            return;
        };
        let now = (self.clock)();
        let work = lock(&self.browsers).maintain(now, guest.limits(), guest.spares);
        for (environment, end) in work.end {
            let _ = self.end_environment(&environment, &end.to_string());
        }
        for _ in 0..work.boot {
            if !self.room_for(VmSize::Browser, "") {
                break;
            }
            let environment = lock(&self.browsers).spare_opening();
            let service = Arc::clone(self);
            std::thread::spawn(move || match service.boot_browser(&environment, "") {
                Ok(named) => lock(&service.browsers).spare_booted(named),
                Err(error) => {
                    lock(&service.browsers).spare_failed();
                    eprintln!("tilo-legacy hosted: {environment}: {error}");
                }
            });
        }
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

    /// The host is being taken back: take no more wakes, and suspend every
    /// awake VM onto the state directory, all at once, since the notice is
    /// short and each suspend mostly waits on its disk. A VM whose suspend
    /// fails is stopped, so its disk is consistent and it boots afresh.
    pub fn evict(&self) {
        {
            let _admission = lock(&self.admission);
            self.evicting.store(true, Ordering::SeqCst);
        }
        let (browsers, others): (Vec<Arc<Entry>>, Vec<Arc<Entry>>) = lock(&self.vms)
            .values()
            .cloned()
            .partition(|entry| *lock(&entry.size) == VmSize::Browser);
        std::thread::scope(|scope| {
            for entry in &others {
                scope.spawn(move || {
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
        // A browser environment keeps nothing, and a snapshot of a page is
        // worth no host: they are stopped with everything they wrote.
        let environments: Vec<String> = browsers.iter().map(|entry| entry.id()).collect();
        *lock(&self.browsers) = Pool::default();
        for environment in environments {
            let _ = self.end_environment(&environment, "the host is being taken back");
        }
    }

    /// Whether the host is being taken back.
    #[must_use]
    pub fn evicting(&self) -> bool {
        self.evicting.load(Ordering::SeqCst)
    }

    /// What the awake VMs take of the memory budget, and what waiting
    /// wakes ask for.
    #[must_use]
    pub fn load(&self) -> HostLoad {
        let awake_mib = self.awake_mib("");
        let now = (self.clock)();
        let mut waiting = lock(&self.waiting);
        waiting.retain(|_, (_, at)| now.saturating_sub(*at) <= WAITING_FOR_MS);
        HostLoad {
            budget_mib: self.memory_budget_mib,
            awake_mib,
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
            Request::StopBrowser { workspace } => self
                .stop_browser(&workspace)
                .map_or_else(|error| Reply::Error(error.to_string()), |()| Reply::Done),
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
                jail: None,
                browser: None,
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
    fn a_host_being_taken_back_wakes_nothing_and_keeps_its_vms_for_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        existing(&config, "ws1", "acct-1", None);
        let service = Service::open(config.clone(), clock(0)).unwrap();
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
        let elsewhere = Service::open(next, clock(10_000)).unwrap();
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
    fn a_host_wakes_no_more_than_its_memory_budget_holds() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let service = Service::open(config, clock(0))
            .unwrap()
            .with_memory_budget(12 * 1024);
        let admitted = |workspace: &str, size: VmSize| {
            service
                .admit(&request(workspace, &format!("acct-{workspace}"), size))
                .map(|_| ())
        };
        admitted("ws1", VmSize::Default).unwrap();
        admitted("ws2", VmSize::Small).unwrap();
        // phase: a VM already awake is not counted twice
        admitted("ws1", VmSize::Default).unwrap();
        // phase: one more would pass the budget, whoever asks: it waits, and is demand
        assert!(matches!(
            admitted("ws3", VmSize::Small),
            Err(EnsureError::HostFull)
        ));
        let Reply::Busy(words) = service.handle(Request::Ensure {
            vm: request("ws3", "acct-ws3", VmSize::Small),
            shares: Vec::new(),
        }) else {
            panic!("a full host makes the wake wait");
        };
        assert_eq!(words, "Starting another machine for it, about 3 minutes.");
        assert_eq!(
            service.load(),
            HostLoad {
                budget_mib: Some(12 * 1024),
                awake_mib: 12 * 1024,
                waiting_mib: 4 * 1024,
            }
        );
        // phase: once one sleeps there is room again
        service
            .entry("ws2", "default")
            .unwrap()
            .awake
            .store(false, Ordering::SeqCst);
        admitted("ws3", VmSize::Small).unwrap();
        assert_eq!(service.load().waiting_mib, 0, "a woken VM waits no more");
    }

    #[test]
    fn browser_environments_count_in_the_hosts_memory_and_keep_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let service = Service::open(config.clone(), clock(0))
            .unwrap()
            .with_memory_budget(4 * 1024);
        let entry = service
            .insert(
                VmIdentity {
                    vm: "ws1/browser".into(),
                    workspace: "ws1".into(),
                    environment: "browser".into(),
                    account: "acct-1".into(),
                    size: VmSize::Browser,
                },
                config.files("ws1", "browser"),
            )
            .unwrap();
        entry.awake.store(true, Ordering::SeqCst);
        assert_eq!(
            service.load().awake_mib,
            2 * 1024,
            "a browser environment takes its size of the budget, like any VM"
        );
        assert!(
            matches!(
                service.admit(&request("ws2", "acct-2", VmSize::Small)),
                Err(EnsureError::HostFull)
            ),
            "a 4 GiB Linux VM no longer fits beside it"
        );
        // phase: stopping it deletes what it wrote, and bills nobody
        lock(&entry.vm).stop().unwrap();
        assert!(
            !config.files("ws1", "browser").state.exists(),
            "a browser environment keeps nothing"
        );
        assert!(
            service.ledger.records("acct-1").unwrap().is_empty(),
            "the relay counts page actions, not the machine"
        );
    }

    #[test]
    fn a_workspace_is_handed_the_address_its_session_dials_the_relay_at() {
        use super::super::browser::{self, BrowserGuest};
        use super::super::network::Endpoint;

        let dir = tempfile::tempdir().unwrap();
        let with_relay = |relay: &str| {
            let mut config = config(dir.path());
            let machine = config.settings.machine.clone();
            config.settings.browser =
                Some(BrowserGuest::new(machine, Endpoint::parse(relay).unwrap()));
            Service::open(config, clock(0)).unwrap()
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
        let line = local.hand_off(&browser::test_material("127.0.0.1:7704")).unwrap();
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
        // phase: a host that runs no browser environments hands nothing on
        let none = Service::open(config(dir.path()), clock(0)).unwrap();
        assert!(matches!(
            none.hand_off(&browser::test_material("127.0.0.1:7704")),
            Err(BrowserError::NotOffered)
        ));
        // and a material the session could not join with refuses the open
        assert!(matches!(
            local.hand_off("{}"),
            Err(BrowserError::Invalid(_))
        ));
    }
    #[test]
    fn a_service_restarted_mid_life_deletes_a_browser_environment() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let files = config.files("spare", "browser-1");
        let mut meter = Meter::new(VmIdentity {
            vm: "spare/browser-1".into(),
            workspace: "spare".into(),
            environment: "browser-1".into(),
            account: "spare".into(),
            size: VmSize::Browser,
        });
        meter.started(1_000);
        meter.save(&files.meter()).unwrap();
        std::fs::create_dir_all(files.vmm()).unwrap();
        std::fs::write(files.disk(), b"a disk").unwrap();

        let service = Service::open(config, clock(10_000)).unwrap();
        assert!(
            service.status().is_empty(),
            "a browser environment is not reopened where it was left"
        );
        assert!(!files.state.exists(), "what it left is deleted");
    }

    #[test]
    fn a_host_without_a_browser_guest_runs_no_browser_environments() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::open(config(dir.path()), clock(0)).unwrap();
        let Reply::Error(message) = service.handle(Request::OpenBrowser {
            browser: OpenBrowser {
                workspace: "ws1".into(),
                account: "acct-1".into(),
                material: "{}".into(),
            },
        }) else {
            panic!("a host with no browser guest image opens nothing");
        };
        assert_eq!(
            message,
            "this host runs no browser environments: it has no browser guest image"
        );
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
        let service = Service::open(config, clock(0)).unwrap();
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
