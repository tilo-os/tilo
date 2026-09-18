//! Existing packet relay + authenticated qOS relay, with a replaceable native subscription.
use crate::SessionOwner;
use anyhow::{Context, Result, bail};
use iroh::{Endpoint, RelayMap, RelayMode, endpoint::presets};
use iroh_base::{EndpointAddr, RelayUrl};
use iroh_relay::server::{RelayConfig as PacketConfig, Server, ServerConfig};
use qos_relay::{
    Inbound, Relay, RelayClient, RelayCoeditSession, RelayConfig, WorkspaceBinding,
    daemon::RelayIdentity,
};
use qos_user_identity::RevocationList;
use serde_json::json;
use std::{net::Ipv4Addr, path::Path, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

pub struct NativeSubscription {
    session: RelayCoeditSession,
    ready: bool,
}
impl std::ops::Deref for NativeSubscription {
    type Target = RelayCoeditSession;
    fn deref(&self) -> &Self::Target {
        &self.session
    }
}
impl NativeSubscription {
    pub fn is_ready(&self) -> bool {
        self.ready
    }
    pub async fn recv(&mut self) -> Option<Inbound> {
        let event = self.session.recv().await;
        if matches!(event, Some(Inbound::Ready { .. })) {
            self.ready = true;
        }
        event
    }
    pub fn into_client(self) -> RelayClient {
        self.session.into_client()
    }
}
pub struct LocalHost {
    pub relay: Relay,
    packet: Server,
    pub address: EndpointAddr,
    pub packet_url: RelayUrl,
    mode: RelayMode,
}
struct Reconnect {
    task: tokio::task::JoinHandle<Result<NativeSubscription>>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Reconnect {
    async fn cancel(mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Ok(Ok(session)) = (&mut self.task).await {
            session.into_client().close().await;
        }
    }
}
impl Drop for Reconnect {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}
fn revocations(owner: &SessionOwner) -> Result<RevocationList> {
    let mut list = RevocationList::new();
    for entry in owner.peer().revocations() {
        list.insert(entry)?;
    }
    Ok(list)
}
impl LocalHost {
    pub async fn start(owner: &SessionOwner) -> Result<Self> {
        let mut config = ServerConfig::default();
        config.relay = Some(PacketConfig::new((Ipv4Addr::LOCALHOST, 0)));
        let packet = Server::spawn(config).await?;
        let packet_url: RelayUrl = format!(
            "http://{}",
            packet
                .http_addr()
                .context("packet relay HTTP listener unavailable")?
        )
        .parse()?;
        let mode = RelayMode::Custom(RelayMap::from(packet_url.clone()));
        let workspace = qos_relay::room_workspace_id(owner.peer().room())?;
        let state = owner.root().join("relay");
        let config = RelayConfig {
            state_root: state.clone(),
            identity_root: owner.root().join("host-identity"),
            full_replica_status: state.join("full-replica/status.json"),
            home_peer_bundle: state.join("home-peer.toml"),
            trust_policy: qos_relay::TrustPolicy::AllowList {
                users: vec![hex::encode(owner.identity.root.verifying_key().to_bytes())],
            },
            iroh_listen: (Ipv4Addr::LOCALHOST, 0).into(),
            workspaces: vec![WorkspaceBinding {
                id: workspace,
                pin: true,
                replication: qos_relay::ReplicationIntent::FullWorkspace,
                admitted_users: vec![],
            }],
            ..Default::default()
        };
        let relay = Relay::start_with_relay_mode(
            config,
            RelayIdentity {
                user_public_key: owner.identity.root.verifying_key().to_bytes(),
                device_signing_key: owner.identity.host.clone(),
                device_cert: owner.identity.host_cert.clone(),
                revocations: revocations(owner)?,
            },
            mode.clone(),
        )
        .await?;
        let this = Self {
            address: EndpointAddr::new(relay.node_addr().id).with_relay_url(packet_url.clone()),
            relay,
            packet,
            packet_url,
            mode,
        };
        this.update_membership(owner).await?;
        tokio::time::timeout(Duration::from_secs(15), this.relay.endpoint.online())
            .await
            .context("qOS relay did not become online")?;
        Ok(this)
    }
    pub async fn update_membership(&self, owner: &SessionOwner) -> Result<()> {
        let revoked = revocations(owner)?;
        let members = owner
            .peer()
            .certificates()
            .into_iter()
            .filter(|cert| {
                !revoked.is_revoked(&cert.device_public_key)
                    && owner
                        .peer()
                        .permissions(cert.device_public_key, web_time::SystemTime::now())
                        .is_ok()
            })
            .map(|cert| cert.device_public_key)
            .collect();
        self.relay
            .set_workspace_membership(
                qos_relay::room_workspace_id(owner.peer().room())?,
                true,
                members,
                owner.root().join("relay"),
            )
            .await?;
        Ok(())
    }
    fn begin_connection(&self, owner: &SessionOwner, delay: Duration) -> Result<Reconnect> {
        let mode = self.mode.clone();
        let address = self.address.clone();
        let root = owner.identity.root.verifying_key().to_bytes();
        let host = owner.identity.host_cert.device_public_key;
        let certificate = owner.identity.native_cert.clone();
        let key = owner.identity.native.clone();
        let revoked = revocations(owner)?;
        let workspace = qos_relay::room_workspace_id(owner.peer().room())?;
        let (cancel, mut cancelled) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            tokio::select! { _=tokio::time::sleep(delay)=>{}, _=&mut cancelled=>bail!("native reconnect cancelled") }
            let endpoint = Endpoint::builder(presets::Minimal)
                .relay_mode(mode)
                .bind_addr((Ipv4Addr::LOCALHOST, 0))?
                .bind()
                .await?;
            let attempt = async {
                endpoint.online().await;
                let client = RelayClient::connect_to_enrolled_home_peer(
                    endpoint.clone(),
                    address,
                    root,
                    host,
                    certificate.clone(),
                    key,
                    revoked,
                )
                .await?;
                let session =
                    RelayCoeditSession::join(client, workspace, certificate.device_public_key)?;
                // The relay streams pending events BEFORE Ready. Admit them in the
                // ordinary serialized owner loop, with transport backpressure.
                Ok(NativeSubscription {
                    session,
                    ready: false,
                })
            };
            let result = tokio::select! {
                result=tokio::time::timeout(Duration::from_secs(4),attempt)=>result.context("native reconnect timed out").and_then(|value|value),
                _=&mut cancelled=>Err(anyhow::anyhow!("native reconnect cancelled")),
            };
            if result.is_err() {
                endpoint.close().await;
            }
            result
        });
        Ok(Reconnect {
            task,
            cancel: Some(cancel),
        })
    }
    pub async fn connect(&self, owner: &SessionOwner) -> Result<NativeSubscription> {
        let mut reconnect = self.begin_connection(owner, Duration::ZERO)?;
        (&mut reconnect.task)
            .await
            .context("native reconnect task failed")?
    }
    pub async fn shutdown(self) -> Result<()> {
        self.relay.shutdown().await;
        self.packet.shutdown().await?;
        Ok(())
    }
}
/// Publication never creates success: these exact messages were committed with the canonical aggregate.
pub fn publish(owner: &SessionOwner, session: &NativeSubscription) -> Result<()> {
    if !session.is_ready() {
        bail!("native subscription has not finished its initial replay");
    }
    use qos_peer_state::transport::{Retention, retention};
    if !owner.permits_publication() {
        bail!("durable barrier failed; publication is suspended until restore");
    }
    for bytes in owner.outgoing() {
        let message: qos_peer_state::NeighbourMsg = postcard::from_bytes(bytes)?;
        match retention(&message, true) {
            Some((channel, Retention::Checkpoint)) => {
                session.publish_checkpoint(channel, bytes.clone())?
            }
            Some((channel, Retention::Delta)) => {
                session.publish_retained_delta(channel, bytes.clone())?
            }
            None => session.publish_presence(bytes.clone())?,
        }
    }
    Ok(())
}
async fn serve_status(mut socket: TcpStream, status: serde_json::Value) -> Result<()> {
    let mut request = Vec::new();
    let mut chunk = [0u8; 2048];
    while !request.windows(4).any(|part| part == b"\r\n\r\n") {
        let count = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut chunk)).await??;
        if count == 0 {
            return Ok(());
        }
        request.extend_from_slice(&chunk[..count]);
        if request.len() > 8192 {
            bail!("status request too large");
        }
    }
    let request = std::str::from_utf8(&request)?;
    let first = request.lines().next().unwrap_or_default();
    let (code, body) = if first == "GET /status HTTP/1.1" {
        (200, status)
    } else {
        (404, json!({"error":"route unavailable"}))
    };
    let bytes = serde_json::to_vec(&body)?;
    socket.write_all(format!("HTTP/1.1 {code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", bytes.len()).as_bytes()).await?;
    socket.write_all(&bytes).await?;
    socket.shutdown().await?;
    Ok(())
}
fn public_status(
    owner: &SessionOwner,
    connected: bool,
    error: &Option<String>,
) -> serde_json::Value {
    let mut value = serde_json::to_value(owner.status()).unwrap_or_else(|_| json!({}));
    value["native_peer"] = json!(if connected {
        "subscribed"
    } else {
        "disconnected"
    });
    value["application_state"] = json!(if owner.permits_publication() {
        "admitted"
    } else {
        "incomplete"
    });
    value["error"] = json!(
        error
            .as_ref()
            .cloned()
            .or_else(|| owner.peer().incomplete_reason())
    );
    value
}
pub async fn run(mut owner: SessionOwner, ready_path: &Path) -> Result<()> {
    let host = LocalHost::start(&owner).await?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let mut session: Option<NativeSubscription> = None;
    let mut error = None;
    let mut reconnect: Option<Reconnect> = None;
    let mut delay = Duration::ZERO;
    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut stdin_open = true;
    let metadata = json!({"version":1,"pid":std::process::id(),"platform":std::env::consts::OS,"fixture_origin":format!("http://{}",listener.local_addr()?),"status_path":"/status","packet_relay":host.packet_url,"relay_address":host.address,"room":owner.peer().room(),"workspace_id":qos_relay::room_workspace_id(owner.peer().room())?.to_string(),"state_root":owner.root(),"records":owner.peer().rows(),"enrollments":[]});
    atomic_file::write_with_mode(ready_path, &serde_json::to_vec_pretty(&metadata)?, 0o600)?;
    crate::store::sync_dir(
        ready_path
            .parent()
            .context("ready file requires parent directory")?,
    )?;
    loop {
        if session.is_none() && reconnect.is_none() && owner.permits_publication() {
            reconnect = Some(host.begin_connection(&owner, delay)?);
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            connection = async { match reconnect.as_mut() {Some(attempt)=>(&mut attempt.task).await.context("native reconnect task failed")?,None=>std::future::pending().await} }, if reconnect.is_some() => {
                reconnect.take();
                match connection {
                    Ok(next) => {
                        session=Some(next); error=Some("Native subscription is applying retained state".into()); delay=Duration::from_secs(1);
                    }
                    Err(problem) => { error = Some(problem.to_string()); delay = if delay.is_zero() { Duration::from_secs(1) } else { (delay * 2).min(Duration::from_secs(4)) }; }
                }
            }
            inbound = async { match session.as_mut() { Some(current) => current.recv().await, None => std::future::pending().await } } => {
                match inbound {
                    Some(Inbound::Ready {..}) => {
                        owner.refresh_checkpoint()?;
                        if let Some(current)=session.as_ref() {
                            match publish(&owner,current) {
                                Ok(())=>error=None,
                                Err(problem)=>{error=Some(problem.to_string());if let Some(old)=session.take(){old.into_client().close().await;}}
                            }
                        }
                    },
                    Some(message) => {
                        let (origin, bytes) = match message {
                            Inbound::Delta {author,data} => (qos_peer_state::TransportOrigin::RelayDelta(author),data),
                            Inbound::Presence {author,payload} => (qos_peer_state::TransportOrigin::UntrustedPresence(author),payload),
                            Inbound::Ready {..} => unreachable!(),
                        };
                        match postcard::from_bytes(&bytes).map_err(anyhow::Error::from).and_then(|message|owner.receive(origin,message)) {
                            Ok(receipt) => {
                                if !owner.permits_publication() {error=owner.peer().incomplete_reason();if let Some(old)=session.take(){old.into_client().close().await;}}
                                else if receipt.is_some() { if let Some(current) = session.as_ref().filter(|current|current.is_ready()) { if let Err(problem) = publish(&owner,current) { error = Some(problem.to_string()); if let Some(old)=session.take(){ old.into_client().close().await; } } } }
                            },
                            Err(problem) => { error = Some(problem.to_string()); if !owner.permits_publication() { if let Some(old)=session.take(){old.into_client().close().await;} } }
                        }
                    }
                    None => { error=Some("native subscription disconnected; reconnecting with durable state".into()); if let Some(old)=session.take(){old.into_client().close().await;} }
                }
            }
            result = listener.accept() => {
                let (socket,_)=result?; let status=public_status(&owner,session.as_ref().is_some_and(|current|current.is_ready()),&error);
                tokio::spawn(async move { let _=serve_status(socket,status).await; });
            }
            line = stdin.next_line(), if stdin_open => {
                match line? {
                    None => stdin_open=false,
                    Some(line) if line == "shutdown" => break,
                    Some(line) if line == "reconnect" => { if let Some(old)=session.take(){old.into_client().close().await;} delay=Duration::ZERO; },
                    Some(line) if line == "status" => println!("{}",public_status(&owner,session.as_ref().is_some_and(|current|current.is_ready()),&error)),
                    Some(line) if line == "flush" => println!("{}",serde_json::to_string(&owner.flush()?)?),
                    Some(line) if line.starts_with("export ") => { match owner.export(Path::new(&line[7..])) {Ok(())=>println!("Export committed"),Err(problem)=>eprintln!("Export failed: {problem}")} },
                    Some(_) => eprintln!("Use status, flush, reconnect, export /absolute/path, or shutdown"),
                }
            }
        }
    }
    if let Some(attempt) = reconnect.take() {
        attempt.cancel().await;
    }
    let flushed = owner.flush();
    if let Some(current) = session {
        current.into_client().close().await;
    }
    host.shutdown().await?;
    std::fs::remove_file(ready_path)?;
    flushed?;
    Ok(())
}
