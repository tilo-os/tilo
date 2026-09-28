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
}

/// The relay the command line started.
pub struct HostedRelay {
    pub relay: Relay,
}

impl HostedRelay {
    /// Stop the relay.
    pub async fn shutdown(self) {
        self.relay.shutdown().await;
    }
}

/// Start the relay.
///
/// # Errors
/// The relay did not start.
pub async fn start_relay(
    cfg: tilo_relay::RelayConfig,
    identity: RelayIdentity,
    _args: &WebArgs,
    _state_root: &Path,
) -> anyhow::Result<HostedRelay> {
    let relay = Relay::start(cfg, identity)
        .await
        .context("start the relay")?;
    Ok(HostedRelay { relay })
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
            atomic_file::write_with_mode(&path, &seeds, 0o600).context("save the dev identity")?;
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

/// Certify every browser device that asks, as the dev user root, and
/// approve every agent grant a page asks for with its delegation.
async fn approve_every_browser(
    mut approver: tilo_relay_client::Approver,
    root: &SigningKey,
) -> anyhow::Result<()> {
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

/// What a page (or Cloud's `dev:tilo`) needs to connect: one JSON line on
/// stdout, and instructions on stderr.
fn announce(web: &WebHandle, user: &[u8; 32], allowed: &[String], state: &Path) {
    let localhost = format!("https://localhost:{}", web.addr.port());
    let user = hex::encode(user);
    println!(
        "{}",
        serde_json::json!({
            "origin": web.origin,
            "localhostOrigin": localhost,
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
         WebTransport pins it by hash.\n",
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
    announce(&web, &user, &web_args.allowed_origins, &state);
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
    .context("connect the dev approver")?;
    tokio::select! {
        result = approve_every_browser(approver, &root) => result?,
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
                        Some(code) if !code.trim().is_empty() => Operator::Admit(code.trim().into()),
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
                if agents.iter().any(|agent| agent.request_digest == pending.request_digest)
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
    let approver_cert = enrol(&root, &approver_key, "room approver", DeviceCapabilities::primary())?;
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
