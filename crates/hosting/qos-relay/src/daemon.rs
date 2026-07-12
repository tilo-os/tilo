//! The long-running daemon: binds an iroh endpoint, accepts on
//! `qos-relay/1`, runs the auth handshake, hands off to
//! [`crate::peer::run_peer`].
//!
//! Also exposes the `Relay` struct so integration tests can spin up
//! two relays in the same process and let them merge.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{Endpoint, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh_base::{EndpointAddr, PublicKey};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use qos_user_identity::{DeviceCert, RevocationList};

use crate::auth::{self, Hello, HelloAck2, PeerRole};
use crate::client::RelayClient;
use crate::config::{PeerRelayConfig, RelayConfig};
use crate::metrics::Metrics;
use crate::peer::{WorkspaceRegistry, now_ns, run_peer};
use crate::policy::TrustPolicy;
use crate::store::load_or_create_iroh_secret;
use crate::transport::{ALPN, Envelope, MAX_FRAME_BYTES};
use crate::workspace::{Membership, SharedWorkspace, WorkspaceRuntime};

/// Failure modes for `Relay::start`.
#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    /// Iroh endpoint bind / accept.
    #[error("iroh: {0}")]
    Iroh(String),
    /// On-disk persistence.
    #[error("store: {0}")]
    Store(#[from] crate::store::StoreError),
    /// Config load / save.
    #[error("config: {0}")]
    Config(#[from] crate::config::ConfigError),
    /// I/O — bubbled up from filesystem ops during startup.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Identity file missing / malformed.
    #[error("identity: {0}")]
    Identity(String),
}

/// A running relay. Dropping the handle stops the daemon (and every
/// federation dial task with it).
pub struct Relay {
    /// Iroh endpoint the daemon accepts on.
    pub endpoint: Endpoint,
    /// Iroh router that wires the ALPN → protocol handler.
    _router: Router,
    /// Runtime state shared with peer tasks.
    workspaces: WorkspaceRegistry,
    /// Metrics — cheap counters.
    metrics: Arc<Metrics>,
    /// Own cert (device cert, cap `can_act_as_host`).
    own_cert: DeviceCert,
    /// Own device signing key.
    own_signing_key: ed25519_dalek::SigningKey,
    /// Own user pubkey — the one this relay chains to.
    own_user_public_key: [u8; 32],
    /// Federation dial tasks — one per known peer relay. Aborted
    /// (per-handle) when the `Relay` drops.
    peer_relay_tasks: Arc<std::sync::Mutex<Vec<AbortOnDrop>>>,
}

/// A tokio `JoinHandle` that aborts on drop — used so that dropping
/// `Relay` tears down every outbound peer-relay reconnect task.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Cheap clone-able handle for admin/observer callers.
#[derive(Clone)]
pub struct RelayHandle {
    /// Reachability info a peer can dial.
    pub node_addr: EndpointAddr,
    /// Metrics view — read-only.
    pub metrics: Arc<Metrics>,
}

/// Bundle of identity material the relay needs to run. Loaded by
/// the daemon's `main.rs` from
/// [`qos_user_identity::persist`] and passed in explicitly so the
/// integration tests can wire a `tempdir`-based identity.
pub struct RelayIdentity {
    /// The user root pubkey the relay chains to.
    pub user_public_key: [u8; 32],
    /// This device's signing key.
    pub device_signing_key: ed25519_dalek::SigningKey,
    /// This device's signed cert (issued by the user root).
    pub device_cert: DeviceCert,
    /// Current revocation list (may be empty).
    pub revocations: RevocationList,
}

impl Relay {
    /// Start the daemon with the given config + identity.
    ///
    /// Binds an iroh endpoint (using the persistent secret at
    /// `<state_root>/iroh-secret.bin`), opens each bound workspace
    /// on disk, and spawns an accept task that runs the auth
    /// handshake + dispatches to `run_peer` for every inbound
    /// connection.
    pub async fn start(cfg: RelayConfig, identity: RelayIdentity) -> Result<Self, RelayError> {
        std::fs::create_dir_all(&cfg.state_root)?;
        let secret_bytes = load_or_create_iroh_secret(&cfg.state_root)?;
        let secret = iroh::SecretKey::from_bytes(&secret_bytes);

        let endpoint = Endpoint::builder()
            .secret_key(secret)
            .bind()
            .await
            .map_err(|e| RelayError::Iroh(e.to_string()))?;

        // Build workspace registry.
        let mut wsmap: HashMap<u64, SharedWorkspace> = HashMap::new();
        for binding in &cfg.workspaces {
            let rt = WorkspaceRuntime::open(
                &cfg.state_root,
                binding.id,
                binding.pin,
                Membership::default(),
            )?;
            wsmap.insert(binding.id, Arc::new(AsyncMutex::new(rt)));
        }
        let workspaces: WorkspaceRegistry = Arc::new(AsyncMutex::new(wsmap));

        let metrics = Arc::new(Metrics::default());

        let handler = RelayProtocol {
            workspaces: workspaces.clone(),
            trust_policy: cfg.trust_policy.clone(),
            revocations: Arc::new(std::sync::RwLock::new(identity.revocations.clone())),
            own_user_pk: identity.user_public_key,
            own_cert: identity.device_cert.clone(),
            own_sk: identity.device_signing_key.clone(),
            metrics: metrics.clone(),
        };
        let router = Router::builder(endpoint.clone())
            .accept(ALPN, handler)
            .spawn();

        // Spawn one reconnect-with-backoff task per configured peer
        // relay. Each task dials via `RelayClient::connect` (same code
        // path any client uses), exchanges `PeerRelayHello`, subscribes
        // to the intersection, then feeds inbound Deltas into the
        // local `WorkspaceRuntime::fanout_delta` — no new fan-out.
        let peer_relay_tasks: Arc<std::sync::Mutex<Vec<AbortOnDrop>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        for peer_cfg in cfg.peer_relays.iter().cloned() {
            let task = spawn_peer_relay_dial(
                peer_cfg,
                endpoint.clone(),
                identity.user_public_key,
                identity.device_cert.clone(),
                identity.device_signing_key.clone(),
                identity.revocations.clone(),
                workspaces.clone(),
                metrics.clone(),
            );
            peer_relay_tasks.lock().unwrap().push(AbortOnDrop(task));
        }

        Ok(Self {
            endpoint,
            _router: router,
            workspaces,
            metrics,
            own_cert: identity.device_cert,
            own_signing_key: identity.device_signing_key,
            own_user_public_key: identity.user_public_key,
            peer_relay_tasks,
        })
    }

    /// Add a peer relay after startup — used by tests + the eventual
    /// admin CLI. Same reconnect-with-backoff loop as the config-time
    /// path.
    pub fn add_peer_relay(&self, peer: PeerRelayConfig) {
        let node_id = match parse_iroh_node_id(&peer.iroh_node_id) {
            Some(id) => id,
            None => {
                tracing::warn!(?peer.iroh_node_id, "peer relay iroh_node_id not parseable, skipping");
                return;
            }
        };
        self.add_peer_relay_at(EndpointAddr::new(node_id), peer.label, peer.priority);
    }

    /// Add a peer relay with a fully-resolved endpoint address —
    /// same dial loop, but skips iroh discovery. Useful for tests
    /// that already know the target's iroh addresses, and for
    /// pinned-address deployments.
    pub fn add_peer_relay_at(&self, addr: EndpointAddr, label: String, priority: i32) {
        let task = spawn_peer_relay_dial_at(
            addr,
            label,
            priority,
            self.endpoint.clone(),
            self.own_user_public_key,
            self.own_cert.clone(),
            self.own_signing_key.clone(),
            RevocationList::new(),
            self.workspaces.clone(),
            self.metrics.clone(),
        );
        self.peer_relay_tasks
            .lock()
            .unwrap()
            .push(AbortOnDrop(task));
    }

    /// Cheap external handle.
    pub fn handle(&self) -> RelayHandle {
        RelayHandle {
            node_addr: self.endpoint.addr(),
            metrics: self.metrics.clone(),
        }
    }

    /// The iroh endpoint address (node id + reachable addresses) —
    /// what a peer needs to dial this relay.
    pub fn node_addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// Bind (or re-bind) a workspace's membership set. Called by
    /// the admin CLI after `bind-workspace`.
    pub async fn set_workspace_membership(
        &self,
        workspace_id: u64,
        pinned: bool,
        members: Vec<[u8; 32]>,
        state_root: PathBuf,
    ) -> Result<(), RelayError> {
        let mut map = self.workspaces.lock().await;
        if let Some(existing) = map.get(&workspace_id).cloned() {
            existing.lock().await.membership.set(members);
            return Ok(());
        }
        let mut rt = WorkspaceRuntime::open(&state_root, workspace_id, pinned, Membership::default())?;
        rt.membership.set(members);
        map.insert(workspace_id, Arc::new(AsyncMutex::new(rt)));
        Ok(())
    }

    /// Own user identity — for tests.
    pub fn own_user_public_key(&self) -> [u8; 32] {
        self.own_user_public_key
    }
}

/// The `ProtocolHandler` iroh wires to the ALPN.
#[derive(Clone)]
struct RelayProtocol {
    workspaces: WorkspaceRegistry,
    trust_policy: TrustPolicy,
    revocations: Arc<std::sync::RwLock<RevocationList>>,
    own_user_pk: [u8; 32],
    own_cert: DeviceCert,
    own_sk: ed25519_dalek::SigningKey,
    metrics: Arc<Metrics>,
}

impl std::fmt::Debug for RelayProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RelayProtocol")
    }
}

impl ProtocolHandler for RelayProtocol {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let this = self.clone();
        let (send, recv) = connection.accept_bi().await?;
        // Run the whole session in a separate task so we return
        // promptly and the iroh router can accept the next
        // connection.
        tokio::spawn(async move {
            if let Err(e) = handle_conn(this, connection, send, recv).await {
                tracing::debug!("relay conn ended: {e}");
            }
        });
        Ok(())
    }
}

async fn handle_conn(
    ctx: RelayProtocol,
    connection: iroh::endpoint::Connection,
    mut send: SendStream,
    mut recv: RecvStream,
) -> Result<(), RelayError> {
    ctx.metrics.inc_conns_accepted();
    // 1. Read the client Hello.
    let hello_bytes = read_frame(&mut recv, MAX_FRAME_BYTES)
        .await
        .map_err(|e| RelayError::Iroh(format!("read hello: {e}")))?;
    let hello: Hello = postcard::from_bytes(&hello_bytes)
        .map_err(|e| RelayError::Iroh(format!("decode hello: {e}")))?;
    let now_ns = crate::peer::now_ns();
    let revs = ctx.revocations.read().unwrap().clone();
    let client_peer = match auth::accept_hello(&hello, &ctx.own_user_pk, &ctx.trust_policy, &revs, now_ns) {
        Ok(p) => p,
        Err(e) => {
            ctx.metrics.inc_auth_rejected();
            return Err(RelayError::Iroh(format!("auth reject: {e}")));
        }
    };
    // 2. Reply with HelloAck.
    let mut server_nonce = [0u8; 32];
    {
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut server_nonce);
    }
    let ack = auth::build_hello_ack(ctx.own_cert.clone(), &ctx.own_sk, server_nonce, &hello.client_nonce);
    let ack_bytes = postcard::to_allocvec(&ack)
        .map_err(|e| RelayError::Iroh(format!("encode ack: {e}")))?;
    write_frame(&mut send, &ack_bytes)
        .await
        .map_err(|e| RelayError::Iroh(format!("write ack: {e}")))?;
    // 3. Read HelloAck2.
    let ack2_bytes = read_frame(&mut recv, MAX_FRAME_BYTES)
        .await
        .map_err(|e| RelayError::Iroh(format!("read ack2: {e}")))?;
    let ack2: HelloAck2 = postcard::from_bytes(&ack2_bytes)
        .map_err(|e| RelayError::Iroh(format!("decode ack2: {e}")))?;
    if let Err(e) = auth::verify_hello_ack2(&ack2, &client_peer.device_public_key, &server_nonce) {
        ctx.metrics.inc_auth_rejected();
        return Err(RelayError::Iroh(format!("auth ack2 reject: {e}")));
    }

    ctx.metrics.inc_conns_authed();

    // 4. Hand off to the peer loop.
    let workspaces = ctx.workspaces.clone();
    let res = run_peer(client_peer, recv, send, workspaces).await;
    connection.closed().await;
    if let Err(e) = res {
        tracing::debug!("peer session ended with error: {e}");
    }
    Ok(())
}

/// Read a 4-byte-LE-prefix framed message from `recv`.
pub async fn read_frame(
    recv: &mut RecvStream,
    max: usize,
) -> Result<Vec<u8>, std::io::Error> {
    let mut prefix = [0u8; 4];
    recv.read_exact(&mut prefix).await.map_err(std::io::Error::other)?;
    let n = u32::from_le_bytes(prefix) as usize;
    if n == 0 || n > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame size {} out of range", n),
        ));
    }
    let mut buf = vec![0u8; n];
    recv.read_exact(&mut buf).await.map_err(std::io::Error::other)?;
    Ok(buf)
}

/// Write a 4-byte-LE-prefix framed message to `send`.
pub async fn write_frame(send: &mut SendStream, data: &[u8]) -> Result<(), std::io::Error> {
    send.write_all(&(data.len() as u32).to_le_bytes()).await?;
    if !data.is_empty() {
        send.write_all(data).await?;
    }
    Ok(())
}

/// Spawn the outbound reconnect-with-backoff task for one peer
/// relay identified by its config entry. Parses the iroh id and
/// hands off to [`spawn_peer_relay_dial_at`].
fn spawn_peer_relay_dial(
    peer_cfg: PeerRelayConfig,
    endpoint: Endpoint,
    own_user_pk: [u8; 32],
    own_cert: DeviceCert,
    own_sk: ed25519_dalek::SigningKey,
    revocations: RevocationList,
    workspaces: WorkspaceRegistry,
    metrics: Arc<Metrics>,
) -> JoinHandle<()> {
    let node_id = parse_iroh_node_id(&peer_cfg.iroh_node_id);
    let label = peer_cfg.label.clone();
    let priority = peer_cfg.priority;
    tokio::spawn(async move {
        let node_id = match node_id {
            Some(id) => id,
            None => {
                tracing::warn!(?peer_cfg.iroh_node_id, "peer relay iroh_node_id not parseable, skipping");
                return;
            }
        };
        let addr = EndpointAddr::new(node_id);
        peer_relay_dial_loop(
            addr, label, priority, endpoint, own_user_pk, own_cert, own_sk,
            revocations, workspaces, metrics,
        )
        .await;
    })
}

/// Spawn the outbound reconnect-with-backoff task for one peer
/// relay, with a fully-resolved endpoint address. The task loops
/// forever (until the returned join handle is aborted) — reconnect
/// gap is `1s, 2s, 4s, 8s, …, capped at 60s`. While connected it
/// holds a `RelayClient` session and mirrors every inbound Delta
/// into the local workspace runtime via the SAME `fanout_delta`
/// device peers use — that's the reuse: no separate federation
/// delivery path.
#[allow(clippy::too_many_arguments)]
fn spawn_peer_relay_dial_at(
    addr: EndpointAddr,
    label: String,
    priority: i32,
    endpoint: Endpoint,
    own_user_pk: [u8; 32],
    own_cert: DeviceCert,
    own_sk: ed25519_dalek::SigningKey,
    revocations: RevocationList,
    workspaces: WorkspaceRegistry,
    metrics: Arc<Metrics>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        peer_relay_dial_loop(
            addr, label, priority, endpoint, own_user_pk, own_cert, own_sk,
            revocations, workspaces, metrics,
        )
        .await;
    })
}

#[allow(clippy::too_many_arguments)]
async fn peer_relay_dial_loop(
    addr: EndpointAddr,
    label: String,
    _priority: i32,
    endpoint: Endpoint,
    own_user_pk: [u8; 32],
    own_cert: DeviceCert,
    own_sk: ed25519_dalek::SigningKey,
    revocations: RevocationList,
    workspaces: WorkspaceRegistry,
    _metrics: Arc<Metrics>,
) {
    let mut backoff_ms: u64 = 1_000;
    loop {
        let ep = endpoint.clone();
        match RelayClient::connect(
            ep,
            addr.clone(),
            own_user_pk,
            own_cert.clone(),
            own_sk.clone(),
            revocations.clone(),
        )
        .await
        {
            Ok(client) => {
                tracing::info!(%label, "peer relay federated");
                backoff_ms = 1_000;
                if let Err(e) = run_peer_relay_session(&client, &workspaces).await {
                    tracing::debug!(%label, "peer relay session ended: {e}");
                }
            }
            Err(e) => {
                tracing::debug!(%label, "peer relay dial failed: {e}");
            }
        }
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms * 2).min(60_000);
    }
}

/// One iteration of the federated peer-relay session. Sends a
/// `PeerRelayHello` with the workspaces we host, waits for the
/// peer's `PeerRelayHello`, `Subscribe`s to the intersection, then
/// drains inbound Deltas + Presence into the LOCAL workspace
/// runtime via `fanout_delta` / `fanout_presence` — the very same
/// entry points a device peer's incoming envelope hits.
async fn run_peer_relay_session(
    client: &RelayClient,
    workspaces: &WorkspaceRegistry,
) -> Result<(), String> {
    // 1. Advertise what we host.
    let hosted_local: Vec<u64> = {
        let map = workspaces.lock().await;
        map.keys().copied().collect()
    };
    client
        .send(Envelope::PeerRelayHello {
            hosted_workspaces: hosted_local.clone(),
        })
        .map_err(|_| "outbound closed".to_string())?;

    // 2. Wait for the peer's PeerRelayHello, then subscribe to
    //    the intersection.
    let mut intersection: Vec<u64> = Vec::new();
    let mut hello_seen = false;
    // Bounded wait for the hello — do a few short recv polls.
    for _ in 0..25 {
        let env = tokio::time::timeout(Duration::from_millis(200), client.recv()).await;
        match env {
            Ok(Some(Envelope::PeerRelayHello { hosted_workspaces })) => {
                let local: std::collections::HashSet<u64> = hosted_local.iter().copied().collect();
                for id in hosted_workspaces {
                    if local.contains(&id) {
                        intersection.push(id);
                    }
                }
                hello_seen = true;
                break;
            }
            Ok(Some(other)) => {
                // Not the hello we wanted; buffer nothing — plain
                // devices should not talk on this session. Just log.
                tracing::debug!("peer-relay session got unexpected pre-hello envelope: {other:?}");
            }
            Ok(None) => return Err("connection closed pre-hello".into()),
            Err(_) => {} // timeout; keep waiting a bit
        }
    }
    if !hello_seen {
        return Err("peer relay never sent PeerRelayHello".into());
    }

    // 3. Subscribe to each intersection workspace. This is a plain
    //    `Subscribe` envelope — the accepter side's `handle_envelope`
    //    registers us as a subscriber-with-role-PeerRelay (because
    //    our own cert has `can_act_as_host = true`, that role was
    //    stamped at handshake time).
    for id in &intersection {
        client
            .send(Envelope::Subscribe { workspace_id: *id })
            .map_err(|_| "outbound closed on subscribe".to_string())?;
    }

    // 4. Drain inbound envelopes for the life of this connection AND
    //    periodically run anti-entropy against the upstream relay.
    //
    //    Live fan-out (Delta / Presence) is forwarded LOCALLY via the
    //    same `fanout_delta` / `fanout_presence` device peers use;
    //    `PeerRole::PeerRelay` suppresses re-echo to other peer relays.
    //
    //    Anti-entropy is the recovery tier layered on top: on a timer we
    //    advertise our local materialized heads (`SyncRequest`) so the
    //    upstream backfills any change we are missing. This heals two
    //    cases live fan-out cannot: a change kept off the live path by
    //    echo-suppression (the middle hop of a relay chain — a relay does
    //    not re-forward a PeerRelay-origin delta to another peer relay),
    //    and anything missed across a reconnect/restart. A converged pair
    //    exchanges empty `SyncResponse`s, so this quiesces to near-zero
    //    work once caught up.
    let mut resync = tokio::time::interval(Duration::from_millis(500));
    resync.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            // `interval` fires its first tick immediately, so the initial
            // resync goes out as soon as we enter the loop.
            _ = resync.tick() => {
                for id in &intersection {
                    send_sync_request_for(client, workspaces, *id).await;
                }
            }
            maybe = client.recv() => {
                let env = match maybe {
                    Some(e) => e,
                    None => return Ok(()), // clean EOF
                };
                match &env {
                    Envelope::Delta {
                        workspace_id,
                        author_device_public_key,
                        delta,
                        ..
                    } => {
                        let ws = {
                            let map = workspaces.lock().await;
                            map.get(workspace_id).cloned()
                        };
                        if let Some(ws) = ws {
                            let mut wr = ws.lock().await;
                            let raw = delta.clone();
                            let seq = now_ns();
                            let author = *author_device_public_key;
                            let _ = wr.fanout_delta(
                                &author,
                                PeerRole::PeerRelay,
                                env.clone(),
                                &raw,
                                seq,
                            );
                        }
                    }
                    Envelope::Presence { workspace_id, .. } => {
                        let ws = {
                            let map = workspaces.lock().await;
                            map.get(workspace_id).cloned()
                        };
                        if let Some(ws) = ws {
                            // Presence has no persistent author field on
                            // the wire; use zero as a sentinel — presence
                            // fanout only compares to subscriber pubkeys
                            // and the "don't echo to peer relays" filter.
                            ws.lock()
                                .await
                                .fanout_presence(&[0u8; 32], PeerRole::PeerRelay, env.clone());
                        }
                    }
                    Envelope::SyncResponse {
                        workspace_id,
                        delta_since,
                    } => {
                        // The upstream backfilled changes we were missing.
                        // Materialize them into our pinned doc and fan the
                        // batch out to our LOCAL devices so they converge —
                        // even when echo-suppression kept these changes off
                        // the live fan-out path.
                        if !delta_since.is_empty() {
                            let ws = {
                                let map = workspaces.lock().await;
                                map.get(workspace_id).cloned()
                            };
                            if let Some(ws) = ws {
                                let seq = now_ns();
                                let _ = ws.lock().await.ingest_sync_changes(
                                    *workspace_id,
                                    delta_since,
                                    seq,
                                );
                            }
                        }
                    }
                    Envelope::Ready { .. } => {}
                    Envelope::NotAuthorised { workspace_id, reason } => {
                        tracing::warn!(workspace_id, %reason, "peer relay refused subscribe");
                    }
                    Envelope::Ping => {
                        let _ = client.send(Envelope::Pong);
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Advertise our local materialized heads for `ws_id` to the upstream
/// peer relay via a [`Envelope::SyncRequest`]. Best-effort: a workspace
/// we don't host, or a closed outbound, is silently skipped.
async fn send_sync_request_for(
    client: &RelayClient,
    workspaces: &WorkspaceRegistry,
    ws_id: u64,
) {
    let have_heads = {
        let map = workspaces.lock().await;
        match map.get(&ws_id) {
            Some(ws) => ws.lock().await.current_heads(),
            None => return,
        }
    };
    let _ = client.send(Envelope::SyncRequest {
        workspace_id: ws_id,
        have_heads,
    });
}

/// Parse the hex-encoded iroh node id string into an iroh
/// `PublicKey`. Accepts upper/lowercase.
fn parse_iroh_node_id(hex_id: &str) -> Option<PublicKey> {
    let bytes = hex::decode(hex_id).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    PublicKey::from_bytes(&arr).ok()
}
