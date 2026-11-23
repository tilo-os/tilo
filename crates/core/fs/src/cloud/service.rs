//! The per-user cloud account service and its mount processes.
//!
//! `fs cloud-service` (user unit `tilo-fs-cloud.service`) follows the file accounts of the
//! workspaces this device holds, as the session tells them ([`super::account_source`]), and
//! keeps exactly one mount per enabled file account at
//! `~/Remote/<account label>`. An account that appears or is enabled gets a mount
//! process with its one short-lived credential on stdin; one disabled, reconfigured or
//! removed has its mount stopped, and removal also drops its cache and queued state.
//! The service reports each mount's health to the session, which keeps it in the
//! workspace's accounts that every member's Settings reads.
//!
//! It serves its mounts' control (`runtime_fs::cloud`) on two sockets, each mode 0600
//! in its own 0700 directory: the account socket lists mounts with their health and
//! capabilities, syncs, pauses and resumes; the file socket also asks a mount for a
//! file's revisions, to restore a revision or for a share link. Each request travels
//! down the mount's stdin and its answer comes back on stdout, so no credential
//! crosses either socket. Neither the service nor its mounts open a vault: a password
//! is released by the session from the workspace's vault entry the account names, and
//! an access token comes from the handoff store, which runs Settings' credential helper
//! when it is missing, expired or rejected.

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

use accounts::{Account, AccountId, SyncHealth};
use cloud_source::{Credential, CredentialSource};
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MountEvent {
    Health(MountHealth),
    CredentialRejected,
    CredentialExpired,
    Reply(MountReply),
    Exited,
}

pub trait RunningMount: Send {
    fn send_credential(&mut self, credential: &Credential) -> io::Result<()>;
    fn send(&mut self, message: &ToMount) -> io::Result<()>;
    fn capabilities(&self) -> CloudCapabilities;
    fn stop(self: Box<Self>) -> io::Result<()>;
}

/// Starts mounts: [`ProcessLauncher`] runs `fs mount-cloud` children with a FUSE mount
/// each, [`InProcessLauncher`] runs volumes in this process without the kernel mount.
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
    pub mounts_root: PathBuf,
    pub state_root: PathBuf,
    /// The file accounts and what each signs in with: [`super::SessionAccounts`] in
    /// production.
    pub accounts: Arc<dyn super::AccountSource>,
    /// Where to serve the account socket ([`CloudSocket::Accounts`]: list, sync now,
    /// pause, resume); `None` serves none, and tools drive [`CloudService::request`].
    pub control_socket: Option<PathBuf>,
    /// Where to serve the file socket ([`CloudSocket::Files`]: everything, file
    /// requests included); `None` serves none. It must not share a directory with
    /// `control_socket`, which a sandbox may be granted.
    pub file_socket: Option<PathBuf>,
}

/// How often accounts that could not start (offline, no credential yet) are tried
/// again while nothing else happens.
const RETRY_INTERVAL: Duration = Duration::from_secs(60);
/// How long a control connection waits for the service's answer; shorter than the
/// client's own timeout, so the client hears why.
const CONTROL_ANSWER: Duration = Duration::from_secs(80);
const MAX_CONTROL_CONNECTIONS: usize = 8;
/// A control connection that sends nothing for this long is closed, so idle clients
/// cannot hold every slot.
const CONTROL_IDLE: Duration = Duration::from_secs(30);
/// The accounts paused through the control socket, below the state root.
const PAUSED_FILE: &str = "paused.json";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Need {
    Current,
    Replacement,
}

struct Mounted {
    spec: MountSpec,
    dir_name: String,
    running: Box<dyn RunningMount>,
    health: Option<MountHealth>,
}

enum Signal {
    Accounts,
    Event(String, MountEvent),
    Control(CloudRequest, Sender<CloudResponse>),
}

pub struct CloudService {
    config: ServiceConfig,
    launcher: Box<dyn Launcher>,
    mounts: HashMap<String, Mounted>,
    credentials: HashMap<String, Arc<dyn CredentialSource>>,
    retry: HashSet<String>,
    paused: BTreeSet<String>,
    /// File requests sent to a mount, by request id: the account and where the answer
    /// goes.
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
        let saved = serde_json::to_vec(&self.paused)
            .map_err(io::Error::other)
            .and_then(|bytes| tilo_files::native::atomic::write_creating_parent(&path, &bytes));
        if let Err(error) = saved {
            tracing::warn!(%error, "cannot record the paused cloud accounts");
        }
    }

    fn accounts(&self) -> Vec<Account> {
        self.config.accounts.accounts()
    }

    fn account(&self, id: &str) -> Option<Account> {
        self.accounts()
            .into_iter()
            .find(|account| account.id.as_str() == id)
    }

    /// Say how `id`'s sync stands ([`super::AccountSource::report`]).
    fn report(&self, id: &str, health: SyncHealth) {
        self.config.accounts.report(&AccountId::from(id), health);
    }

    /// The account's credential, from where it signs in with
    /// ([`super::AccountSource::credential`]); a failure is reported as the account's sync.
    fn credential(&mut self, account_id: &str, need: Need) -> Option<Credential> {
        let source = if let Some(source) = self.credentials.get(account_id) {
            Arc::clone(source)
        } else {
            let account = self.account(account_id)?;
            let source = self.config.accounts.credential(&account);
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
                self.report(account_id, SyncHealth::Offline);
                self.retry.insert(account_id.to_owned());
                None
            }
            Err(error) => {
                tracing::info!(%error, "no credential for a cloud account");
                self.report(account_id, SyncHealth::NeedsSignIn);
                None
            }
        }
    }

    pub fn mounted(&self) -> Vec<(String, PathBuf)> {
        let mut out: Vec<(String, PathBuf)> = self
            .mounts
            .iter()
            .map(|(id, m)| (id.clone(), self.config.mounts_root.join(&m.dir_name)))
            .collect();
        out.sort();
        out
    }

    pub fn reconcile(&mut self) {
        self.retry.clear();
        let store = self.accounts();
        self.credentials.retain(|id, _| {
            store
                .iter()
                .find(|account| account.id.as_str() == id)
                .is_some_and(|a| providers::source_spec(a).is_some())
        });
        let wanted: Vec<MountSpec> = store.iter().filter_map(providers::spec_for).collect();
        tracing::info!(
            accounts = store.len(),
            mounts = ?wanted.iter().map(|spec| spec.account_id.as_str()).collect::<Vec<_>>(),
            "the cloud service follows the file accounts"
        );
        let labels: Vec<(&str, &str)> = store
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
            let removed = !store.iter().any(|account| account.id.as_str() == id);
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
                self.report(&spec.account_id, SyncHealth::Error);
                continue;
            }
            let signals = self.signals.clone();
            let id = spec.account_id.clone();
            let events: Box<dyn Fn(MountEvent) + Send + Sync> = Box::new(move |event| {
                let _ = signals.send(Signal::Event(id.clone(), event));
            });
            self.report(&spec.account_id, SyncHealth::Syncing);
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
                    tracing::info!(account = %spec.account_id, "a cloud account's mount started");
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
                    self.report(&spec.account_id, SyncHealth::Error);
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
        if let Err(error) = mounted.running.stop() {
            tracing::warn!(%error, "cloud mount did not stop cleanly");
        }
        let _ = fs::remove_dir(self.config.mounts_root.join(&mounted.dir_name));
        self.credentials.remove(id);
        self.drop_pending(id);
        if removed {
            let _ = fs::remove_dir_all(self.config.state_root.join(state_dir_name(id)));
            if self.paused.remove(id) {
                self.save_paused();
            }
        }
    }

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

    fn on_event(&mut self, id: &str, event: MountEvent) {
        match event {
            MountEvent::Health(health) => {
                if let Some(mounted) = self.mounts.get_mut(id) {
                    mounted.health = Some(health);
                }
                let health = match health {
                    MountHealth::Syncing => SyncHealth::Syncing,
                    MountHealth::Healthy => SyncHealth::Healthy,
                    MountHealth::Offline => SyncHealth::Offline,
                    MountHealth::NeedsAuthentication => SyncHealth::NeedsSignIn,
                    MountHealth::Error => SyncHealth::Error,
                };
                self.report(id, health);
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
                signed_in: self
                    .config
                    .accounts
                    .signed_in(&AccountId::from(id.as_str())),
            })
            .collect();
        mounts.sort_by(|a, b| a.account.cmp(&b.account));
        mounts
    }

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

    /// Handle one control request; `reply` gets the answer, at once or when the mount
    /// answers a file request.
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
                } else if self.account(&account).is_some() {
                    // A mount that could not start (offline, no credential yet) is tried
                    // again next on this loop rather than at the next retry; starting it
                    // can take a while, so the answer does not wait for it.
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

    /// Serve the control protocol on `path` from now on, answering what `kind` serves
    /// and refusing the rest. Requests are handled on the service's loop, like an
    /// account change.
    pub fn serve_control(&self, path: &Path, kind: CloudSocket) -> io::Result<()> {
        bind_control(path, kind, self.signals.clone())
    }

    /// Answer one control request the way the socket does, handling mount events until
    /// it is answered or `timeout` passes; for tools and tests.
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

    pub fn pump(&mut self) {
        while let Ok(signal) = self.inbox.try_recv() {
            self.handle(signal);
        }
    }

    /// Handle events until `account_id` reports `health`, for at most `timeout`.
    /// Returns whether it did.
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

    /// Serve until the process is stopped: reconcile now and whenever the accounts change.
    pub fn run(mut self) -> io::Result<()> {
        let signals = Mutex::new(self.signals.clone());
        let _watch = self.config.accounts.watch(Box::new(move || {
            let _ = signals.lock_recover().send(Signal::Accounts);
        }))?;
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
                tracing::warn!(%error, ?kind, "cannot serve cloud mount control");
            }
        }
        self.reconcile();
        loop {
            let timeout = (!self.retry.is_empty()).then_some(RETRY_INTERVAL);
            let signal = if let Some(timeout) = timeout {
                match self.inbox.recv_timeout(timeout) {
                    Ok(signal) => Some(signal),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        self.reconcile();
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => None,
                }
            } else {
                self.inbox.recv().ok()
            };
            let Some(signal) = signal else { break };
            self.handle(signal);
            self.pump();
        }
        Ok(())
    }

    pub fn shutdown(&mut self) {
        let ids: Vec<String> = self.mounts.keys().cloned().collect();
        for id in ids {
            self.unmount(&id, false);
        }
    }
}

/// Serve the control protocol on `path`: each request is handled on the service's own
/// loop, like an account change. The socket is the user's alone (0600 in a 0700
/// directory) and answers only what `kind` serves.
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

/// Serve one control connection until it closes, idles past [`CONTROL_IDLE`] or sends a
/// bad frame. A request `kind` does not serve is refused here and never reaches the
/// service's loop or a mount.
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

/// Carry out a control message from the service on `volume`, the one path for mount
/// processes and in-process mounts. A file request runs on its own thread (a provider
/// round trip, never on a FUSE thread or the pipe) and `reply` gets the answer.
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

fn prepare_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub struct ProcessLauncher {
    pub executable: PathBuf,
    pub ready_timeout: Duration,
}

struct ChildMount {
    child: Child,
    stdin: Option<ChildStdin>,
    mount_point: PathBuf,
    exited: Receiver<()>,
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
            let _ = mount.halt();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the cloud mount did not become ready",
            ));
        }
        Ok(Box::new(mount))
    }
}

impl ChildMount {
    fn halt(&mut self) -> io::Result<()> {
        // Closing stdin asks the mount to unmount and exit.
        self.stdin.take();
        if self.exited.recv_timeout(Duration::from_secs(10)).is_ok() {
            let status = self.child.wait()?;
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other("cloud mount exited unsuccessfully"))
            };
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = Command::new("fusermount3")
            .arg("-u")
            .arg(&self.mount_point)
            .status();
        Err(io::Error::other(
            "the cloud mount did not stop cleanly before its deadline",
        ))
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

    fn stop(mut self: Box<Self>) -> io::Result<()> {
        self.halt()
    }
}

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

    fn stop(self: Box<Self>) -> io::Result<()> {
        self.volume.shutdown();
        self.volumes.lock_recover().remove(&self.account_id);
        Ok(())
    }
}

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
    crate::fuse_session::unmount_and_join(session, mount_point)?;
    for worker in workers {
        let _ = worker.join();
    }
    Ok(())
}
