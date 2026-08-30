//! Real SSH transport, built on `russh`.
//!
//! A live SSH session runs the async russh event loop on its own OS thread
//! (a small current-thread tokio runtime); the UI thread talks to it over
//! channels. Remote PTY output is streamed to a [`crate::vt::VtGrid`] that
//! the session surface renders; keystrokes flow the other way into the PTY.
//!
//! Nothing here is simulated: [`SshSession::connect`] opens a real TCP
//! connection, runs the SSH key exchange, authenticates with a real
//! password or private key, and requests a real interactive shell. On any
//! failure the shared [`SessionStatus`] carries the honest error — the UI
//! never falls back to a fabricated terminal.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail};
use russh::client;
use russh::keys::{PrivateKeyWithHashAlg, load_secret_key};
use russh::{ChannelMsg, Disconnect};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::session::SessionStatus;
use crate::vt::VtGrid;

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
}

/// Resolve the auth method from a lookup function (the process environment
/// in production, an in-test map in the unit tests). Password auth reads
/// `REMOTE_SSH_PASSWORD`; key auth reads `REMOTE_SSH_KEY` (falling back to
/// the usual `~/.ssh` identities) and the optional `REMOTE_SSH_KEY_PASSPHRASE`.
///
/// Split out from the environment so it can be tested without mutating
/// process-global state.
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
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .map(|n| home.join(".ssh").join(n))
        .collect()
}

/// A live SSH session handle held by the UI.
pub struct SshSession {
    grid: VtGrid,
    status: Arc<Mutex<SessionStatus>>,
    output_rx: Receiver<Vec<u8>>,
    input_tx: UnboundedSender<Vec<u8>>,
    // Kept so the worker thread isn't detached-orphaned; on drop the input
    // channel closes, which tells the worker to disconnect.
    _worker: std::thread::JoinHandle<()>,
}

impl SshSession {
    /// Start connecting. Returns immediately with a handle whose status is
    /// `Connecting`; the background thread advances it to `Connected` or
    /// `Failed(reason)`.
    pub fn connect(cfg: SshConfig) -> SshSession {
        let status = Arc::new(Mutex::new(SessionStatus::Connecting));
        let (output_tx, output_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (input_tx, input_rx) = unbounded_channel::<Vec<u8>>();
        let grid = VtGrid::new(cfg.cols as usize, cfg.rows as usize);

        let status_worker = status.clone();
        let worker = std::thread::Builder::new()
            .name("remote-ssh".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        *status_worker.lock().unwrap() =
                            SessionStatus::Failed(format!("runtime: {e}"));
                        return;
                    }
                };
                rt.block_on(async move {
                    if let Err(e) =
                        run_session(cfg, status_worker.clone(), output_tx, input_rx).await
                    {
                        let mut s = status_worker.lock().unwrap();
                        if !matches!(*s, SessionStatus::Failed(_)) {
                            *s = SessionStatus::Failed(e.to_string());
                        }
                    }
                });
            })
            .expect("spawn remote-ssh worker");

        SshSession {
            grid,
            status,
            output_rx,
            input_tx,
            _worker: worker,
        }
    }

    pub fn status(&self) -> SessionStatus {
        self.status.lock().unwrap().clone()
    }

    /// Drain any pending remote output into the grid. Returns true if the
    /// grid changed (so the UI knows to repaint).
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.output_rx.try_recv() {
                Ok(bytes) => {
                    self.grid.feed(&bytes);
                    changed = true;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        changed
    }

    pub fn grid(&self) -> &VtGrid {
        &self.grid
    }

    /// Send keystrokes to the remote PTY.
    pub fn send_input(&self, bytes: Vec<u8>) {
        let _ = self.input_tx.send(bytes);
    }
}

/// The russh client event handler. We accept the server's host key
/// trust-on-first-use — there is no `known_hosts` verification yet, which is
/// why live sessions stay gated behind `REMOTE_LIVE_SESSIONS`.
struct Handler;

impl client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// The async body: connect, authenticate, open a PTY shell, and pump bytes
/// both ways until either side closes.
async fn run_session(
    cfg: SshConfig,
    status: Arc<Mutex<SessionStatus>>,
    output_tx: std::sync::mpsc::Sender<Vec<u8>>,
    mut input_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
) -> anyhow::Result<()> {
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_hours(1)),
        ..Default::default()
    });

    let mut session = client::connect(config, (cfg.host.as_str(), cfg.port), Handler)
        .await
        .map_err(|e| anyhow!("connect: {e}"))?;

    let auth_ok = match &cfg.auth {
        SshAuth::Password(pw) => session
            .authenticate_password(cfg.user.clone(), pw.clone())
            .await?
            .success(),
        SshAuth::Key { path, passphrase } => {
            let key = load_secret_key(path, passphrase.as_deref())
                .map_err(|e| anyhow!("load key {}: {e}", path.display()))?;
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

    *status.lock().unwrap() = SessionStatus::Connected;

    let mut channel = channel;
    loop {
        tokio::select! {
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { data }) => {
                    if output_tx.send(data.to_vec()).is_err() { break; }
                }
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    if output_tx.send(data.to_vec()).is_err() { break; }
                }
                Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break,
                _ => {}
            },
            input = input_rx.recv() => match input {
                Some(bytes) => { channel.data(&bytes[..]).await?; }
                // UI dropped the sender → tear the session down cleanly.
                None => {
                    let _ = channel.eof().await;
                    break;
                }
            },
        }
    }

    let _ = session
        .disconnect(Disconnect::ByApplication, "", "en")
        .await;
    let mut s = status.lock().unwrap();
    if *s == SessionStatus::Connected {
        *s = SessionStatus::Closed;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_auth_password_reads_env() {
        let env = |k: &str| (k == "REMOTE_SSH_PASSWORD").then(|| "hunter2".to_string());
        match resolve_auth(false, env, &[]).unwrap() {
            SshAuth::Password(p) => assert_eq!(p, "hunter2"),
            other => panic!("expected password auth, got {other:?}"),
        }
    }

    #[test]
    fn resolve_auth_password_missing_is_error() {
        assert!(resolve_auth(false, |_| None, &[]).is_err());
    }

    #[test]
    fn resolve_auth_key_prefers_env_over_defaults() {
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
            other => panic!("expected key auth, got {other:?}"),
        }
    }

    #[test]
    fn default_key_paths_are_under_dot_ssh() {
        let paths = default_key_paths();
        assert!(paths.iter().any(|p| p.ends_with(".ssh/id_ed25519")));
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
            session.data(channel, b"qos-remote ready\r\n".to_vec())?;
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
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        // Stand up the server, bind an ephemeral port, and serve in the bg.
        let port = rt.block_on(async {
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
        });

        // Real client — its own thread + runtime, exactly as production.
        let mut ssh = SshSession::connect(SshConfig {
            host: "127.0.0.1".into(),
            port,
            user: "tester".into(),
            auth: SshAuth::Password("secret".into()),
            cols: 80,
            rows: 24,
        });

        // Poll to Connected.
        let mut connected = false;
        for _ in 0..200 {
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
        for _ in 0..200 {
            ssh.pump();
            if ssh.grid().lines().iter().any(|l| {
                l.cells
                    .iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .contains("qos-remote ready")
            }) {
                saw_banner = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(saw_banner, "banner never arrived in the grid");

        // Prove the upstream pipe: send a byte, expect the echo back.
        ssh.send_input(b"Z".to_vec());
        let mut saw_echo = false;
        for _ in 0..200 {
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
    }
}
