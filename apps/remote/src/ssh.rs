//! Real SSH transport, built on `russh`.
//!
//! A live SSH session runs the async russh event loop as a task on the shared
//! app runtime; the UI thread talks to it over channels. Remote PTY output is
//! streamed to a [`crate::vt::VtGrid`] that the session surface renders;
//! keystrokes flow the other way into the PTY.
//!
//! Nothing here is simulated: [`SshSession::connect`] opens a real TCP
//! connection, runs the SSH key exchange, authenticates with a real
//! password or private key, and requests a real interactive shell. On any
//! failure the shared [`SessionStatus`] carries the honest error — the UI
//! never falls back to a fabricated terminal.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail};
use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, load_secret_key};
use russh::{ChannelMsg, Disconnect};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::{Receiver, Sender, channel, error::TrySendError};

use crate::session::SessionStatus;
use crate::vt::VtGrid;
use ui::lock::MutexRecover as _;

/// Keep the UI responsive under a stalled SSH transport without allowing a
/// key-repeat storm or a hostile peer to accumulate unbounded process memory.
/// These queues intentionally apply backpressure at the transport boundary.
const SSH_INPUT_QUEUE_CAPACITY: usize = 256;
const SSH_OUTPUT_QUEUE_CAPACITY: usize = 128;
/// Agent-supplied `send-text` is useful for commands, not bulk file transfer.
/// Bounding it keeps a single invocation from monopolising the PTY worker.
pub const MAX_SSH_INPUT_BYTES: usize = 64 * 1024;

/// How to authenticate an SSH connection. Secrets live only here, in
/// volatile memory — never in the connection document.
#[derive(Clone, Debug)]
pub enum SshAuth {
    Password(String),
    Key {
        path: PathBuf,
        passphrase: Option<String>,
    },
}

/// Everything needed to open an SSH session.
#[derive(Clone, Debug)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    pub cols: u16,
    pub rows: u16,
    /// Device-private trust database; never part of a shared Remote profile.
    pub known_hosts: KnownHostStore,
}

/// The public identity that a person compares out-of-band before trusting an
/// SSH server. It has no credentials and is safe to show in an error surface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostKeyIdentity {
    pub algorithm: String,
    pub fingerprint: String,
}

/// Why a host key was rejected. The connection is always stopped before
/// authentication when one of these is present.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostKeyIssue {
    FirstContact {
        observed: HostKeyIdentity,
    },
    Changed {
        expected: HostKeyIdentity,
        observed: HostKeyIdentity,
    },
    StoreFailure(String),
}

impl HostKeyIssue {
    pub fn message(&self, host: &str, port: u16) -> String {
        match self {
            Self::FirstContact { observed } => format!(
                "SSH host key for {host}:{port} needs explicit trust: {} {}",
                observed.algorithm, observed.fingerprint
            ),
            Self::Changed { expected, observed } => format!(
                "SSH host key changed for {host}:{port}. Expected {} {}; received {} {}. Connection blocked.",
                expected.algorithm, expected.fingerprint, observed.algorithm, observed.fingerprint
            ),
            Self::StoreFailure(error) => format!("SSH host-key store is unavailable: {error}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct KnownHostEntry {
    host: String,
    port: u16,
    identity: HostKeyIdentity,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct KnownHosts {
    entries: Vec<KnownHostEntry>,
}

/// The setting the trust database is kept in: this device's own, never
/// synced to the person's other devices and never part of a shared Remote
/// profile. Held as raw JSON so a corrupt database is refused rather than
/// read as empty.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KnownHostsSetting {
    #[serde(default)]
    entries: serde_json::Value,
}

impl ui::storage::SettingsSchema for KnownHostsSetting {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::DeviceLocal;
}

/// This device's SSH host-key trust: a device-local setting of the app.
#[derive(Clone, Debug)]
pub struct KnownHostStore {
    settings: ui::storage::Settings<KnownHostsSetting>,
}

impl KnownHostStore {
    /// The trust database in the app's settings in the document its host
    /// gave it.
    #[must_use]
    pub fn in_settings(settings: ui::storage::Settings<KnownHostsSetting>) -> Self {
        Self { settings }
    }

    /// The trust database in the session's settings.
    #[must_use]
    pub fn of_session() -> Self {
        Self::in_settings(ui::storage::Settings::for_app(
            crate::resource::REMOTE_APP_ID,
        ))
    }

    fn load(&self) -> Result<KnownHosts, String> {
        match self.settings.get().entries {
            serde_json::Value::Null => Ok(KnownHosts::default()),
            entries @ serde_json::Value::Array(_) => serde_json::from_value(entries)
                .map(|entries| KnownHosts { entries })
                .map_err(|error| format!("the SSH host-key store is unreadable: {error}")),
            _ => Err("the SSH host-key store is unreadable".into()),
        }
    }

    fn write(&self, known_hosts: &KnownHosts) -> Result<(), String> {
        let entries =
            serde_json::to_value(&known_hosts.entries).map_err(|error| error.to_string())?;
        self.settings
            .set(&KnownHostsSetting { entries })
            .map_err(|error| error.to_string())
    }
}

/// Explicitly trust the exact fingerprint shown by a failed first-contact
/// check. A changed key is never overwritten: callers must forget it first.
pub fn trust_host_key(
    store: &KnownHostStore,
    host: &str,
    port: u16,
    observed: &HostKeyIdentity,
) -> Result<(), String> {
    let mut known_hosts = store.load()?;
    if let Some(entry) = known_hosts
        .entries
        .iter()
        .find(|entry| entry.host == host && entry.port == port)
    {
        return if entry.identity == *observed {
            Ok(())
        } else {
            Err("a different trusted host key already exists; forget it before retrusting".into())
        };
    }
    known_hosts.entries.push(KnownHostEntry {
        host: host.to_string(),
        port,
        identity: observed.clone(),
    });
    store.write(&known_hosts)
}

/// Explicitly forget the local trust entry. This only changes device-private
/// trust metadata; no shared connection profile or remote host is touched.
///
/// # Errors
///
/// Returns an error if the local trust store cannot be safely read or written.
pub fn forget_host_key(store: &KnownHostStore, host: &str, port: u16) -> Result<bool, String> {
    let mut known_hosts = store.load()?;
    let before = known_hosts.entries.len();
    known_hosts
        .entries
        .retain(|entry| entry.host != host || entry.port != port);
    if known_hosts.entries.len() == before {
        return Ok(false);
    }
    store.write(&known_hosts)?;
    Ok(true)
}

fn verify_known_host(
    known_hosts: &KnownHosts,
    host: &str,
    port: u16,
    observed: HostKeyIdentity,
) -> Result<(), HostKeyIssue> {
    match known_hosts
        .entries
        .iter()
        .find(|entry| entry.host == host && entry.port == port)
    {
        Some(entry) if entry.identity == observed => Ok(()),
        Some(entry) => Err(HostKeyIssue::Changed {
            expected: entry.identity.clone(),
            observed,
        }),
        None => Err(HostKeyIssue::FirstContact { observed }),
    }
}

/// Resolve how to authenticate. Password auth needs the connection's saved
/// password; key auth uses the first of the usual `~/.ssh` identities that
/// exists, unlocked with the connection's saved passphrase if it has one.
/// Credentials come from this device's secret store, never the document.
pub fn resolve_auth(
    prefer_key: bool,
    secret: Option<String>,
    default_keys: &[PathBuf],
) -> anyhow::Result<SshAuth> {
    if prefer_key {
        let path = default_keys
            .iter()
            .find(|p| p.exists())
            .cloned()
            .ok_or_else(|| anyhow!("no SSH key: add ~/.ssh/id_ed25519 or ~/.ssh/id_ecdsa"))?;
        Ok(SshAuth::Key {
            path,
            passphrase: secret,
        })
    } else {
        let pw = secret.ok_or_else(|| {
            anyhow!("no SSH password saved on this device: edit the connection to enter it")
        })?;
        Ok(SshAuth::Password(pw))
    }
}

/// Standard on-disk identity paths, most-preferred first.
pub fn default_key_paths() -> Vec<PathBuf> {
    let home = tilo_paths::home().unwrap_or_default();
    ["id_ed25519", "id_ecdsa"]
        .iter()
        .map(|n| home.join(".ssh").join(n))
        .collect()
}

/// Which way a file moves over an SSH session.
#[derive(Clone, Debug)]
pub enum TransferDirection {
    /// From this device to the remote machine.
    Put {
        local_path: PathBuf,
        remote_path: String,
    },
    /// From the remote machine to this device.
    Fetch {
        remote_path: String,
        local_path: PathBuf,
    },
}

/// One file transfer handed to the session worker.
struct TransferCommand {
    direction: TransferDirection,
    job: ui::jobs::Running,
    completion: std::sync::mpsc::Sender<Result<u64, String>>,
}

/// A live SSH session handle held by the UI.
pub struct SshSession {
    grid: VtGrid,
    status: Arc<Mutex<SessionStatus>>,
    output_rx: Receiver<Vec<u8>>,
    input_tx: Sender<InputMessage>,
    command_tx: Sender<TransferCommand>,
    host_key_issue: Arc<Mutex<Option<HostKeyIssue>>>,
    // The session task on the app runtime. Dropping the handle does not
    // cancel it: the input channel closes with this session, which tells the
    // task to disconnect cleanly.
    _worker: ui::rt::JoinHandle<()>,
}

impl SshSession {
    /// Start connecting. Returns immediately with a handle whose status is
    /// `Connecting`; a task on the app runtime advances it to `Connected` or
    /// `Failed(reason)`.
    ///
    /// # Errors
    ///
    /// Returns an error if the app runtime cannot be started.
    pub fn connect(cfg: SshConfig) -> anyhow::Result<SshSession> {
        let status = Arc::new(Mutex::new(SessionStatus::Connecting));
        let (output_tx, output_rx) = channel::<Vec<u8>>(SSH_OUTPUT_QUEUE_CAPACITY);
        let (input_tx, input_rx) = channel::<InputMessage>(SSH_INPUT_QUEUE_CAPACITY);
        let (command_tx, command_rx) = channel::<TransferCommand>(4);
        let grid = VtGrid::new(cfg.cols as usize, cfg.rows as usize);
        let host_key_issue = Arc::new(Mutex::new(None));

        let status_worker = status.clone();
        let host_key_issue_worker = host_key_issue.clone();
        let worker = ui::rt::try_app()?.spawn(async move {
            if let Err(e) = run_session(
                cfg,
                status_worker.clone(),
                host_key_issue_worker,
                output_tx,
                input_rx,
                command_rx,
            )
            .await
            {
                let mut s = status_worker.lock_recover();
                if !matches!(*s, SessionStatus::Failed(_)) {
                    *s = SessionStatus::Failed(e.to_string());
                }
            }
        });

        Ok(SshSession {
            grid,
            status,
            output_rx,
            input_tx,
            command_tx,
            host_key_issue,
            _worker: worker,
        })
    }

    pub fn status(&self) -> SessionStatus {
        self.status.lock_recover().clone()
    }

    /// The rejected host-key observation, if verification stopped the
    /// handshake. This public metadata is deliberately separate from secrets.
    pub fn host_key_issue(&self) -> Option<HostKeyIssue> {
        self.host_key_issue.lock_recover().clone()
    }

    /// Drain any pending remote output into the grid. Returns true if the
    /// grid changed (so the UI knows to repaint).
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        while let Ok(bytes) = self.output_rx.try_recv() {
            self.grid.feed(&bytes);
            changed = true;
        }
        changed
    }

    pub fn grid(&self) -> &VtGrid {
        &self.grid
    }

    /// Send keystrokes to the remote PTY.
    pub fn send_input(&self, bytes: Vec<u8>) -> Result<(), String> {
        self.queue_input(InputMessage { bytes, ack: None })
    }

    /// Queue bytes and receive an acknowledgement only after russh has
    /// accepted them on the live channel. Resource actions use this instead
    /// of mistaking insertion into the UI→worker queue for delivery.
    pub fn send_input_ack(
        &self,
        bytes: Vec<u8>,
    ) -> Result<std::sync::mpsc::Receiver<Result<(), String>>, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.queue_input(InputMessage {
            bytes,
            ack: Some(tx),
        })?;
        Ok(rx)
    }

    /// Move one file over this session. The work runs on the session's
    /// worker, which reports the bytes to `job` and answers through the
    /// returned channel when it ends.
    pub fn transfer(
        &self,
        direction: TransferDirection,
        job: ui::jobs::Running,
    ) -> Result<std::sync::mpsc::Receiver<Result<u64, String>>, String> {
        let (completion, done) = std::sync::mpsc::channel();
        self.command_tx
            .try_send(TransferCommand {
                direction,
                job,
                completion,
            })
            .map_err(|error: TrySendError<TransferCommand>| match error {
                TrySendError::Full(_) => {
                    "the session is already moving as many files as it allows".to_string()
                }
                TrySendError::Closed(_) => "the SSH session is closed".to_string(),
            })?;
        Ok(done)
    }

    fn queue_input(&self, message: InputMessage) -> Result<(), String> {
        if message.bytes.is_empty() {
            return Err("SSH input cannot be empty".into());
        }
        if message.bytes.len() > MAX_SSH_INPUT_BYTES {
            return Err(format!(
                "SSH input exceeds the {} per-operation limit",
                ui::format::memory(MAX_SSH_INPUT_BYTES as u64)
            ));
        }
        self.input_tx
            .try_send(message)
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    "SSH input is busy; wait for the remote session to catch up".into()
                }
                TrySendError::Closed(_) => "SSH session input channel is closed".into(),
            })
    }
}

struct InputMessage {
    bytes: Vec<u8>,
    ack: Option<std::sync::mpsc::Sender<Result<(), String>>>,
}

/// Fail-closed host-key verifier. It checks only a device-private database;
/// saved Remote profiles remain safe to collaborate on without carrying trust.
struct Handler {
    host: String,
    port: u16,
    known_hosts: KnownHostStore,
    issue: Arc<Mutex<Option<HostKeyIssue>>>,
}

fn host_key_identity(key: &russh::keys::PublicKeyOrCertificate) -> HostKeyIdentity {
    let public_key = key.public_key();
    HostKeyIdentity {
        algorithm: public_key.algorithm().as_str().to_string(),
        fingerprint: public_key.fingerprint(HashAlg::Sha256).to_string(),
    }
}

impl client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let observed = host_key_identity(server_public_key);
        let issue = match self.known_hosts.load() {
            Ok(known_hosts) => {
                match verify_known_host(&known_hosts, &self.host, self.port, observed) {
                    Ok(()) => return Ok(true),
                    Err(issue) => issue,
                }
            }
            Err(error) => HostKeyIssue::StoreFailure(error),
        };
        *self.issue.lock_recover() = Some(issue);
        Ok(false)
    }
}

/// The async body: connect, authenticate, open a PTY shell, and pump bytes
/// both ways until either side closes.
/// Load a private key on the CPU pool: decrypting a passphrase-protected key
/// runs a deliberately slow KDF, which must not stall the runtime thread.
async fn load_key(
    path: &Path,
    passphrase: Option<&str>,
) -> anyhow::Result<russh::keys::PrivateKey> {
    let (key_path, key_passphrase) = (path.to_path_buf(), passphrase.map(str::to_owned));
    ui::rt::cpu(move || load_secret_key(&key_path, key_passphrase.as_deref()))
        .await
        .map_err(|e| anyhow!("load key {}: {e}", path.display()))?
        .map_err(|e| anyhow!("load key {}: {e}", path.display()))
}

/// Authenticate the session the way `cfg` says: a password, or a key from
/// `path`, ordered by whatever hash the server supports.
async fn authenticate(
    session: &mut client::Handle<Handler>,
    cfg: &SshConfig,
) -> anyhow::Result<()> {
    let auth_ok = match &cfg.auth {
        SshAuth::Password(pw) => session
            .authenticate_password(cfg.user.clone(), pw.clone())
            .await?
            .success(),
        SshAuth::Key { path, passphrase } => {
            let key = load_key(path, passphrase.as_deref()).await?;
            let hash = session.best_supported_rsa_hash().await?.flatten();
            session
                .authenticate_publickey(
                    cfg.user.clone(),
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await?
                .success()
        }
    };
    if !auth_ok {
        bail!("Authentication failed");
    }
    Ok(())
}

async fn run_session(
    cfg: SshConfig,
    status: Arc<Mutex<SessionStatus>>,
    host_key_issue: Arc<Mutex<Option<HostKeyIssue>>>,
    output_tx: Sender<Vec<u8>>,
    mut input_rx: Receiver<InputMessage>,
    mut commands: Receiver<TransferCommand>,
) -> anyhow::Result<()> {
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(3600)),
        ..Default::default()
    });

    let host = cfg.host.clone();
    let port = cfg.port;
    let mut session = client::connect(
        config,
        (cfg.host.as_str(), cfg.port),
        Handler {
            host: cfg.host.clone(),
            port: cfg.port,
            known_hosts: cfg.known_hosts.clone(),
            issue: Arc::clone(&host_key_issue),
        },
    )
    .await
    .map_err(|e| {
        host_key_issue.lock_recover().as_ref().map_or_else(
            || anyhow!("connect: {e}"),
            |issue| anyhow!(issue.message(&host, port)),
        )
    })?;

    authenticate(&mut session, &cfg).await?;

    let channel = session.channel_open_session().await?;
    channel
        .request_pty(
            false,
            "xterm-256color",
            u32::from(cfg.cols),
            u32::from(cfg.rows),
            0,
            0,
            &[],
        )
        .await?;
    channel.request_shell(true).await?;

    *status.lock_recover() = SessionStatus::Connected;

    let mut channel = channel;
    let session = std::sync::Arc::new(session);
    loop {
        tokio::select! {
            command = commands.recv() => {
                if let Some(command) = command {
                    let session = std::sync::Arc::clone(&session);
                    tokio::spawn(async move { run_transfer(session, command).await });
                }
            }
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                    // Awaiting bounded output capacity deliberately slows the
                    // SSH read loop when the UI cannot consume output. This is
                    // real backpressure rather than silently dropping terminal
                    // bytes or growing an unbounded queue.
                    if output_tx.send(data.to_vec()).await.is_err() { break; }
                }
                Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break,
                _ => {}
            },
            input = input_rx.recv() => if let Some(input) = input {
                match channel.data(&input.bytes[..]).await {
                    Ok(()) => {
                        if let Some(ack) = input.ack {
                            let _ = ack.send(Ok(()));
                        }
                    }
                    Err(error) => {
                        if let Some(ack) = input.ack {
                            let _ = ack.send(Err(error.to_string()));
                        }
                        return Err(error.into());
                    }
                }
            } else {
                // UI dropped the sender → tear the session down cleanly.
                let _ = channel.eof().await;
                break;
            },
        }
    }

    let _ = session
        .disconnect(Disconnect::ByApplication, "", "en")
        .await;
    {
        let mut s = status.lock_recover();
        if *s == SessionStatus::Connected {
            *s = SessionStatus::Closed;
        }
    }
    Ok(())
}

/// Run one file transfer over a second SSH channel, then end its job.
async fn run_transfer(session: std::sync::Arc<client::Handle<Handler>>, command: TransferCommand) {
    let TransferCommand {
        direction,
        job,
        completion,
    } = command;
    let result = match &direction {
        TransferDirection::Put {
            local_path,
            remote_path,
        } => put_file(&session, local_path, remote_path, &job).await,
        TransferDirection::Fetch {
            remote_path,
            local_path,
        } => fetch_file(&session, remote_path, local_path, &job).await,
    };
    match &result {
        Ok(bytes) => job.done(format!(
            "Transferred {}: {}",
            transfer_name(&direction),
            ui::format::memory(*bytes)
        )),
        Err(reason) if job.is_stopped() => job.stopped(),
        Err(reason) => job.failed(reason.clone()),
    }
    let _ = completion.send(result);
}

/// The file a transfer names, for a job's ending sentence.
pub(crate) fn transfer_name(direction: &TransferDirection) -> String {
    match direction {
        TransferDirection::Put { local_path, .. } => local_path.file_name().map_or_else(
            || local_path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        ),
        TransferDirection::Fetch { remote_path, .. } => remote_path.clone(),
    }
}

/// A path as one POSIX shell word, so a remote path is never read as shell
/// syntax.
fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

/// Send a local file to `remote_path`, reporting the bytes to `job`.
async fn put_file(
    session: &client::Handle<Handler>,
    local_path: &Path,
    remote_path: &str,
    job: &ui::jobs::Running,
) -> Result<u64, String> {
    if remote_path.trim().is_empty() {
        return Err("the remote path is empty".into());
    }
    let mut file = tokio::fs::File::open(local_path)
        .await
        .map_err(|error| format!("could not read {}: {error}", local_path.display()))?;
    let total = file
        .metadata()
        .await
        .map_err(|error| format!("could not read {}: {error}", local_path.display()))?
        .len();
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|error| format!("the SSH session could not open a transfer channel: {error}"))?;
    channel
        .exec(
            true,
            format!("cat > {}", shell_quote(remote_path)).into_bytes(),
        )
        .await
        .map_err(|error| format!("the remote machine refused the transfer: {error}"))?;
    let mut buffer = vec![0_u8; 32 * 1024];
    let mut sent = 0_u64;
    job.progress(0, total);
    loop {
        if job.is_stopped() {
            let _ = channel.close().await;
            return Err("stopped by the person".into());
        }
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| format!("could not read {}: {error}", local_path.display()))?;
        if read == 0 {
            break;
        }
        let filled = buffer.get(..read).ok_or_else(|| {
            format!(
                "the read returned {read}, past the {}-byte buffer",
                buffer.len()
            )
        })?;
        channel
            .data(filled)
            .await
            .map_err(|error| format!("the remote machine closed the transfer: {error}"))?;
        sent = sent.saturating_add(u64::try_from(read).unwrap_or(0));
        job.progress(sent, total);
    }
    channel
        .eof()
        .await
        .map_err(|error| format!("the remote machine closed the transfer: {error}"))?;
    let (status, stderr) = finish_channel(&mut channel).await;
    if status != Some(0) {
        return Err(transfer_failure(remote_path, status, &stderr));
    }
    Ok(sent)
}

/// Receive `remote_path` into `local_path`, reporting the bytes seen.
async fn fetch_file(
    session: &client::Handle<Handler>,
    remote_path: &str,
    local_path: &Path,
    job: &ui::jobs::Running,
) -> Result<u64, String> {
    if remote_path.trim().is_empty() {
        return Err("the remote path is empty".into());
    }
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|error| format!("the SSH session could not open a transfer channel: {error}"))?;
    channel
        .exec(
            true,
            format!("cat {}", shell_quote(remote_path)).into_bytes(),
        )
        .await
        .map_err(|error| format!("the remote machine refused the transfer: {error}"))?;
    let mut file = tokio::fs::File::create(local_path)
        .await
        .map_err(|error| format!("could not write {}: {error}", local_path.display()))?;
    let mut received = 0_u64;
    let mut stderr = Vec::new();
    let mut status = None;
    job.seen(0);
    loop {
        if job.is_stopped() {
            let _ = channel.close().await;
            drop(file);
            let _ = tokio::fs::remove_file(local_path).await;
            return Err("stopped by the person".into());
        }
        match channel.wait().await {
            Some(ChannelMsg::Data { data }) => {
                if let Err(error) = file.write_all(&data).await {
                    let _ = tokio::fs::remove_file(local_path).await;
                    return Err(format!("could not write {}: {error}", local_path.display()));
                }
                received = received.saturating_add(u64::try_from(data.len()).unwrap_or(0));
                job.seen(received);
            }
            Some(ChannelMsg::ExtendedData { data, .. }) => stderr.extend_from_slice(&data),
            Some(ChannelMsg::ExitStatus { exit_status }) => status = Some(exit_status),
            Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break,
            _ => {}
        }
    }
    if let Err(error) = file.flush().await {
        let _ = tokio::fs::remove_file(local_path).await;
        return Err(format!("could not write {}: {error}", local_path.display()));
    }
    if status != Some(0) {
        let _ = tokio::fs::remove_file(local_path).await;
        return Err(transfer_failure(remote_path, status, &stderr));
    }
    Ok(received)
}

/// Read a command channel to its exit status, keeping its error text.
async fn finish_channel<S>(channel: &mut russh::Channel<S>) -> (Option<u32>, Vec<u8>)
where
    S: From<(russh::ChannelId, ChannelMsg)> + Send + Sync + 'static,
{
    let mut stderr = Vec::new();
    let mut status = None;
    loop {
        match channel.wait().await {
            Some(ChannelMsg::ExtendedData { data, .. }) => stderr.extend_from_slice(&data),
            Some(ChannelMsg::ExitStatus { exit_status }) => status = Some(exit_status),
            Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break,
            _ => {}
        }
    }
    (status, stderr)
}

/// Why a remote command did not finish a transfer, in the remote's own words
/// when it supplied any.
fn transfer_failure(remote_path: &str, status: Option<u32>, stderr: &[u8]) -> String {
    let detail = String::from_utf8_lossy(stderr).trim().to_owned();
    match (status, detail.is_empty()) {
        (Some(code), false) => {
            format!("the remote machine could not transfer {remote_path} (status {code}): {detail}")
        }
        (Some(code), true) => {
            format!("the remote machine could not transfer {remote_path} (status {code})")
        }
        (None, false) => {
            format!("the remote machine did not report the transfer's outcome: {detail}")
        }
        (None, true) => "the remote machine did not report the transfer's outcome".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use tilo_test_env::hang_bound;

    #[test]
    fn auth_resolution_reads_saved_secrets_and_default_keys() {
        eprintln!("phase resolve_auth_password_uses_the_saved_password");
        {
            match resolve_auth(false, Some("hunter2".into()), &[]).unwrap() {
                SshAuth::Password(p) => assert_eq!(p, "hunter2"),
                other @ SshAuth::Key { .. } => panic!("expected password auth, got {other:?}"),
            }
        }

        eprintln!("phase resolve_auth_password_missing_is_error");
        {
            assert!(resolve_auth(false, None, &[]).is_err());
        }

        eprintln!("phase resolve_auth_key_uses_the_first_existing_default");
        {
            let directory =
                std::env::temp_dir().join(format!("tilo-remote-keys-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&directory).unwrap();
            let present = directory.join("id_ecdsa");
            fs::write(&present, b"key").unwrap();
            let defaults = vec![directory.join("id_ed25519"), present.clone()];
            match resolve_auth(true, Some("pw".into()), &defaults).unwrap() {
                SshAuth::Key { path, passphrase } => {
                    assert_eq!(path, present);
                    assert_eq!(passphrase.as_deref(), Some("pw"));
                }
                other @ SshAuth::Password(_) => panic!("expected key auth, got {other:?}"),
            }
            assert!(
                resolve_auth(true, None, &[directory.join("missing")]).is_err(),
                "no identity file is an error"
            );
            fs::remove_dir_all(&directory).unwrap();
        }

        eprintln!("phase default_key_paths_are_under_dot_ssh");
        {
            let paths = default_key_paths();
            assert!(paths.iter().any(|p| p.ends_with(".ssh/id_ed25519")));
        }
    }

    /// A trust database in a scratch profile's settings document, and the
    /// directory that holds it.
    fn test_known_hosts() -> (KnownHostStore, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let store = KnownHostStore::in_settings(
            ui::storage::Settings::for_app(crate::resource::REMOTE_APP_ID)
                .in_profile(directory.path()),
        );
        (store, directory)
    }

    #[test]
    fn known_hosts_store_fails_closed_until_exactly_trusted() {
        eprintln!("phase device_private_known_hosts_fails_closed_until_an_exact_key_is_trusted");
        {
            let (store, directory) = test_known_hosts();
            let first = HostKeyIdentity {
                algorithm: "ssh-ed25519".into(),
                fingerprint: "SHA256:first".into(),
            };
            let changed = HostKeyIdentity {
                algorithm: "ssh-ed25519".into(),
                fingerprint: "SHA256:changed".into(),
            };

            let empty = store.load().unwrap();
            assert!(
                matches!(
                    verify_known_host(&empty, "host.example", 2222, first.clone()),
                    Err(HostKeyIssue::FirstContact { observed }) if observed == first
                ),
                "first contact must fail closed until the displayed key is explicitly trusted"
            );
            trust_host_key(&store, "host.example", 2222, &first).unwrap();
            let stored = store.load().unwrap();
            assert_eq!(stored.entries[0].identity, first);
            assert!(
                verify_known_host(&stored, "host.example", 2222, first.clone()).is_ok(),
                "an exact host:port:key match reconnects"
            );
            assert!(
                matches!(
                    verify_known_host(&stored, "host.example", 2222, changed.clone()),
                    Err(HostKeyIssue::Changed { expected, observed }) if expected == first && observed == changed
                ),
                "a changed key must fail closed"
            );
            assert!(
                trust_host_key(&store, "host.example", 2222, &changed).is_err(),
                "a changed key cannot silently replace a pin"
            );

            // The pin is this device's: a device-local setting, never one
            // that roams to the person's other devices.
            let document = serde_json::Value::Object(ui::storage::settings_at(directory.path()));
            assert_eq!(
                document["app_device_local"][crate::resource::REMOTE_APP_ID]["entries"][0]["identity"]
                    ["fingerprint"],
                "SHA256:first",
                "{document}"
            );
            assert!(document.get("app_personal").is_none());

            assert!(forget_host_key(&store, "host.example", 2222).unwrap());
            assert!(!forget_host_key(&store, "host.example", 2222).unwrap());
        }

        eprintln!("phase corrupt_known_hosts_store_fails_closed");
        {
            let (store, directory) = test_known_hosts();
            ui::storage::SettingsSection::named("app_device_local")
                .in_profile(directory.path())
                .update(
                    serde_json::json!({
                        crate::resource::REMOTE_APP_ID: { "entries": "not a list" }
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                )
                .unwrap();
            assert!(
                store.load().is_err(),
                "corrupt trust state cannot be treated as empty"
            );
            assert!(
                trust_host_key(
                    &store,
                    "host.example",
                    22,
                    &HostKeyIdentity {
                        algorithm: "ssh-ed25519".into(),
                        fingerprint: "SHA256:any".into(),
                    }
                )
                .is_err(),
                "nothing is trusted over a corrupt store"
            );
        }
    }

    // ---- Real round-trip against a loopback russh server ----------------
    //
    // This is the gated "real" proof for SSH: a genuine russh *server* on a
    // 127.0.0.1 socket, a genuine `SshSession` client connecting to it over
    // TCP (real KEX, real password auth, real PTY shell channel). We assert
    // the client reaches `Connected`, receives the server's shell banner
    // into the grid, and that a keystroke sent upstream is echoed back.

    use russh::server::{self, Auth, Msg, Server as _, Session};
    use russh::{Channel, ChannelId};

    /// The loopback server's tiny file system, and the transfers it is
    /// receiving, by channel.
    /// What the loopback server holds for a path: its bytes, and for a `cat >`
    /// the channel and path they are being written to.
    type Served = Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>;
    type Receiving = Arc<Mutex<std::collections::HashMap<ChannelId, (String, Vec<u8>)>>>;

    #[derive(Clone, Default)]
    struct TestFiles {
        files: Served,
        open: Receiving,
    }

    impl TestFiles {
        /// The bytes served for `path`, read under the lock and handed on
        /// outside it so no guard outlives the call.
        fn served(&self, path: &str) -> Option<Vec<u8>> {
            let files = self.files.lock().unwrap();
            files.get(path).cloned()
        }
    }

    #[derive(Clone)]
    struct TestServer {
        files: TestFiles,
    }

    impl server::Server for TestServer {
        type Handler = TestHandler;
        fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> TestHandler {
            TestHandler {
                files: self.files.clone(),
            }
        }
    }

    struct TestHandler {
        files: TestFiles,
    }

    impl server::Handler for TestHandler {
        type Error = russh::Error;

        async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
            if user == "tester" && password == "secret" {
                Ok(Auth::Accept)
            } else {
                Ok(Auth::reject())
            }
        }

        async fn channel_open_session(
            &mut self,
            _channel: Channel<Msg>,
            reply: server::ChannelOpenHandle,
            _session: &mut Session,
        ) -> Result<(), Self::Error> {
            reply.accept().await;
            Ok(())
        }

        async fn shell_request(
            &mut self,
            channel: ChannelId,
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            session.channel_success(channel)?;
            session.data(channel, b"tilo-remote ready\r\n".to_vec())?;
            Ok(())
        }

        async fn exec_request(
            &mut self,
            channel: ChannelId,
            data: &[u8],
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            let command = String::from_utf8_lossy(data);
            let path = command
                .trim_start_matches("cat > ")
                .trim_start_matches("cat ")
                .trim_matches('\'')
                .to_owned();
            session.channel_success(channel)?;
            if command.starts_with("cat > ") {
                self.files
                    .open
                    .lock()
                    .unwrap()
                    .insert(channel, (path, Vec::new()));
            } else if let Some(bytes) = self.files.served(&path) {
                session.data(channel, bytes)?;
                session.exit_status_request(channel, 0)?;
                session.eof(channel)?;
                session.close(channel)?;
            } else {
                session.extended_data(
                    channel,
                    1,
                    format!("cat: {path}: No such file or directory").into_bytes(),
                )?;
                session.exit_status_request(channel, 1)?;
                session.eof(channel)?;
                session.close(channel)?;
            }
            Ok(())
        }

        async fn channel_eof(
            &mut self,
            channel: ChannelId,
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            let received = self.files.open.lock().unwrap().remove(&channel);
            if let Some((path, bytes)) = received {
                self.files.files.lock().unwrap().insert(path, bytes);
                session.exit_status_request(channel, 0)?;
                session.eof(channel)?;
                session.close(channel)?;
            }
            Ok(())
        }

        async fn data(
            &mut self,
            channel: ChannelId,
            data: &[u8],
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            if let Some((_, received)) = self.files.open.lock().unwrap().get_mut(&channel) {
                received.extend_from_slice(data);
                return Ok(());
            }
            // Echo input back prefixed, so the client can prove the upstream
            // pipe works end-to-end.
            let mut echo = b"echo:".to_vec();
            echo.extend_from_slice(data);
            session.data(channel, echo)?;
            Ok(())
        }
    }

    // A throwaway Ed25519 host key (generated with ssh-keygen) so the test
    // server has an identity without pulling an RNG dependency.
    const TEST_HOST_KEY: &str = "\
-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACCuFBwOt8vsblgfGgvFgrD/sFWDu+bxylWhkpDgq/FItgAAAKAv68KGL+vC
hgAAAAtzc2gtZWQyNTUxOQAAACCuFBwOt8vsblgfGgvFgrD/sFWDu+bxylWhkpDgq/FItg
AAAEBZipAmIHSgB3PdNuFSxUx0OMiQ8hQmxBhvFbCOREICU64UHA63y+xuWB8aC8WCsP+w
VYO75vHKVaGSkOCr8Ui2AAAAF3Fvcy1yZW1vdGUtdGVzdC1ob3N0a2V5AQIDBAUG
-----END OPENSSH PRIVATE KEY-----
";

    #[test]
    fn real_ssh_round_trip_over_loopback() {
        let (port, _files) = served_files([]);

        let (known_hosts, known_hosts_directory) = test_known_hosts();
        let host_key = russh::keys::decode_secret_key(TEST_HOST_KEY, None).unwrap();
        let identity = HostKeyIdentity {
            algorithm: host_key.public_key().algorithm().as_str().to_string(),
            fingerprint: host_key
                .public_key()
                .fingerprint(HashAlg::Sha256)
                .to_string(),
        };
        trust_host_key(&known_hosts, "127.0.0.1", port, &identity).unwrap();

        // Real client — a task on the app runtime, exactly as production.
        let mut ssh = SshSession::connect(SshConfig {
            host: "127.0.0.1".into(),
            port,
            user: "tester".into(),
            auth: SshAuth::Password("secret".into()),
            cols: 80,
            rows: 24,
            known_hosts,
        })
        .expect("the mock ssh transport connects");

        tilo_test_env::wait_until("the client to reach Connected", || match ssh.status() {
            SessionStatus::Connected => true,
            SessionStatus::Failed(e) => panic!("connect failed: {e}"),
            _ => false,
        });

        tilo_test_env::wait_until("the banner in the grid", || {
            ssh.pump();
            ssh.grid().lines().iter().any(|l| {
                l.cells
                    .iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .contains("tilo-remote ready")
            })
        });

        // Prove the upstream pipe: send a byte, expect the echo back.
        let acknowledgement = ssh.send_input_ack(b"Z".to_vec()).unwrap();
        assert_eq!(
            acknowledgement
                .recv_timeout(tilo_test_env::hang_bound())
                .unwrap(),
            Ok(()),
            "resource input is acknowledged only after the SSH channel write"
        );
        tilo_test_env::wait_until("the server's echo of the upstream keystroke", || {
            ssh.pump();
            ssh.grid().lines().iter().any(|l| {
                l.cells
                    .iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .contains("echo:Z")
            })
        });
        drop(ssh);
        drop(known_hosts_directory);
    }

    #[test]
    fn a_file_crosses_a_real_session_both_ways_and_a_missing_one_says_why() {
        let (port, files) = served_files([("remote.txt", b"from the remote machine".to_vec())]);

        let (ssh, known_hosts_directory) = trusted_session_on(port);

        let directory = tempfile::tempdir().unwrap();
        let local = directory.path().join("local.bin");
        std::fs::write(&local, b"from this device").unwrap();
        let put = ui::jobs::start(ui::jobs::Spec::new("Put a file"));
        let done = ssh
            .transfer(
                TransferDirection::Put {
                    local_path: local,
                    remote_path: "upload.bin".into(),
                },
                put.clone(),
            )
            .unwrap();
        assert_eq!(
            done.recv_timeout(hang_bound()).unwrap(),
            Ok(b"from this device".len() as u64)
        );
        assert_eq!(
            files.files.lock().unwrap().get("upload.bin").cloned(),
            Some(b"from this device".to_vec())
        );
        let put_row = ui::jobs::rows()
            .into_iter()
            .find(|job| job.id() == put.id())
            .expect("the put job is in the jobs surface");
        assert!(matches!(
            put_row.outcome(),
            Some(ui::jobs::Outcome::Done(_))
        ));
        assert_eq!(
            put_row.progress(),
            &ui::jobs::Progress::Measured {
                done: b"from this device".len() as u64,
                total: b"from this device".len() as u64,
            },
            "the put reports the bytes it moved against the file's size"
        );

        let fetched = directory.path().join("fetched.txt");
        let fetch = ui::jobs::start(ui::jobs::Spec::new("Fetch a file"));
        let done = ssh
            .transfer(
                TransferDirection::Fetch {
                    remote_path: "remote.txt".into(),
                    local_path: fetched.clone(),
                },
                fetch.clone(),
            )
            .unwrap();
        assert_eq!(
            done.recv_timeout(hang_bound()).unwrap(),
            Ok(b"from the remote machine".len() as u64)
        );
        assert_eq!(std::fs::read(&fetched).unwrap(), b"from the remote machine");
        let fetch_row = ui::jobs::rows()
            .into_iter()
            .find(|job| job.id() == fetch.id())
            .expect("the fetch job is in the jobs surface");
        assert_eq!(
            fetch_row.progress(),
            &ui::jobs::Progress::Seen {
                seen: b"from the remote machine".len() as u64,
            },
            "the fetch reports the bytes it has seen"
        );

        let missing = directory.path().join("missing.txt");
        let job = ui::jobs::start(ui::jobs::Spec::new("Fetch a missing file"));
        let done = ssh
            .transfer(
                TransferDirection::Fetch {
                    remote_path: "missing.txt".into(),
                    local_path: missing.clone(),
                },
                job.clone(),
            )
            .unwrap();
        let error = done.recv_timeout(hang_bound()).unwrap().unwrap_err();
        assert!(error.contains("missing.txt"), "{error}");
        assert!(error.contains("No such file or directory"), "{error}");
        assert!(!missing.exists(), "a failed fetch leaves no empty file");
        assert!(matches!(
            ui::jobs::rows().iter().find(|row| row.id() == job.id()).map(ui::jobs::Job::outcome),
            Some(Some(ui::jobs::Outcome::Failed(reason))) if reason.contains("missing.txt")
        ));

        drop(ssh);
        drop(known_hosts_directory);
    }

    /// A loopback server holding `by_path`'s files, on a port of its own.
    fn served_files(
        by_path: impl IntoIterator<Item = (&'static str, Vec<u8>)>,
    ) -> (u16, TestFiles) {
        ui::rt::app()
            .block_on(async move {
                let host_key = russh::keys::decode_secret_key(TEST_HOST_KEY, None).unwrap();
                let config = Arc::new(server::Config {
                    inactivity_timeout: None,
                    auth_rejection_time: Duration::from_millis(50),
                    keys: vec![host_key],
                    ..Default::default()
                });
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                    .await
                    .unwrap();
                let port = listener.local_addr().unwrap().port();
                let files = TestFiles::default();
                for (path, bytes) in by_path {
                    files.files.lock().unwrap().insert(path.into(), bytes);
                }
                let for_the_server = files.clone();
                tokio::spawn(async move {
                    let mut server = TestServer {
                        files: for_the_server,
                    };
                    let _ = server.run_on_socket(config, &listener).await;
                });
                (port, files)
            })
            .unwrap()
    }

    /// A connected session to the loopback server on `port`, whose host key
    /// this device already trusts. The returned directory holds that trust.
    fn trusted_session_on(port: u16) -> (SshSession, tempfile::TempDir) {
        let (known_hosts, known_hosts_directory) = test_known_hosts();
        let host_key = russh::keys::decode_secret_key(TEST_HOST_KEY, None).unwrap();
        let identity = HostKeyIdentity {
            algorithm: host_key.public_key().algorithm().as_str().to_string(),
            fingerprint: host_key
                .public_key()
                .fingerprint(HashAlg::Sha256)
                .to_string(),
        };
        trust_host_key(&known_hosts, "127.0.0.1", port, &identity).unwrap();
        let ssh = SshSession::connect(SshConfig {
            host: "127.0.0.1".into(),
            port,
            user: "tester".into(),
            auth: SshAuth::Password("secret".into()),
            cols: 80,
            rows: 24,
            known_hosts,
        })
        .unwrap();
        tilo_test_env::wait_until("the client to reach Connected", || {
            ssh.status() == SessionStatus::Connected
        });
        (ssh, known_hosts_directory)
    }
}
