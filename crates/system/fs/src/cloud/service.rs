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
//! The service also serves its mounts' control (`runtime_fs::cloud`) on two
//! sockets, each mode 0600 in its own 0700 directory. The account socket
//! (`$XDG_RUNTIME_DIR/tilo-fs-cloud/control.sock`, which Settings' sandbox
//! is granted) lists the mounts with their health and provider
//! capabilities, syncs one now (which also retries one that could not
//! start), and pauses and resumes one (remembered across restarts). The
//! file socket (`$XDG_RUNTIME_DIR/tilo-fs-cloud-files/files.sock`) also
//! asks a mount for a file's revisions, to restore a revision or for a
//! share link; the account socket refuses those. Each request travels down
//! the mount's stdin like a credential and its answer comes back on stdout.
//! No credential crosses either socket.
//!
//! Neither this service nor its mounts open Settings' account vault. The
//! service reads each account's short-lived credential from the handoff
//! store ([`accounts::HandoffCredentialSource`]); when it is missing, expired
//! or rejected, the handoff runs Settings' credential helper
//! (`tilo-account-credential refresh <id>`, through bounded-command), which
//! alone holds the vault for that one call. Mount processes see one
//! credential.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use accounts::{
    AccountId, AccountStore, AccountSyncReporter, CredentialRefresher, HandoffCredentialSource,
    HandoffStore,
};
use cloud_source::{Credential, CredentialSource as _};
use runtime_fs::cloud::{
    CLOUD_FRAMING, CloudCapabilities, CloudMount, CloudRequest, CloudResponse, CloudSocket,
    MountHealth,
};

use super::credential::{
    CAPABILITIES_PREFIX, EXPIRED_LINE, HEALTH_PREFIX, MountReply, PipeCredential, READY_LINE,
    REJECTED_LINE, REPLY_PREFIX, ToMount, WireCredential, capabilities_line, line, reply_line,
};
use super::providers::{self, MountSpec};
use super::volume::{CloudVolume, DEFAULT_IDLE_POLL_MAX, VolumeConfig};
use tilo_lock::MutexRecover as _;

/// What a running mount tells the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MountEvent {
    Health(MountHealth),
    CredentialRejected,
    CredentialExpired,
    /// The answer to a [`ToMount::Request`].
    Reply(MountReply),
    Exited,
}

/// A started mount.
pub trait RunningMount: Send {
    fn send_credential(&mut self, credential: &Credential) -> io::Result<()>;
    /// Send a control message: sync now, pause, resume or a file request.
    fn send(&mut self, message: &ToMount) -> io::Result<()>;
    /// What the mount's provider offers beyond files and folders.
    fn capabilities(&self) -> CloudCapabilities;
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
    /// Where to serve the account socket ([`CloudSocket::Accounts`]: list,
    /// sync now, pause, resume); `None` serves none (tools and tests drive
    /// [`CloudService::request`]).
    pub control_socket: Option<PathBuf>,
    /// Where to serve the file socket ([`CloudSocket::Files`]: everything,
    /// file requests included); `None` serves none. It must not share a
    /// directory with `control_socket`, which a sandbox may be granted.
    pub file_socket: Option<PathBuf>,
}

/// How often accounts that could not start (offline, no credential yet)
/// are tried again while nothing else happens.
const RETRY_INTERVAL: Duration = Duration::from_secs(60);
/// How long a control connection waits for the service's answer; shorter
/// than the client's own timeout, so the client hears why.
const CONTROL_ANSWER: Duration = Duration::from_secs(80);
/// Control connections served at once.
const MAX_CONTROL_CONNECTIONS: usize = 8;
/// A control connection that sends nothing for this long is closed, so idle
/// clients cannot hold every slot.
const CONTROL_IDLE: Duration = Duration::from_secs(30);
/// The accounts paused through the control socket, below the state root.
const PAUSED_FILE: &str = "paused.json";

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
    /// The health the mount last reported.
    health: Option<MountHealth>,
}

enum Signal {
    Accounts,
    Event(String, MountEvent),
    /// A control request and where its answer goes.
    Control(CloudRequest, Sender<CloudResponse>),
}

pub struct CloudService {
    config: ServiceConfig,
    launcher: Box<dyn Launcher>,
    mounts: HashMap<String, Mounted>,
    credentials: HashMap<String, Arc<HandoffCredentialSource>>,
    /// Accounts to start again after [`RETRY_INTERVAL`].
    retry: HashSet<String>,
    /// Accounts whose background sync is paused; kept in the state root.
    paused: BTreeSet<String>,
    /// File requests sent to a mount, by request id: the account and where
    /// the answer goes.
    pending: HashMap<u64, (String, Sender<CloudResponse>)>,
    next_request: u64,
    signals: Sender<Signal>,
    inbox: Receiver<Signal>,
}

fn state_dir_name(account_id: &str) -> String {
    hex::encode(account_id.as_bytes())
}

impl CloudService {
    pub fn new(config: ServiceConfig, launcher: Box<dyn Launcher>) -> Self {
        let (signals, inbox) = mpsc::channel();
        let paused = fs::read(config.state_root.join(PAUSED_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            config,
            launcher,
            mounts: HashMap::new(),
            credentials: HashMap::new(),
            retry: HashSet::new(),
            paused,
            pending: HashMap::new(),
            next_request: 1,
            signals,
            inbox,
        }
    }

    fn save_paused(&self) {
        let path = self.config.state_root.join(PAUSED_FILE);
        let saved = fs::create_dir_all(&self.config.state_root).and_then(|()| {
            let bytes = serde_json::to_vec(&self.paused).map_err(io::Error::other)?;
            let staged = path.with_extension("json.tmp");
            fs::write(&staged, bytes)?;
            fs::rename(&staged, &path)
        });
        if let Err(error) = saved {
            tracing::warn!(%error, "cannot record the paused cloud accounts");
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
            // A paused account's mount starts paused: its workers never run
            // a pass or an upload before the pause.
            let started = MountSpec {
                paused: self.paused.contains(&spec.account_id),
                ..spec.clone()
            };
            match self
                .launcher
                .start(&started, &credential, &mount_point, &state_dir, events)
            {
                Ok(running) => {
                    self.mounts.insert(
                        spec.account_id.clone(),
                        Mounted {
                            spec: spec.clone(),
                            dir_name,
                            running,
                            health: None,
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
        self.drop_pending(id);
        if removed {
            let _ = fs::remove_dir_all(self.config.state_root.join(state_dir_name(id)));
            let _ = self.reporter(id).remove();
            if self.paused.remove(id) {
                self.save_paused();
            }
        }
    }

    /// Answer the file requests still waiting on `id`'s mount: it stopped.
    fn drop_pending(&mut self, id: &str) {
        self.pending.retain(|_, (account, reply)| {
            if account == id {
                let _ = reply.send(CloudResponse::Error("the account's mount stopped".into()));
                false
            } else {
                true
            }
        });
    }

    /// Handle one mount event (health, credential refresh, exit).
    fn on_event(&mut self, id: &str, event: MountEvent) {
        match event {
            MountEvent::Health(health) => {
                if let Some(mounted) = self.mounts.get_mut(id) {
                    mounted.health = Some(health);
                }
                let reporter = self.reporter(id);
                let _ = match health {
                    MountHealth::Syncing => reporter.started(),
                    MountHealth::Healthy => reporter.succeeded(),
                    MountHealth::Offline => reporter.offline(),
                    MountHealth::NeedsAuthentication => reporter.needs_authentication(),
                    MountHealth::Error => reporter.failed(),
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
            MountEvent::Reply(reply) => {
                let Some((_, answer)) = self.pending.remove(&reply.id) else {
                    return;
                };
                let response = match reply.result {
                    Ok(file) => CloudResponse::File(file),
                    Err(error) => CloudResponse::Error(error),
                };
                let _ = answer.send(response);
            }
            MountEvent::Exited => {
                // Started again by the next reconcile.
                if let Some(mounted) = self.mounts.remove(id) {
                    let _ = fs::remove_dir(self.config.mounts_root.join(&mounted.dir_name));
                }
                self.drop_pending(id);
            }
        }
    }

    /// The mounted accounts as the control protocol lists them.
    fn mount_list(&self) -> Vec<CloudMount> {
        let mut mounts: Vec<CloudMount> = self
            .mounts
            .iter()
            .map(|(id, mounted)| CloudMount {
                account: id.clone(),
                mount_point: self.config.mounts_root.join(&mounted.dir_name),
                health: mounted.health,
                paused: self.paused.contains(id),
                capabilities: mounted.running.capabilities(),
            })
            .collect();
        mounts.sort_by(|a, b| a.account.cmp(&b.account));
        mounts
    }

    /// Send `message` to `account`'s mount.
    fn send_to(&mut self, account: &str, message: &ToMount) -> Result<(), String> {
        let mounted = self
            .mounts
            .get_mut(account)
            .ok_or_else(|| "the account is not mounted".to_string())?;
        mounted.running.send(message).map_err(|error| {
            tracing::warn!(%error, "cannot reach a cloud mount");
            "the account's mount is not answering".to_string()
        })
    }

    /// Handle one control request; `reply` gets the answer, at once or
    /// when the mount answers a file request.
    fn control(&mut self, request: CloudRequest, reply: Sender<CloudResponse>) {
        let done = |result: Result<(), String>| match result {
            Ok(()) => CloudResponse::Done,
            Err(error) => CloudResponse::Error(error),
        };
        let response = match request {
            CloudRequest::List => CloudResponse::Mounts(self.mount_list()),
            CloudRequest::SyncNow { account } => {
                if self.mounts.contains_key(&account) {
                    done(self.send_to(&account, &ToMount::SyncNow))
                } else if self
                    .store()
                    .get(&AccountId::from(account.as_str()))
                    .is_some()
                {
                    // A mount that could not start (offline, no credential
                    // yet) is tried again next on this loop rather than at
                    // the next retry; starting it can take a while, so the
                    // answer does not wait for it.
                    let _ = self.signals.send(Signal::Accounts);
                    CloudResponse::Done
                } else {
                    CloudResponse::Error("no such account".into())
                }
            }
            CloudRequest::Pause { account } => {
                let result = self.send_to(&account, &ToMount::Pause);
                if result.is_ok() && self.paused.insert(account) {
                    self.save_paused();
                }
                done(result)
            }
            CloudRequest::Resume { account } => {
                let result = self.send_to(&account, &ToMount::Resume);
                if result.is_ok() && self.paused.remove(&account) {
                    self.save_paused();
                }
                done(result)
            }
            CloudRequest::File { account, request } => {
                let id = self.next_request;
                self.next_request = self.next_request.wrapping_add(1);
                match self.send_to(&account, &ToMount::Request { id, request }) {
                    Ok(()) => {
                        self.pending.insert(id, (account, reply));
                        return;
                    }
                    Err(error) => CloudResponse::Error(error),
                }
            }
        };
        let _ = reply.send(response);
    }

    /// Serve the control protocol on `path` from now on, answering what
    /// `kind` serves and refusing the rest. Requests are handled on the
    /// service's loop ([`Self::run`], [`Self::pump`]), like an account
    /// change.
    pub fn serve_control(&self, path: &Path, kind: CloudSocket) -> io::Result<()> {
        bind_control(path, kind, self.signals.clone())
    }

    /// Answer one control request the way the socket does, handling mount
    /// events until it is answered or `timeout` passes (tools and tests).
    pub fn request(&mut self, request: CloudRequest, timeout: Duration) -> CloudResponse {
        let (answer, answered) = mpsc::channel();
        self.control(request, answer);
        let deadline = Instant::now().checked_add(timeout);
        loop {
            match answered.try_recv() {
                Ok(response) => return response,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return CloudResponse::Error("the request was dropped".into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            let left = deadline.map_or(timeout, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
            if left.is_zero() {
                return CloudResponse::Error("the mount did not answer in time".into());
            }
            if let Ok(signal) = self.inbox.recv_timeout(left.min(Duration::from_millis(20))) {
                self.handle(signal);
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
    pub fn wait_for_health(
        &mut self,
        account_id: &str,
        health: MountHealth,
        timeout: Duration,
    ) -> bool {
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
            Signal::Control(request, reply) => self.control(request, reply),
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
        let accounts_dir = match &self.config.config_root {
            Some(root) => root.join("accounts"),
            None => accounts::config_root()
                .ok_or_else(|| io::Error::other("no user config directory"))?
                .join("accounts"),
        };
        let _watcher = watch_accounts(&accounts_dir, self.signals.clone())?;
        // A grant of the account socket's directory must never reach file
        // requests: a file socket inside it is not served.
        let file_socket = self.config.file_socket.clone().filter(|files| {
            let shared = self
                .config
                .control_socket
                .as_deref()
                .and_then(Path::parent)
                .is_some_and(|granted| files.starts_with(granted));
            if shared {
                tracing::warn!("the cloud file socket shares the account socket's directory");
            }
            !shared
        });
        let sockets = [
            (self.config.control_socket.clone(), CloudSocket::Accounts),
            (file_socket, CloudSocket::Files),
        ];
        for (socket, kind) in sockets {
            if let Some(socket) = socket
                && let Err(error) = self.serve_control(&socket, kind)
            {
                // The mounts work without their control; keep serving them.
                tracing::warn!(%error, ?kind, "cannot serve cloud mount control");
            }
        }
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

/// Serve the control protocol on `path`: each request is handled on the
/// service's own loop, like an account change. The socket is the user's
/// alone (0600 in a 0700 directory), and answers only what `kind` serves.
fn bind_control(path: &Path, kind: CloudSocket, signals: Sender<Signal>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        prepare_dir(parent)?;
    }
    let listener = tilo_files::native::private::bind_socket(path)?;
    let open = Arc::new(AtomicUsize::new(0));
    std::thread::Builder::new()
        .name("cloud-control".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONTROL_CONNECTIONS {
                    open.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                let signals = signals.clone();
                let done = Arc::clone(&open);
                let spawned = std::thread::Builder::new()
                    .name("cloud-control-connection".into())
                    .spawn(move || {
                        serve_connection(stream, kind, &signals);
                        done.fetch_sub(1, Ordering::SeqCst);
                    });
                if spawned.is_err() {
                    open.fetch_sub(1, Ordering::SeqCst);
                }
            }
        })?;
    Ok(())
}

/// Serve one control connection until it closes, idles past
/// [`CONTROL_IDLE`] or sends a bad frame. A request `kind` does not serve
/// is refused here and never reaches the service's loop or a mount.
fn serve_connection(mut stream: UnixStream, kind: CloudSocket, signals: &Sender<Signal>) {
    if stream.set_read_timeout(Some(CONTROL_IDLE)).is_err() {
        return;
    }
    while let Ok(Some(request)) = CLOUD_FRAMING.read_value::<_, CloudRequest>(&mut stream) {
        if !kind.serves(&request) {
            let refused = CloudResponse::Error("this socket does not serve that request".into());
            if CLOUD_FRAMING.write_value(&mut stream, &refused).is_err() {
                break;
            }
            continue;
        }
        let (answer, answered) = mpsc::channel();
        if signals.send(Signal::Control(request, answer)).is_err() {
            break;
        }
        let response = answered
            .recv_timeout(CONTROL_ANSWER)
            .unwrap_or_else(|_| CloudResponse::Error("the cloud service did not answer".into()));
        if CLOUD_FRAMING.write_value(&mut stream, &response).is_err() {
            break;
        }
    }
}

/// Carry out a control message from the service on `volume`: the one path
/// for mount processes and in-process mounts. A file request runs on its
/// own thread (a provider round trip, never on a FUSE thread or the pipe)
/// and `reply` gets the answer.
pub fn dispatch(
    volume: &Arc<CloudVolume>,
    message: ToMount,
    reply: &Arc<dyn Fn(MountReply) + Send + Sync>,
) {
    match message {
        ToMount::SyncNow => volume.sync_now(),
        ToMount::Pause => volume.pause(),
        ToMount::Resume => volume.resume(),
        ToMount::Request { id, request } => {
            if !volume.begin_request() {
                reply(MountReply {
                    id,
                    result: Err("too many requests are running; try again".into()),
                });
                return;
            }
            let worker = Arc::clone(volume);
            let answer = Arc::clone(reply);
            let spawned = std::thread::Builder::new()
                .name("cloud-request".into())
                .spawn(move || {
                    let result = worker.answer(&request);
                    worker.finish_request();
                    answer(MountReply { id, result });
                });
            if spawned.is_err() {
                volume.finish_request();
                reply(MountReply {
                    id,
                    result: Err("the mount cannot start the request".into()),
                });
            }
        }
        ToMount::Start(_) | ToMount::Credential(_) => {}
    }
}

/// Subscribe to mutations of the account store on the normal service channel.
fn watch_accounts(
    directory: &Path,
    signals: Sender<Signal>,
) -> io::Result<notify::RecommendedWatcher> {
    use notify::Watcher as _;
    fs::create_dir_all(directory)?;
    let store = directory.join("store.json");
    let mut watcher = notify::recommended_watcher(move |event| {
        if account_store_changed(event, &store) {
            let _ = signals.send(Signal::Accounts);
        }
    })
    .map_err(io::Error::other)?;
    watcher
        .watch(directory, notify::RecursiveMode::NonRecursive)
        .map_err(io::Error::other)?;
    Ok(watcher)
}

fn account_store_changed(event: notify::Result<notify::Event>, store: &Path) -> bool {
    let Ok(event) = event else {
        // Queue failure can hide a mutation, so reload conservatively.
        return true;
    };
    if event.need_rescan() {
        return true;
    }
    if matches!(event.kind, notify::EventKind::Access(_)) {
        // Loading this file is not a configuration change. Inotify reports
        // the service's own open/read/close and would otherwise feed itself.
        return false;
    }
    if matches!(
        event.kind,
        notify::EventKind::Any | notify::EventKind::Other
    ) {
        return true;
    }
    event
        .paths
        .iter()
        .any(|path| path == store || Some(path.as_path()) == store.parent())
}

#[cfg(test)]
mod watcher_tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn account_store_reads_do_not_retrigger_the_service_watcher() {
        let root = tempfile::tempdir().unwrap();
        AccountStore::new().save_in(root.path()).unwrap();
        let directory = root.path().join("accounts");
        let (signals, inbox) = mpsc::channel();
        let _watcher = watch_accounts(&directory, signals).unwrap();
        let store = AccountStore::load_in(root.path());
        assert!(store.all().is_empty());
        assert!(
            matches!(
                inbox.recv_timeout(Duration::from_millis(300)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "reloading accounts must not queue another reload"
        );
    }

    #[test]
    fn watcher_tracks_atomic_store_changes_but_not_its_status_reports() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("accounts");
        let (signals, inbox) = mpsc::channel();
        let _watcher = watch_accounts(&directory, signals).unwrap();
        AccountSyncReporter::in_root("fixture", root.path())
            .offline()
            .unwrap();
        assert!(
            matches!(
                inbox.recv_timeout(Duration::from_millis(300)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "mount health reports must not trigger account reconciliation"
        );
        AccountStore::new().save_in(root.path()).unwrap();
        assert!(matches!(
            inbox.recv_timeout(tilo_test_env::hang_bound()),
            Ok(Signal::Accounts)
        ));
        while inbox.recv_timeout(Duration::from_millis(100)).is_ok() {}
        // The writer replaces the inode. Subsequent replacement and deletion
        // must still be observed through the containing-directory watch.
        AccountStore::new().save_in(root.path()).unwrap();
        assert!(matches!(
            inbox.recv_timeout(tilo_test_env::hang_bound()),
            Ok(Signal::Accounts)
        ));
        while inbox.recv_timeout(Duration::from_millis(100)).is_ok() {}
        fs::remove_file(accounts::store_path_in(root.path())).unwrap();
        assert!(matches!(
            inbox.recv_timeout(tilo_test_env::hang_bound()),
            Ok(Signal::Accounts)
        ));
    }

    #[test]
    fn watcher_errors_and_rescan_still_invalidate_accounts() {
        use notify::{Event, EventKind, event::Flag};
        let store = Path::new("fixture/accounts/store.json");
        assert!(account_store_changed(
            Err(notify::Error::generic("fixture")),
            store
        ));
        let rescan = Event::new(EventKind::Other).set_flag(Flag::Rescan);
        assert!(account_store_changed(Ok(rescan), store));
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
    /// What the child said its provider offers, before it was ready.
    capabilities: Arc<Mutex<CloudCapabilities>>,
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
        let capabilities = Arc::new(Mutex::new(CloudCapabilities::default()));
        let reported = Arc::clone(&capabilities);
        std::thread::spawn(move || {
            let mut ready = Some(ready_tx);
            for line in BufReader::new(stdout).lines() {
                // A reply may carry a share link: user data, never logged.
                let Ok(line) = line else { break };
                if let Some(json) = line.strip_prefix(REPLY_PREFIX) {
                    if let Ok(reply) = serde_json::from_str::<MountReply>(json) {
                        events(MountEvent::Reply(reply));
                    }
                } else if let Some(json) = line.strip_prefix(CAPABILITIES_PREFIX) {
                    if let Ok(offered) = serde_json::from_str(json) {
                        *reported.lock_recover() = offered;
                    }
                } else if line == READY_LINE {
                    if let Some(tx) = ready.take() {
                        let _ = tx.send(());
                    }
                } else if line == REJECTED_LINE {
                    events(MountEvent::CredentialRejected);
                } else if line == EXPIRED_LINE {
                    events(MountEvent::CredentialExpired);
                } else if let Some(state) = line.strip_prefix(HEALTH_PREFIX)
                    && let Some(health) = MountHealth::parse(state)
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
            capabilities,
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
        self.send(&ToMount::Credential(WireCredential::from(credential)))
    }

    fn send(&mut self, message: &ToMount) -> io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::other("mount stopped"))?;
        stdin.write_all(&line(message))?;
        stdin.flush()
    }

    fn capabilities(&self) -> CloudCapabilities {
        *self.capabilities.lock_recover()
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
        self.volumes.lock_recover().get(account_id).cloned()
    }
}

struct InProcessMount {
    account_id: String,
    volume: Arc<CloudVolume>,
    credentials: Arc<PipeCredential>,
    volumes: Arc<Mutex<HashMap<String, Arc<CloudVolume>>>>,
    /// Hands file-request answers to the service as events.
    reply: Arc<dyn Fn(MountReply) + Send + Sync>,
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
        let replies = Arc::clone(&events);
        let reply: Arc<dyn Fn(MountReply) + Send + Sync> =
            Arc::new(move |answer| replies(MountEvent::Reply(answer)));
        let volume = CloudVolume::open(
            source,
            VolumeConfig {
                dir: state_dir.to_path_buf(),
                cache_budget: spec.cache_budget,
                poll_interval: Duration::from_secs(30),
                idle_poll_max: DEFAULT_IDLE_POLL_MAX,
            },
            Box::new(move |h| health(MountEvent::Health(h))),
        )?;
        if spec.paused {
            volume.pause();
        }
        self.volumes
            .lock_recover()
            .insert(spec.account_id.clone(), Arc::clone(&volume));
        Ok(Box::new(InProcessMount {
            account_id: spec.account_id.clone(),
            volume,
            credentials,
            volumes: Arc::clone(&self.volumes),
            reply,
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

    fn send(&mut self, message: &ToMount) -> io::Result<()> {
        // The same message a mount process parses from its stdin.
        let message: ToMount = serde_json::from_slice(&line(message))?;
        match message {
            ToMount::Credential(wire) => self.credentials.update(wire.into()),
            control => dispatch(&self.volume, control, &self.reply),
        }
        Ok(())
    }

    fn capabilities(&self) -> CloudCapabilities {
        self.volume.control_capabilities()
    }

    fn stop(self: Box<Self>) {
        self.volume.shutdown();
        self.volumes.lock_recover().remove(&self.account_id);
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
            idle_poll_max: DEFAULT_IDLE_POLL_MAX,
        },
        Box::new(|health| {
            let mut out = io::stdout();
            let _ = writeln!(out, "{HEALTH_PREFIX}{}", health.as_str());
            let _ = out.flush();
        }),
    )?;
    if spec.paused {
        volume.pause();
    }
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
        writeln!(out, "{}", capabilities_line(volume.control_capabilities()))?;
        writeln!(out, "{READY_LINE}")?;
        out.flush()?;
    }
    let reply: Arc<dyn Fn(MountReply) + Send + Sync> = Arc::new(|answer| {
        let mut out = io::stdout();
        let _ = writeln!(out, "{}", reply_line(&answer));
        let _ = out.flush();
    });
    // Credentials and control keep arriving on stdin; its end is the stop
    // signal.
    credentials.pump(input, |message| dispatch(&volume, message, &reply), || {});
    volume.shutdown();
    session.umount_and_join()?;
    for worker in workers {
        let _ = worker.join();
    }
    Ok(())
}
