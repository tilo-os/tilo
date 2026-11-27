//! A person's server as a deployment runs it: the device daemon (`tilo-runtime`, of the
//! `tilo-daemon` package) started as a child process with a server configuration, over a home of
//! its own on this machine, and signed in with a ticket one of its person's devices printed. It
//! is reached through this machine's loopback relay ([`super::loopback`]) and answers its
//! workspaces' actions, their apps' included, as the person's device it is: the apps it was
//! started with are installed as a computer has them ([`super::apps`]), and run where it runs.
//!
//! The run builds the daemon before its tests start: t's `starts` for the package under test, or
//! the `lounge-siblings` setup of `.config/nextest.toml`; the launcher and the tilolang runtime
//! it runs apps with likewise (`view-siblings`).

use std::ffi::OsString;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget,
};
use tilo_device_identity::IdentityPaths;
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::DeviceCert;

/// The person's server, running as its daemon until this is dropped.
pub struct ServerDaemon {
    child: Option<Child>,
    home: tempfile::TempDir,
    addr: iroh::EndpointAddr,
    web: std::net::SocketAddr,
    key: SigningKey,
    cert: DeviceCert,
    /// The environment that names its apps, their launcher and runtimes' publisher.
    installed: Vec<(&'static str, OsString)>,
}

impl Drop for ServerDaemon {
    fn drop(&mut self) {
        self.stop();
    }
}

impl ServerDaemon {
    /// A machine started as its person's server and signed in with `ticket`, called `label`: its
    /// configuration keeps the workspaces placed on it, answers their apps' actions and serves
    /// its web UI on loopback, and it relays through this machine's loopback relay. Returns once
    /// it keeps and its action link answers. `apps` (directories under `apps/tl`) are installed
    /// on it, with the runtimes they run on.
    #[must_use]
    pub fn start(ticket: &str, label: &str, apps: &[&str]) -> Self {
        let home = tempfile::tempdir().expect("the server's home");
        let root = home.path();
        let installed = super::apps::install(&root.join("installed"), &root.join("data"), apps);
        // A port the kernel had free: the configuration names the web UI's address, and the
        // flows reach the server's pages there.
        let web = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .and_then(|listener| listener.local_addr())
            .expect("a free port for the web UI");
        let config = root.join("server.toml");
        std::fs::write(
            &config,
            format!(
                "identity_root = \"{}\"\n\
                 iroh_listen = \"127.0.0.1:0\"\n\
                 packet_relay_url = \"{}\"\n\
                 web_listen = \"{web}\"\n\
                 app_serving = true\n",
                root.join("identity").display(),
                super::loopback_url(),
            ),
        )
        .expect("the server's configuration");
        std::fs::create_dir_all(root.join("logs")).expect("the server's log directory");
        std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
            .create(root.join("runtime"))
            .expect("the server's runtime directory");
        let mut child = spawn(root, &config, &installed);
        let env_out = root.join("session.env");
        let native = root.join("runtime/tilo/native.sock");
        // A machine nobody signed in to answers its device actions on its native endpoint, and
        // its login among them, before it keeps anything.
        until(&mut child, root, "answer on its native endpoint", || {
            tilo_socket::connect(&native).ok().map(drop)
        });
        log_in(&native, ticket, label, root);
        let addr = until(&mut child, root, "keep, once signed in", || named(&env_out));
        let identity = IdentityPaths::under(root.join("identity"));
        let key = tilo_device_identity::persist::read_device_key(&identity.device_privkey)
            .expect("the key the server's login filed");
        let cert = tilo_device_identity::persist::read_owner_cert(&identity)
            .expect("the certificate the server's login filed")
            .expect("the person's certificate");
        Self {
            child: Some(child),
            home,
            addr,
            web,
            key,
            cert,
            installed,
        }
    }

    /// Where the server's endpoint answers, naming the loopback relay it is reached through.
    #[must_use]
    pub fn node_addr(&self) -> iroh::EndpointAddr {
        self.addr.clone()
    }

    /// The server's device key, which its login made.
    #[must_use]
    pub fn key(&self) -> &SigningKey {
        &self.key
    }

    /// The server's certificate, which its person's root signed when it logged in.
    #[must_use]
    pub fn cert(&self) -> &DeviceCert {
        &self.cert
    }

    /// Where the server serves its web UI.
    #[must_use]
    pub fn web(&self) -> std::net::SocketAddr {
        self.web
    }

    /// The daemon's own home: its identity, configuration and state are below it.
    #[must_use]
    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// The daemon's native endpoint, which its own device's connections reach it at.
    #[must_use]
    pub fn native(&self) -> PathBuf {
        self.home.path().join("runtime/tilo/native.sock")
    }

    /// What the daemon logged, for a failure that says why.
    #[must_use]
    pub fn log(&self) -> String {
        said(self.home.path())
    }

    /// The server stopped, as a machine that goes away is: nothing answers at its address.
    pub fn stop(&mut self) {
        drop(
            self.child
                .take()
                .map(|mut child| child.kill().and_then(|()| child.wait())),
        );
    }

    /// The server started again over the home it keeps, as a machine that comes back is: the same
    /// device, answering at the address it names once it keeps again.
    pub fn start_again(&mut self) {
        let root = self.home.path();
        let env_out = root.join("session.env");
        std::fs::remove_file(&env_out).expect("the address the server named before");
        let mut child = spawn(root, &root.join("server.toml"), &self.installed);
        self.addr = until(&mut child, root, "keep again", || named(&env_out));
        self.child = Some(child);
    }
}

/// The address the daemon named in `env_out` once it keeps.
fn named(env_out: &Path) -> Option<iroh::EndpointAddr> {
    let line = std::fs::read_to_string(env_out).ok()?;
    let value = line.trim().strip_prefix("TILO_COLLAB_HOST='")?;
    serde_json::from_str(value.strip_suffix('\'')?).ok()
}

/// The daemon over `root`, as its server configuration `config` starts it with the apps whose
/// environment is `installed`, logging below it.
fn spawn(root: &Path, config: &Path, installed: &[(&'static str, OsString)]) -> Child {
    let log = std::fs::File::create(root.join("logs/daemon.log")).expect("the daemon's log");
    let mut command = Command::new(tilo_test_env::sibling_binary("tilo-daemon", "tilo-runtime"));
    command
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .env("TILO_IDENTITY_ROOT", root.join("identity"))
        .env("TILO_SERVER_CONFIG", config)
        .env("TILO_SERVER_SESSION_ENV_OUT", root.join("session.env"))
        .env("TILO_SECRETS_DIR", root.join("secrets"))
        .env("TILO_IDENTITY_FILE_SECRETS", "1")
        .envs(installed.iter().map(|(name, value)| (*name, value)))
        .env(super::loopback_relay().0, super::loopback_relay().1)
        .env(tilo_endpoint_reachability::NETWORKS_ENV, "127.0.0.1")
        .env(
            "RUST_LOG",
            std::env::var("TILO_SERVER_DAEMON_LOG").unwrap_or_else(|_| "info".to_owned()),
        )
        .env("NO_COLOR", "1")
        .env_remove("TILO_RUNTIME_PROFILE")
        .env_remove("TILO_DEVICE_PORT")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .stdin(Stdio::null())
        .stdout(log.try_clone().expect("the log's own handle"))
        .stderr(log);
    command.spawn_locked().expect("the server's daemon starts")
}

/// What `probe` answers once it answers, while the daemon below `root` runs.
fn until<T>(child: &mut Child, root: &Path, what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            panic!(
                "the server's daemon ended ({status}) before it would {what}:\n{}",
                said(root)
            );
        }
        if let Some(answer) = probe() {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "the server's daemon did not {what}:\n{}",
            said(root)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Sign the daemon at `native` in with `ticket`, as `tilo login` does on its person's own
/// connection: the call, then its state asked for until it ends.
fn log_in(native: &Path, ticket: &str, label: &str, root: &Path) {
    let invocation = tilo_actions::InvocationId::new();
    let mut argument = serde_json::Map::new();
    argument.insert("ticket".into(), ticket.into());
    argument.insert("agent".into(), "tilo".into());
    argument.insert("label".into(), label.into());
    let request = RemoteActionMessage::Invoke(InvokeRequest {
        asks: false,
        invocation,
        target: RemoteActionTarget {
            provider: tilo_device_identity::login::PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(
                tilo_device_identity::login::RESOURCE,
                tilo_device_identity::login::LOG_IN,
            ),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: Some(55_000),
        appearance: None,
        signed: None,
    });
    let stream = tilo_socket::connect(native).expect("the daemon answers");
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .expect("a read bound");
    let mut writer = stream.try_clone().expect("the stream's own handle");
    let mut send = |message: RemoteActionMessage| {
        writer
            .write_all(&RemoteActionFrame::new(message).encode().expect("a frame"))
            .and_then(|()| writer.write_all(b"\n"))
            .expect("the call is sent");
    };
    let mut reader = BufReader::new(stream);
    let mut answer = || {
        let mut line = String::new();
        reader.read_line(&mut line).expect("the daemon answers");
        RemoteActionFrame::decode(line.trim_end().as_bytes())
            .expect("the answer is a frame")
            .message
    };
    send(request);
    let mut status = match answer() {
        RemoteActionMessage::Receipt(receipt) => receipt.status,
        other => panic!("the login is answered with a receipt, not {other:?}"),
    };
    // A login answered at once, as a machine no person's certificate names yet answers it, is
    // followed until it ends.
    let deadline = tilo_test_env::hang_deadline();
    let mut correlation = 0_u64;
    while matches!(status, ReceiptStatus::Accepted) {
        correlation += 1;
        send(RemoteActionMessage::Observe {
            correlation,
            invocation,
            wait_ms: Some(55_000),
            seen: None,
        });
        let state = loop {
            match answer() {
                RemoteActionMessage::Update {
                    state,
                    correlation: Some(answered),
                    ..
                } if answered == correlation => break state,
                RemoteActionMessage::Update { .. } => {}
                other => panic!("an ask is answered with an update, not {other:?}"),
            }
        };
        let state = state.expect("the daemon keeps the login it took");
        if state.is_terminal() {
            status = state.receipt().status;
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the server's login did not end: {state:?}\n{}",
            said(root)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        matches!(status, ReceiptStatus::Completed { .. }),
        "the server logs in with its person's ticket: {status:?}\n{}",
        said(root)
    );
}

/// What the daemon below `root` logged.
fn said(root: &Path) -> String {
    std::fs::read_to_string(root.join("logs/daemon.log")).unwrap_or_default()
}
