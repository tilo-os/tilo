//! Peer-relay federation, the dialing side: a host keeps a session to an
//! upstream host of its own person and holds the workspaces both host in
//! step with it.
//!
//! The session dials through the same handshake and client every device
//! uses, says which workspaces it carries (`PeerRelayHello`), and subscribes
//! to those both sides host. Subscribing replays everything the upstream
//! retains of each, as it does for any host that keeps a whole room, and
//! live fan-out carries every later change. Inbound changes are admitted
//! through the same ordered persistence owner device peers use.
//!
//! Anti-entropy (`SyncRequest`, answered with the changes the requester's
//! materialized heads lack) runs on events only: once when the session
//! starts, again for as long as an answer still brings something new, and
//! whenever the upstream says it withheld a change from this host. An
//! upstream does not forward a change another federated host sent it to a
//! third (that would loop a mesh), so a change crossing two hops reaches the
//! far host this way. A converged session does nothing until a change
//! arrives: no timer runs while it is connected.
//!
//! A session carries changes one way, from the host dialed to the host
//! dialing. Two certified hosts each dial the other. A device governing a
//! room another host of its person serves keeps no dial loop: as its host
//! opens the room it opens one session to each such host, which brings what
//! that host took in meanwhile, and that host dials it back at once
//! ([`PeerRelayDialer::spawn_woken`]). Each session such a host opens is
//! answered with one back ([`PeerRelayDialer::answer`]) that lasts as long
//! as it does.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use iroh::endpoint::Endpoint;
use iroh_base::EndpointAddr;
use tilo_lock::RwLockRecover as _;
use tilo_relay_session::client::RelayClient;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::auth::PeerRole;
use crate::handshake::LiveIdentity;
use crate::metrics::Metrics;
use crate::peer::WorkspaceRegistry;
use crate::transport::Envelope;

/// Which host a federation session dials, and how its identity is pinned.
#[derive(Clone, Debug)]
pub enum Upstream {
    /// A certified host of this host's own person, at `addr`.
    Relay(EndpointAddr),
    /// The device `device` of this host's own person, hosting its rooms at
    /// `addr`: the governor of a room this host also serves.
    OwnDevice {
        addr: EndpointAddr,
        device: [u8; 32],
    },
    /// The certified host `device` of this host's own person, at `addr`:
    /// a host serving a room this device governs.
    OwnHost {
        addr: EndpointAddr,
        device: [u8; 32],
    },
}

impl Upstream {
    const fn addr(&self) -> &EndpointAddr {
        match self {
            Self::Relay(addr) | Self::OwnDevice { addr, .. } | Self::OwnHost { addr, .. } => addr,
        }
    }
}

/// What an outbound federation session carries: the local endpoint, this
/// host's own identity (its revocation list read afresh for every dial and
/// session), the workspaces it hosts and its metrics. `scope`, when given,
/// is the only workspaces the session offers: a host serving one room for
/// its governor federates that room alone.
pub struct PeerRelayDialer {
    pub endpoint: Endpoint,
    pub identity: LiveIdentity,
    pub workspaces: WorkspaceRegistry,
    pub metrics: Arc<Metrics>,
    pub scope: Option<Vec<u64>>,
}

impl PeerRelayDialer {
    /// Spawn the reconnect-with-backoff task for `upstream`. The task runs
    /// until its handle is aborted. While connected it holds one session;
    /// after a session ends or a dial fails it waits `1s, 2s, 4s, …` up to
    /// 60s before dialing again.
    pub fn spawn(self, upstream: Upstream, label: String) -> JoinHandle<()> {
        self.spawn_woken(upstream, label, Arc::default())
    }

    /// [`Self::spawn`], dialing again at once whenever `wake` is notified
    /// while it waits out a backoff: the upstream is back and said so.
    pub fn spawn_woken(
        self,
        upstream: Upstream,
        label: String,
        wake: Arc<tokio::sync::Notify>,
    ) -> JoinHandle<()> {
        tokio::spawn(self.dial_loop(upstream, label, wake))
    }

    async fn dial_loop(self, upstream: Upstream, label: String, wake: Arc<tokio::sync::Notify>) {
        let mut backoff_ms: u64 = 1_000;
        loop {
            match self.dial(&upstream).await {
                Ok(client) => {
                    tracing::info!(%label, "peer relay federated");
                    backoff_ms = 1_000;
                    if let Err(error) = self.run_session(&client, true).await {
                        tracing::debug!(%label, "peer relay session ended: {error}");
                    }
                }
                Err(error) => {
                    tracing::debug!(%label, "peer relay dial failed: {error}");
                }
            }
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(backoff_ms)) => {}
                () = wake.notified() => backoff_ms = 1_000,
            }
            backoff_ms = backoff_ms.saturating_mul(2).min(60_000);
        }
    }

    async fn dial(&self, upstream: &Upstream) -> Result<RelayClient, String> {
        let identity = &self.identity;
        let revocations = identity.revocations.read_recover().clone();
        let addr = upstream.addr().clone();
        let client = match upstream {
            Upstream::Relay(_) => {
                RelayClient::connect(
                    self.endpoint.clone(),
                    addr,
                    identity.user_public_key,
                    identity.cert.clone(),
                    identity.signing_key.clone(),
                    revocations,
                )
                .await
            }
            Upstream::OwnDevice { device, .. } => {
                RelayClient::connect_v2_to_own_device(
                    self.endpoint.clone(),
                    addr,
                    identity.user_public_key,
                    *device,
                    identity.cert.clone(),
                    identity.signing_key.clone(),
                    revocations,
                )
                .await
            }
            Upstream::OwnHost { device, .. } => {
                RelayClient::connect_v2_to_enrolled_home_peer(
                    self.endpoint.clone(),
                    addr,
                    identity.user_public_key,
                    *device,
                    identity.cert.clone(),
                    identity.signing_key.clone(),
                    revocations,
                )
                .await
            }
        };
        client.map_err(|error| error.to_string())
    }

    /// Open one session to `upstream`, a host serving a room this device
    /// governs: what its members change there reaches this device that way,
    /// as what changes here reaches it over the host's own session.
    ///
    /// Given `inbound`, the session that host opened here, this answers it
    /// and lasts as long as it does; when it cannot be held, `inbound` is
    /// ended too, and the host, which keeps dialing while it is up, opens it
    /// again and is answered again. Without, it is the session this device
    /// opens as its host opens the room, and lasts until it ends or the
    /// host's own session is answered. This device keeps no timer for
    /// either.
    pub fn answer(
        self,
        upstream: Upstream,
        inbound: Option<mpsc::Sender<()>>,
        label: String,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let session = async {
                let client = self.dial(&upstream).await?;
                tracing::info!(%label, "in session with a host serving a room governed here");
                self.run_session(&client, false).await
            };
            let answered = async {
                match &inbound {
                    Some(inbound) => inbound.closed().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                result = session => {
                    if let Err(error) = result {
                        tracing::debug!(%label, "session to a host ended: {error}");
                    }
                    if let Some(inbound) = &inbound {
                        let _ = inbound.try_send(());
                    }
                }
                () = answered => {}
            }
        })
    }

    /// One federated session: agree on the workspaces both host (by
    /// `hello`, or this host's scope when the upstream serves a room this
    /// device governs), subscribe to them, then follow the upstream until
    /// the session ends.
    async fn run_session(&self, client: &RelayClient, hello: bool) -> Result<(), String> {
        let hosted_local = self.carried().await;
        let intersection = if hello {
            client
                .send(Envelope::PeerRelayHello {
                    hosted_workspaces: hosted_local.clone(),
                })
                .map_err(|_| "outbound closed".to_string())?;
            negotiate_intersection(client, &hosted_local).await?
        } else {
            hosted_local
        };
        for id in &intersection {
            client
                .send(Envelope::Subscribe { workspace_id: *id })
                .map_err(|_| "outbound closed on subscribe".to_string())?;
        }
        let lease = FederationLease(Arc::new(AtomicBool::new(true)));
        let (failed, mut failures) = tokio::sync::mpsc::channel(1);
        let source = crate::peer::StateSource::federated(
            client.relay_peer.clone(),
            client.outbound.clone(),
            failed,
            self.identity.revocations.clone(),
            self.identity.user_public_key,
            lease.0.clone(),
        );
        for id in &intersection {
            let workspace = self.workspaces.lock().await.get(id).cloned();
            if let Some(workspace) = workspace {
                let mut runtime = workspace.lock().await;
                runtime.record_peer_relay(client.relay_peer.device_public_key);
                if let Some(previous) = runtime
                    .outbound_federation
                    .insert(client.relay_peer.device_public_key, lease.0.clone())
                {
                    previous.store(false, Ordering::SeqCst);
                }
            }
        }
        let session = Session {
            client,
            workspaces: &self.workspaces,
            source: &source,
            metrics: &self.metrics,
        };
        tokio::select! {
            result = session.follow(&intersection) => result,
            _ = failures.recv() => Err("federated state persistence/authorization failed".into()),
        }
    }

    /// The workspaces this session carries: those in scope, or every
    /// workspace hosted here.
    async fn carried(&self) -> Vec<u64> {
        let map = self.workspaces.lock().await;
        match &self.scope {
            Some(scope) => scope
                .iter()
                .copied()
                .filter(|id| map.contains_key(id))
                .collect(),
            None => map.keys().copied().collect(),
        }
    }
}

/// Clears a federation session's lease on every workspace it registered
/// with once the session ends, so nothing is sent on it after.
struct FederationLease(Arc<AtomicBool>);

impl Drop for FederationLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Wait for the upstream's `PeerRelayHello` and intersect it with what this
/// host carries. The upstream answers a hello with its own before anything
/// else of the session; anything else first is logged and skipped.
async fn negotiate_intersection(
    client: &RelayClient,
    hosted_local: &[u64],
) -> Result<Vec<u64>, String> {
    loop {
        match client.recv().await {
            Some(Envelope::PeerRelayHello { hosted_workspaces }) => {
                let local: std::collections::HashSet<u64> = hosted_local.iter().copied().collect();
                return Ok(hosted_workspaces
                    .into_iter()
                    .filter(|id| local.contains(id))
                    .collect());
            }
            Some(other) => {
                tracing::debug!("peer-relay session got unexpected pre-hello envelope: {other:?}");
            }
            None => return Err("connection closed pre-hello".into()),
        }
    }
}

/// A connected federation session.
struct Session<'a> {
    client: &'a RelayClient,
    workspaces: &'a WorkspaceRegistry,
    source: &'a crate::peer::StateSource,
    metrics: &'a Arc<Metrics>,
}

impl Session<'_> {
    /// Follow the upstream for the life of the connection: ask once for what
    /// each shared workspace lacks, then admit what arrives, asking again
    /// only on an event (see the module documentation).
    async fn follow(&self, intersection: &[u64]) -> Result<(), String> {
        for id in intersection {
            self.ask(*id).await?;
        }
        // The last non-empty answer of each workspace: one repeated unchanged
        // brought nothing this host could take in, and asking again would
        // only repeat it.
        let mut answered: HashMap<u64, blake3::Hash> = HashMap::new();
        while let Some(envelope) = self.client.recv().await {
            match envelope {
                Envelope::SyncResponse {
                    workspace_id,
                    delta_since,
                } => {
                    if delta_since.is_empty() {
                        answered.remove(&workspace_id);
                        continue;
                    }
                    let digest = blake3::hash(&delta_since);
                    if answered.insert(workspace_id, digest) == Some(digest) {
                        continue;
                    }
                    self.backfill(workspace_id, delta_since).await?;
                    if intersection.contains(&workspace_id) {
                        self.ask(workspace_id).await?;
                    }
                }
                // From an upstream, a request means this host is behind: it
                // withheld a change another federated host sent it.
                Envelope::SyncRequest { workspace_id, .. } => {
                    if intersection.contains(&workspace_id) {
                        self.ask(workspace_id).await?;
                    }
                }
                other => self.admit(other).await?,
            }
        }
        Ok(())
    }

    async fn workspace(&self, id: u64) -> Option<crate::workspace::SharedWorkspace> {
        self.workspaces.lock().await.get(&id).cloned()
    }

    /// Admit one inbound envelope: a change through the ordered state owner
    /// (and to the room's follower, when one listens), presence to local
    /// subscribers, and the session's housekeeping.
    async fn admit(&self, envelope: Envelope) -> Result<(), String> {
        match &envelope {
            Envelope::Delta {
                workspace_id,
                author_device_public_key,
                delta,
                ..
            } => {
                let Some(workspace) = self.workspace(*workspace_id).await else {
                    return Ok(());
                };
                let tap = workspace.lock().await.governor_tap.clone();
                let notice = tap.and_then(|tap| {
                    let (_, _, body) = crate::coedit::retention_view(delta)?;
                    Some((tap, *author_device_public_key, body.to_vec()))
                });
                crate::peer::enqueue_federated(
                    &workspace,
                    envelope,
                    self.source.clone(),
                    self.metrics.clone(),
                    false,
                )
                .await
                .map_err(|error| error.to_string())?;
                if let Some((tap, author, body)) = notice {
                    crate::peer::tell_governor(&tap, author, body).await;
                }
            }
            Envelope::Presence { workspace_id, .. } => {
                if let Some(workspace) = self.workspace(*workspace_id).await {
                    // Presence carries no author on the wire; zero stands in.
                    // Fan-out only compares it to subscribers and keeps it
                    // from other federated hosts.
                    workspace.lock().await.fanout_presence(
                        &[0u8; 32],
                        PeerRole::PeerRelay,
                        &envelope,
                        None,
                    );
                }
            }
            Envelope::NotAuthorised {
                workspace_id,
                reason,
            } => {
                tracing::warn!(workspace_id, %reason, "peer relay refused subscribe");
            }
            Envelope::Ping => {
                let _ = self.client.send(Envelope::Pong);
            }
            _ => {}
        }
        Ok(())
    }

    /// Take in the changes the upstream backfilled, and fan them out to this
    /// host's own devices.
    async fn backfill(&self, workspace_id: u64, delta_since: Vec<u8>) -> Result<(), String> {
        let Some(workspace) = self.workspace(workspace_id).await else {
            return Ok(());
        };
        let envelope = Envelope::Delta {
            workspace_id,
            author_device_public_key: [0; 32],
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: delta_since,
            signature: [0; 64],
        };
        crate::peer::enqueue_federated(
            &workspace,
            envelope,
            self.source.clone(),
            self.metrics.clone(),
            true,
        )
        .await
        .map_err(|error| error.to_string())
    }

    /// Ask the upstream for what `workspace_id` lacks: send its materialized
    /// heads, read in the owner's order, so the request follows every change
    /// admitted before it.
    async fn ask(&self, workspace_id: u64) -> Result<(), String> {
        let Some(workspace) = self.workspace(workspace_id).await else {
            return Ok(());
        };
        let owner = crate::persistence::handle(&workspace).await;
        let outbound = self.client.outbound.clone();
        let source = self.source.clone();
        owner
            .submit(
                1,
                |state| Ok(state.heads()),
                move |result| async move {
                    let result = async {
                        let heads = result?;
                        let runtime = crate::workspace_io::fresh_membership(&workspace).await?;
                        if !source.authorized(&runtime) {
                            return Err(crate::persistence::error("federation session changed"));
                        }
                        outbound
                            .try_send(Envelope::SyncRequest {
                                workspace_id,
                                have_heads: heads.clone(),
                            })
                            .map_err(|_| {
                                crate::persistence::error("federation outbound backpressure")
                            })?;
                        Ok(heads)
                    }
                    .await;
                    if result.is_err() {
                        source.failed();
                    }
                    result
                },
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}
