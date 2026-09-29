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

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{anyhow, bail};
use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, load_secret_key};
use russh::{ChannelMsg, Disconnect};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{Receiver, Sender, channel, error::TrySendError};

use crate::session::SessionStatus;
use crate::vt::VtGrid;

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
    pub known_hosts_path: PathBuf,
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

fn load_known_hosts(path: &Path) -> Result<KnownHosts, String> {
    validate_private_store_path(path)?;
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(KnownHosts::default()),
        Err(error) => Err(error.to_string()),
    }
}

fn write_known_hosts(path: &Path, known_hosts: &KnownHosts) -> Result<(), String> {
    validate_private_store_path(path)?;
    let bytes = serde_json::to_vec_pretty(known_hosts).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600)
        .map_err(|error| error.to_string())?;
    validate_private_store_path(path)
}

/// Per-device default location for host trust, intentionally outside the
/// collaborative document. Tests and managed deployments can override it.
///
/// # Errors
///
/// Returns an error when no private state root exists or when the configured
/// store path is not a private, non-symlinked directory/file pair.
pub fn default_known_hosts_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("REMOTE_SSH_KNOWN_HOSTS") {
        let path = PathBuf::from(path);
        validate_private_store_path(&path)?;
        return Ok(path);
    }
    let state = tilo_paths::xdg::state_home()
        .ok_or_else(|| "no private state directory is available for SSH host trust".to_string())?;
    let directory = state.join("tilo").join("remote");
    ensure_private_directory(&directory)?;
    let path = directory.join("known-hosts.json");
    validate_private_store_path(&path)?;
    Ok(path)
}

/// The trust database is an integrity boundary. A world- or group-writable
/// parent permits an attacker to replace it; a symlinked path permits it to
/// point elsewhere. Reject both rather than trying to repair arbitrary paths.
fn validate_private_store_path(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "SSH host-key store has no private parent directory".to_string())?;
    validate_private_directory(parent)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
                return Err("SSH host-key store must be a regular file".into());
            }
            validate_private_mode(&metadata, "SSH host-key store")?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_private_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|error| error.to_string())?;
            private_fs::restrict_directory(path).map_err(|error| error.to_string())?;
            validate_private_directory(path)
        }
        Err(error) => Err(error.to_string()),
    }
}

fn validate_private_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err("SSH host-key store parent must be a real directory".into());
    }
    validate_private_mode(&metadata, "SSH host-key store parent")
}

fn validate_private_mode(metadata: &fs::Metadata, label: &str) -> Result<(), String> {
    if !private_fs::is_owner_only(metadata) {
        return Err(format!(
            "{label} must not be accessible to group or other users"
        ));
    }
    Ok(())
}

/// Explicitly trust the exact fingerprint shown by a failed first-contact
/// check. A changed key is never overwritten: callers must forget it first.
///
/// # Errors
///
/// Returns an error for an insecure/corrupt trust store or when the target
/// already has a different trusted key.
pub fn trust_host_key(
    path: &Path,
    host: &str,
    port: u16,
    observed: &HostKeyIdentity,
) -> Result<(), String> {
    let mut known_hosts = load_known_hosts(path)?;
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
    write_known_hosts(path, &known_hosts)
}

/// Explicitly forget the local trust entry. This only changes device-private
/// trust metadata; no shared connection profile or remote host is touched.
///
/// # Errors
///
/// Returns an error if the local trust store cannot be safely read or written.
pub fn forget_host_key(path: &Path, host: &str, port: u16) -> Result<bool, String> {
    let mut known_hosts = load_known_hosts(path)?;
    let before = known_hosts.entries.len();
    known_hosts
        .entries
        .retain(|entry| entry.host != host || entry.port != port);
    if known_hosts.entries.len() == before {
        return Ok(false);
    }
    write_known_hosts(path, &known_hosts)?;
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

/// Resolve the auth method from a lookup function (the process environment
/// in production, an in-test map in the unit tests). Password auth reads
/// `REMOTE_SSH_PASSWORD`; key auth reads `REMOTE_SSH_KEY` (falling back to
/// the usual `~/.ssh` identities) and the optional `REMOTE_SSH_KEY_PASSPHRASE`.
///
/// Split out from the environment so it can be tested without mutating
/// process-global state.
///
/// # Errors
///
/// Returns an error if key auth is requested and neither `REMOTE_SSH_KEY`
/// nor any default identity file exists, or if password auth is requested
/// and `REMOTE_SSH_PASSWORD` is unset.
pub fn resolve_auth(
    prefer_key: bool,
    get: impl Fn(&str) -> Option<String>,
    default_keys: &[PathBuf],
) -> anyhow::Result<SshAuth> {
    if prefer_key {
        let path = match get("REMOTE_SSH_KEY") {
            Some(p) => PathBuf::from(p),
            None => default_keys
                .iter()
                .find(|p| p.exists())
                .cloned()
                .ok_or_else(|| {
                    anyhow!("no SSH key: set REMOTE_SSH_KEY or add ~/.ssh/id_ed25519")
                })?,
        };
        Ok(SshAuth::Key {
            path,
            passphrase: get("REMOTE_SSH_KEY_PASSPHRASE"),
        })
    } else {
        let pw = get("REMOTE_SSH_PASSWORD")
            .ok_or_else(|| anyhow!("no SSH password: set REMOTE_SSH_PASSWORD"))?;
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

/// A live SSH session handle held by the UI.
pub struct SshSession {
    grid: VtGrid,
    status: Arc<Mutex<SessionStatus>>,
    output_rx: Receiver<Vec<u8>>,
    input_tx: Sender<InputMessage>,
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
            )
            .await
            {
                let mut s = lock_recover(&status_worker);
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
            host_key_issue,
            _worker: worker,
        })
    }

    pub fn status(&self) -> SessionStatus {
        lock_recover(&self.status).clone()
    }

    /// The rejected host-key observation, if verification stopped the
    /// handshake. This public metadata is deliberately separate from secrets.
    pub fn host_key_issue(&self) -> Option<HostKeyIssue> {
        lock_recover(&self.host_key_issue).clone()
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
    ///
    /// # Errors
    ///
    /// Returns an error when input is empty, too large, the bounded queue is
    /// full, or the session worker has stopped.
    pub fn send_input(&self, bytes: Vec<u8>) -> Result<(), String> {
        self.queue_input(InputMessage { bytes, ack: None })
    }

    /// Queue bytes and receive an acknowledgement only after russh has
    /// accepted them on the live channel. Resource actions use this instead
    /// of mistaking insertion into the UI→worker queue for delivery.
    ///
    /// # Errors
    ///
    /// Returns an error if the input worker has already stopped.
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

    fn queue_input(&self, message: InputMessage) -> Result<(), String> {
        if message.bytes.is_empty() {
            return Err("SSH input cannot be empty".into());
        }
        if message.bytes.len() > MAX_SSH_INPUT_BYTES {
            return Err(format!(
                "SSH input exceeds the {} KiB per-operation limit",
                MAX_SSH_INPUT_BYTES / 1024
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

/// Locks `m`, recovering a poisoned mutex instead of panicking: one
/// worker thread's earlier panic must not cascade into every later read
/// of shared session state.
fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Fail-closed host-key verifier. It checks only a device-private database;
/// saved Remote profiles remain safe to collaborate on without carrying trust.
struct Handler {
    host: String,
    port: u16,
    known_hosts_path: PathBuf,
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
        let issue = match load_known_hosts(&self.known_hosts_path) {
            Ok(known_hosts) => {
                match verify_known_host(&known_hosts, &self.host, self.port, observed) {
                    Ok(()) => return Ok(true),
                    Err(issue) => issue,
                }
            }
            Err(error) => HostKeyIssue::StoreFailure(error),
        };
        *lock_recover(&self.issue) = Some(issue);
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

async fn run_session(
    cfg: SshConfig,
    status: Arc<Mutex<SessionStatus>>,
    host_key_issue: Arc<Mutex<Option<HostKeyIssue>>>,
    output_tx: Sender<Vec<u8>>,
    mut input_rx: Receiver<InputMessage>,
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
            known_hosts_path: cfg.known_hosts_path.clone(),
            issue: Arc::clone(&host_key_issue),
        },
    )
    .await
    .map_err(|e| {
        lock_recover(&host_key_issue).as_ref().map_or_else(
            || anyhow!("connect: {e}"),
            |issue| anyhow!(issue.message(&host, port)),
        )
    })?;

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

    *lock_recover(&status) = SessionStatus::Connected;

    let mut channel = channel;
    loop {
        tokio::select! {
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
        let mut s = lock_recover(&status);
        if *s == SessionStatus::Connected {
            *s = SessionStatus::Closed;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
    const HANG_BOUND: Duration = Duration::from_secs(30);

    #[test]
    fn auth_resolution_reads_env_and_default_keys() {
        eprintln!("phase resolve_auth_password_reads_env");
        {
            let env = |k: &str| (k == "REMOTE_SSH_PASSWORD").then(|| "hunter2".to_string());
            match resolve_auth(false, env, &[]).unwrap() {
                SshAuth::Password(p) => assert_eq!(p, "hunter2"),
                other @ SshAuth::Key { .. } => panic!("expected password auth, got {other:?}"),
            }
        }

        eprintln!("phase resolve_auth_password_missing_is_error");
        {
            assert!(resolve_auth(false, |_| None, &[]).is_err());
        }

        eprintln!("phase resolve_auth_key_prefers_env_over_defaults");
        {
            let env = |k: &str| match k {
                "REMOTE_SSH_KEY" => Some("/keys/explicit".to_string()),
                "REMOTE_SSH_KEY_PASSPHRASE" => Some("pw".to_string()),
                _ => None,
            };
            let defaults = vec![PathBuf::from("/keys/default")];
            match resolve_auth(true, env, &defaults).unwrap() {
                SshAuth::Key { path, passphrase } => {
                    assert_eq!(path, PathBuf::from("/keys/explicit"));
                    assert_eq!(passphrase.as_deref(), Some("pw"));
                }
                other @ SshAuth::Password(_) => panic!("expected key auth, got {other:?}"),
            }
        }

        eprintln!("phase default_key_paths_are_under_dot_ssh");
        {
            let paths = default_key_paths();
            assert!(paths.iter().any(|p| p.ends_with(".ssh/id_ed25519")));
        }
    }

    fn test_known_hosts_path() -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("tilo-remote-known-hosts-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        directory.join("known-hosts.json")
    }

    #[test]
    fn known_hosts_store_fails_closed_until_exactly_trusted() {
        eprintln!("phase device_private_known_hosts_fails_closed_until_an_exact_key_is_trusted");
        {
            let path = test_known_hosts_path();
            let first = HostKeyIdentity {
                algorithm: "ssh-ed25519".into(),
                fingerprint: "SHA256:first".into(),
            };
            let changed = HostKeyIdentity {
                algorithm: "ssh-ed25519".into(),
                fingerprint: "SHA256:changed".into(),
            };

            let empty = load_known_hosts(&path).unwrap();
            assert!(
                matches!(
                    verify_known_host(&empty, "host.example", 2222, first.clone()),
                    Err(HostKeyIssue::FirstContact { observed }) if observed == first
                ),
                "first contact must fail closed until the displayed key is explicitly trusted"
            );
            trust_host_key(&path, "host.example", 2222, &first).unwrap();
            let stored = load_known_hosts(&path).unwrap();
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
                trust_host_key(&path, "host.example", 2222, &changed).is_err(),
                "a changed key cannot silently replace a pin"
            );
            assert!(forget_host_key(&path, "host.example", 2222).unwrap());
            assert!(!forget_host_key(&path, "host.example", 2222).unwrap());
            fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }

        eprintln!("phase corrupt_known_hosts_store_fails_closed");
        {
            let path = test_known_hosts_path();
            fs::write(&path, b"not JSON").unwrap();
            assert!(
                load_known_hosts(&path).is_err(),
                "corrupt trust state cannot be treated as empty"
            );
            fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn known_hosts_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let path = test_known_hosts_path();
        let identity = HostKeyIdentity {
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:private".into(),
        };
        trust_host_key(&path, "host.example", 22, &identity).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
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

    #[derive(Clone)]
    struct TestServer;

    impl server::Server for TestServer {
        type Handler = TestHandler;
        fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> TestHandler {
            TestHandler
        }
    }

    struct TestHandler;

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

        async fn data(
            &mut self,
            channel: ChannelId,
            data: &[u8],
            session: &mut Session,
        ) -> Result<(), Self::Error> {
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
        // Stand up the server, bind an ephemeral port, and serve in the bg.
        let port = ui::rt::app()
            .block_on(async {
                let host_key = russh::keys::decode_secret_key(TEST_HOST_KEY, None).unwrap();
                let config = Arc::new(server::Config {
                    inactivity_timeout: Some(Duration::from_secs(30)),
                    auth_rejection_time: Duration::from_millis(50),
                    keys: vec![host_key],
                    ..Default::default()
                });
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                    .await
                    .unwrap();
                let port = listener.local_addr().unwrap().port();
                tokio::spawn(async move {
                    let mut server = TestServer;
                    let _ = server.run_on_socket(config, &listener).await;
                });
                port
            })
            .unwrap();

        let known_hosts_path = test_known_hosts_path();
        let host_key = russh::keys::decode_secret_key(TEST_HOST_KEY, None).unwrap();
        let identity = HostKeyIdentity {
            algorithm: host_key.public_key().algorithm().as_str().to_string(),
            fingerprint: host_key
                .public_key()
                .fingerprint(HashAlg::Sha256)
                .to_string(),
        };
        trust_host_key(&known_hosts_path, "127.0.0.1", port, &identity).unwrap();
        let known_hosts_directory = known_hosts_path.parent().unwrap().to_path_buf();

        // Real client — a task on the app runtime, exactly as production.
        let mut ssh = SshSession::connect(SshConfig {
            host: "127.0.0.1".into(),
            port,
            user: "tester".into(),
            auth: SshAuth::Password("secret".into()),
            cols: 80,
            rows: 24,
            known_hosts_path,
        })
        .expect("the mock ssh transport connects");

        // Poll to Connected.
        let mut connected = false;
        let deadline = ui::time::now() + HANG_BOUND;
        while ui::time::now() < deadline {
            match ssh.status() {
                SessionStatus::Connected => {
                    connected = true;
                    break;
                }
                SessionStatus::Failed(e) => panic!("connect failed: {e}"),
                _ => std::thread::sleep(Duration::from_millis(25)),
            }
        }
        assert!(connected, "client never reached Connected");

        // Drain the banner into the grid.
        let mut saw_banner = false;
        let deadline = ui::time::now() + HANG_BOUND;
        while ui::time::now() < deadline {
            ssh.pump();
            if ssh.grid().lines().iter().any(|l| {
                l.cells
                    .iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .contains("tilo-remote ready")
            }) {
                saw_banner = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(saw_banner, "banner never arrived in the grid");

        // Prove the upstream pipe: send a byte, expect the echo back.
        let acknowledgement = ssh.send_input_ack(b"Z".to_vec()).unwrap();
        assert_eq!(
            acknowledgement
                .recv_timeout(tilo_test_env::hang_bound())
                .unwrap(),
            Ok(()),
            "resource input is acknowledged only after the SSH channel write"
        );
        let mut saw_echo = false;
        let deadline = ui::time::now() + HANG_BOUND;
        while ui::time::now() < deadline {
            ssh.pump();
            if ssh.grid().lines().iter().any(|l| {
                l.cells
                    .iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .contains("echo:Z")
            }) {
                saw_echo = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(saw_echo, "server never echoed the upstream keystroke");
        drop(ssh);
        fs::remove_dir_all(known_hosts_directory).unwrap();
    }
}
