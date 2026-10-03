//! The per-user cloud account service and its mount processes.
//!
//! `fs cloud-service` (user unit `tilo-fs-cloud.service`) watches the
//! account store Settings writes, and keeps exactly one mount per enabled
//! file account at `~/Remote/<account label>`, the way `fs
//! recipient-service` keeps received shares at `~/Shared/…`:
//!
//! - an account appears or is enabled → a mount process starts with the
//!   account's one short-lived credential on its stdin;
//! - an account is disabled, its configuration changes or it is removed →
//!   the mount stops and its directory goes; removal also drops the
//!   account's cache and queued state;
//! - a mount reports health → the service records it in the account
//!   status store Settings and Files read.
//!
//! Neither this service nor its mounts open Settings' account vault. The
//! service reads each account's short-lived credential from the handoff
//! store ([`accounts::HandoffCredentialSource`]); when it is missing, expired
//! or rejected, the handoff runs Settings' credential helper
//! (`tilo-account-credential refresh <id>`, through bounded-command), which
//! alone holds the vault for that one call. Mount processes see one
//! credential.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use accounts::{
    AccountId, AccountStore, AccountSyncReporter, CredentialRefresher, HandoffCredentialSource,
    HandoffStore,
};
use cloud_source::{Credential, CredentialSource as _};

use super::credential::{
    EXPIRED_LINE, HEALTH_PREFIX, PipeCredential, READY_LINE, REJECTED_LINE, ToMount,
    WireCredential, line,
};
use super::providers::{self, MountSpec};
use super::volume::{CloudVolume, Health, VolumeConfig};

/// What a running mount tells the service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountEvent {
    Health(Health),
    CredentialRejected,
    CredentialExpired,
    Exited,
}

/// A started mount.
pub trait RunningMount: Send {
    fn send_credential(&mut self, credential: &Credential) -> io::Result<()>;
    /// Unmount and wait for the mount to end.
    fn stop(self: Box<Self>);
}

/// Starts mounts. [`ProcessLauncher`] runs `fs mount-cloud` children with a
/// FUSE mount each; [`InProcessLauncher`] runs volumes in this process
/// without the kernel mount (tools and tests).
pub trait Launcher: Send + Sync {
    fn start(
        &self,
        spec: &MountSpec,
        credential: &Credential,
        mount_point: &Path,
        state_dir: &Path,
        events: Box<dyn Fn(MountEvent) + Send + Sync>,
    ) -> io::Result<Box<dyn RunningMount>>;
}

pub struct ServiceConfig {
    /// The Settings config root; `None` is the user's (`$XDG_CONFIG_HOME/tilo`).
    pub config_root: Option<PathBuf>,
    /// Where account folders appear (`~/Remote`).
    pub mounts_root: PathBuf,
    /// Per-account private state: metadata, queue, cache.
    pub state_root: PathBuf,
    /// Publishes a fresh handoff credential when one is missing, expired or
    /// rejected: [`accounts::CommandRefresher`] (Settings' credential
    /// helper) in production.
    pub refresher: Arc<dyn CredentialRefresher>,
}

/// How often accounts that could not start (offline, no credential yet)
/// are tried again while nothing else happens.
const RETRY_INTERVAL: Duration = Duration::from_secs(60);

/// Why the service needs an account's credential.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Need {
    /// To start a mount, or because the mount's one expired.
    Current,
    /// The server rejected the one the mount holds.
    Replacement,
}

struct Mounted {
    spec: MountSpec,
    dir_name: String,
    running: Box<dyn RunningMount>,
}

enum Signal {
    Accounts,
    Event(String, MountEvent),
}

pub struct CloudService {
    config: ServiceConfig,
    launcher: Box<dyn Launcher>,
    mounts: HashMap<String, Mounted>,
    credentials: HashMap<String, Arc<HandoffCredentialSource>>,
    /// Accounts to start again after [`RETRY_INTERVAL`].
    retry: HashSet<String>,
    signals: Sender<Signal>,
    inbox: Receiver<Signal>,
}

fn state_dir_name(account_id: &str) -> String {
    hex::encode(account_id.as_bytes())
}

impl CloudService {
    pub fn new(config: ServiceConfig, launcher: Box<dyn Launcher>) -> Self {
        let (signals, inbox) = mpsc::channel();
        Self {
            config,
            launcher,
            mounts: HashMap::new(),
            credentials: HashMap::new(),
            retry: HashSet::new(),
            signals,
            inbox,
        }
    }

    fn store(&self) -> AccountStore {
        match &self.config.config_root {
            Some(root) => AccountStore::load_in(root),
            None => AccountStore::load(),
        }
    }

    fn handoff(&self) -> Option<HandoffStore> {
        match &self.config.config_root {
            Some(root) => Some(HandoffStore::open_in(root)),
            None => HandoffStore::open(),
        }
    }

    fn reporter(&self, id: &str) -> AccountSyncReporter {
        match &self.config.config_root {
            Some(root) => AccountSyncReporter::in_root(AccountId::from(id), root.clone()),
            None => AccountSyncReporter::new(AccountId::from(id)),
        }
    }

    /// The account's credential from the handoff store, refreshed through
    /// the credential helper when needed. Failures are reported to the
    /// account status store.
    fn credential(&mut self, account_id: &str, need: Need) -> Option<Credential> {
        let source = if let Some(source) = self.credentials.get(account_id) {
            Arc::clone(source)
        } else {
            let Some(handoff) = self.handoff() else {
                tracing::warn!("no config directory for the credential handoff");
                let _ = self.reporter(account_id).failed();
                return None;
            };
            let source = Arc::new(HandoffCredentialSource::new(
                AccountId::from(account_id),
                handoff,
                Arc::clone(&self.config.refresher),
            ));
            self.credentials
                .insert(account_id.to_owned(), Arc::clone(&source));
            source
        };
        if need == Need::Replacement {
            source.rejected();
        }
        match source.credential() {
            Ok(credential) => Some(credential),
            Err(cloud_source::Error::Offline) => {
                let _ = self.reporter(account_id).offline();
                self.retry.insert(account_id.to_owned());
                None
            }
            Err(error) => {
                tracing::info!(%error, "no credential for a cloud account");
                let _ = self.reporter(account_id).needs_authentication();
                None
            }
        }
    }

    /// The account folders currently mounted: `(account id, directory)`.
    pub fn mounted(&self) -> Vec<(String, PathBuf)> {
        let mut out: Vec<(String, PathBuf)> = self
            .mounts
            .iter()
            .map(|(id, m)| (id.clone(), self.config.mounts_root.join(&m.dir_name)))
            .collect();
        out.sort();
        out
    }

    /// Bring the mounts in line with the account store.
    pub fn reconcile(&mut self) {
        self.retry.clear();
        let store = self.store();
        let wanted: Vec<MountSpec> = store.all().iter().filter_map(providers::spec_for).collect();
        let labels: Vec<(&str, &str)> = store
            .all()
            .iter()
            .filter(|account| providers::is_file_account(account))
            .map(|account| (account.id.as_str(), account.display_name.as_str()))
            .collect();
        let names: HashMap<String, String> = cloud_source::tree::mount_names(&labels)
            .into_iter()
            .collect();

        let stale: Vec<String> = self
            .mounts
            .iter()
            .filter(|(id, mounted)| {
                !wanted.iter().any(|spec| {
                    &spec.account_id == *id
                        && *spec == mounted.spec
                        && names.get(*id) == Some(&mounted.dir_name)
                })
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            let removed = store.get(&AccountId::from(id.as_str())).is_none();
            self.unmount(&id, removed);
        }

        for spec in &wanted {
            if self.mounts.contains_key(&spec.account_id) {
                continue;
            }
            let Some(dir_name) = names.get(&spec.account_id).cloned() else {
                continue;
            };
            let Some(credential) = self.credential(&spec.account_id, Need::Current) else {
                continue;
            };
            let mount_point = self.config.mounts_root.join(&dir_name);
            let state_dir = self
                .config
                .state_root
                .join(state_dir_name(&spec.account_id));
            if let Err(error) = prepare_dir(&mount_point).and_then(|()| prepare_dir(&state_dir)) {
                tracing::warn!(%error, "cannot prepare a cloud mount directory");
                let _ = self.reporter(&spec.account_id).failed();
                continue;
            }
            let signals = self.signals.clone();
            let id = spec.account_id.clone();
            let events: Box<dyn Fn(MountEvent) + Send + Sync> = Box::new(move |event| {
                let _ = signals.send(Signal::Event(id.clone(), event));
            });
            let _ = self.reporter(&spec.account_id).started();
            match self
                .launcher
                .start(spec, &credential, &mount_point, &state_dir, events)
            {
                Ok(running) => {
                    self.mounts.insert(
                        spec.account_id.clone(),
                        Mounted {
                            spec: spec.clone(),
                            dir_name,
                            running,
                        },
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, "cloud mount failed to start");
                    let _ = self.reporter(&spec.account_id).failed();
                    let _ = fs::remove_dir(&mount_point);
                    self.retry.insert(spec.account_id.clone());
                }
            }
        }
    }

    fn unmount(&mut self, id: &str, removed: bool) {
        let Some(mounted) = self.mounts.remove(id) else {
            return;
        };
        mounted.running.stop();
        let _ = fs::remove_dir(self.config.mounts_root.join(&mounted.dir_name));
        self.credentials.remove(id);
        if removed {
            let _ = fs::remove_dir_all(self.config.state_root.join(state_dir_name(id)));
            let _ = self.reporter(id).remove();
        }
    }

    /// Handle one mount event (health, credential refresh, exit).
    fn on_event(&mut self, id: &str, event: MountEvent) {
        match event {
            MountEvent::Health(health) => {
                let reporter = self.reporter(id);
                let _ = match health {
                    Health::Syncing => reporter.started(),
                    Health::Healthy => reporter.succeeded(),
                    Health::Offline => reporter.offline(),
                    Health::NeedsAuthentication => reporter.needs_authentication(),
                    Health::Error => reporter.failed(),
                };
            }
            MountEvent::CredentialRejected | MountEvent::CredentialExpired => {
                if !self.mounts.contains_key(id) {
                    return;
                }
                let need = if event == MountEvent::CredentialRejected {
                    Need::Replacement
                } else {
                    Need::Current
                };
                if let (Some(credential), Some(mounted)) =
                    (self.credential(id, need), self.mounts.get_mut(id))
                {
                    let _ = mounted.running.send_credential(&credential);
                }
            }
            MountEvent::Exited => {
                // Started again by the next reconcile.
                if let Some(mounted) = self.mounts.remove(id) {
                    let _ = fs::remove_dir(self.config.mounts_root.join(&mounted.dir_name));
                }
            }
        }
    }

    /// Process queued events without blocking.
    pub fn pump(&mut self) {
        while let Ok(signal) = self.inbox.try_recv() {
            self.handle(signal);
        }
    }

    /// Handle events until `account_id` reports `health`, for at most
    /// `timeout`. Returns whether it did.
    pub fn wait_for_health(&mut self, account_id: &str, health: Health, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now().checked_add(timeout);
        loop {
            let left = deadline.map_or(timeout, |deadline| {
                deadline.saturating_duration_since(std::time::Instant::now())
            });
            let Ok(signal) = self.inbox.recv_timeout(left) else {
                return false;
            };
            let hit = matches!(
                &signal,
                Signal::Event(id, MountEvent::Health(h)) if id == account_id && *h == health
            );
            self.handle(signal);
            if hit {
                return true;
            }
        }
    }

    fn handle(&mut self, signal: Signal) {
        match signal {
            Signal::Accounts => self.reconcile(),
            Signal::Event(id, event) => {
                let exited = event == MountEvent::Exited;
                self.on_event(&id, event);
                if exited {
                    self.reconcile();
                }
            }
        }
    }

    /// Serve until the process is stopped: reconcile now and whenever the
    /// account store changes.
    pub fn run(mut self) -> io::Result<()> {
        use notify::Watcher as _;
        let accounts_dir = match &self.config.config_root {
            Some(root) => root.join("accounts"),
            None => accounts::config_root()
                .ok_or_else(|| io::Error::other("no user config directory"))?
                .join("accounts"),
        };
        fs::create_dir_all(&accounts_dir)?;
        let signals = self.signals.clone();
        let mut watcher = notify::recommended_watcher(move |_event| {
            let _ = signals.send(Signal::Accounts);
        })
        .map_err(io::Error::other)?;
        watcher
            .watch(&accounts_dir, notify::RecursiveMode::NonRecursive)
            .map_err(io::Error::other)?;
        self.reconcile();
        loop {
            let signal = if self.retry.is_empty() {
                self.inbox.recv().ok()
            } else {
                match self.inbox.recv_timeout(RETRY_INTERVAL) {
                    Ok(signal) => Some(signal),
                    Err(mpsc::RecvTimeoutError::Timeout) => Some(Signal::Accounts),
                    Err(mpsc::RecvTimeoutError::Disconnected) => None,
                }
            };
            let Some(signal) = signal else { break };
            self.handle(signal);
            self.pump();
        }
        Ok(())
    }

    /// Stop every mount.
    pub fn shutdown(&mut self) {
        let ids: Vec<String> = self.mounts.keys().cloned().collect();
        for id in ids {
            self.unmount(&id, false);
        }
    }
}

fn prepare_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

// ---- mounts as child processes ---------------------------------------------

/// Runs each mount as `fs mount-cloud` with a FUSE mount.
pub struct ProcessLauncher {
    pub executable: PathBuf,
    /// How long a mount may take to report readiness.
    pub ready_timeout: Duration,
}

struct ChildMount {
    child: Child,
    stdin: Option<ChildStdin>,
    mount_point: PathBuf,
    /// Signalled when the child's stdout closes (it exited).
    exited: Receiver<()>,
}

impl Launcher for ProcessLauncher {
    fn start(
        &self,
        spec: &MountSpec,
        credential: &Credential,
        mount_point: &Path,
        state_dir: &Path,
        events: Box<dyn Fn(MountEvent) + Send + Sync>,
    ) -> io::Result<Box<dyn RunningMount>> {
        let mut child = Command::new(&self.executable)
            .arg("mount-cloud")
            .arg("--mount-point")
            .arg(mount_point)
            .arg("--state-dir")
            .arg(state_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no stdin"))?;
        stdin.write_all(&line(&ToMount::Start(spec.clone())))?;
        stdin.write_all(&line(&ToMount::Credential(WireCredential::from(
            credential,
        ))))?;
        stdin.flush()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout"))?;
        let (ready_tx, ready_rx) = mpsc::channel();
        let (exit_tx, exited) = mpsc::channel();
        std::thread::spawn(move || {
            let mut ready = Some(ready_tx);
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line == READY_LINE {
                    if let Some(tx) = ready.take() {
                        let _ = tx.send(());
                    }
                } else if line == REJECTED_LINE {
                    events(MountEvent::CredentialRejected);
                } else if line == EXPIRED_LINE {
                    events(MountEvent::CredentialExpired);
                } else if let Some(state) = line.strip_prefix(HEALTH_PREFIX)
                    && let Some(health) = Health::parse(state)
                {
                    events(MountEvent::Health(health));
                }
            }
            let _ = exit_tx.send(());
            events(MountEvent::Exited);
        });
        let mut mount = ChildMount {
            child,
            stdin: Some(stdin),
            mount_point: mount_point.to_path_buf(),
            exited,
        };
        if ready_rx.recv_timeout(self.ready_timeout).is_err() {
            mount.halt();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the cloud mount did not become ready",
            ));
        }
        Ok(Box::new(mount))
    }
}

impl ChildMount {
    fn halt(&mut self) {
        // Closing stdin asks the mount to unmount and exit.
        self.stdin.take();
        if self.exited.recv_timeout(Duration::from_secs(10)).is_ok() {
            let _ = self.child.wait();
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = Command::new("fusermount3")
            .arg("-u")
            .arg(&self.mount_point)
            .status();
    }
}

impl RunningMount for ChildMount {
    fn send_credential(&mut self, credential: &Credential) -> io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::other("mount stopped"))?;
        stdin.write_all(&line(&ToMount::Credential(WireCredential::from(
            credential,
        ))))?;
        stdin.flush()
    }

    fn stop(mut self: Box<Self>) {
        self.halt();
    }
}

// ---- mounts in this process ------------------------------------------------

/// Runs volumes in this process without a kernel mount. The volumes are
/// reachable through [`InProcessLauncher::volume`].
#[derive(Default, Clone)]
pub struct InProcessLauncher {
    volumes: Arc<Mutex<HashMap<String, Arc<CloudVolume>>>>,
}

impl InProcessLauncher {
    pub fn volume(&self, account_id: &str) -> Option<Arc<CloudVolume>> {
        self.volumes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(account_id)
            .cloned()
    }
}

struct InProcessMount {
    account_id: String,
    volume: Arc<CloudVolume>,
    credentials: Arc<PipeCredential>,
    volumes: Arc<Mutex<HashMap<String, Arc<CloudVolume>>>>,
}

impl Launcher for InProcessLauncher {
    fn start(
        &self,
        spec: &MountSpec,
        credential: &Credential,
        _mount_point: &Path,
        state_dir: &Path,
        events: Box<dyn Fn(MountEvent) + Send + Sync>,
    ) -> io::Result<Box<dyn RunningMount>> {
        let events = Arc::new(events);
        let asked = Arc::clone(&events);
        let credentials = Arc::new(PipeCredential::new(
            Some(credential.clone()),
            Box::new(EventWriter(Box::new(move |event| asked(event)))),
        ));
        let source = providers::open(spec, Arc::clone(&credentials) as _)
            .map_err(|e| io::Error::other(e.to_string()))?;
        let health = Arc::clone(&events);
        let volume = CloudVolume::open(
            source,
            VolumeConfig {
                dir: state_dir.to_path_buf(),
                cache_budget: spec.cache_budget,
                poll_interval: Duration::from_secs(30),
            },
            Box::new(move |h| health(MountEvent::Health(h))),
        )?;
        self.volumes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(spec.account_id.clone(), Arc::clone(&volume));
        Ok(Box::new(InProcessMount {
            account_id: spec.account_id.clone(),
            volume,
            credentials,
            volumes: Arc::clone(&self.volumes),
        }))
    }
}

/// Turns the credential source's "rejected" and "expired" lines into
/// events.
struct EventWriter(Box<dyn Fn(MountEvent) + Send>);

impl Write for EventWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.starts_with(REJECTED_LINE.as_bytes()) {
            (self.0)(MountEvent::CredentialRejected);
        } else if buf.starts_with(EXPIRED_LINE.as_bytes()) {
            (self.0)(MountEvent::CredentialExpired);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl RunningMount for InProcessMount {
    fn send_credential(&mut self, credential: &Credential) -> io::Result<()> {
        self.credentials.update(credential.clone());
        Ok(())
    }

    fn stop(self: Box<Self>) {
        self.volume.shutdown();
        self.volumes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.account_id);
    }
}

// ---- the mount process ------------------------------------------------------

/// `fs mount-cloud`: read the spec and credential from stdin, mount the
/// account at `mount_point` and serve until stdin closes.
pub fn run_mount(mount_point: &Path, state_dir: &Path) -> io::Result<()> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut first = String::new();
    input.read_line(&mut first)?;
    let Ok(ToMount::Start(spec)) = serde_json::from_str::<ToMount>(&first) else {
        return Err(io::Error::other("expected a mount specification"));
    };
    let mut second = zeroize::Zeroizing::new(String::new());
    input.read_line(&mut second)?;
    let initial = match serde_json::from_str::<ToMount>(&second) {
        Ok(ToMount::Credential(wire)) => Some(Credential::from(wire)),
        _ => None,
    };
    drop(second);
    let credentials = Arc::new(PipeCredential::new(initial, Box::new(io::stdout())));
    let source = providers::open(&spec, Arc::clone(&credentials) as _)
        .map_err(|e| io::Error::other(e.to_string()))?;
    let volume = CloudVolume::open(
        source,
        VolumeConfig {
            dir: state_dir.to_path_buf(),
            cache_budget: spec.cache_budget,
            poll_interval: Duration::from_secs(30),
        },
        Box::new(|health| {
            let mut out = io::stdout();
            let _ = writeln!(out, "{HEALTH_PREFIX}{}", health.as_str());
            let _ = out.flush();
        }),
    )?;
    let workers = volume.spawn_workers();
    let mut config = fuser::Config::default();
    config.mount_options = vec![
        fuser::MountOption::FSName("tilo-cloud".into()),
        fuser::MountOption::Subtype("tilo-cloud".into()),
        fuser::MountOption::DefaultPermissions,
        fuser::MountOption::NoDev,
        fuser::MountOption::NoSuid,
    ];
    config.n_threads = Some(4);
    config.acl = fuser::SessionACL::Owner;
    let session = fuser::spawn_mount(
        super::fuse::CloudFs::new(Arc::clone(&volume)),
        mount_point,
        &config,
    )?;
    {
        let mut out = io::stdout();
        writeln!(out, "{READY_LINE}")?;
        out.flush()?;
    }
    // Credentials keep arriving on stdin; its end is the stop signal.
    credentials.pump(input, || {});
    volume.shutdown();
    session.umount_and_join()?;
    for worker in workers {
        let _ = worker.join();
    }
    Ok(())
}
