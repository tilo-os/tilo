//! Real SSH transport, built on `russh`: a shell on a PTY channel, its host key verified
//! before sign-in, and files moved over channels of their own.
//!
//! A session connects on a thread of its own and publishes what the shell printed and how it
//! stands. A key this device has not trusted stops the connection before authentication, with
//! the key it observed; a changed key stops it too. The trust is the app's to keep.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{anyhow, bail};
use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, load_secret_key};
use russh::{ChannelMsg, Disconnect};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::Moved;
use crate::trust::{HostKeyIdentity, HostKeyIssue, verify};

/// Keep the loop responsive under a stalled SSH transport without allowing an unbounded queue.
const SSH_OUTPUT_QUEUE_CAPACITY: usize = 128;
/// Agent-supplied `send-text` is useful for commands, not bulk file transfer.
pub const MAX_SSH_INPUT_BYTES: usize = 64 * 1024;
/// How long an action waits for its input to reach the channel before saying it did not.
const INPUT_ACKNOWLEDGEMENT: Duration = Duration::from_secs(5);

/// How to authenticate an SSH connection. Secrets live only here, in memory — never in a saved
/// connection.
#[derive(Clone, Debug)]
pub enum SshAuth {
    Password(String),
    Key {
        path: PathBuf,
        passphrase: Option<String>,
    },
}

/// One saved machine to reach over SSH.
#[derive(Clone, Debug)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    pub cols: u16,
    pub rows: u16,
    /// The host key this device trusted for the machine, when it trusted one: the connection is
    /// refused unless the machine presents exactly it.
    pub expected: Option<HostKeyIdentity>,
}

/// How an SSH session stands, as the window says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SshStatus {
    /// The handshake, the sign-in or the shell is in flight.
    Connecting,
    /// The shell is live.
    Connected,
    /// The remote closed the shell.
    Closed,
    /// The connection, the host key or the sign-in failed, in words a person reads; a host key
    /// that was refused names the decision the person has to make.
    Failed {
        why: String,
        issue: Option<HostKeyIssue>,
    },
}

/// What a saved connection's credentials resolve to: a password this device saved, or the first
/// of the usual `~/.ssh` identities that exists, unlocked with its saved passphrase.
///
/// # Errors
///
/// Why there is no credential to sign in with.
pub fn resolve_auth(
    prefer_key: bool,
    secret: Option<String>,
    default_keys: &[PathBuf],
) -> anyhow::Result<SshAuth> {
    if prefer_key {
        let path = default_keys
            .iter()
            .find(|path| path.exists())
            .cloned()
            .ok_or_else(|| anyhow!("no SSH key: add ~/.ssh/id_ed25519 or ~/.ssh/id_ecdsa"))?;
        Ok(SshAuth::Key {
            path,
            passphrase: secret,
        })
    } else {
        let password = secret.ok_or_else(|| {
            anyhow!("no SSH password saved on this device: edit the connection to enter it")
        })?;
        Ok(SshAuth::Password(password))
    }
}

/// Standard on-disk identity paths, most-preferred first.
#[must_use]
pub fn default_key_paths() -> Vec<PathBuf> {
    let home = tilo_paths::home().unwrap_or_default();
    ["id_ed25519", "id_ecdsa"]
        .iter()
        .map(|name| home.join(".ssh").join(name))
        .collect()
}

/// The login name a connection leaves out: this device's person.
#[must_use]
pub fn default_user() -> String {
    tilo_paths::session::user_name().unwrap_or_else(|| "root".to_owned())
}

/// Which way one file goes.
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

/// The file a transfer names, for a job's ending sentence.
#[must_use]
pub fn transfer_name(direction: &TransferDirection) -> String {
    match direction {
        TransferDirection::Put { local_path, .. } => local_path.file_name().map_or_else(
            || local_path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        ),
        TransferDirection::Fetch { remote_path, .. } => remote_path.clone(),
    }
}

/// What one transfer reports as it moves, and whether the person stopped it.
pub struct Transfer {
    pub progress: MovedAt,
    pub stopped: Arc<AtomicBool>,
}

struct TransferCommand {
    direction: TransferDirection,
    moved: MovedAt,
    stopped: Arc<AtomicBool>,
    completion: std::sync::mpsc::Sender<Result<u64, String>>,
}

/// A progress report kept for the transfer's thread.
type MovedAt = Arc<dyn Fn(Moved) + Send + Sync>;

struct InputMessage {
    bytes: Vec<u8>,
    ack: Option<std::sync::mpsc::Sender<Result<(), String>>>,
}

/// A live SSH session: its status, what its shell printed, and the queues it takes work on.
pub struct SshSession {
    status: Arc<Mutex<SshStatus>>,
    issue: Arc<Mutex<Option<HostKeyIssue>>>,
    output: Mutex<Option<Receiver<Vec<u8>>>>,
    input_tx: UnboundedSender<InputMessage>,
    command_tx: UnboundedSender<TransferCommand>,
    resizes: UnboundedSender<(u16, u16)>,
    shutdown: UnboundedSender<()>,
    _worker: std::thread::JoinHandle<()>,
}

impl SshSession {
    /// Start connecting; answers at once, with a session whose status is `Connecting`. `said` is
    /// told each way the session stands.
    ///
    /// # Errors
    ///
    /// Why the session's thread could not start.
    pub fn start(
        cfg: SshConfig,
        said: Box<dyn Fn(SshStatus) + Send + Sync>,
    ) -> Result<Self, String> {
        let status = Arc::new(Mutex::new(SshStatus::Connecting));
        let issue: Arc<Mutex<Option<HostKeyIssue>>> = Arc::new(Mutex::new(None));
        let (output_tx, mut output_rx) =
            tokio::sync::mpsc::channel::<Vec<u8>>(SSH_OUTPUT_QUEUE_CAPACITY);
        let (bridge_tx, bridge_rx) =
            std::sync::mpsc::sync_channel::<Vec<u8>>(SSH_OUTPUT_QUEUE_CAPACITY);
        let (input_tx, input_rx) = unbounded_channel::<InputMessage>();
        let (command_tx, command_rx) = unbounded_channel::<TransferCommand>();
        let (resizes, resize_rx) = unbounded_channel::<(u16, u16)>();
        let (shutdown, shutdown_rx) = unbounded_channel::<()>();

        std::thread::Builder::new()
            .name("remote-ssh-output".into())
            .spawn(move || {
                while let Some(chunk) = output_rx.blocking_recv() {
                    if bridge_tx.send(chunk).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| format!("could not start the SSH session's output: {error}"))?;

        let status_worker = Arc::clone(&status);
        let issue_worker = Arc::clone(&issue);
        let worker = std::thread::Builder::new()
            .name("remote-ssh".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let reason = format!("could not start the SSH client: {error}");
                        said(SshStatus::Failed {
                            why: reason,
                            issue: None,
                        });
                        return;
                    }
                };
                let ended = runtime.block_on(run_session(
                    cfg,
                    &status_worker,
                    &issue_worker,
                    Lines {
                        output_tx,
                        input_rx,
                        commands: command_rx,
                        resizes: resize_rx,
                        shutdown: shutdown_rx,
                    },
                    &*said,
                ));
                let ended = match ended {
                    Ok(()) => SshStatus::Closed,
                    Err(error) => SshStatus::Failed {
                        why: error.to_string(),
                        issue: issue_worker
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .clone(),
                    },
                };
                let mut now = status_worker.lock().unwrap_or_else(PoisonError::into_inner);
                // A failure the loop already reported stays as it was said; an ended session is
                // reported closed only when it was live.
                let final_status = match (&*now, ended) {
                    (SshStatus::Failed { .. }, _) => now.clone(),
                    (_, failed @ SshStatus::Failed { .. }) => failed,
                    _ => SshStatus::Closed,
                };
                *now = final_status.clone();
                drop(now);
                said(final_status);
            })
            .map_err(|error| format!("could not start the SSH session: {error}"))?;

        Ok(Self {
            status,
            issue,
            output: Mutex::new(Some(bridge_rx)),
            input_tx,
            command_tx,
            resizes,
            shutdown,
            _worker: worker,
        })
    }

    /// How the session stands now.
    #[must_use]
    pub fn status(&self) -> SshStatus {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The rejected host-key observation, if verification stopped the connection.
    #[must_use]
    pub fn host_key_issue(&self) -> Option<HostKeyIssue> {
        self.issue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Take what the shell prints: the reader of it, once.
    pub fn take_output(&self) -> Option<Receiver<Vec<u8>>> {
        self.output
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Send keystrokes to the remote PTY without waiting for them to reach it.
    ///
    /// # Errors
    ///
    /// Empty input, input over the limit, or a closed session.
    pub fn send_input(&self, bytes: Vec<u8>) -> Result<(), String> {
        self.queue_input(InputMessage { bytes, ack: None })
            .map(drop)
    }

    /// Queue bytes and answer once russh accepted them on the live channel, so a caller knows
    /// they reached the remote shell rather than a queue here.
    ///
    /// # Errors
    ///
    /// Empty input, input over the limit, a closed session, or the channel refusing the write.
    pub fn send_input_ack(&self, bytes: Vec<u8>) -> Result<(), String> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.queue_input(InputMessage {
            bytes,
            ack: Some(tx),
        })?;
        match rx.recv_timeout(INPUT_ACKNOWLEDGEMENT) {
            Ok(answer) => answer,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                Err("the remote machine did not take the input in time".to_owned())
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err("the SSH session ended before the input reached it".to_owned())
            }
        }
    }

    fn queue_input(&self, message: InputMessage) -> Result<(), String> {
        if message.bytes.is_empty() {
            return Err("SSH input cannot be empty".to_owned());
        }
        if message.bytes.len() > MAX_SSH_INPUT_BYTES {
            return Err(format!(
                "SSH input exceeds the {MAX_SSH_INPUT_BYTES}-byte per-operation limit"
            ));
        }
        self.input_tx
            .send(message)
            .map_err(|_| "SSH session input channel is closed".to_owned())
    }

    /// Tell the remote shell its window is `cols` × `rows` cells.
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.resizes.send((cols, rows));
    }

    /// Move one file over this session, answering the bytes moved once it finished. Blocks the
    /// calling thread: a job's own thread runs it. `progress` hears each move, and `stopped`
    /// ends it early.
    ///
    /// # Errors
    ///
    /// Why the transfer could not finish, in words a person reads.
    pub fn transfer(
        &self,
        direction: TransferDirection,
        transfer: Transfer,
    ) -> Result<u64, String> {
        let (completion, done) = std::sync::mpsc::channel();
        self.command_tx
            .send(TransferCommand {
                direction,
                moved: transfer.progress,
                stopped: transfer.stopped,
                completion,
            })
            .map_err(|_| "the SSH session is closed".to_owned())?;
        done.recv()
            .map_err(|_| "the SSH session ended before the transfer finished".to_owned())?
    }

    /// End the session: the remote shell's channel closes and the connection goes.
    pub fn close(&self) {
        let _ = self.shutdown.send(());
    }
}

/// Fail-closed host-key verifier. It checks only the key this device trusted for the machine;
/// a saved connection stays safe to collaborate on without carrying trust.
struct Handler {
    expected: Option<HostKeyIdentity>,
    issue: Arc<Mutex<Option<HostKeyIssue>>>,
}

fn host_key_identity(key: &russh::keys::PublicKeyOrCertificate) -> HostKeyIdentity {
    let public_key = key.public_key();
    HostKeyIdentity {
        algorithm: public_key.algorithm().as_str().to_owned(),
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
        match verify(self.expected.as_ref(), observed) {
            Ok(()) => Ok(true),
            Err(issue) => {
                *self.issue.lock().unwrap_or_else(PoisonError::into_inner) = Some(issue);
                Ok(false)
            }
        }
    }
}

/// Load a private key on the CPU pool: decrypting a passphrase-protected key runs a deliberately
/// slow KDF, which must not stall the session's thread.
async fn load_key(
    path: &Path,
    passphrase: Option<&str>,
) -> anyhow::Result<russh::keys::PrivateKey> {
    let (key_path, key_passphrase) = (path.to_path_buf(), passphrase.map(str::to_owned));
    tokio::task::spawn_blocking(move || load_secret_key(&key_path, key_passphrase.as_deref()))
        .await
        .map_err(|error| anyhow!("load key {}: {error}", path.display()))?
        .map_err(|error| anyhow!("load key {}: {error}", path.display()))
}

/// Authenticate the session the way `cfg` says: a password, or a key from disk.
async fn authenticate(
    session: &mut client::Handle<Handler>,
    cfg: &SshConfig,
) -> anyhow::Result<()> {
    let auth_ok = match &cfg.auth {
        SshAuth::Password(password) => session
            .authenticate_password(cfg.user.clone(), password.clone())
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

type SharedStatus = Arc<Mutex<SshStatus>>;

/// The channels a session's loop owns: the shell's output, the input typed into it, the
/// transfers asked of it, its resizes and its end.
struct Lines {
    output_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    input_rx: UnboundedReceiver<InputMessage>,
    commands: UnboundedReceiver<TransferCommand>,
    resizes: UnboundedReceiver<(u16, u16)>,
    shutdown: UnboundedReceiver<()>,
}

async fn run_session(
    cfg: SshConfig,
    status: &SharedStatus,
    issue: &Arc<Mutex<Option<HostKeyIssue>>>,
    lines: Lines,
    said: &(dyn Fn(SshStatus) + Send + Sync),
) -> anyhow::Result<()> {
    let Lines {
        output_tx,
        mut input_rx,
        mut commands,
        mut resizes,
        mut shutdown,
    } = lines;
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
            expected: cfg.expected.clone(),
            issue: Arc::clone(issue),
        },
    )
    .await
    .map_err(|error| {
        issue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or_else(
                || anyhow!("connect: {error}"),
                |rejected| anyhow!(rejected.message(&host, port)),
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

    {
        *status.lock().unwrap_or_else(PoisonError::into_inner) = SshStatus::Connected;
        said(SshStatus::Connected);
    }

    let mut channel = channel;
    let session = Arc::new(session);
    loop {
        tokio::select! {
            command = commands.recv() => {
                if let Some(command) = command {
                    let session = Arc::clone(&session);
                    tokio::spawn(async move { run_transfer(session, command).await });
                }
            }
            size = resizes.recv() => {
                if let Some((columns, rows)) = size {
                    channel
                        .window_change(u32::from(columns), u32::from(rows), 0, 0)
                        .await?;
                }
            }
            queued = shutdown.recv() => {
                if queued.is_some() {
                    let _ = channel.eof().await;
                    break;
                }
            }
            message = channel.wait() => match message {
                Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                    // Awaiting bounded output capacity deliberately slows the SSH read loop when
                    // the window cannot take output: real backpressure rather than dropping
                    // terminal bytes or growing an unbounded queue.
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
                let _ = channel.eof().await;
                break;
            },
        }
    }

    let _ = session
        .disconnect(Disconnect::ByApplication, "", "en")
        .await;
    Ok(())
}

/// Run one file transfer over a channel of its own, then answer it.
async fn run_transfer(session: Arc<client::Handle<Handler>>, command: TransferCommand) {
    let TransferCommand {
        direction,
        moved,
        stopped,
        completion,
    } = command;
    let result = match &direction {
        TransferDirection::Put {
            local_path,
            remote_path,
        } => put_file(&session, local_path, remote_path, &*moved, &stopped).await,
        TransferDirection::Fetch {
            remote_path,
            local_path,
        } => fetch_file(&session, remote_path, local_path, &*moved, &stopped).await,
    };
    let _ = completion.send(result);
}

/// A path as one POSIX shell word, so a remote path is never read as shell syntax.
fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

/// Send a local file to `remote_path`, reporting the bytes moved.
async fn put_file(
    session: &client::Handle<Handler>,
    local_path: &Path,
    remote_path: &str,
    moved: &(dyn Fn(Moved) + Send + Sync),
    stopped: &AtomicBool,
) -> Result<u64, String> {
    if remote_path.trim().is_empty() {
        return Err("the remote path is empty".to_owned());
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
    moved(Moved::Measured { done: 0, total });
    loop {
        if stopped.load(Ordering::Acquire) {
            let _ = channel.close().await;
            return Err("stopped by the person".to_owned());
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
        moved(Moved::Measured { done: sent, total });
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
    moved: &(dyn Fn(Moved) + Send + Sync),
    stopped: &AtomicBool,
) -> Result<u64, String> {
    if remote_path.trim().is_empty() {
        return Err("the remote path is empty".to_owned());
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
    moved(Moved::Seen { seen: 0 });
    loop {
        if stopped.load(Ordering::Acquire) {
            let _ = channel.close().await;
            drop(file);
            let _ = tokio::fs::remove_file(local_path).await;
            return Err("stopped by the person".to_owned());
        }
        match channel.wait().await {
            Some(ChannelMsg::Data { data }) => {
                if let Err(error) = file.write_all(&data).await {
                    let _ = tokio::fs::remove_file(local_path).await;
                    return Err(format!("could not write {}: {error}", local_path.display()));
                }
                received = received.saturating_add(u64::try_from(data.len()).unwrap_or(0));
                moved(Moved::Seen { seen: received });
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

/// Why a remote command did not finish a transfer, in the remote's own words when it gave any.
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
        (None, true) => "the remote machine did not report the transfer's outcome".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use tilo_test_env::hang_bound;

    tilo_test_env::scenario! {
        fn auth_resolution_reads_saved_secrets_and_default_keys;

        phase resolve_auth_password_uses_the_saved_password;
        {
            match resolve_auth(false, Some("hunter2".into()), &[]).unwrap() {
                SshAuth::Password(password) => assert_eq!(password, "hunter2"),
                other @ SshAuth::Key { .. } => panic!("expected password auth, got {other:?}"),
            }
            assert!(resolve_auth(false, None, &[]).is_err(), "no password is an error");
        }

        phase resolve_auth_key_uses_the_first_existing_default;
        {
            let directory =
                tempfile::tempdir().expect("a scratch folder for the identity files");
            let present = directory.path().join("id_ecdsa");
            fs::write(&present, b"key").unwrap();
            let defaults = vec![directory.path().join("id_ed25519"), present.clone()];
            match resolve_auth(true, Some("pw".into()), &defaults).unwrap() {
                SshAuth::Key { path, passphrase } => {
                    assert_eq!(path, present);
                    assert_eq!(passphrase.as_deref(), Some("pw"));
                }
                other @ SshAuth::Password(_) => panic!("expected key auth, got {other:?}"),
            }
            assert!(
                resolve_auth(true, None, &[directory.path().join("missing")]).is_err(),
                "no identity file is an error"
            );
        }

        phase default_key_paths_are_under_dot_ssh;
        {
            let paths = default_key_paths();
            assert!(paths.iter().any(|path| path.ends_with(".ssh/id_ed25519")));
        }
    }

    use russh::server::{self, Auth, Msg, Server as _, Session};
    use russh::{Channel, ChannelId};

    /// The loopback server's tiny file system, and the transfers it is in the middle of.
    type Served = Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>;
    type Receiving = Arc<Mutex<std::collections::HashMap<ChannelId, (String, Vec<u8>)>>>;

    #[derive(Clone, Default)]
    struct TestFiles {
        files: Served,
        open: Receiving,
    }

    impl TestFiles {
        /// The bytes served for `path`, read under the lock and handed on.
        fn served(&self, path: &str) -> Option<Vec<u8>> {
            self.files.lock().unwrap().get(path).cloned()
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
            let mut echo = b"echo:".to_vec();
            echo.extend_from_slice(data);
            session.data(channel, echo)?;
            Ok(())
        }
    }

    // A key made for these tests alone; it guards nothing.
    const TEST_HOST_KEY: &str = concat!(
        "-----BEGIN OPENSSH PRIVATE KEY-----\n", // example test host key
        "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\n",
        "QyNTUxOQAAACCuFBwOt8vsblgfGgvFgrD/sFWDu+bxylWhkpDgq/FItgAAAKAv68KGL+vC\n",
        "hgAAAAtzc2gtZWQyNTUxOQAAACCuFBwOt8vsblgfGgvFgrD/sFWDu+bxylWhkpDgq/FItg\n",
        "AAAEBZipAmIHSgB3PdNuFSxUx0OMiQ8hQmxBhvFbCOREICU64UHA63y+xuWB8aC8WCsP+w\n",
        "VYO75vHKVaGSkOCr8Ui2AAAAF3Fvcy1yZW1vdGUtdGVzdC1ob3N0a2V5AQIDBAUG\n",
        "-----END OPENSSH PRIVATE KEY-----\n",
    );

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime for the loopback server")
    }

    /// A loopback server holding `by_path`'s files, on a port of its own.
    fn served_files(
        by_path: impl IntoIterator<Item = (&'static str, Vec<u8>)>,
    ) -> (u16, TestFiles) {
        let host_key = russh::keys::decode_secret_key(TEST_HOST_KEY, None).unwrap();
        let config = Arc::new(server::Config {
            inactivity_timeout: None,
            auth_rejection_time: Duration::from_millis(50),
            keys: vec![host_key],
            ..Default::default()
        });
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let files = TestFiles::default();
        for (path, bytes) in by_path {
            files.files.lock().unwrap().insert(path.into(), bytes);
        }
        let for_the_server = files.clone();
        std::thread::spawn(move || {
            runtime().block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let mut server = TestServer {
                    files: for_the_server,
                };
                let _ = server.run_on_socket(config, &listener).await;
            });
        });
        (port, files)
    }

    fn identity() -> HostKeyIdentity {
        let host_key = russh::keys::decode_secret_key(TEST_HOST_KEY, None).unwrap();
        HostKeyIdentity {
            algorithm: host_key.public_key().algorithm().as_str().to_owned(),
            fingerprint: host_key
                .public_key()
                .fingerprint(HashAlg::Sha256)
                .to_string(),
        }
    }

    struct Said(Arc<Mutex<Vec<SshStatus>>>);

    impl Said {
        fn new() -> (Self, Box<dyn Fn(SshStatus) + Send + Sync>) {
            let statuses = Arc::new(Mutex::new(Vec::new()));
            let heard = Arc::clone(&statuses);
            (
                Self(statuses),
                Box::new(move |status| {
                    heard
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(status);
                }),
            )
        }

        fn all(&self) -> Vec<SshStatus> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    fn trusted_session_on(port: u16) -> (SshSession, std::sync::mpsc::Receiver<Vec<u8>>, Said) {
        let (said, callback) = Said::new();
        let ssh = SshSession::start(
            SshConfig {
                host: "127.0.0.1".into(),
                port,
                user: "tester".into(),
                auth: SshAuth::Password("secret".into()),
                cols: 80,
                rows: 24,
                expected: Some(identity()),
            },
            callback,
        )
        .unwrap();
        let output = ssh.take_output().expect("a fresh session's output");
        tilo_test_env::wait_until("the client to reach Connected", || {
            ssh.status() == SshStatus::Connected
        });
        (ssh, output, said)
    }

    /// What a session's output holds, read until it contains `needle` or its channel closes.
    fn output_says(output: &std::sync::mpsc::Receiver<Vec<u8>>, needle: &str) -> String {
        let mut seen = Vec::new();
        loop {
            match output.recv_timeout(hang_bound()) {
                Ok(chunk) => {
                    seen.extend_from_slice(&chunk);
                    let text = String::from_utf8_lossy(&seen).into_owned();
                    if text.contains(needle) {
                        return text;
                    }
                }
                Err(error) => panic!("{needle:?} never arrived: {error}; saw {seen:?}"),
            }
        }
    }

    #[test]
    fn real_ssh_round_trip_over_loopback() {
        let (port, _files) = served_files([]);
        let (ssh, output, _said) = trusted_session_on(port);

        let banner = output_says(&output, "tilo-remote ready");
        assert!(banner.contains("tilo-remote ready"), "{banner}");

        ssh.send_input_ack(b"Z".to_vec())
            .expect("the shell takes the input");
        let echoed = output_says(&output, "echo:Z");
        assert!(echoed.contains("echo:Z"), "{echoed}");
        ssh.close();
    }

    #[test]
    fn a_first_contact_is_refused_with_the_key_it_observed_and_never_signed_in() {
        let (port, _files) = served_files([]);
        let (said, callback) = Said::new();
        let ssh = SshSession::start(
            SshConfig {
                host: "127.0.0.1".into(),
                port,
                user: "tester".into(),
                auth: SshAuth::Password("secret".into()),
                cols: 80,
                rows: 24,
                expected: None,
            },
            callback,
        )
        .unwrap();
        let _output = ssh.take_output();
        tilo_test_env::wait_until("the first contact to fail closed", || {
            matches!(ssh.status(), SshStatus::Failed { .. })
        });
        assert_eq!(
            ssh.host_key_issue(),
            Some(HostKeyIssue::FirstContact {
                observed: identity()
            }),
            "the observed key is shown for the person to check"
        );
        assert!(
            matches!(ssh.status(), SshStatus::Failed { ref why, .. } if why.contains("explicit trust"))
        );
        let statuses = said.all();
        assert!(
            statuses
                .iter()
                .all(|status| *status != SshStatus::Connected),
            "no sign-in happened: {statuses:?}"
        );
    }

    #[test]
    fn a_changed_host_key_is_refused_and_names_both_keys() {
        let (port, _files) = served_files([]);
        let (said, callback) = Said::new();
        let expected = HostKeyIdentity {
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:the-one-we-trusted".into(),
        };
        let ssh = SshSession::start(
            SshConfig {
                host: "127.0.0.1".into(),
                port,
                user: "tester".into(),
                auth: SshAuth::Password("secret".into()),
                cols: 80,
                rows: 24,
                expected: Some(expected.clone()),
            },
            callback,
        )
        .unwrap();
        let _output = ssh.take_output();
        tilo_test_env::wait_until("the changed key to fail closed", || {
            matches!(ssh.status(), SshStatus::Failed { .. })
        });
        let issue = ssh.host_key_issue().expect("the observed key");
        assert!(
            matches!(&issue, HostKeyIssue::Changed { expected: held, observed } if *held == expected && observed == &identity()),
            "{issue:?}"
        );
        assert!(
            said.all()
                .iter()
                .all(|status| *status != SshStatus::Connected)
        );
    }

    #[test]
    fn a_file_crosses_a_real_session_both_ways_and_a_missing_one_says_why() {
        let (port, files) = served_files([("remote.txt", b"from the remote machine".to_vec())]);
        let (ssh, _output, _said) = trusted_session_on(port);

        let directory = tempfile::tempdir().unwrap();
        let local = directory.path().join("local.bin");
        std::fs::write(&local, b"from this device").unwrap();
        let progress = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&progress);
        let put = ssh
            .transfer(
                TransferDirection::Put {
                    local_path: local,
                    remote_path: "upload.bin".into(),
                },
                Transfer {
                    progress: Arc::new(move |moved| {
                        recorded
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push(moved);
                    }),
                    stopped: Arc::new(AtomicBool::new(false)),
                },
            )
            .unwrap();
        assert_eq!(put, b"from this device".len() as u64);
        assert_eq!(
            files.files.lock().unwrap().get("upload.bin").cloned(),
            Some(b"from this device".to_vec())
        );
        let moves = progress.lock().unwrap().clone();
        assert_eq!(
            moves.last(),
            Some(&Moved::Measured {
                done: b"from this device".len() as u64,
                total: b"from this device".len() as u64,
            }),
            "the put reports the bytes it moved against the file's size: {moves:?}"
        );

        let fetched = directory.path().join("fetched.txt");
        let fetch = ssh
            .transfer(
                TransferDirection::Fetch {
                    remote_path: "remote.txt".into(),
                    local_path: fetched.clone(),
                },
                Transfer {
                    progress: Arc::new(|_| {}),
                    stopped: Arc::new(AtomicBool::new(false)),
                },
            )
            .unwrap();
        assert_eq!(fetch, b"from the remote machine".len() as u64);
        assert_eq!(std::fs::read(&fetched).unwrap(), b"from the remote machine");

        let missing = directory.path().join("missing.txt");
        let error = ssh
            .transfer(
                TransferDirection::Fetch {
                    remote_path: "missing.txt".into(),
                    local_path: missing.clone(),
                },
                Transfer {
                    progress: Arc::new(|_| {}),
                    stopped: Arc::new(AtomicBool::new(false)),
                },
            )
            .unwrap_err();
        assert!(error.contains("missing.txt"), "{error}");
        assert!(error.contains("No such file or directory"), "{error}");
        assert!(!missing.exists(), "a failed fetch leaves no empty file");
        ssh.close();
    }

    #[test]
    fn a_transfer_stops_when_the_person_stops_it() {
        let (port, _files) = served_files([]);
        let (ssh, _output, _said) = trusted_session_on(port);
        let directory = tempfile::tempdir().unwrap();
        let local = directory.path().join("big.bin");
        std::fs::write(&local, vec![b'x'; 4096]).unwrap();
        let stop = Arc::new(AtomicBool::new(true));
        let error = ssh
            .transfer(
                TransferDirection::Put {
                    local_path: local,
                    remote_path: "big.bin".into(),
                },
                Transfer {
                    progress: Arc::new(|_| {}),
                    stopped: Arc::clone(&stop),
                },
            )
            .unwrap_err();
        assert!(error.contains("stopped by the person"), "{error}");
        ssh.close();
    }
}
