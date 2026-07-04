//! The long-running daemon: binds an iroh endpoint, accepts on
//! `qos-relay/1`, runs the auth handshake, hands off to
//! [`crate::peer::run_peer`].
//!
//! Also exposes the `Relay` struct so integration tests can spin up
//! two relays in the same process and let them merge.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use iroh::endpoint::{Endpoint, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh_base::EndpointAddr;
use tokio::sync::Mutex as AsyncMutex;

use qos_user_identity::{DeviceCert, RevocationList};

use crate::auth::{self, Hello, HelloAck2};
use crate::config::RelayConfig;
use crate::metrics::Metrics;
use crate::peer::{WorkspaceRegistry, run_peer};
use crate::policy::TrustPolicy;
use crate::store::load_or_create_iroh_secret;
use crate::transport::{ALPN, MAX_FRAME_BYTES};
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

/// A running relay. Dropping the handle stops the daemon.
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
    #[allow(dead_code)]
    own_cert: DeviceCert,
    /// Own device signing key.
    #[allow(dead_code)]
    own_signing_key: ed25519_dalek::SigningKey,
    /// Own user pubkey — the one this relay chains to.
    own_user_public_key: [u8; 32],
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

        Ok(Self {
            endpoint,
            _router: router,
            workspaces,
            metrics,
            own_cert: identity.device_cert,
            own_signing_key: identity.device_signing_key,
            own_user_public_key: identity.user_public_key,
        })
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
