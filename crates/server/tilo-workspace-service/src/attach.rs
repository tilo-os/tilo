//! The dialing side of host attachment: a host keeps a session to an
//! upstream host holding a workspace with it, its own person's or the host of
//! another person the workspace's governance names, and holds the workspaces
//! both host in step with it.
//!
//! The session dials through the same handshake and client every device
//! uses, says which workspaces it carries (`PeerServerHello`), and subscribes
//! to those both sides host. Subscribing replays everything the upstream
//! retains of each, as it does for any host that keeps a whole workspace, and
//! live fan-out carries every later change. Inbound changes are admitted
//! through the same ordered persistence owner device peers use.
//!
//! Anti-entropy (`SyncRequest`, answered with the changes the requester's
//! materialized heads lack) runs on events only: once when the session
//! starts, again for as long as an answer still brings something new, and
//! whenever the upstream says it withheld a change from this host. An
//! upstream does not forward a change another host attached to it sent it
//! back to a third (that would loop), so a change crossing two hops reaches
//! the far host this way. A converged session does nothing until a change
//! arrives: no timer runs while it is connected.
//!
//! A session carries changes one way, from the host dialed to the host
//! dialing: every holder of a workspace dials every other.
//!
//! Where each session stands ([`Link`]) is kept in the workspace's [`Links`]:
//! reaching the other holder, taking in what it lacks of it, current with
//! it, or unable to reach it since some time. The holder says so in the
//! workspace's state, where every device reads which holder is short of what.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use iroh::endpoint::Endpoint;
use iroh_base::EndpointAddr;
use tilo_lock::MutexRecover as _;
use tilo_lock::RwLockRecover as _;
use tilo_server_session::client::ServerClient;
use tokio::task::JoinHandle;

use crate::handshake::LiveIdentity;
use crate::metrics::Metrics;
use crate::peer::WorkspaceRegistry;
use crate::transport::Envelope;
use tilo_server_protocol::WorkspaceId;

/// Which host an attach session dials: the device `device` of the person
/// `person`, at `addr`, both pinned by the handshake. The governor of a
/// workspace this host also serves, or a host serving a workspace this
/// device governs.
#[derive(Clone, Debug)]
pub struct Upstream {
    pub addr: EndpointAddr,
    pub device: [u8; 32],
    pub person: [u8; 32],
}

/// The devices whose sessions to this host authenticated, as they do.
///
/// A device that comes back, perhaps at another address, opens its
/// sessions here first. A host dialing it hears that and dials it again
/// at once, where the device is now reachable, rather than finishing a
/// dial to where it was or waiting out its backoff.
#[derive(Clone)]
pub struct Arrivals(tokio::sync::broadcast::Sender<[u8; 32]>);

impl Default for Arrivals {
    fn default() -> Self {
        Self(tokio::sync::broadcast::channel(ARRIVALS).0)
    }
}

/// Arrivals a slow dial loop may fall behind by: one that falls further
/// behind dials again, as it would on any arrival.
const ARRIVALS: usize = 64;

impl Arrivals {
    /// A session of `device` authenticated here.
    pub(crate) fn arrived(&self, device: [u8; 32]) {
        let _ = self.0.send(device);
    }

    fn watch(&self) -> tokio::sync::broadcast::Receiver<[u8; 32]> {
        self.0.subscribe()
    }
}

/// Wait until `device` arrives in `arrivals`.
async fn arrival(arrivals: &mut tokio::sync::broadcast::Receiver<[u8; 32]>, device: [u8; 32]) {
    loop {
        match arrivals.recv().await {
            Ok(arrived) if arrived == device => return,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                return std::future::pending().await;
            }
        }
    }
}

/// Where this device's session to another holder of a workspace stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// Not reached since this device started: what it lacks of the other
    /// is not known yet.
    Reaching,
    /// Reached, and taking in what the other holds and this device lacks.
    CatchingUp,
    /// Holding everything the other holds.
    Current,
    /// Not reached since `since_ns` (ns since the epoch).
    Unreachable { since_ns: u64 },
}

/// Where each of a workspace's sessions to its other holders stands, by the
/// other holder's device, what this device holds back of what they sent for
/// lack of history, how much of the content the workspace names it lacks,
/// and a wake-up for whoever says so in the workspace.
#[derive(Clone, Default)]
pub struct Links(Arc<LinksInner>);

#[derive(Default)]
struct LinksInner {
    links: std::sync::Mutex<BTreeMap<[u8; 32], Link>>,
    /// The holders a change is held back from until the history it builds
    /// on arrives.
    held_from: std::sync::Mutex<std::collections::BTreeSet<[u8; 32]>>,
    /// Blobs of the content the workspace's files name that this device lacks.
    content_missing: std::sync::atomic::AtomicU32,
    changed: tokio::sync::Notify,
}

impl Links {
    /// The session to `device` stands at `link`. A holder unreachable before
    /// stays unreachable since it first was.
    pub(crate) fn set(&self, device: [u8; 32], link: Link) {
        let mut links = self.0.links.lock_recover();
        let link = match (links.get(&device), link) {
            (Some(Link::Unreachable { since_ns }), Link::Unreachable { .. }) => Link::Unreachable {
                since_ns: *since_ns,
            },
            _ => link,
        };
        if links.insert(device, link) != Some(link) {
            drop(links);
            self.0.changed.notify_one();
        }
    }

    /// No session to `device` is kept any more.
    pub(crate) fn forget(&self, device: &[u8; 32]) {
        if self.0.links.lock_recover().remove(device).is_some() {
            self.0.changed.notify_one();
        }
    }

    /// Where each session stands now.
    pub(crate) fn now(&self) -> BTreeMap<[u8; 32], Link> {
        self.0.links.lock_recover().clone()
    }

    /// Wait until a session's link changes, or say it changed at once.
    pub(crate) async fn changed(&self) {
        self.0.changed.notified().await;
    }

    /// Changes from `held_from` are held back for history this device lacks.
    pub(crate) fn hold_from(&self, held_from: std::collections::BTreeSet<[u8; 32]>) {
        if *self.0.held_from.lock_recover() != held_from {
            *self.0.held_from.lock_recover() = held_from;
            self.0.changed.notify_one();
        }
    }

    /// The holders a change is held back from.
    pub(crate) fn held_from(&self) -> std::collections::BTreeSet<[u8; 32]> {
        self.0.held_from.lock_recover().clone()
    }

    /// This device lacks `blobs` of the content the workspace names.
    pub fn lacks_content(&self, blobs: u32) {
        if self.0.content_missing.swap(blobs, Ordering::SeqCst) != blobs {
            self.0.changed.notify_one();
        }
    }

    /// How many blobs of the content the workspace names this device lacks.
    pub(crate) fn content_missing(&self) -> u32 {
        self.0.content_missing.load(Ordering::SeqCst)
    }

    /// Have the next [`Self::changed`] return at once.
    pub(crate) fn touch(&self) {
        self.0.changed.notify_one();
    }
}

/// What an outbound attach session carries: the local endpoint, this host's
/// own identity (its revocation list read afresh for every dial and
/// session), the workspaces it hosts and its metrics. `scope`, when given,
/// is the only workspaces the session offers: a holder of a workspace
/// beside another attaches that workspace alone. `links`, when given, is told
/// where the session stands.
pub struct HostDialer {
    pub endpoint: Endpoint,
    pub identity: LiveIdentity,
    /// The live handshake policy, which what the session carries is
    /// trusted by ([`crate::peer::StateSource::leased`]).
    pub trust_policy: Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    pub workspaces: WorkspaceRegistry,
    pub metrics: Arc<Metrics>,
    pub scope: Option<Vec<WorkspaceId>>,
    pub links: Option<Links>,
}

impl HostDialer {
    /// Spawn the reconnect-with-backoff task for `upstream`. The task runs
    /// until its handle is aborted. While connected it holds one session;
    /// after a session ends or a dial fails it waits `1s, 2s, 4s, …` up to
    /// 60s before dialing again, or dials again as soon as the upstream's
    /// device opens a session here ([`Arrivals`]): a dial still under way
    /// to where it was is given up, and a wait for the next dial cut short.
    pub(crate) fn spawn_woken(
        self,
        upstream: Upstream,
        label: String,
        arrivals: &Arrivals,
    ) -> JoinHandle<()> {
        tokio::spawn(self.dial_loop(upstream, label, arrivals.watch()))
    }

    async fn dial_loop(
        self,
        upstream: Upstream,
        label: String,
        mut arrivals: tokio::sync::broadcast::Receiver<[u8; 32]>,
    ) {
        let device = upstream.device;
        let mut backoff_ms: u64 = 1_000;
        self.stands(device, Link::Reaching);
        loop {
            let dialed = tokio::select! {
                dialed = self.dial(&upstream) => Some(dialed),
                () = arrival(&mut arrivals, device) => None,
            };
            match dialed {
                Some(Ok(client)) => {
                    tracing::info!(%label, "attached to the upstream host");
                    backoff_ms = 1_000;
                    if let Err(error) = self.run_session(&client).await {
                        tracing::debug!(%label, "attach session ended: {error}");
                    }
                    // What arrived while the session lasted says nothing
                    // of the device now.
                    arrivals = arrivals.resubscribe();
                }
                Some(Err(error)) => {
                    tracing::debug!(%label, "attach dial failed: {error}");
                    self.unreachable(device);
                }
                None => {
                    tracing::debug!(%label, "the upstream's device is back; dialing it again");
                    backoff_ms = 1_000;
                    continue;
                }
            }
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(backoff_ms)) => {
                    backoff_ms = backoff_ms.saturating_mul(2).min(60_000);
                }
                () = arrival(&mut arrivals, device) => backoff_ms = 1_000,
            }
        }
    }

    async fn dial(&self, upstream: &Upstream) -> Result<ServerClient, String> {
        let identity = &self.identity;
        let revocations = identity.revocations.read_recover().clone();
        ServerClient::connect_v2_to_enrolled_home_peer(
            self.endpoint.clone(),
            upstream.addr.clone(),
            upstream.person,
            upstream.device,
            identity.cert.clone(),
            identity.signing_key.clone(),
            revocations,
        )
        .await
        .map_err(|error| error.to_string())
    }

    /// Tell this session's links that the one to `device` stands at `link`.
    fn stands(&self, device: [u8; 32], link: Link) {
        if let Some(links) = &self.links {
            links.set(device, link);
        }
    }

    /// The holder `device` was not reached: since now, or since it first was
    /// not.
    fn unreachable(&self, device: [u8; 32]) {
        self.stands(
            device,
            Link::Unreachable {
                since_ns: tilo_clock::unix_now_ns(),
            },
        );
    }

    /// One attach session: agree on the workspaces both hold, subscribe to
    /// them, then follow the upstream until the session ends.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the session borrows the client and the workspaces and is driven by the select until it ends; it holds no lock"
    )]
    async fn run_session(&self, client: &ServerClient) -> Result<(), String> {
        let hosted_local = self.carried().await;
        client
            .send(Envelope::PeerServerHello {
                hosted_workspaces: hosted_local.clone(),
            })
            .map_err(|_| "outbound closed".to_string())?;
        let intersection = negotiate_intersection(client, &hosted_local).await?;
        for id in &intersection {
            client
                .send(Envelope::Subscribe { workspace_id: *id })
                .map_err(|_| "outbound closed on subscribe".to_string())?;
        }
        let lease = AttachLease(Arc::new(AtomicBool::new(true)));
        let (failed, mut failures) = tokio::sync::mpsc::channel(1);
        let source = crate::peer::StateSource::leased(
            client.server_peer.clone(),
            client.outbound.clone(),
            failed,
            self.identity.revocations.clone(),
            self.trust_policy.clone(),
            self.identity.user_public_key,
            lease.0.clone(),
        );
        for id in &intersection {
            let workspace = self.workspaces.lock().await.get(id).cloned();
            if let Some(workspace) = workspace {
                let mut runtime = workspace.lock().await;
                runtime.note_attached_host(client.server_peer.device_public_key);
                if let Some(previous) = runtime
                    .outbound_leases
                    .insert(client.server_peer.device_public_key, lease.0.clone())
                {
                    previous.store(false, Ordering::SeqCst);
                }
            }
        }
        let session = Session {
            client,
            own_device: self.identity.cert.device_public_key,
            workspaces: &self.workspaces,
            source: &source,
            metrics: &self.metrics,
            stands: &|link| self.stands(client.server_peer.device_public_key, link),
        };
        tokio::select! {
            result = session.follow(&intersection) => result,
            _ = failures.recv() => Err("state from an attached host failed persistence/authorization".into()),
        }
    }

    /// The workspaces this session carries: those in scope, or every
    /// workspace hosted here.
    async fn carried(&self) -> Vec<WorkspaceId> {
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

/// Clears an attach session's lease on every workspace it registered with
/// once the session ends, so nothing is sent on it after.
struct AttachLease(Arc<AtomicBool>);

impl Drop for AttachLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Wait for the upstream's `PeerServerHello` and intersect it with what this
/// host carries. The upstream answers a hello with its own before anything
/// else of the session; anything else first is logged and skipped.
async fn negotiate_intersection(
    client: &ServerClient,
    hosted_local: &[WorkspaceId],
) -> Result<Vec<WorkspaceId>, String> {
    loop {
        match client.recv().await {
            Some(Envelope::PeerServerHello { hosted_workspaces }) => {
                let local: std::collections::HashSet<WorkspaceId> =
                    hosted_local.iter().copied().collect();
                return Ok(hosted_workspaces
                    .into_iter()
                    .filter(|id| local.contains(id))
                    .collect());
            }
            Some(other) => {
                tracing::debug!("attach session got unexpected pre-hello envelope: {other:?}");
            }
            None => return Err("connection closed pre-hello".into()),
        }
    }
}

/// A connected attach session.
struct Session<'a> {
    client: &'a ServerClient,
    /// This server's own device, which answers the other holder's asks.
    own_device: [u8; 32],
    workspaces: &'a WorkspaceRegistry,
    source: &'a crate::peer::StateSource,
    metrics: &'a Arc<Metrics>,
    /// Tells the session's links where it stands.
    stands: &'a (dyn Fn(Link) + Sync),
}

impl Session<'_> {
    /// Follow the upstream for the life of the connection: ask once for what
    /// each shared workspace lacks, then admit what arrives, asking again
    /// only on an event (see the module documentation).
    async fn follow(&self, intersection: &[WorkspaceId]) -> Result<(), String> {
        for id in intersection {
            self.ask(*id).await?;
        }
        // The last non-empty answer of each workspace: one repeated unchanged
        // brought nothing this host could take in, and asking again would
        // only repeat it.
        let mut answered: HashMap<WorkspaceId, blake3::Hash> = HashMap::new();
        while let Some(envelope) = self.client.recv().await {
            match envelope {
                Envelope::SyncResponse {
                    workspace_id,
                    delta_since,
                } => {
                    if delta_since.is_empty() {
                        answered.remove(&workspace_id);
                        (self.stands)(Link::Current);
                        continue;
                    }
                    let digest = blake3::hash(&delta_since);
                    if answered.insert(workspace_id, digest) == Some(digest) {
                        (self.stands)(Link::Current);
                        continue;
                    }
                    (self.stands)(Link::CatchingUp);
                    self.backfill(workspace_id, delta_since).await?;
                    if intersection.contains(&workspace_id) {
                        self.ask(workspace_id).await?;
                    }
                }
                // From an upstream, a request means this host is behind: it
                // withheld a change another attached host sent it.
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

    async fn workspace(&self, id: WorkspaceId) -> Option<crate::workspace::SharedWorkspace> {
        self.workspaces.lock().await.get(&id).cloned()
    }

    /// Admit one inbound envelope: a change through the ordered state owner
    /// (and to the workspace's follower, when one listens), presence to local
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
                // An upstream replays all it retains of the workspace at once,
                // faster than this host keeps it: each change is kept before the
                // next is taken, so a workspace with a long history fills no
                // queue and the session holds.
                crate::peer::enqueue_from_host(
                    &workspace,
                    envelope,
                    self.source.clone(),
                    self.metrics.clone(),
                    false,
                )
                .await
                .map_err(|error| error.to_string())?
                .kept()
                .await;
                if let Some((tap, author, body)) = notice {
                    crate::peer::tell_governor(&workspace, &tap, author, body).await;
                }
            }
            Envelope::RetiredPresence {
                workspace_id,
                payload,
            } => {
                if let Some(workspace) = self.workspace(*workspace_id).await {
                    // Presence carries no author on the wire; zero stands
                    // in. Fan-out only compares it to subscribers and keeps
                    // it from other attached hosts.
                    let mut runtime = workspace.lock().await;
                    runtime.fanout_presence(&[0u8; 32], &envelope);
                    let authority = runtime.workspace_authority.clone();
                    drop(runtime);
                    self.answer_holder(&workspace, authority.as_ref(), *workspace_id, payload);
                }
            }
            Envelope::NotAuthorised {
                workspace_id,
                refusal,
            } => {
                tracing::warn!(workspace_id = %workspace_id, %refusal, "attach session refused subscribe");
            }
            Envelope::Ping => {
                let _ = self.client.send(Envelope::Pong);
            }
            _ => {}
        }
        Ok(())
    }

    /// Answer the other holder's own ask for content this server keeps of a
    /// workspace both hold (the files it lacks, for one): back on this
    /// session, addressed to that holder's device, which keeps what it asked
    /// for. What a member of the other holder asks, that member's own
    /// holder answers; only the holder itself is answered here.
    fn answer_holder(
        &self,
        workspace: &crate::workspace::SharedWorkspace,
        authority: Option<&crate::workspaces::WorkspaceAuthority>,
        workspace_id: WorkspaceId,
        payload: &[u8],
    ) {
        let holder = &self.client.server_peer;
        let Some(asked) = crate::AttributedPresence::decode(payload) else {
            return;
        };
        if asked.author != holder.device_public_key || authority.is_none() {
            return;
        }
        let outbound = self.client.outbound.clone();
        crate::peer::answer_content(
            workspace,
            authority,
            workspace_id,
            (self.own_device, holder.device_public_key),
            holder.user_public_key,
            asked.body,
            move |_, envelope| {
                if outbound.try_send(envelope).is_err() {
                    tracing::debug!("a piece for the other holder did not fit its session");
                }
            },
        );
    }

    /// Take in the changes the upstream backfilled, and fan them out to this
    /// host's own devices.
    async fn backfill(
        &self,
        workspace_id: WorkspaceId,
        delta_since: Vec<u8>,
    ) -> Result<(), String> {
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
        crate::peer::enqueue_from_host(
            &workspace,
            envelope,
            self.source.clone(),
            self.metrics.clone(),
            true,
        )
        .await
        .map_err(|error| error.to_string())?
        .kept()
        .await;
        // What the governor held back for lack of this history is taken in
        // with it.
        let authority = workspace.lock().await.workspace_authority.clone();
        if let Some(authority) = authority
            && let Some(workspaces) = authority.workspaces()
        {
            workspaces.history_arrived(authority.workspace());
        }
        Ok(())
    }

    /// Ask the upstream for what `workspace_id` lacks: send its materialized
    /// heads, read in the owner's order, so the request follows every change
    /// admitted before it.
    async fn ask(&self, workspace_id: WorkspaceId) -> Result<(), String> {
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
                            return Err(crate::persistence::error("attach session changed"));
                        }
                        outbound
                            .try_send(Envelope::SyncRequest {
                                workspace_id,
                                have_heads: heads.clone(),
                            })
                            .map_err(|_| {
                                crate::persistence::error("attach outbound backpressure")
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
