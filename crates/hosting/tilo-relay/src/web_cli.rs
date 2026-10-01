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
    /// Also serve the HTTPS routes as plain HTTP at this loopback address,
    /// so a local page needs no certificate exception (the live plane then
    /// uses the HTTPS stream fallback).
    #[arg(long = "web-plain", value_name = "ADDR")]
    pub web_plain: Option<SocketAddr>,
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
    /// `openai:<base url>` (an OpenAI-compatible chat-completions endpoint)
    /// or `cloud:<base url>` (Tilo Cloud's metered
    /// `…/api/machines/<id>/ai/v1`, billed to the machine's AI credits). The
    /// key comes from `TILO_AGENT_API_KEY`, or `TILO_AGENT_API_KEY_FILE`,
    /// read before every call so a rotated token needs no restart. Default
    /// `$TILO_AGENT_MODEL`, else `settings`.
    #[arg(long = "agent-model", value_name = "MODEL")]
    pub model: Option<String>,
    /// The model id for `openai:` (required) or `cloud:` (default
    /// `gpt-4.1-mini-2025-04-14`). Default `$TILO_AGENT_MODEL_NAME`.
    #[arg(long = "agent-model-name", value_name = "ID")]
    pub model_name: Option<String>,
    /// Let agents' apps use this user's own files, Keychain or Secret
    /// Service, as a person at this machine would. Without it each agent
    /// has a profile of its own under the state directory and secrets
    /// kept in memory.
    #[arg(long = "agent-host-profile")]
    pub host_profile: bool,
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
            host_profile: self.host_profile,
            catalogue: None,
        }))
    }
}

/// The relay, with its agents when `--agents` asked for them.
pub struct HostedRelay {
    pub relay: Relay,
    #[cfg(feature = "agents")]
    agents: Option<tilo_agent_participant::AgentHost>,
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

/// Start the relay; with `--agents`, with its agent supervisor as the
/// app-serving plane and the agents kept under `state_root/agents`.
///
/// # Errors
/// The relay did not start, or the agent state could not be read.
#[cfg_attr(
    not(feature = "agents"),
    allow(unused_variables, reason = "agents only")
)]
pub async fn start_relay(
    cfg: tilo_relay::RelayConfig,
    identity: RelayIdentity,
    args: &WebArgs,
    state_root: &Path,
) -> anyhow::Result<HostedRelay> {
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
        });
    }
    let relay = Relay::start(cfg, identity)
        .await
        .context("start the relay")?;
    Ok(HostedRelay {
        relay,
        #[cfg(feature = "agents")]
        agents: None,
    })
}

fn web_config(args: &WebArgs, listen: SocketAddr, state_root: &Path) -> WebConfig {
    let tls = match &args.tls {
        TlsArg::Dev => TlsSource::Development {
            hosts: vec!["localhost".into(), listen.ip().to_string()],
            store: Some(state_root.join("web-dev-identity")),
        },
        TlsArg::Files { chain, key } => TlsSource::Files {
            certificate_chain: chain.clone(),
            private_key: key.clone(),
        },
    };
    WebConfig {
        listen,
        tls,
        allowed_origins: args.allowed_origins.clone(),
        plain_listen: args.web_plain,
    }
}

/// Start the listener `run --web` asked for, if any.
///
/// # Errors
/// The listener cannot bind or load its certificate.
pub async fn serve(
    relay: &Relay,
    args: &WebArgs,
    state_root: &Path,
) -> anyhow::Result<Option<WebHandle>> {
    let Some(listen) = args.web else {
        return Ok(None);
    };
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

fn enrol(
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

/// Certify every browser device that asks, as the dev user root, and
/// approve every agent grant a page asks for with its delegation, until
/// the session fails.
async fn approve_every_browser(
    mut approver: tilo_relay_client::Approver,
    root: &SigningKey,
) -> anyhow::Result<std::convert::Infallible> {
    loop {
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
                    |(cert, delegation)| tilo_relay::certify::ApproverDecision::IssueAgent {
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
}

/// What the room's operator sends on stdin, one per line.
enum Operator {
    /// Certify the pending browser that displays this pairing code.
    Admit(String),
    Shutdown,
}

/// Operator lines from stdin, read on their own thread: a blocking read
/// must never hold up the host.
fn operator_lines() -> anyhow::Result<tokio::sync::mpsc::Receiver<Operator>> {
    use std::io::BufRead;
    let (sender, receiver) = tokio::sync::mpsc::channel(16);
    std::thread::Builder::new()
        .name("tilo-room-operator".into())
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                let command = match line.trim() {
                    "shutdown" => Operator::Shutdown,
                    other => match other.strip_prefix("admit ") {
                        Some(code) if !code.trim().is_empty() => {
                            Operator::Admit(code.trim().into())
                        }
                        _ => continue,
                    },
                };
                if sender.blocking_send(command).is_err() {
                    break;
                }
            }
            let _ = sender.blocking_send(Operator::Shutdown);
        })
        .context("spawn the operator reader")?;
    Ok(receiver)
}

/// Certify, as the room's user root, exactly the pending browsers whose
/// pairing codes the operator admitted, each once; announce each on stdout.
/// Agent grants are never approved.
async fn approve_admitted(
    mut approver: tilo_relay_client::Approver,
    root: &SigningKey,
    mut operator: tokio::sync::mpsc::Receiver<Operator>,
) -> anyhow::Result<()> {
    let mut admitted = std::collections::BTreeSet::new();
    loop {
        while let Ok(command) = operator.try_recv() {
            match command {
                Operator::Admit(code) => {
                    admitted.insert(code);
                }
                Operator::Shutdown => return Ok(()),
            }
        }
        if !admitted.is_empty() {
            let agents = approver
                .list_agent_proposals()
                .await
                .context("list agent proposals")?;
            for pending in approver
                .list_pending()
                .await
                .context("list certification requests")?
            {
                if agents
                    .iter()
                    .any(|agent| agent.request_digest == pending.request_digest)
                    || !admitted.remove(&pending.pairing_code)
                {
                    continue;
                }
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
                if certified {
                    println!(
                        "{}",
                        serde_json::json!({
                            "event": "member_certified",
                            "device": hex::encode(pending.request.device_public_key),
                            "label": pending.request.device_label,
                            "pairingCode": pending.pairing_code,
                        })
                    );
                }
            }
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(100)) => {}
            command = operator.recv() => match command {
                Some(Operator::Admit(code)) => { admitted.insert(code); }
                Some(Operator::Shutdown) | None => return Ok(()),
            },
        }
    }
}

/// A short-lived shared room: a throwaway user root kept under `--state`,
/// the browser listener, and an approver that certifies only the browsers
/// the operator admits. Its first stdout line is the connection JSON `dev`
/// prints; each certified browser follows as a `member_certified` line.
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
    let web = relay
        .serve_web(web_config(&args.web, listen, &state))
        .await
        .context("start the browser listener")?;
    println!(
        "{}",
        serde_json::json!({
            "origin": web.origin,
            "plainOrigin": web.plain_origin,
            "userPublicKey": hex::encode(user),
            "certificateSha256": web.certificate_sha256,
            "allowedOrigins": args.web.allowed_origins,
        })
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
        result = approve_admitted(approver, &root, operator) => result?,
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

    async fn endpoint() -> iroh::endpoint::Endpoint {
        iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .bind()
            .await
            .unwrap()
    }

    /// A browser asks the host for a certificate; the dev approver answers.
    async fn certify_a_browser(relay: &Relay, user: [u8; 32]) -> CertificationOutcome {
        let device = key();
        let now = tilo_clock::unix_now_ns();
        let request = CertificationRequest {
            version: CERTIFICATION_SCHEMA_VERSION,
            user_public_key: user,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Browser".into(),
            requested: DeviceCapabilities::member(),
            nonce: rand::random(),
            issued_at_ns: now.saturating_sub(1_000_000_000),
            expires_at_ns: now.saturating_add(60_000_000_000),
            signature: [0; 64],
        }
        .sign(&device)
        .unwrap();
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
}
