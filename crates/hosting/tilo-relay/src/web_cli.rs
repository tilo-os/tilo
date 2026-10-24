//! The browser listener from the command line (feature `web`): `run --web`
//! for a configured host, and `dev` for a throwaway local host that
//! certifies every browser that asks.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use ed25519_dalek::SigningKey;
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::web::{TlsSource, WebConfig, WebHandle};
use tilo_relay_protocol::room_operator::{self, OperatorCommand, RefusalReason, RoomEvent};
use tilo_user_identity::{
    CertificationPolicy, DeviceCapabilities, DeviceCert, RevocationList, issue_device_cert,
    sign_device_cert,
};

/// The console origins Cloud's `npm run dev:tilo` serves by default.
const CLOUD_DEV_ORIGINS: [&str; 2] = ["http://127.0.0.1:9998", "http://localhost:9998"];

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
    /// Serve browsers (HTTPS on TCP, WebTransport on UDP) at this address.
    #[arg(long, value_name = "ADDR")]
    pub web: Option<SocketAddr>,
    /// A page origin allowed to call the host cross-origin; repeatable.
    #[arg(long = "allowed-origin", value_name = "ORIGIN")]
    pub allowed_origins: Vec<String>,
    /// `dev` (self-signed, pinned by WebTransport) or
    /// `files:<chain.pem>,<key.pem>`.
    #[arg(long, default_value = "dev", value_name = "SOURCE")]
    pub tls: TlsArg,
    /// The host name browsers reach this listener by (the certificate's
    /// name), on an unspecified address: the origin the host announces, and
    /// with `--tls dev` the development certificate's first name (a node
    /// reached by its LAN address).
    #[arg(long = "public-host", value_name = "NAME")]
    pub public_host: Option<String>,
    /// Also serve the HTTPS routes as plain HTTP at this loopback address,
    /// so a local page needs no certificate exception (the live plane then
    /// uses the HTTPS stream fallback).
    #[arg(long = "web-plain", value_name = "ADDR")]
    pub web_plain: Option<SocketAddr>,
    /// The `https://` origin a TLS-terminating proxy publishes
    /// `--web-plain` at (a public room behind an ingress); it lets
    /// `--web-plain` bind a non-loopback address.
    #[arg(long = "public-origin", value_name = "ORIGIN", requires = "web_plain")]
    pub public_origin: Option<String>,
    /// The web entry (ending in `/`, e.g. `https://app.example/`) where the
    /// Tilo app opens this host's links: announced in catalogue locators,
    /// so `share-link` and bots mint links under it. Overrides the config's
    /// `web_entry`.
    #[arg(long = "web-entry", value_name = "URL")]
    pub web_entry: Option<String>,
    /// The `https://` origin browsers dial WebTransport at, when the page
    /// origin is behind a proxy that passes HTTP only: `--web` is reached
    /// directly under this name (its certificate from `--tls files:…`),
    /// and the HTTPS stream on the page origin stays the fallback.
    #[arg(long = "live-origin", value_name = "ORIGIN", requires = "web")]
    pub live_origin: Option<String>,
    /// The request header the proxy in front of the host names a client's
    /// country in (such as `CF-IPCountry`). A person approving a new device
    /// is shown it; nothing is looked up.
    #[arg(long = "country-header", value_name = "NAME", requires = "web")]
    pub country_header: Option<String>,
    /// The peers `--country-header` is believed from, as comma-separated
    /// `address[/prefix]` ranges; by default this machine only.
    #[arg(
        long = "country-proxies",
        value_name = "RANGES",
        default_value = "127.0.0.0/8,::1",
        requires = "country_header"
    )]
    pub country_proxies: tilo_relay_protocol::proxies::TrustedProxies,
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
    /// each visitor and each room receives a day is budgeted
    /// (`TILO_RELAY_FETCH_PROXY_*`, see the room service's README).
    #[arg(long = "fetch-proxy")]
    pub fetch_proxy: bool,
    /// The device key (hex) of the session beside this node, the shell that
    /// runs the person's apps: pages learn it on `/node` to open apps there.
    /// While the node waits for its certificate, its root certifies the
    /// session with it, as one of the person's devices, and the certificate
    /// is written to `--session-cert-out`.
    #[arg(long = "session-device", value_name = "HEX", requires = "web")]
    pub session_device: Option<String>,
    /// Where the session's certificate is written (canonical encoding).
    #[arg(
        long = "session-cert-out",
        value_name = "PATH",
        requires_all = ["session_device", "install_token_file"],
        conflicts_with = "session_identity_root"
    )]
    pub session_cert_out: Option<PathBuf>,
    /// The session's identity tree (its `TILO_IDENTITY_ROOT`), holding its
    /// device key: install writes its certificate and the person's user key
    /// there, so the session starts as an enrolled device of that person.
    #[arg(
        long = "session-identity-root",
        value_name = "DIR",
        requires_all = ["session_device", "install_token_file"]
    )]
    pub session_identity_root: Option<PathBuf>,
    /// Once serving, write this relay's endpoint for the session beside it
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
    /// Run agents on this host: pages request grants (approved with their
    /// delegation on a root-holding device), spawn agents and steer them.
    /// Grants and agents are kept under the state directory and resume
    /// when the host restarts.
    #[arg(long)]
    pub agents: bool,
    /// How agents plan natural-language tasks: `settings` (the provider
    /// selected in Settings, through tilo-model-gateway), `scripted` (a
    /// deterministic keyword planner for tests and demos),
    /// `openai:<base url>` (an OpenAI-compatible chat-completions endpoint),
    /// `anthropic:<base url>` (Anthropic's Messages API, e.g.
    /// `https://api.anthropic.com`) or `cloud:<base url>` (Tilo Cloud's
    /// metered `…/api/machines/<id>/ai/v1`, billed to the machine's AI
    /// credits). The
    /// key comes from `TILO_AGENT_API_KEY`, or `TILO_AGENT_API_KEY_FILE`,
    /// read before every call so a rotated token needs no restart. Default
    /// `$TILO_AGENT_MODEL`, else `settings`.
    #[arg(long = "agent-model", value_name = "MODEL")]
    pub model: Option<String>,
    /// The model id for `openai:` and `anthropic:` (required) or `cloud:` (default
    /// `gpt-4.1-mini-2025-04-14`). Default `$TILO_AGENT_MODEL_NAME`.
    #[arg(long = "agent-model-name", value_name = "ID")]
    pub model_name: Option<String>,
    /// Without `--agents`, still serve the rooms' records to their pages:
    /// the host runs an action of a room record for whoever the room's
    /// policy lets ask, such as a member linking their own account on the
    /// room's Discord from their profile card. `--agents` serves them too.
    #[arg(long)]
    pub room_records: bool,
}

#[cfg(feature = "agents")]
impl AgentArgs {
    fn options(
        &self,
        state_dir: PathBuf,
    ) -> anyhow::Result<Option<tilo_relay::agents::AgentOptions>> {
        if !self.agents {
            return Ok(None);
        }
        let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        let spec = self
            .model
            .clone()
            .or_else(|| env("TILO_AGENT_MODEL"))
            .unwrap_or_else(|| "settings".into());
        let name = self
            .model_name
            .clone()
            .or_else(|| env("TILO_AGENT_MODEL_NAME"));
        let model = tilo_agent_participant::ModelChoice::parse(
            &spec,
            name.as_deref(),
            tilo_agent_participant::ApiKey::from_env(
                env("TILO_AGENT_API_KEY"),
                env("TILO_AGENT_API_KEY_FILE"),
            ),
        )
        .map_err(|error| anyhow::anyhow!("--agent-model: {error}"))?;
        tracing::info!(model = %model.describe(), state = %state_dir.display(), "agents on this host");
        Ok(Some(tilo_relay::agents::AgentOptions {
            state_dir,
            model,
            catalogue: None,
        }))
    }
}

/// The relay, with its agents when `--agents` asked for them.
pub struct HostedRelay {
    pub relay: Relay,
    #[cfg(feature = "agents")]
    agents: Option<tilo_agent_participant::AgentHost>,
    /// Its state's lock, released once it stopped.
    _lock: tilo_relay::state_lock::StateLock,
}

impl HostedRelay {
    /// Stop the agents (they resume on the next start), then the relay.
    pub async fn shutdown(self) {
        #[cfg(feature = "agents")]
        if let Some(agents) = self.agents {
            agents.shutdown();
        }
        self.relay.shutdown().await;
    }
}

/// `--web-entry`, when given, is the entry the relay's catalogue announces.
fn announce_web_entry(cfg: &mut tilo_relay::RelayConfig, args: &WebArgs) {
    if let Some(entry) = &args.web_entry {
        cfg.web_entry = Some(entry.clone());
    }
}

/// Start the relay; with `--agents`, with its agent supervisor as the
/// app-serving plane and the agents kept under `state_root/agents`; with
/// `--room-records` alone, serving the rooms' records without agents.
///
/// # Errors
/// The relay did not start, or the agent state could not be read.
#[cfg_attr(
    not(feature = "agents"),
    allow(unused_variables, reason = "agents only")
)]
pub async fn start_relay(
    mut cfg: tilo_relay::RelayConfig,
    identity: RelayIdentity,
    args: &WebArgs,
    state_root: &Path,
) -> anyhow::Result<HostedRelay> {
    if let Some(max) = env_count("TILO_RELAY_MAX_ROOM_GRANTS") {
        cfg.max_room_grants = Some(max);
    }
    announce_web_entry(&mut cfg, args);
    let lock =
        tilo_relay::state_lock::StateLock::acquire(&cfg.state_root).map_err(anyhow::Error::msg)?;
    #[cfg(feature = "agents")]
    let options = args.agents.options(state_root.join("agents"))?;
    #[cfg(feature = "agents")]
    if let Some(options) = options {
        let (relay, agents) = tilo_relay::agents::start(cfg, identity, options)
            .await
            .context("start the relay with agents")?;
        return Ok(HostedRelay {
            relay,
            agents: Some(agents),
            _lock: lock,
        });
    }
    #[cfg(feature = "agents")]
    if args.agents.room_records {
        let relay = tilo_relay::agents::start_serving_records(cfg, identity, None)
            .await
            .context("start the relay serving its rooms' records")?;
        return Ok(HostedRelay {
            relay,
            agents: None,
            _lock: lock,
        });
    }
    let relay = Relay::start(cfg, identity)
        .await
        .context("start the relay")?;
    Ok(HostedRelay {
        relay,
        #[cfg(feature = "agents")]
        agents: None,
        _lock: lock,
    })
}

pub(crate) fn web_config(args: &WebArgs, listen: SocketAddr, state_root: &Path) -> WebConfig {
    let tls = match &args.tls {
        TlsArg::Dev => TlsSource::Development {
            hosts: args
                .public_host
                .iter()
                .cloned()
                .chain(["localhost".into(), listen.ip().to_string()])
                .collect(),
            store: Some(state_root.join("web-dev-identity")),
        },
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
        live_origin: args.live_origin.clone(),
        limits: web_limits(),
        country: args
            .country_header
            .clone()
            .map(|name| tilo_relay::web::CountryHeader {
                name,
                proxies: args.country_proxies.clone(),
            }),
        fetch_proxy: args.fetch_proxy.then(|| tilo_relay::web::FetchProxySetup {
            config: fetch_proxy_config(),
            metrics_file: Some(
                tilo_relay_protocol::metrics::room_textfile_dir(state_root)
                    .join("fetch-proxy.prom"),
            ),
        }),
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
        window: env_count("TILO_RELAY_FETCH_PROXY_BUDGET_HOURS")
            .filter(|hours| *hours > 0)
            .map_or(budget_defaults.window, |hours| {
                Duration::from_secs(
                    u64::try_from(hours)
                        .unwrap_or(u64::MAX)
                        .saturating_mul(3600),
                )
            }),
        visitor_bytes: budget(
            "TILO_RELAY_FETCH_PROXY_VISITOR_MIB",
            budget_defaults.visitor_bytes,
        ),
        room_bytes: budget(
            "TILO_RELAY_FETCH_PROXY_ROOM_MIB",
            budget_defaults.room_bytes,
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
    let device_bytes = env_count("TILO_RELAY_FETCH_PROXY_DEVICE_MIB_PER_HOUR").map_or(
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
                "TILO_RELAY_FETCH_PROXY_DEVICE_PER_MIN",
                |per_minute| per_minute,
                defaults.device_requests,
            ),
            device_bytes,
            destination_requests: rate(
                "TILO_RELAY_FETCH_PROXY_DESTINATION_PER_MIN",
                |per_minute| per_minute * 0.4,
                defaults.destination_requests,
            ),
            device_in_flight: env_count("TILO_RELAY_FETCH_PROXY_DEVICE_IN_FLIGHT")
                .unwrap_or(defaults.device_in_flight),
            in_flight: env_count("TILO_RELAY_FETCH_PROXY_IN_FLIGHT").unwrap_or(defaults.in_flight),
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

fn web_limits() -> tilo_relay::web::WebLimits {
    let defaults = tilo_relay::web::WebLimits::default();
    tilo_relay::web::WebLimits {
        max_sessions: env_count("TILO_RELAY_MAX_WEB_SESSIONS").unwrap_or(defaults.max_sessions),
        max_handshakes: env_count("TILO_RELAY_MAX_HANDSHAKES").unwrap_or(defaults.max_handshakes),
        max_live_grants: env_count("TILO_RELAY_MAX_LIVE_GRANTS")
            .unwrap_or(defaults.max_live_grants),
    }
}

/// Start the listener `run --web` asked for, if any.
///
/// # Errors
/// The listener cannot bind or load its certificate.
/// Before the relay starts: when `--install-token-file` is given and the
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
    let paths = tilo_user_identity::IdentityPaths::under(identity_root);
    if paths.device_cert.is_file() {
        return Ok(());
    }
    let token = std::fs::read_to_string(token_file)
        .with_context(|| format!("read {}", token_file.display()))?;
    let (device, _) =
        tilo_user_identity::load_or_create_device_key(&paths).context("create the device key")?;
    println!(
        "{}",
        serde_json::json!({
            "event": "awaiting_certificate",
            "devicePublicKey": hex::encode(device.verifying_key().to_bytes()),
        })
    );
    let session = match (
        session_device(args)?,
        &args.session_cert_out,
        &args.session_identity_root,
    ) {
        (Some(public_key), Some(cert_out), _) => Some(tilo_relay::web::install::SessionDevice {
            public_key,
            cert_out: cert_out.clone(),
            user_key_out: None,
        }),
        (Some(public_key), None, Some(root)) => {
            let paths = tilo_user_identity::IdentityPaths::under(root);
            Some(tilo_relay::web::install::SessionDevice {
                public_key,
                cert_out: paths.device_cert,
                user_key_out: Some(paths.user_pubkey),
            })
        }
        _ => None,
    };
    let cert = tilo_relay::web::install::await_certificate(
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
    relay: &Relay,
    args: &WebArgs,
    state_root: &Path,
) -> anyhow::Result<Option<WebHandle>> {
    let Some(listen) = args.web else {
        return Ok(None);
    };
    let device = session_device(args)?;
    if let Some(device) = device {
        relay.set_node_session(device);
    }
    if let Some(path) = &args.session_env_out {
        let endpoint =
            serde_json::to_string(&relay.node_addr()).context("encode the relay's endpoint")?;
        let line = format!("TILO_COLLAB_HOST='{endpoint}'\n");
        tilo_files::native::atomic::write_with_mode(path, line.as_bytes(), 0o644)
            .with_context(|| format!("write {}", path.display()))?;
    }
    let web = relay
        .serve_web(web_config(args, listen, state_root))
        .await
        .context("start the browser listener")?;
    tracing::info!(origin = %web.origin, allowed = ?args.allowed_origins, "serving browsers");
    Ok(Some(web))
}

/// `tilo-relay dev`.
#[derive(clap::Args, Clone, Debug)]
pub struct DevArgs {
    #[command(flatten)]
    pub web: WebArgs,
    /// Keeps the throwaway user root, the host's device and every room, so
    /// browsers stay certified across restarts. Delete it to start over.
    #[arg(long, value_name = "DIR")]
    pub state: Option<PathBuf>,
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
    };
    sign_device_cert(&mut cert, root).context("sign a dev device certificate")?;
    Ok(cert)
}

/// The first wait before reopening a lost approver session; it doubles up
/// to [`APPROVER_RETRY_MAX`].
const APPROVER_RETRY_MIN: Duration = Duration::from_millis(250);
const APPROVER_RETRY_MAX: Duration = Duration::from_secs(10);
/// A session that lasted this long starts the next wait from the minimum.
const APPROVER_SETTLED: Duration = Duration::from_secs(30);

/// Keep a dev approver answering for as long as the host runs. A session
/// that fails (the network changed under it, a path closed, the host was
/// slow to answer) is logged and reopened with `connect` after a backoff;
/// it never ends the host.
async fn approve_forever<C, F>(mut connect: C, root: &SigningKey) -> std::convert::Infallible
where
    C: FnMut() -> F,
    F: std::future::Future<Output = anyhow::Result<tilo_relay_client::Approver>>,
{
    let mut wait = APPROVER_RETRY_MIN;
    loop {
        let opened = tokio::time::Instant::now();
        let error = match connect().await {
            Ok(approver) => match approve_every_browser(approver, root).await {
                Err(error) => error,
                Ok(never) => match never {},
            },
            Err(error) => error.context("connect the dev approver"),
        };
        if opened.elapsed() >= APPROVER_SETTLED {
            wait = APPROVER_RETRY_MIN;
        }
        tracing::warn!(error = %format!("{error:#}"), retry_in = ?wait, "dev approver session lost; reopening it");
        tokio::time::sleep(wait).await;
        wait = wait.saturating_mul(2).min(APPROVER_RETRY_MAX);
    }
}

/// The relay's address with its bound sockets on loopback too, so an
/// approver in the same process keeps a path when the machine's network
/// addresses change.
fn loopback_addr(relay: &Relay) -> iroh::EndpointAddr {
    let mut addr = relay.node_addr();
    for socket in relay.bound_sockets() {
        let ip = match socket.ip() {
            std::net::IpAddr::V4(ip) if ip.is_unspecified() => std::net::Ipv4Addr::LOCALHOST.into(),
            std::net::IpAddr::V6(ip) if ip.is_unspecified() => std::net::Ipv6Addr::LOCALHOST.into(),
            ip => ip,
        };
        addr = addr.with_ip_addr(SocketAddr::new(ip, socket.port()));
    }
    addr
}

/// Certify every browser device that asks, as the dev user root, approve
/// every agent grant a page asks for with its delegation, and allow once
/// each action an agent asks its person about, until the session fails.
async fn approve_every_browser(
    mut approver: tilo_relay_client::Approver,
    root: &SigningKey,
) -> anyhow::Result<std::convert::Infallible> {
    loop {
        for asked in approver
            .list_confirmations()
            .await
            .context("list agents' questions")?
        {
            tracing::info!(question = %asked.question(), room = %asked.room, "allowing an agent's action once");
            approver
                .answer(asked.id, tilo_relay::certify::ConfirmationAnswer::AllowOnce)
                .await
                .context("answer an agent's question")?;
        }
        let agents = approver
            .list_agent_proposals()
            .await
            .context("list agent proposals")?;
        for pending in approver
            .list_pending()
            .await
            .context("list certification requests")?
        {
            if let Some(agent) = agents
                .iter()
                .find(|agent| agent.request_digest == pending.request_digest)
            {
                let now = tilo_clock::unix_now_ns();
                let decision = issue_device_cert(
                    &pending.request,
                    root,
                    &CertificationPolicy::agent(),
                    &RevocationList::new(),
                    now,
                )
                .ok()
                .zip(
                    agent
                        .proposal
                        .sign(root, pending.request.device_public_key, now)
                        .ok(),
                )
                .map_or(
                    tilo_relay::certify::ApproverDecision::Deny,
                    |(cert, delegation)| tilo_relay::certify::ApproverDecision::IssueDelegate {
                        cert,
                        delegation,
                    },
                );
                tracing::info!(
                    agent = %agent.proposal.label,
                    rooms = ?agent.proposal.scope.rooms,
                    apps = ?agent.proposal.scope.apps,
                    pairing_code = %pending.pairing_code,
                    "approving an agent grant"
                );
                approver
                    .decide(pending.request_digest, decision)
                    .await
                    .context("answer an agent grant")?;
                continue;
            }
            let decision = issue_device_cert(
                &pending.request,
                root,
                &CertificationPolicy::browser(),
                &RevocationList::new(),
                tilo_clock::unix_now_ns(),
            )
            .map_or(
                tilo_relay::certify::ApproverDecision::Deny,
                tilo_relay::certify::ApproverDecision::Issue,
            );
            tracing::info!(
                device = %pending.request.device_label,
                pairing_code = %pending.pairing_code,
                "certifying a browser"
            );
            approver
                .decide(pending.request_digest, decision)
                .await
                .context("answer a certification request")?;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// What a page (or Cloud's `dev:tilo`) needs to connect, and the Iroh
/// address a native runtime names in `TILO_COLLAB_HOST`: one JSON line on
/// stdout, and instructions on stderr.
fn announce(
    web: &WebHandle,
    iroh: &iroh::EndpointAddr,
    user: &[u8; 32],
    allowed: &[String],
    state: &Path,
) {
    let localhost = format!("https://localhost:{}", web.addr.port());
    let user = hex::encode(user);
    let iroh = serde_json::to_string(iroh).unwrap_or_default();
    println!(
        "{}",
        serde_json::json!({
            "origin": web.origin,
            "localhostOrigin": localhost,
            "plainOrigin": web.plain_origin,
            "irohAddr": iroh,
            "userPublicKey": user,
            "certificateSha256": web.certificate_sha256,
            "allowedOrigins": allowed,
            "state": state,
        })
    );
    eprintln!(
        "\nTilo dev host (development only: it certifies every browser that asks)\n  \
         HTTPS + WebTransport: {localhost}\n  user root:           {user}\n  \
         allowed origins:     {}\n  state:               {}\n\n\
         Cloud console:\n  npm run dev:tilo -- --port 9998 --host-origin {localhost} --user-public-key {user}\n\n\
         Open {localhost} in that browser once and accept the development certificate;\n\
         WebTransport pins it by hash.\n\n\
         Native runtime (its identity must be this user root):\n  TILO_COLLAB_HOST='{iroh}'\n",
        allowed.join(" "),
        state.display(),
    );
}

/// A local host for development: a throwaway user root kept under
/// `--state`, the browser listener, and an approver that certifies every
/// browser device that asks. Rooms of that user are bound on first use.
///
/// # Errors
/// The state directory, relay or listener cannot be set up.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the guard only lives for the lookup; the value kept is a clone"
)]
pub async fn dev(
    args: DevArgs,
    shutdown: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let state = args
        .state
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("tilo-relay-dev"));
    std::fs::create_dir_all(&state).with_context(|| format!("create {}", state.display()))?;
    let [root, host, approver_key] = dev_identity(&state)?;
    let user = root.verifying_key().to_bytes();
    let host_cert = enrol(&root, &host, "dev host", DeviceCapabilities::host())?;
    let approver_cert = enrol(
        &root,
        &approver_key,
        "dev approver",
        DeviceCapabilities::primary(),
    )?;
    let config = tilo_relay::RelayConfig {
        state_root: state.join("relay"),
        identity_root: state.join("identity"),
        ..Default::default()
    };
    let hosted = start_relay(
        config,
        RelayIdentity {
            user_public_key: user,
            device_signing_key: host,
            device_cert: host_cert,
            revocations: RevocationList::new(),
        },
        &args.web,
        &state,
    )
    .await?;
    let relay = &hosted.relay;
    let mut web_args = args.web.clone();
    if web_args.allowed_origins.is_empty() {
        web_args.allowed_origins = CLOUD_DEV_ORIGINS
            .iter()
            .map(|&origin| origin.into())
            .collect();
    }
    // The session beside this host that launches apps for web members, as
    // `run --web` takes it: without it a web page lists nothing to open.
    if let Some(device) = session_device(&web_args)? {
        relay.set_node_session(device);
    }
    let listen = web_args
        .web
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 9443)));
    let web = relay
        .serve_web(web_config(&web_args, listen, &state))
        .await
        .context("start the browser listener")?;
    announce(
        &web,
        &relay.node_addr(),
        &user,
        &web_args.allowed_origins,
        &state,
    );
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .context("bind the approver's endpoint")?;
    let connect = || {
        let (endpoint, cert, key, host) = (
            &endpoint,
            approver_cert.clone(),
            &approver_key,
            loopback_addr(relay),
        );
        async move {
            Ok(tilo_relay_client::Approver::connect(
                endpoint,
                host,
                cert,
                key,
                &RevocationList::new(),
            )
            .await?)
        }
    };
    tokio::select! {
        never = approve_forever(connect, &root) => match never {},
        result = shutdown => result?,
    }
    drop(web);
    endpoint.close().await;
    hosted.shutdown().await;
    Ok(())
}

/// `tilo-relay room`.
#[derive(clap::Args, Clone, Debug)]
pub struct RoomArgs {
    #[command(flatten)]
    pub web: WebArgs,
    /// The room's throwaway user root, host device and documents. The
    /// operator removes it when the room ends.
    #[arg(long, value_name = "DIR")]
    pub state: PathBuf,
    #[cfg(feature = "agents")]
    #[command(flatten)]
    pub persistent: crate::persistent_room_cli::PersistentArgs,
}

/// One operator line from stdin, or why it could not be read.
type OperatorLine = Result<OperatorCommand, String>;

/// Operator lines from stdin, read on their own thread: a blocking read
/// must never hold up the host.
pub(crate) fn operator_lines() -> anyhow::Result<tokio::sync::mpsc::Receiver<OperatorLine>> {
    operator_lines_from(std::io::stdin())
}

/// [`OperatorCommand`]s from `input`, one per line. A line that does not
/// decode is passed on as an error and ends the reading, so the room stops
/// instead of skipping it; the end of `input` is a shutdown.
fn operator_lines_from(
    input: impl std::io::Read + Send + 'static,
) -> anyhow::Result<tokio::sync::mpsc::Receiver<OperatorLine>> {
    use std::io::BufRead;
    let (sender, receiver) = tokio::sync::mpsc::channel(16);
    std::thread::Builder::new()
        .name("tilo-room-operator".into())
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
            let _ = sender.blocking_send(Ok(OperatorCommand::Shutdown {}));
        })
        .context("spawn the operator reader")?;
    Ok(receiver)
}

/// What the room approver does with one admitted pairing code.
#[derive(Debug, PartialEq, Eq)]
enum Admission {
    /// Exactly one pending browser request displays the code: certify it.
    Certify(Box<tilo_relay::certify::PendingCertification>),
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
        pending: &[tilo_relay::certify::PendingCertification],
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

/// Certify, as the room's user root, exactly the pending browsers whose
/// pairing codes the operator admitted, each once, and `report` the outcome
/// of every admitted code. Agent grants are never approved. A malformed
/// operator line is an error.
async fn approve_admitted(
    mut approver: tilo_relay_client::Approver,
    root: &SigningKey,
    mut operator: tokio::sync::mpsc::Receiver<OperatorLine>,
    mut admissions: Admissions,
    mut report: impl FnMut(RoomEvent),
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
                                .decide(digest, tilo_relay::certify::ApproverDecision::Deny)
                                .await
                                .context("deny a certification request")?;
                        }
                        RoomEvent::AdmitRefused {
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
                    command @ (OperatorCommand::PreAdmit { .. }
                    | OperatorCommand::Spectate { .. }
                    | OperatorCommand::WaitList { .. }),
                )) => {
                    anyhow::bail!("a short-lived room takes no {command:?}");
                }
                Some(Err(error)) => {
                    tracing::error!(%error, "unreadable room operator line");
                    anyhow::bail!("unreadable room operator line: {error}");
                }
            },
        }
    }
}

/// Answers the one request an admitted code matched: a browser certificate
/// under the room's root, or a denial when the policy refuses it.
async fn certify_admitted(
    approver: &mut tilo_relay_client::Approver,
    root: &SigningKey,
    pending: tilo_relay::certify::PendingCertification,
) -> anyhow::Result<RoomEvent> {
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
                tilo_relay::certify::ApproverDecision::Deny,
                tilo_relay::certify::ApproverDecision::Issue,
            ),
        )
        .await
        .context("answer a certification request")?;
    Ok(if certified {
        RoomEvent::MemberCertified {
            device: hex::encode(pending.request.device_public_key),
            label: pending.request.device_label,
            pairing_code: pending.pairing_code,
        }
    } else {
        RoomEvent::AdmitRefused {
            pairing_code: pending.pairing_code,
            reason: RefusalReason::PolicyDenied,
        }
    })
}

/// A short-lived shared room: a throwaway user root kept under `--state`,
/// the browser listener, and an approver that certifies only the browsers
/// the operator admits. It speaks [`room_operator`]: commands on stdin,
/// events on stdout, starting with [`RoomEvent::Started`].
///
/// # Errors
/// The state directory, relay or listener cannot be set up.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the guard only lives for the lookup; the value kept is a clone"
)]
pub async fn room(
    args: RoomArgs,
    shutdown: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    // Every change the room receives is decided against its app's surface,
    // a bot's included: the room's own apps carry theirs, others come from
    // the registry.
    tilo_relay::surfaces::decide_receipt_from_registry(
        &std::env::var("TILO_REGISTRY")
            .unwrap_or_else(|_| tilo_relay::surfaces::DEFAULT_REGISTRY.to_owned()),
    );
    #[cfg(feature = "agents")]
    if args.persistent.persistent.is_some() {
        return crate::persistent_room_cli::run(args, shutdown).await;
    }
    let state = args.state;
    std::fs::create_dir_all(&state).with_context(|| format!("create {}", state.display()))?;
    let listen = args.web.web.context("a room needs --web")?;
    let operator = operator_lines()?;
    let [root, host, approver_key] = dev_identity(&state)?;
    let user = root.verifying_key().to_bytes();
    let host_cert = enrol(&root, &host, "room host", DeviceCapabilities::host())?;
    let approver_cert = enrol(
        &root,
        &approver_key,
        "room approver",
        DeviceCapabilities::primary(),
    )?;
    let config = tilo_relay::RelayConfig {
        state_root: tilo_relay_protocol::metrics::room_relay_root(&state),
        identity_root: state.join("identity"),
        ..Default::default()
    };
    let hosted = start_relay(
        config,
        RelayIdentity {
            user_public_key: user,
            device_signing_key: host,
            device_cert: host_cert,
            revocations: RevocationList::new(),
        },
        &args.web,
        &state,
    )
    .await?;
    let relay = &hosted.relay;
    let web = relay
        .serve_web(web_config(&args.web, listen, &state))
        .await
        .context("start the browser listener")?;
    println!(
        "{}",
        RoomEvent::Started {
            version: room_operator::VERSION,
            origin: web.origin.clone(),
            plain_origin: web.plain_origin.clone(),
            user_public_key: hex::encode(user),
            certificate_sha256: web.certificate_sha256.clone(),
            allowed_origins: args.web.allowed_origins.clone(),
            room: None,
        }
        .to_line()
    );
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .context("bind the approver's endpoint")?;
    let approver = tilo_relay_client::Approver::connect(
        &endpoint,
        relay.node_addr(),
        approver_cert,
        &approver_key,
        &RevocationList::new(),
    )
    .await
    .context("connect the room approver")?;
    tokio::select! {
        result = approve_admitted(
            approver,
            &root,
            operator,
            Admissions::new(Duration::from_secs(room_operator::ADMIT_WAIT_SECS)),
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
    use tilo_relay_client::{CertificationOutcome, request_certification};
    use tilo_user_identity::{CERTIFICATION_SCHEMA_VERSION, CertificationRequest};

    fn key() -> SigningKey {
        SigningKey::from_bytes(&rand::random())
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
                std::iter::once("room").chain(args.iter().copied()),
            )
            .unwrap()
            .web
        };
        let mut cfg = tilo_relay::RelayConfig {
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

    async fn endpoint() -> iroh::endpoint::Endpoint {
        iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .bind()
            .await
            .unwrap()
    }

    /// A browser's signed request to be certified under `user`.
    fn browser_request(user: [u8; 32]) -> CertificationRequest {
        let device = key();
        let now = tilo_clock::unix_now_ns();
        CertificationRequest {
            version: CERTIFICATION_SCHEMA_VERSION,
            user_public_key: user,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Browser".into(),
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

    /// A browser asks the host for a certificate; the dev approver answers.
    async fn certify_a_browser(relay: &Relay, user: [u8; 32]) -> CertificationOutcome {
        let request = browser_request(user);
        let browser = endpoint().await;
        let outcome = tokio::time::timeout(
            Duration::from_secs(20),
            request_certification(&browser, relay.node_addr(), request, |_| {}),
        )
        .await
        .expect("the approver never answered")
        .unwrap();
        browser.close().await;
        outcome
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the relay serves the whole scenario and `shutdown` consumes it at the end"
    )]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_lost_approver_connection_is_reopened_and_the_host_keeps_serving() {
        let tmp = tempfile::tempdir().unwrap();
        let [root, host, approver_key] = [key(), key(), key()];
        let user = root.verifying_key().to_bytes();
        let host_cert = enrol(&root, &host, "dev host", DeviceCapabilities::host()).unwrap();
        let approver_cert = enrol(
            &root,
            &approver_key,
            "dev approver",
            DeviceCapabilities::primary(),
        )
        .unwrap();
        let relay = Relay::start(
            tilo_relay::RelayConfig {
                state_root: tmp.path().join("relay"),
                identity_root: tmp.path().join("identity"),
                ..Default::default()
            },
            RelayIdentity {
                user_public_key: user,
                device_signing_key: host,
                device_cert: host_cert,
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();

        // Every session gets an endpoint of its own, handed to the test so
        // it can cut the connection from under the approver.
        let (sessions_tx, mut sessions) = tokio::sync::mpsc::unbounded_channel();
        let host_addr = loopback_addr(&relay);
        let approving = tokio::spawn(async move {
            let connect = || {
                let (sessions_tx, cert, key, host) = (
                    sessions_tx.clone(),
                    approver_cert.clone(),
                    approver_key.clone(),
                    host_addr.clone(),
                );
                async move {
                    let endpoint = endpoint().await;
                    let _ = sessions_tx.send(endpoint.clone());
                    Ok(tilo_relay_client::Approver::connect(
                        &endpoint,
                        host,
                        cert,
                        &key,
                        &RevocationList::new(),
                    )
                    .await?)
                }
            };
            approve_forever(connect, &root).await
        });

        let first = sessions.recv().await.unwrap();
        assert!(matches!(
            certify_a_browser(&relay, user).await,
            CertificationOutcome::Issued { .. }
        ));

        first.close().await;

        assert!(matches!(
            certify_a_browser(&relay, user).await,
            CertificationOutcome::Issued { .. }
        ));
        assert!(
            sessions.try_recv().is_ok(),
            "the second answer came from a reopened session"
        );
        assert!(!approving.is_finished(), "the approver gave up");
        approving.abort();
        relay.shutdown().await;
    }

    /// A pending browser request as the approver lists it, showing `code`.
    fn pending(code: &str) -> tilo_relay::certify::PendingCertification {
        let request = browser_request(key().verifying_key().to_bytes());
        tilo_relay::certify::PendingCertification {
            request_digest: request.digest().unwrap(),
            pairing_code: code.into(),
            request,
            requested_at_ns: 0,
            country: None,
        }
    }

    #[test]
    fn pairing_code_collision_does_not_certify_stranger() {
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

    #[test]
    fn an_admitted_code_certifies_its_one_browser_and_is_refused_after_the_bound() {
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
            Some(Ok(OperatorCommand::Shutdown {})),
            "the operator closing stdin ends the room"
        );
    }

    /// A room's relay under a fresh root kept in `state`, with the room
    /// approver's session open on it: the relay, the root, the approver and
    /// the approver's endpoint.
    async fn room_relay(
        state: &Path,
    ) -> (
        Relay,
        SigningKey,
        tilo_relay_client::Approver,
        iroh::endpoint::Endpoint,
    ) {
        let [root, host, approver_key] = [key(), key(), key()];
        let host_cert = enrol(&root, &host, "room host", DeviceCapabilities::host()).unwrap();
        let approver_cert = enrol(
            &root,
            &approver_key,
            "room approver",
            DeviceCapabilities::primary(),
        )
        .unwrap();
        let relay = Relay::start(
            tilo_relay::RelayConfig {
                state_root: tilo_relay_protocol::metrics::room_relay_root(state),
                identity_root: state.join("identity"),
                ..Default::default()
            },
            RelayIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_signing_key: host,
                device_cert: host_cert,
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        let approver_endpoint = endpoint().await;
        let approver = tilo_relay_client::Approver::connect(
            &approver_endpoint,
            loopback_addr(&relay),
            approver_cert,
            &approver_key,
            &RevocationList::new(),
        )
        .await
        .unwrap();
        (relay, root, approver, approver_endpoint)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_room_certifies_exactly_the_admitted_browser_and_refuses_an_unmatched_code() {
        let tmp = tempfile::tempdir().unwrap();
        let (relay, root, approver, approver_endpoint) = room_relay(tmp.path()).await;
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

        let request = browser_request(user);
        let device = hex::encode(request.device_public_key);
        let (shown, code) = tokio::sync::oneshot::channel();
        let browser = endpoint().await;
        let asking = tokio::spawn({
            let (browser, host) = (browser.clone(), relay.node_addr());
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
            Some(RoomEvent::AdmitRefused {
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
            Some(RoomEvent::MemberCertified {
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
                .expect("the room kept running after a malformed operator line")
                .unwrap()
                .is_err(),
            "a malformed operator line stops the room with an error"
        );
        browser.close().await;
        approver_endpoint.close().await;
        relay.shutdown().await;
    }
}
