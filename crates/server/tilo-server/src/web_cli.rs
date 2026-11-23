//! The browser listener from the command line (feature `web`): `run --web`
//! for a configured host, and `dev` for a throwaway local host that
//! certifies every browser that asks.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use ed25519_dalek::SigningKey;
use std::sync::Arc;
use tilo_server::daemon::{Server, ServerIdentity};
use tilo_server::web::{TlsSource, WebConfig, WebHandle};
use tilo_server_protocol::operator::{self, OperatorCommand, OperatorEvent, RefusalReason};
use tilo_user_identity::{
    CertificationPolicy, DeviceCapabilities, DeviceCert, RevocationList, issue_device_cert,
    sign_device_cert,
};

/// How the listener gets its certificate.
#[derive(Clone, Debug)]
pub enum TlsArg {
    /// A self-signed development certificate, kept under the state root.
    Dev,
    /// PEM certificate chain and private key.
    Files { chain: PathBuf, key: PathBuf },
}

impl std::str::FromStr for TlsArg {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text == "dev" {
            return Ok(Self::Dev);
        }
        let files = text
            .strip_prefix("files:")
            .ok_or("expected dev or files:<chain.pem>,<key.pem>")?;
        let (chain, key) = files
            .split_once(',')
            .ok_or("expected files:<chain.pem>,<key.pem>")?;
        Ok(Self::Files {
            chain: chain.into(),
            key: key.into(),
        })
    }
}

/// Browser listener flags shared by `run` and `dev`.
#[derive(clap::Args, Clone, Debug)]
pub struct WebArgs {
    /// Serve browsers (HTTPS on TCP) at this address.
    #[arg(long, value_name = "ADDR")]
    pub web: Option<SocketAddr>,
    /// A page origin allowed to call the host cross-origin; repeatable.
    #[arg(long = "allowed-origin", value_name = "ORIGIN")]
    pub allowed_origins: Vec<String>,
    /// `dev` (self-signed) or `files:<chain.pem>,<key.pem>`.
    #[arg(long, default_value = "dev", value_name = "SOURCE")]
    pub tls: TlsArg,
    /// The host name browsers reach this listener by (the certificate's
    /// name), on an unspecified address: the origin the host announces, and
    /// with `--tls dev` the development certificate's first name (a node
    /// reached by its LAN address).
    #[arg(long = "public-host", value_name = "NAME")]
    pub public_host: Option<String>,
    /// Also serve the HTTPS routes as plain HTTP at this loopback address,
    /// so a local page needs no certificate exception.
    #[arg(long = "web-plain", value_name = "ADDR")]
    pub web_plain: Option<SocketAddr>,
    /// The `https://` origin a TLS-terminating proxy publishes
    /// `--web-plain` at (a public workspace behind an ingress); it lets
    /// `--web-plain` bind a non-loopback address.
    #[arg(long = "public-origin", value_name = "ORIGIN", requires = "web_plain")]
    pub public_origin: Option<String>,
    /// Another origin a proxy publishes this server at, beside
    /// `--public-origin` (a front that hands a second name on to the same
    /// listener); repeatable. An agent's setup code is made for a page that
    /// came by any of them, and for no name the server was not told here.
    #[arg(
        long = "public-origin-also",
        value_name = "ORIGIN",
        requires = "public_origin"
    )]
    pub public_origins_also: Vec<String>,
    /// Take WebTransport sessions, how pages reach this host, at this UDP
    /// address instead of beside the HTTPS listener: for a deployment whose
    /// HTTPS arrives through a proxy that carries no UDP.
    #[arg(long = "link-listen", value_name = "ADDR", requires = "link_origin")]
    pub link_listen: Option<SocketAddr>,
    /// The `https://` origin pages open those sessions at; its name must
    /// lead to `--link-listen` and be on the host's certificate. An origin
    /// ending in `:0` takes the port the listener bound, which the host
    /// publishes as the link pages dial.
    #[arg(long = "link-origin", value_name = "ORIGIN", requires = "link_listen")]
    pub link_origin: Option<String>,
    /// Run this host's own iroh packet relay at this address, and have its
    /// pages reach it through it: a page has no UDP socket, so it reaches a
    /// host only through a relay. Without it the host uses the public relays.
    #[arg(long = "packet-relay", value_name = "ADDR")]
    pub packet_relay: Option<SocketAddr>,
    /// The `https://` origin an ingress publishes the packet relay at, in front
    /// of `--packet-relay`: the URL the host advertises, which pages dial. By
    /// default that is the bind address's own `http://`. Without
    /// `--packet-relay`, the server is reached through the packet relay at
    /// this URL and has none of its own: how a server behind a router nobody
    /// dials into is reached from anywhere.
    #[arg(long = "packet-relay-url", value_name = "URL")]
    pub packet_relay_url: Option<String>,
    /// The web entry (ending in `/`, e.g. `https://app.example/`) where the
    /// Tilo app opens this host's links: announced in catalogue locators,
    /// so `share-link` and bots mint links under it. Overrides the config's
    /// `web_entry`.
    #[arg(long = "web-entry", value_name = "URL")]
    pub web_entry: Option<String>,
    /// Without a device certificate yet, create the device key, print its
    /// public half and serve only `/peer/v1/install` on `--web` until a
    /// root-signed host certificate for it arrives with the token in this
    /// file; then start as usual.
    #[arg(long = "install-token-file", value_name = "PATH", requires = "web")]
    pub install_token_file: Option<PathBuf>,
    /// Serve the members' pages a fetch proxy at `/peer/v1/fetch`, so apps in
    /// a page read sites that send no CORS headers (feeds, video directories,
    /// calendar subscriptions) through this host. Only public addresses are
    /// reached; requests, bytes and destinations are rate-limited, and what
    /// each visitor and each workspace receives a day is budgeted
    /// (`TILO_SERVER_FETCH_PROXY_*`, see the workspace service's README).
    #[arg(long = "fetch-proxy")]
    pub fetch_proxy: bool,
    /// The key Apple gave for its push service (the `.p8` file, which its
    /// owner alone may read, mode 600): with it, what waits for a person
    /// reaches the Tilo apps of theirs that are closed. Without it no
    /// closed app is told. It needs the three flags after it.
    #[arg(long = "apple-push-key", value_name = "PATH")]
    pub apple_push_key: Option<PathBuf>,
    /// That key's id, as Apple's developer account lists it.
    #[arg(long = "apple-push-key-id", value_name = "ID")]
    pub apple_push_key_id: Option<String>,
    /// The id of the team the key and the apps belong to.
    #[arg(long = "apple-push-team", value_name = "ID")]
    pub apple_push_team: Option<String>,
    /// An app pushes are sent for, by its bundle identifier; repeatable.
    #[arg(long = "apple-push-topic", value_name = "APP")]
    pub apple_push_topics: Vec<String>,
    /// The device key (hex) of the session beside this node, the shell that
    /// runs the person's apps: pages learn it on `/node` to open apps there.
    /// While the node waits for its certificate, its root certifies the
    /// session with it, as one of the person's devices, and the certificate
    /// is written to `--session-identity-root`.
    #[arg(long = "session-device", value_name = "HEX", requires = "web")]
    pub session_device: Option<String>,
    /// The session's identity tree (its `TILO_IDENTITY_ROOT`), holding its
    /// device key: install writes its certificate and the person's user key
    /// there, so the session starts as an enrolled device of that person.
    #[arg(
        long = "session-identity-root",
        value_name = "DIR",
        requires_all = ["session_device", "install_token_file"]
    )]
    pub session_identity_root: Option<PathBuf>,
    /// Once serving, write this server's endpoint for the session beside it
    /// as `TILO_COLLAB_HOST='<endpoint JSON>'`, a systemd `EnvironmentFile`:
    /// the session dials its node without anyone copying the address.
    #[arg(
        long = "session-env-out",
        value_name = "PATH",
        requires = "session_device"
    )]
    pub session_env_out: Option<PathBuf>,
    #[cfg(feature = "agents")]
    #[command(flatten)]
    pub agents: AgentArgs,
}

/// Agents on the host (feature `agents`).
#[cfg(feature = "agents")]
#[derive(clap::Args, Clone, Debug)]
pub struct AgentArgs {
    /// Serve workspace resources to pages and host the workspace's roster agents:
    /// the host runs an action of a workspace record for whoever the workspace's
    /// policy lets ask, such as a member linking their own account on the
    /// workspace's Discord from their profile card.
    #[arg(long)]
    pub workspace_documents: bool,
}

#[cfg(feature = "agents")]
impl AgentArgs {
    fn options(
        &self,
        state_dir: PathBuf,
    ) -> anyhow::Result<Option<tilo_server::agents::AgentOptions>> {
        if !self.workspace_documents {
            return Ok(None);
        }
        Ok(Some(tilo_server::agents::AgentOptions {
            state_dir,
            catalogue: None,
            shell: None,
            browsers: tilo_server::hosted_browser::HostedBrowsers::from_env()
                .map_err(anyhow::Error::msg)?
                .map(std::sync::Arc::new),
        }))
    }
}

/// The server, with its roster agents when `--workspace-documents` asked for them.
pub struct HostedServer {
    pub server: Server,
    #[cfg(feature = "agents")]
    agents: Option<tilo_agent_session::AgentService>,
    /// The agents it runs for the workspaces it governs.
    #[cfg(feature = "agents")]
    host_agents: Option<tokio::task::JoinHandle<()>>,
    /// What lets those agents' turns end before it stops.
    #[cfg(feature = "agents")]
    draining: Option<tilo_workspace_service::agents::Draining>,
    /// Its state's lock, released once it stopped.
    _lock: tilo_server::state_lock::StateLock,
}

/// How long a stopping server waits for its agents' turns to end. A turn this cuts off says so in
/// its reply; one that ends in time is never cut off by a restart.
#[cfg(feature = "agents")]
const TURN_DRAIN: std::time::Duration = std::time::Duration::from_secs(5 * 60);

impl HostedServer {
    /// Begin no agent turn any more and wait until the turns going ended, at most
    /// [`TURN_DRAIN`]: the first thing a server that is stopping does, while everything a turn
    /// works with is still up.
    pub async fn finish_turns(&self) {
        #[cfg(feature = "agents")]
        if let Some(draining) = &self.draining {
            draining.finish(TURN_DRAIN).await;
        }
    }

    /// Stop the roster agents (they resume on the next start), then the server.
    pub async fn shutdown(self) {
        self.finish_turns().await;
        #[cfg(feature = "agents")]
        if let Some(agents) = self.agents {
            agents.shutdown();
        }
        #[cfg(feature = "agents")]
        if let Some(host_agents) = self.host_agents {
            host_agents.abort();
        }
        self.server.shutdown().await;
    }
}

/// `--web-entry`, when given, is the entry the server's catalogue announces.
fn announce_web_entry(cfg: &mut tilo_server::ServerConfig, args: &WebArgs) {
    if let Some(entry) = &args.web_entry {
        cfg.web_entry = Some(entry.clone());
    }
}

/// Start the server; with `--workspace-documents`, serving records, events and roster agents,
/// their grants kept under `state_root/agents`.
///
/// # Errors
/// The server did not start, or the agent state could not be read.
#[cfg_attr(
    not(feature = "agents"),
    allow(unused_variables, reason = "agents only")
)]
pub async fn start_server(
    mut cfg: tilo_server::ServerConfig,
    identity: ServerIdentity,
    args: &WebArgs,
    state_root: &Path,
) -> anyhow::Result<HostedServer> {
    if let Some(max) = env_count("TILO_SERVER_MAX_WORKSPACE_GRANTS") {
        cfg.max_workspace_grants = Some(max);
    }
    announce_web_entry(&mut cfg, args);
    cfg.packet_relay = cfg.packet_relay.or(args.packet_relay);
    cfg.packet_relay_url = cfg
        .packet_relay_url
        .or_else(|| args.packet_relay_url.clone());
    let lock =
        tilo_server::state_lock::StateLock::acquire(&cfg.state_root).map_err(anyhow::Error::msg)?;
    #[cfg(feature = "agents")]
    let options = args.agents.options(state_root.join("agents"))?;
    #[cfg(feature = "agents")]
    if let Some(mut options) = options {
        let shells = tilo_server::agents::host_shells(state_root.join("shells"))
            .map_err(anyhow::Error::msg)?;
        options.shell = Some(shells);
        let browsers = options.browsers.clone();
        let (server, agents, roster) =
            tilo_server::agents::start_workspace_documents(cfg, identity, options)
                .await
                .context("start the server with workspace documents and roster agents")?;
        let (draining, host_agents) = start_roster(&server, roster, browsers);
        return Ok(HostedServer {
            server,
            agents: Some(agents),
            host_agents,
            draining,
            _lock: lock,
        });
    }
    let server = Server::start(cfg, identity)
        .await
        .context("start the server")?;
    Ok(HostedServer {
        server,
        #[cfg(feature = "agents")]
        agents: None,
        #[cfg(feature = "agents")]
        host_agents: None,
        #[cfg(feature = "agents")]
        draining: None,
        _lock: lock,
    })
}

#[cfg(feature = "agents")]
fn start_roster(
    server: &Server,
    roster: tilo_server::agents::RosterServices,
    browsers: Option<std::sync::Arc<tilo_server::hosted_browser::HostedBrowsers>>,
) -> (
    Option<tilo_workspace_service::agents::Draining>,
    Option<tokio::task::JoinHandle<()>>,
) {
    // The agents each workspace made to run here work around the clock, on the model their workspace linked.
    // The devices this server placed itself: no agent reads as one of them for a person.
    let placed = server.environments();
    let started = server.workspaces().cloned().map(|workspaces| {
        // What is judged for a person is counted on their assistant's day,
        // and a question that waits for them reaches their log.
        tilo_workspace_service::agents::telling::start(&workspaces);
        tracing::info!(
            "the workspaces' agents run on this host, each on the model its workspace linked"
        );
        let agents = tilo_workspace_service::agents::ServerAgents::with_sessions(
            workspaces,
            roster.actions,
            std::sync::Arc::new(server.agent_link()),
            roster.sessions,
        )
        .with_events(roster.events)
        .with_initiative(roster.initiative)
        .placing(browsers.map(|browsers| {
            browsers as std::sync::Arc<dyn tilo_workspace_service::agents::windows::Windows>
        }))
        .knowing_placed(placed);
        // Coding agents' harnesses run programs on the computer that starts them: only a
        // server whose operator lets its workspaces' programs run on its own machine starts
        // them here, as it starts their Terminal's programs.
        #[cfg(unix)]
        let agents = if ush::machine_programs::on_machine_from_env() {
            agents.with_coding(tilo_workspace_service::agents::coding::Coders::here())
        } else {
            agents
        };
        (agents.draining(), tokio::spawn(agents.run()))
    });
    started.unzip()
}

/// Hand `server` the key for Apple's push service that `args` name, where
/// they name one: what its people's closed apps are told through.
#[cfg(feature = "agents")]
fn name_apple_push(server: &Server, args: &WebArgs) {
    let Some(key_file) = args.apple_push_key.clone() else {
        return;
    };
    server.set_apple_push(tilo_server::apple_push::ApplePushSetup {
        key_file,
        key_id: args.apple_push_key_id.clone().unwrap_or_default(),
        team_id: args.apple_push_team.clone().unwrap_or_default(),
        topics: args.apple_push_topics.clone(),
    });
}

/// Without its agents this server tells nobody what waits for them, so a
/// key for Apple's push service is named in vain, and it says so.
#[cfg(not(feature = "agents"))]
fn name_apple_push(_server: &Server, args: &WebArgs) {
    if args.apple_push_key.is_some() {
        tracing::warn!(
            "this server tells no closed app: the key for Apple's push service is not used"
        );
    }
}

pub(crate) fn web_config(args: &WebArgs, listen: SocketAddr, state_root: &Path) -> WebConfig {
    let tls = match &args.tls {
        TlsArg::Dev => {
            // The names pages may dial this host by: the name it publishes
            // itself under and loopback.
            let mut hosts: Vec<String> = Vec::new();
            let named = args
                .public_host
                .clone()
                .into_iter()
                .chain(["localhost".to_string(), listen.ip().to_string()]);
            for host in named {
                if !hosts.iter().any(|known| known.eq_ignore_ascii_case(&host)) {
                    hosts.push(host);
                }
            }
            TlsSource::Development {
                hosts,
                store: Some(state_root.join("web-dev-identity")),
            }
        }
        TlsArg::Files { chain, key } => TlsSource::Files {
            certificate_chain: chain.clone(),
            private_key: key.clone(),
            host: args.public_host.clone(),
        },
    };
    WebConfig {
        listen,
        tls,
        allowed_origins: args.allowed_origins.clone(),
        plain_listen: args.web_plain,
        public_origin: args.public_origin.clone(),
        fetch_proxy: args.fetch_proxy.then(|| tilo_server::web::FetchProxySetup {
            config: fetch_proxy_config(),
            metrics_file: Some(
                tilo_server_protocol::metrics::workspace_textfile_dir(state_root)
                    .join("fetch-proxy.prom"),
            ),
        }),
        // The entry's wait list is the workspace's own: whoever starts one sets it
        // on the config it gives the listener.
        wait_list: Arc::default(),
        link: args
            .link_listen
            .zip(args.link_origin.clone())
            .map(|(listen, origin)| tilo_server::web::LinkListen { listen, origin }),
        // The site this host belongs to, which it already announces as the
        // web entry: a link opened at the host's own address is sent there.
        site: args.web_entry.clone(),
    }
}

/// The fetch proxy's bounds, its rates and budgets raised or lowered from
/// the environment for a host whose visitors share addresses or fetch more.
fn fetch_proxy_config() -> tilo_net::proxy::host::Config {
    use tilo_net::proxy::host::{Budgets, Config, Rate, RateLimits};
    let defaults = RateLimits::default();
    let budget_defaults = Budgets::default();
    // A budget in MiB; 0 turns it off.
    let budget = |name: &str, default: Option<u64>| {
        env_count(name).map_or(default, |mib| {
            (mib > 0).then(|| {
                u64::try_from(mib)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(1024 * 1024)
            })
        })
    };
    let budgets = Budgets {
        window: env_count("TILO_SERVER_FETCH_PROXY_BUDGET_HOURS")
            .filter(|hours| *hours > 0)
            .map_or(budget_defaults.window, |hours| {
                Duration::from_secs(
                    u64::try_from(hours)
                        .unwrap_or(u64::MAX)
                        .saturating_mul(3600),
                )
            }),
        visitor_bytes: budget(
            "TILO_SERVER_FETCH_PROXY_VISITOR_MIB",
            budget_defaults.visitor_bytes,
        ),
        workspace_bytes: budget(
            "TILO_SERVER_FETCH_PROXY_WORKSPACE_MIB",
            budget_defaults.workspace_bytes,
        ),
    };
    #[allow(
        clippy::cast_precision_loss,
        reason = "operator-set rates, far below 2^52"
    )]
    let rate = |name: &str, burst_of: fn(f64) -> f64, default: Rate| {
        env_count(name).map_or(default, |per_minute| {
            let per_minute = per_minute as f64;
            Rate {
                burst: burst_of(per_minute).max(1.0),
                per_minute,
            }
        })
    };
    #[allow(
        clippy::cast_precision_loss,
        reason = "operator-set rates, far below 2^52"
    )]
    let device_bytes = env_count("TILO_SERVER_FETCH_PROXY_DEVICE_MIB_PER_HOUR").map_or(
        defaults.device_bytes,
        |mib_per_hour| {
            let per_hour = mib_per_hour as f64 * 1024.0 * 1024.0;
            Rate {
                burst: per_hour.min(defaults.device_bytes.burst).max(1.0),
                per_minute: per_hour / 60.0,
            }
        },
    );
    Config {
        limits: RateLimits {
            device_requests: rate(
                "TILO_SERVER_FETCH_PROXY_DEVICE_PER_MIN",
                |per_minute| per_minute,
                defaults.device_requests,
            ),
            device_bytes,
            destination_requests: rate(
                "TILO_SERVER_FETCH_PROXY_DESTINATION_PER_MIN",
                |per_minute| per_minute * 0.4,
                defaults.destination_requests,
            ),
            device_in_flight: env_count("TILO_SERVER_FETCH_PROXY_DEVICE_IN_FLIGHT")
                .unwrap_or(defaults.device_in_flight),
            in_flight: env_count("TILO_SERVER_FETCH_PROXY_IN_FLIGHT").unwrap_or(defaults.in_flight),
        },
        budgets,
        ..Config::default()
    }
}

/// A count from the environment, for an operator raising a public host's
/// caps without a config file.
fn env_count(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// Start the listener `run --web` asked for, if any.
///
/// # Errors
/// The listener cannot bind or load its certificate.
/// Before the server starts: when `--install-token-file` is given and the
/// identity under `identity_root` has no certificate yet, wait for one on
/// the browser listener. Prints `{"event": "awaiting_certificate",
/// "devicePublicKey": …}` while waiting.
///
/// # Errors
///
/// The token or device key cannot be read, or the listener fails.
pub async fn await_certificate(
    identity_root: &Path,
    args: &WebArgs,
    state_root: &Path,
) -> anyhow::Result<()> {
    let (Some(token_file), Some(listen)) = (&args.install_token_file, args.web) else {
        return Ok(());
    };
    let paths = tilo_device_identity::IdentityPaths::under(identity_root);
    if paths.device_cert.is_file() {
        return Ok(());
    }
    let token = std::fs::read_to_string(token_file)
        .with_context(|| format!("read {}", token_file.display()))?;
    let (device, _) =
        tilo_device_identity::load_or_create_device_key(&paths).context("create the device key")?;
    println!(
        "{}",
        serde_json::json!({
            "event": "awaiting_certificate",
            "devicePublicKey": hex::encode(device.verifying_key().to_bytes()),
        })
    );
    let session = match (session_device(args)?, &args.session_identity_root) {
        (Some(public_key), Some(root)) => {
            let paths = tilo_device_identity::IdentityPaths::under(root);
            Some(tilo_server::web::install::SessionDevice {
                public_key,
                cert_out: paths.device_cert,
                user_key_out: Some(paths.user_pubkey),
            })
        }
        _ => None,
    };
    let cert = tilo_server::web::install::await_certificate(
        &web_config(args, listen, state_root),
        paths,
        &token,
        session,
    )
    .await
    .context("wait for the device certificate")?;
    tracing::info!(
        user = %hex::encode(cert.user_public_key),
        "device certificate installed"
    );
    Ok(())
}

/// `--session-device`, decoded.
fn session_device(args: &WebArgs) -> anyhow::Result<Option<[u8; 32]>> {
    args.session_device
        .as_deref()
        .map(|device| {
            hex::decode(device.trim())
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .context("--session-device must be a 32-byte hex public key")
        })
        .transpose()
}

pub async fn serve(
    server: &Server,
    args: &WebArgs,
    state_root: &Path,
) -> anyhow::Result<Option<WebHandle>> {
    let Some(listen) = args.web else {
        return Ok(None);
    };
    let device = session_device(args)?;
    if let Some(device) = device {
        server.set_node_session(device);
    }
    if let Some(path) = &args.session_env_out {
        let endpoint =
            serde_json::to_string(&server.node_addr()).context("encode the server's endpoint")?;
        let line = format!("TILO_COLLAB_HOST='{endpoint}'\n");
        tilo_files::native::atomic::write_with_mode(path, line.as_bytes(), 0o644)
            .with_context(|| format!("write {}", path.display()))?;
    }
    name_apple_push(server, args);
    let web = server
        .serve_web_also_at(
            web_config(args, listen, state_root),
            args.public_origins_also.clone(),
        )
        .await
        .context("start the browser listener")?;
    tracing::info!(origin = %web.origin, allowed = ?args.allowed_origins, "serving browsers");
    Ok(Some(web))
}

/// Three seeds: user root, host device, approver device.
fn dev_identity(state: &Path) -> anyhow::Result<[SigningKey; 3]> {
    let path = state.join("dev-identity.bin");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let seeds: Vec<u8> = (0..3).flat_map(|_| rand::random::<[u8; 32]>()).collect();
            tilo_files::native::atomic::write_with_mode(&path, &seeds, 0o600)
                .context("save the dev identity")?;
            seeds
        }
        Err(error) => return Err(error).context("read the dev identity"),
    };
    let keys: Vec<SigningKey> = bytes
        .chunks_exact(32)
        .filter_map(|chunk| <[u8; 32]>::try_from(chunk).ok())
        .map(|seed| SigningKey::from_bytes(&seed))
        .collect();
    <[SigningKey; 3]>::try_from(keys)
        .map_err(|_| anyhow::anyhow!("{} is not a dev identity; delete it", path.display()))
}

pub(crate) fn enrol(
    root: &SigningKey,
    device: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> anyhow::Result<DeviceCert> {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).context("sign a dev device certificate")?;
    Ok(cert)
}

/// `tilo-server workspace`: the subcommand keeps the name deployed hosts and
/// ops scripts run.
#[derive(clap::Args, Clone, Debug)]
pub struct WorkspaceArgs {
    #[command(flatten)]
    pub web: WebArgs,
    /// The workspace's throwaway user root, host device and documents. The
    /// operator removes it when the workspace ends.
    #[arg(long, value_name = "DIR")]
    pub state: PathBuf,
    #[cfg(feature = "agents")]
    #[command(flatten)]
    pub persistent: crate::persistent_workspace_cli::PersistentArgs,
}

/// One operator line from stdin, or why it could not be read.
type OperatorLine = Result<OperatorCommand, String>;

/// Operator lines from stdin, read on their own thread: a blocking read
/// must never hold up the host.
pub(crate) fn operator_lines() -> anyhow::Result<tokio::sync::mpsc::Receiver<OperatorLine>> {
    operator_lines_from(std::io::stdin())
}

/// [`OperatorCommand`]s from `input`, one per line. A line that does not
/// decode is passed on as an error and ends the reading, so the workspace stops
/// instead of skipping it; the end of `input` closes the channel, which a
/// short-lived workspace ends with and a persistent one keeps serving through.
fn operator_lines_from(
    input: impl std::io::Read + Send + 'static,
) -> anyhow::Result<tokio::sync::mpsc::Receiver<OperatorLine>> {
    use std::io::BufRead;
    let (sender, receiver) = tokio::sync::mpsc::channel(16);
    std::thread::Builder::new()
        .name("tilo-workspace-operator".into())
        .spawn(move || {
            for line in std::io::BufReader::new(input).lines() {
                let Ok(line) = line else { break };
                let command = OperatorCommand::from_line(&line).map_err(|error| {
                    let shown: String = line.chars().take(120).collect();
                    format!("{error}: {shown:?}")
                });
                let malformed = command.is_err();
                if sender.blocking_send(command).is_err() || malformed {
                    return;
                }
            }
        })
        .context("spawn the operator reader")?;
    Ok(receiver)
}

/// What the workspace approver does with one admitted pairing code.
#[derive(Debug, PartialEq, Eq)]
enum Admission {
    /// Exactly one pending browser request displays the code: certify it.
    Certify(Box<tilo_server::certify::PendingCertification>),
    /// Certify nobody for the code: deny these requests and report why.
    Refuse {
        pairing_code: String,
        reason: RefusalReason,
        deny: Vec<[u8; 32]>,
    },
}

/// The pairing codes the operator admitted, each held until one pending
/// request matches it or `wait` passes.
struct Admissions {
    wait: Duration,
    codes: std::collections::BTreeMap<String, tokio::time::Instant>,
}

impl Admissions {
    const fn new(wait: Duration) -> Self {
        Self {
            wait,
            codes: std::collections::BTreeMap::new(),
        }
    }

    fn admit(&mut self, pairing_code: String, now: tokio::time::Instant) {
        self.codes.insert(pairing_code, now);
    }

    /// What to do now, given the host's pending requests and the digests of
    /// those that are agent grants. A code that one browser request displays
    /// certifies it; a code that several requests display certifies none and
    /// denies them all, since the operator cannot tell which one the guest
    /// meant; a code that only an agent grant displays waits, since agents
    /// are never approved here; a code nobody displayed within `wait` is
    /// refused.
    fn decide(
        &mut self,
        pending: &[tilo_server::certify::PendingCertification],
        agents: &[[u8; 32]],
        now: tokio::time::Instant,
    ) -> Vec<Admission> {
        let wait = self.wait;
        let mut decisions = Vec::new();
        self.codes.retain(|code, admitted_at| {
            let matching: Vec<_> = pending
                .iter()
                .filter(|request| request.pairing_code == *code)
                .collect();
            let refuse = |reason, deny| Admission::Refuse {
                pairing_code: code.clone(),
                reason,
                deny,
            };
            match matching.as_slice() {
                [one] if !agents.contains(&one.request_digest) => {
                    decisions.push(Admission::Certify(Box::new((*one).clone())));
                    false
                }
                [_, _, ..] => {
                    let deny = matching
                        .iter()
                        .map(|request| request.request_digest)
                        .collect();
                    decisions.push(refuse(RefusalReason::AmbiguousCode, deny));
                    false
                }
                _ if now.saturating_duration_since(*admitted_at) >= wait => {
                    decisions.push(refuse(RefusalReason::NoMatchingRequest, Vec::new()));
                    false
                }
                _ => true,
            }
        });
        decisions
    }
}

/// Certify, as the workspace's user root, exactly the pending browsers whose
/// pairing codes the operator admitted, each once, and `report` the outcome
/// of every admitted code. Agent grants are never approved. A malformed
/// operator line is an error.
async fn approve_admitted(
    mut approver: tilo_server_client::Approver,
    root: &SigningKey,
    mut operator: tokio::sync::mpsc::Receiver<OperatorLine>,
    mut admissions: Admissions,
    mut report: impl FnMut(OperatorEvent),
) -> anyhow::Result<()> {
    loop {
        if !admissions.codes.is_empty() {
            let agents: Vec<[u8; 32]> = approver
                .list_agent_proposals()
                .await
                .context("list agent proposals")?
                .iter()
                .map(|agent| agent.request_digest)
                .collect();
            let pending = approver
                .list_pending()
                .await
                .context("list certification requests")?;
            for admission in admissions.decide(&pending, &agents, tokio::time::Instant::now()) {
                let event = match admission {
                    Admission::Certify(pending) => {
                        certify_admitted(&mut approver, root, *pending).await?
                    }
                    Admission::Refuse {
                        pairing_code,
                        reason,
                        deny,
                    } => {
                        for digest in deny {
                            approver
                                .decide(digest, tilo_server::certify::ApproverDecision::Deny)
                                .await
                                .context("deny a certification request")?;
                        }
                        OperatorEvent::AdmitRefused {
                            pairing_code,
                            reason,
                        }
                    }
                };
                report(event);
            }
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(100)) => {}
            line = operator.recv() => match line {
                Some(Ok(OperatorCommand::Admit { pairing_code })) => {
                    admissions.admit(pairing_code, tokio::time::Instant::now());
                }
                Some(Ok(OperatorCommand::Shutdown {})) | None => return Ok(()),
                Some(Ok(
                    OperatorCommand::WaitList { .. }
                    | OperatorCommand::Unlist { .. }
                    | OperatorCommand::Confirm { .. }
                    | OperatorCommand::Status { .. }
                    | OperatorCommand::InviteMailQueued { .. }),
                ) => {
                    // Not the command itself: a wait-list command carries an
                    // email address, which no log keeps.
                    anyhow::bail!("a short-lived workspace takes no command but admit and shutdown");
                }
                Some(Err(error)) => {
                    tracing::error!(%error, "unreadable workspace operator line");
                    anyhow::bail!("unreadable workspace operator line: {error}");
                }
            },
        }
    }
}

/// Answers the one request an admitted code matched: a browser certificate
/// under the workspace's root, or a denial when the policy refuses it.
async fn certify_admitted(
    approver: &mut tilo_server_client::Approver,
    root: &SigningKey,
    pending: tilo_server::certify::PendingCertification,
) -> anyhow::Result<OperatorEvent> {
    let decision = issue_device_cert(
        &pending.request,
        root,
        &CertificationPolicy::browser(),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    );
    let certified = decision.is_ok();
    approver
        .decide(
            pending.request_digest,
            decision.map_or(
                tilo_server::certify::ApproverDecision::Deny,
                tilo_server::certify::ApproverDecision::Issue,
            ),
        )
        .await
        .context("answer a certification request")?;
    Ok(if certified {
        OperatorEvent::MemberCertified {
            device: hex::encode(pending.request.device_public_key),
            label: pending.request.device_label,
            pairing_code: pending.pairing_code,
        }
    } else {
        OperatorEvent::AdmitRefused {
            pairing_code: pending.pairing_code,
            reason: RefusalReason::PolicyDenied,
        }
    })
}

/// A short-lived shared workspace: a throwaway user root kept under `--state`,
/// the browser listener, and an approver that certifies only the browsers
/// the operator admits. It speaks [`workspace_operator`]: commands on stdin,
/// events on stdout, starting with [`OperatorEvent::Started`].
///
/// # Errors
/// The state directory, server or listener cannot be set up.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the guard only lives for the lookup; the value kept is a clone"
)]
pub async fn workspace(
    args: WorkspaceArgs,
    shutdown: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    // Every change the workspace receives is decided against its app's surface,
    // a bot's included: the workspace's own apps carry theirs, others the workspace
    // keeps for them in its content store.
    tilo_server::surfaces::decide_receipt(&args.state.join("surfaces"));
    #[cfg(feature = "agents")]
    if args.persistent.persistent.is_some() {
        return crate::persistent_workspace_cli::run(args, shutdown).await;
    }
    let state = args.state;
    std::fs::create_dir_all(&state).with_context(|| format!("create {}", state.display()))?;
    let listen = args.web.web.context("a workspace needs --web")?;
    let operator = operator_lines()?;
    let [root, host, approver_key] = dev_identity(&state)?;
    let user = root.verifying_key().to_bytes();
    let server_cert = enrol(&root, &host, "workspace host", DeviceCapabilities::member())?;
    let approver_cert = enrol(
        &root,
        &approver_key,
        "workspace approver",
        DeviceCapabilities::primary(),
    )?;
    let config = tilo_server::ServerConfig {
        state_root: tilo_server_protocol::metrics::workspace_server_root(&state),
        identity_root: state.join("identity"),
        ..Default::default()
    };
    let hosted = start_server(
        config,
        ServerIdentity {
            user_public_key: user,
            device_signing_key: host,
            device_cert: server_cert,
            revocations: RevocationList::new(),
        },
        &args.web,
        &state,
    )
    .await?;
    let server = &hosted.server;
    name_apple_push(server, &args.web);
    let web = server
        .serve_web_also_at(
            web_config(&args.web, listen, &state),
            args.web.public_origins_also.clone(),
        )
        .await
        .context("start the browser listener")?;
    println!(
        "{}",
        OperatorEvent::Started {
            version: operator::VERSION,
            origin: web.origin.clone(),
            plain_origin: web.plain_origin.clone(),
            user_public_key: hex::encode(user),
            certificate_sha256: web.certificate_sha256.clone(),
            allowed_origins: args.web.allowed_origins.clone(),
            workspace: None,
            host_info: Some(Box::new(web.host_info().clone())),
        }
        .to_line()
    );
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .context("bind the approver's endpoint")?;
    let approver = tilo_server_client::Approver::connect(
        &endpoint,
        server.node_addr(),
        approver_cert,
        &approver_key,
        &RevocationList::new(),
    )
    .await
    .context("connect the workspace approver")?;
    tokio::select! {
        result = approve_admitted(
            approver,
            &root,
            operator,
            Admissions::new(Duration::from_secs(operator::ADMIT_WAIT_SECS)),
            |event| println!("{}", event.to_line()),
        ) => result?,
        result = shutdown => result?,
    }
    drop(web);
    endpoint.close().await;
    hosted.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_server_client::{CertificationOutcome, request_certification};
    use tilo_user_identity::{CERTIFICATION_SCHEMA_VERSION, CertificationRequest};

    fn key() -> SigningKey {
        SigningKey::from_bytes(&rand::random())
    }

    #[cfg(feature = "agents")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn workspace_documents_starts_the_roster_with_the_deployed_flags() {
        #[derive(clap::Parser)]
        struct Command {
            #[command(flatten)]
            web: WebArgs,
        }
        let args =
            <Command as clap::Parser>::try_parse_from(["workspace", "--workspace-documents"])
                .unwrap();
        let state = tempfile::tempdir().unwrap();
        let (root, device) = (key(), key());
        let device_cert = enrol(&root, &device, "host", DeviceCapabilities::member()).unwrap();
        let hosted = start_server(
            tilo_server::ServerConfig {
                state_root: state.path().join("server"),
                identity_root: state.path().join("identity"),
                packet_relay: Some((std::net::Ipv4Addr::LOCALHOST, 0).into()),
                ..Default::default()
            },
            ServerIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_signing_key: device,
                device_cert,
                revocations: RevocationList::new(),
            },
            &args.web,
            state.path(),
        )
        .await
        .unwrap();
        assert!(hosted.host_agents.is_some());
        assert!(hosted.agents.as_ref().unwrap().state_file().is_file());
        hosted.shutdown().await;
    }

    #[test]
    fn a_web_entry_flag_is_what_the_catalogue_announces() {
        #[derive(clap::Parser)]
        struct Command {
            #[command(flatten)]
            web: WebArgs,
        }
        let parse = |args: &[&str]| {
            <Command as clap::Parser>::try_parse_from(
                std::iter::once("workspace").chain(args.iter().copied()),
            )
            .unwrap()
            .web
        };
        let mut cfg = tilo_server::ServerConfig {
            web_entry: Some("https://configured.example/".into()),
            ..Default::default()
        };
        announce_web_entry(&mut cfg, &parse(&[]));
        assert_eq!(
            cfg.web_entry.as_deref(),
            Some("https://configured.example/")
        );
        announce_web_entry(&mut cfg, &parse(&["--web-entry", "https://app.example/"]));
        assert_eq!(cfg.web_entry.as_deref(), Some("https://app.example/"));
    }

    #[test]
    fn another_published_origin_is_named_beside_the_public_one_and_repeats() {
        #[derive(clap::Parser)]
        struct Command {
            #[command(flatten)]
            web: WebArgs,
        }
        let parse = |args: &[&str]| {
            <Command as clap::Parser>::try_parse_from(
                std::iter::once("workspace").chain(args.iter().copied()),
            )
            .map(|command| command.web.public_origins_also)
        };
        assert_eq!(parse(&[]).unwrap(), Vec::<String>::new());
        let published = [
            "--web-plain",
            "127.0.0.1:0",
            "--public-origin",
            "http://127.0.0.1:24500",
        ];
        let also = [
            "--public-origin-also",
            "https://tilo.wifi.example:24500",
            "--public-origin-also",
            "https://lounge.example",
        ];
        assert_eq!(
            parse(&[published, also].concat()).unwrap(),
            ["https://tilo.wifi.example:24500", "https://lounge.example"]
        );
        assert!(
            parse(&[
                "--web-plain",
                "127.0.0.1:0",
                "--public-origin-also",
                "https://tilo.wifi.example:24500"
            ])
            .is_err(),
            "a second name stands beside a first"
        );
    }

    async fn endpoint() -> iroh::endpoint::Endpoint {
        iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .bind()
            .await
            .unwrap()
    }

    /// A browser named `label`'s signed request to be certified under `user`.
    fn browser_request(user: [u8; 32], label: &str) -> CertificationRequest {
        let device = key();
        let now = tilo_clock::unix_now_ns();
        CertificationRequest {
            version: CERTIFICATION_SCHEMA_VERSION,
            user_public_key: user,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: label.into(),
            requested: DeviceCapabilities::member(),
            nonce: rand::random(),
            invite: [0; 16],
            issued_at_ns: now.saturating_sub(1_000_000_000),
            expires_at_ns: now.saturating_add(60_000_000_000),
            signature: [0; 64],
        }
        .sign(&device)
        .unwrap()
    }

    /// The server's address with its bound sockets on loopback too, so an
    /// approver in the same process keeps a path when the machine's network
    /// addresses change.
    fn loopback_addr(server: &Server) -> iroh::EndpointAddr {
        let mut addr = server.node_addr();
        for socket in server.bound_sockets() {
            let ip = match socket.ip() {
                std::net::IpAddr::V4(ip) if ip.is_unspecified() => {
                    std::net::Ipv4Addr::LOCALHOST.into()
                }
                std::net::IpAddr::V6(ip) if ip.is_unspecified() => {
                    std::net::Ipv6Addr::LOCALHOST.into()
                }
                ip => ip,
            };
            addr = addr.with_ip_addr(SocketAddr::new(ip, socket.port()));
        }
        addr
    }

    /// A pending browser request as the approver lists it, showing `code`.
    fn pending(code: &str) -> tilo_server::certify::PendingCertification {
        let request = browser_request(key().verifying_key().to_bytes(), "Browser");
        tilo_server::certify::PendingCertification {
            request_digest: request.digest().unwrap(),
            pairing_code: code.into(),
            request,
            requested_at_ns: 0,
            sign_in: false,
        }
    }

    tilo_test_env::scenario! {
        fn admissions_certify_only_the_browser_a_code_admitted;

        phase pairing_code_collision_does_not_certify_stranger;
        {
            let mut admissions = Admissions::new(Duration::from_secs(60));
            let now = tokio::time::Instant::now();
            admissions.admit("123456".into(), now);
            let (guest, stranger, unrelated) =
                (pending("123456"), pending("123456"), pending("654321"));
            let decisions = admissions.decide(&[guest.clone(), unrelated, stranger.clone()], &[], now);
            assert_eq!(
                decisions,
                vec![Admission::Refuse {
                    pairing_code: "123456".into(),
                    reason: RefusalReason::AmbiguousCode,
                    deny: vec![guest.request_digest, stranger.request_digest],
                }],
                "neither request showing the shared code is certified"
            );
            assert!(admissions.codes.is_empty(), "the ambiguous code is spent");
        }

        phase an_admitted_code_certifies_its_one_browser_and_is_refused_after_the_bound;
        {
            let wait = Duration::from_secs(60);
            let mut admissions = Admissions::new(wait);
            let now = tokio::time::Instant::now();
            for code in ["111111", "222222", "333333"] {
                admissions.admit(code.into(), now);
            }
            let (guest, agent) = (pending("111111"), pending("333333"));
            let listed = [guest.clone(), agent.clone()];
            let agents = [agent.request_digest];
            assert_eq!(
                admissions.decide(&listed, &agents, now),
                vec![Admission::Certify(Box::new(guest))],
                "the guest's code certifies it; an unseen code and an agent's code wait"
            );
            let almost = now.checked_add(wait / 2).unwrap();
            assert_eq!(admissions.decide(&listed[1..], &agents, almost), vec![]);
            let later = now.checked_add(wait).unwrap();
            let refused = |code: &str| Admission::Refuse {
                pairing_code: code.into(),
                reason: RefusalReason::NoMatchingRequest,
                deny: Vec::new(),
            };
            assert_eq!(
                admissions.decide(&listed[1..], &agents, later),
                vec![refused("222222"), refused("333333")],
                "agents are never approved, so an agent's code is refused like an unseen one"
            );
            assert!(admissions.codes.is_empty());
        }
    }

    #[tokio::test]
    async fn a_malformed_operator_line_stops_the_reading_instead_of_being_skipped() {
        let input = b"{\"op\":\"admit\",\"pairing_code\":\"123456\"}\nadmit 654321\n{\"op\":\"shutdown\"}\n";
        let mut lines = operator_lines_from(std::io::Cursor::new(input.to_vec())).unwrap();
        assert_eq!(
            lines.recv().await,
            Some(Ok(OperatorCommand::Admit {
                pairing_code: "123456".into()
            }))
        );
        assert!(
            matches!(lines.recv().await, Some(Err(error)) if error.contains("admit 654321")),
            "the old text form is reported, not skipped"
        );
        assert_eq!(lines.recv().await, None, "nothing after it is read");

        let mut lines = operator_lines_from(std::io::Cursor::new(Vec::new())).unwrap();
        assert_eq!(
            lines.recv().await,
            None,
            "the operator closing stdin closes the channel"
        );
    }

    /// A workspace's server under a fresh root kept in `state`, with the workspace
    /// approver's session open on it: the server, the root, the approver and
    /// the approver's endpoint.
    async fn workspace_server(
        state: &Path,
    ) -> (
        Server,
        SigningKey,
        tilo_server_client::Approver,
        iroh::endpoint::Endpoint,
    ) {
        let [root, host, approver_key] = [key(), key(), key()];
        let server_cert =
            enrol(&root, &host, "workspace host", DeviceCapabilities::member()).unwrap();
        let approver_cert = enrol(
            &root,
            &approver_key,
            "workspace approver",
            DeviceCapabilities::primary(),
        )
        .unwrap();
        let server = Server::start_with_relay_mode(
            tilo_server::ServerConfig {
                state_root: tilo_server_protocol::metrics::workspace_server_root(state),
                identity_root: state.join("identity"),
                ..Default::default()
            }
            .on_loopback(),
            ServerIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_signing_key: host,
                device_cert: server_cert,
                revocations: RevocationList::new(),
            },
            test_server::relay_mode(),
        )
        .await
        .unwrap();
        let approver_endpoint = endpoint().await;
        let approver = tilo_server_client::Approver::connect(
            &approver_endpoint,
            loopback_addr(&server),
            approver_cert,
            &approver_key,
            &RevocationList::new(),
        )
        .await
        .unwrap();
        (server, root, approver, approver_endpoint)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_workspace_certifies_exactly_the_admitted_browser_and_refuses_an_unmatched_code() {
        let tmp = tempfile::tempdir().unwrap();
        let (server, root, approver, approver_endpoint) = workspace_server(tmp.path()).await;
        let user = root.verifying_key().to_bytes();
        let (operator, commands) = tokio::sync::mpsc::channel(16);
        let (reports, mut reported) = tokio::sync::mpsc::unbounded_channel();
        let approving = tokio::spawn(async move {
            approve_admitted(
                approver,
                &root,
                commands,
                Admissions::new(Duration::from_millis(300)),
                move |event| {
                    let _ = reports.send(event);
                },
            )
            .await
        });

        let request = browser_request(user, "Browser");
        let device = hex::encode(request.device_public_key);
        let (shown, code) = tokio::sync::oneshot::channel();
        let browser = endpoint().await;
        let asking = tokio::spawn({
            let (browser, host) = (browser.clone(), server.node_addr());
            async move {
                request_certification(&browser, host, request, |code| {
                    let _ = shown.send(code.to_owned());
                })
                .await
            }
        });
        let code = tokio::time::timeout(Duration::from_secs(20), code)
            .await
            .expect("the request was never queued")
            .unwrap();

        let wrong = if code == "000000" { "000001" } else { "000000" };
        operator
            .send(Ok(OperatorCommand::Admit {
                pairing_code: wrong.into(),
            }))
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(10), reported.recv())
                .await
                .expect("an unmatched code was never refused"),
            Some(OperatorEvent::AdmitRefused {
                pairing_code: wrong.into(),
                reason: RefusalReason::NoMatchingRequest,
            })
        );
        assert!(
            !asking.is_finished(),
            "a browser whose code was never admitted was answered"
        );

        operator
            .send(Ok(OperatorCommand::Admit {
                pairing_code: code.clone(),
            }))
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(20), reported.recv())
                .await
                .expect("the admitted browser was never certified"),
            Some(OperatorEvent::MemberCertified {
                device,
                label: "Browser".into(),
                pairing_code: code,
            })
        );
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(20), asking)
                .await
                .expect("the browser never heard back")
                .unwrap()
                .unwrap(),
            CertificationOutcome::Issued { .. }
        ));

        operator.send(Err("unreadable".into())).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(10), approving)
                .await
                .expect("the workspace kept running after a malformed operator line")
                .unwrap()
                .is_err(),
            "a malformed operator line stops the workspace with an error"
        );
        browser.close().await;
        approver_endpoint.close().await;
        server.shutdown().await;
    }
}
