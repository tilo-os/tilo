//! The live state pipeline's types: the sessions a delta reaches
//! ([`StateSession`], [`StateSource`], [`Lag`]), what is delivered to them
//! ([`StateDelivery`]), recipient preparation, and the inbound `Delta`
//! handlers.

use super::{
    AgentRegistry, Arc, AuthenticatedPeer, Envelope, HashMap, PeerError, ReplayCatchup,
    ReplayPendingTarget, ServerTrust, SessionCtx, SharedWorkspace, WorkspaceOperation,
    current_session, enqueue_state, mpsc, trace_native_publication, workspace_access,
};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;

/// Handle an inbound `Delta`: validate the author, fan out to every
/// other subscriber via the same path a change from an attached host
/// takes, and account it in the metrics.
#[derive(Clone)]
pub(crate) struct StateSession {
    pub(crate) peer: AuthenticatedPeer,
    pub(crate) tx: mpsc::Sender<StateDelivery>,
    pub(crate) live: Arc<std::sync::atomic::AtomicBool>,
    pub(super) replay: Option<Arc<ReplayCatchup>>,
    pub(super) lag: Option<Arc<Lag>>,
    /// The device only follows its channel groups
    /// ([`Envelope::KeyFollower`]): no device's request to be admitted to
    /// one reaches it.
    pub(super) channel_reader: bool,
}
impl StateSession {
    /// A live session of `peer` whose deliveries go nowhere.
    #[cfg(test)]
    pub(crate) fn for_test(peer: AuthenticatedPeer) -> Self {
        Self {
            peer,
            tx: mpsc::channel(1).0,
            live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            replay: None,
            lag: None,
            channel_reader: false,
        }
    }
    /// A live delivery to workspace `workspace_id` did not fit this session's
    /// queue or the live byte budget; its pending entry stays on disk and the
    /// connection resumes the workspace from there once its queues drain. That
    /// holds between a replay's final cut and its `Ready` too, where a busy
    /// workspace fills the not yet written live queue. Only a session without a
    /// lag to resume is fenced at that boundary, so its reconnect catches up.
    pub(super) fn fall_behind(
        &self,
        sub: &crate::workspace::Subscriber,
        replay_phase: Option<u8>,
        workspace_id: WorkspaceId,
    ) {
        if let Some(lag) = &self.lag {
            lag.mark(workspace_id);
        } else if replay_phase == Some(ReplayCatchup::BOUNDARY) {
            self.live.store(false, std::sync::atomic::Ordering::SeqCst);
            let _ = sub.disconnect.try_send(());
        }
    }
    /// Everything fanned out to this session so far reached its wire: none
    /// of it is queued, left on disk while it lagged, or still replaying.
    pub(crate) fn caught_up(&self) -> bool {
        self.tx.capacity() == self.tx.max_capacity()
            && self.lag.as_ref().is_none_or(|lag| lag.is_empty())
            && self.replay.as_ref().is_none_or(|replay| {
                replay.phase.load(std::sync::atomic::Ordering::SeqCst) != ReplayCatchup::ACTIVE
            })
    }
}
/// The workspaces whose live deliveries a connection could not take and left on
/// disk (its pending queue and the retained log). Once the connection's
/// queues drain it resumes each of them from its position, so the replay
/// carries exactly what it missed; a lagging reader costs disk and latency,
/// never its session. A subscription without a resume log catches up through
/// its ordinary full retained replay, with current authorization checked again.
#[derive(Default)]
pub struct Lag {
    workspaces: std::sync::Mutex<std::collections::BTreeSet<WorkspaceId>>,
    pub(super) wake: tokio::sync::Notify,
}
impl Lag {
    pub(super) fn mark(&self, workspace_id: WorkspaceId) {
        // Counted, not timed: how often a live delivery fell back to replay.
        crate::timing::window("state.lag_mark", crate::timing::start());
        let first = self.workspaces.lock_recover().insert(workspace_id);
        if first {
            self.wake.notify_one();
        }
    }

    pub(super) fn next(&self) -> Option<WorkspaceId> {
        self.workspaces.lock_recover().pop_first()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.workspaces.lock_recover().is_empty()
    }
}

#[derive(Clone)]
pub struct StateSource {
    pub(super) peer: AuthenticatedPeer,
    pub(super) tx: mpsc::Sender<Envelope>,
    pub(super) disconnect: mpsc::Sender<()>,
    pub(super) revocations: Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
    pub(super) policy: Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    pub(super) own_user: [u8; 32],
    pub(super) agents: AgentRegistry,
    pub(super) environments: crate::environment::EnvironmentRegistry,
    pub(super) lease: Option<Arc<std::sync::atomic::AtomicBool>>,
    pub(super) session_live: Option<Arc<std::sync::atomic::AtomicBool>>,
}
impl StateSource {
    /// The source of an outbound lease to a host attached to the workspace,
    /// trusted as this host's sessions are: by `policy`, the live handshake
    /// policy, which admits this host's person, the workspace's members and
    /// the hosts holding it, so what the attached host carries reaches every
    /// member here. It carries no agent delegations: a host carrying this
    /// one's changes is never an agent.
    pub fn leased(
        peer: AuthenticatedPeer,
        tx: mpsc::Sender<Envelope>,
        disconnect: mpsc::Sender<()>,
        revocations: Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
        policy: Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
        own_user: [u8; 32],
        lease: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            peer,
            tx,
            disconnect,
            revocations,
            policy,
            own_user,
            agents: AgentRegistry::default(),
            environments: crate::environment::EnvironmentRegistry::default(),
            lease: Some(lease),
            session_live: None,
        }
    }
    pub(super) fn alive(&self) -> bool {
        !self.tx.is_closed()
            && self
                .session_live
                .as_ref()
                .is_none_or(|live| live.load(std::sync::atomic::Ordering::SeqCst))
    }
    pub fn failed(&self) {
        let _ = self.disconnect.try_send(());
    }
    pub(super) fn new(peer: &AuthenticatedPeer, session: &SessionCtx<'_>) -> Self {
        Self {
            peer: peer.clone(),
            tx: session.outbound.clone(),
            disconnect: session.disconnect.clone(),
            revocations: session.revocations.clone(),
            policy: session.trust_policy.clone(),
            own_user: *session.own_user_public_key,
            agents: session.agents.clone(),
            environments: session.environments.clone(),
            lease: None,
            session_live: session.connection_live.cloned(),
        }
    }
    pub(super) fn trust(&self) -> ServerTrust<'_> {
        ServerTrust {
            revocations: &self.revocations,
            policy: &self.policy,
            own_user: &self.own_user,
            agents: &self.agents,
            environments: &self.environments,
        }
    }
    /// Whether `peer` may hold state in `runtime`'s workspace under this source's
    /// trust: the source itself, or a recipient of its frames.
    pub(super) fn admits(
        &self,
        runtime: &crate::workspace::WorkspaceRuntime,
        peer: &AuthenticatedPeer,
    ) -> bool {
        workspace_access::decide(runtime, &self.trust(), peer, WorkspaceOperation::State).admitted()
    }
    /// Whether this source may still hold the workspace's state over its current
    /// session: what keeps its subscription and its frames flowing.
    pub fn authorized(&self, runtime: &crate::workspace::WorkspaceRuntime) -> bool {
        self.authorized_for(runtime, WorkspaceOperation::State)
    }
    /// Whether this source may take a frame of its own into the workspace now,
    /// on the protocol `op`, changing the record on `channel` (`None` for a frame
    /// that belongs to no record): a browser environment's device writes its
    /// own record and nothing else.
    fn publishes(
        &self,
        runtime: &crate::workspace::WorkspaceRuntime,
        op: WorkspaceOperation,
        channel: Option<[u8; 32]>,
    ) -> bool {
        self.write_reaches(channel) && self.authorized_for(runtime, op)
    }

    /// Whether `channel` is inside what this source may write: everything for
    /// a member, its own record for a browser environment's device.
    fn write_reaches(&self, channel: Option<[u8; 32]>) -> bool {
        crate::environment::environment_of(&self.environments, &self.peer.device_public_key)
            .is_none_or(|environment| environment.reaches(channel))
    }
    fn authorized_for(
        &self,
        runtime: &crate::workspace::WorkspaceRuntime,
        op: WorkspaceOperation,
    ) -> bool {
        self.alive()
            && workspace_access::decide(runtime, &self.trust(), &self.peer, op).admitted()
            && match &self.lease {
                Some(lease) => {
                    lease.load(std::sync::atomic::Ordering::SeqCst)
                        && runtime
                            .outbound_leases
                            .get(&self.peer.device_public_key)
                            .is_some_and(|current| Arc::ptr_eq(current, lease))
                }
                None => current_session(runtime, &self.peer, &self.tx),
            }
    }
}

// Return only channels subject to the ordinary once-per-request live check.
// This is delivery policy, not authority: callers must first revalidate the
// current authenticated source and retain all recipient/session fences.
/// The devices a retained message goes to live when only they can use it
/// ([`crate::payloads::ShellPayloads::audience`]); `None` for everyone.
pub(super) fn live_audience(
    runtime: &crate::workspace::WorkspaceRuntime,
    envelope: &Arc<Envelope>,
) -> Option<std::collections::BTreeSet<[u8; 32]>> {
    let Envelope::Delta { delta, .. } = &**envelope else {
        return None;
    };
    let (crate::coedit::CoeditPayload::Checkpoint { body, .. }, _) =
        postcard::take_from_bytes::<crate::coedit::CoeditPayload>(delta).ok()?
    else {
        return None;
    };
    runtime.store.payloads().audience(&body)
}

pub(super) fn guarded_checkpoint_channel(
    payloads: &dyn crate::payloads::ShellPayloads,
    envelope: &Arc<Envelope>,
) -> Option<[u8; 32]> {
    let Envelope::Delta { delta, .. } = &**envelope else {
        return None;
    };
    let (payload, outer_trailing) =
        postcard::take_from_bytes::<crate::coedit::CoeditPayload>(delta).ok()?;
    let crate::coedit::CoeditPayload::Checkpoint { channel, body } = payload else {
        return None;
    };
    // Unknown wrapper suffixes retain the generic checkpoint check.
    if outer_trailing.is_empty()
        && let Some(checkpoint) = payloads.checkpoint(&channel, &body)
    {
        // Latest-wins state is a sequence of values that each replace the
        // last on one route, not a baseline of content a live device already
        // holds: every one must reach it.
        let latest_wins = match checkpoint.kind {
            crate::payloads::CheckpointKind::Governance
            | crate::payloads::CheckpointKind::Identity => true,
            crate::payloads::CheckpointKind::Other => false,
        };
        if latest_wins {
            return None;
        }
    }
    Some(channel)
}

pub(crate) struct StateDelivery {
    pub(super) envelope: Arc<Envelope>,
    pub(super) cleanup: Option<std::path::PathBuf>,
    pub(super) source: Option<StateSource>,
    pub(super) _budget: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    pub(super) live: Arc<std::sync::atomic::AtomicBool>,
    /// What the fan-out that authorised this delivery under the workspace's lock
    /// read; `None` when the writer must authorise it under the lock again.
    pub(super) fanned: Option<FanoutStamp>,
}

/// The state a fan-out authorised its deliveries in: the authority epoch
/// ([`crate::authority_epoch`]) and the workspace's membership generation, read
/// under the workspace's lock before any recipient was checked.
///
/// Everything that authorisation read for a device recipient and a device
/// author either narrows only with an epoch bump (membership, the workspace's
/// admission, subscriptions, revocations, the trust policy, agent
/// delegations), changes the delivery's own lease flag, or runs out by the
/// clock, which the writer checks without the lock. So
/// while the epoch and generation read the same, the writer need not take
/// the workspace's lock to authorise the delivery again: a crowd's writers took
/// it once per member for every commit, and every turn of the workspace queued
/// behind them.
#[derive(Clone)]
pub(crate) struct FanoutStamp {
    epoch: u64,
    generation: crate::membership_watch::DiskGeneration,
    loaded: u64,
}

impl FanoutStamp {
    /// The stamp of a fan-out holding `runtime`'s lock, unless the workspace's
    /// membership is read from disk on every authorisation (no live watch
    /// vouches for it), which only the lock-holding check does.
    pub(super) fn of(runtime: &crate::workspace::WorkspaceRuntime) -> Option<Self> {
        let epoch = crate::authority_epoch::current();
        if !runtime.membership_is_current() {
            return None;
        }
        Some(Self {
            epoch,
            generation: runtime.disk_generation.clone()?,
            loaded: runtime.loaded_generation?,
        })
    }

    /// Whether nothing the fan-out read has announced a change since.
    fn holds(&self) -> bool {
        crate::authority_epoch::current() == self.epoch
            && self.generation.current() == Some(self.loaded)
    }
}

impl StateDelivery {
    /// Whether this delivery is still authorised exactly as its fan-out
    /// authorised it, decided without the workspace's lock: its lease is live,
    /// nothing the fan-out read has changed ([`FanoutStamp`]), and neither
    /// the recipient's nor the author's certificate or agent delegation has
    /// run out since. `false` means only that the writer must ask under the
    /// lock.
    pub(super) fn still_fanned(
        &self,
        peer: &AuthenticatedPeer,
        session: &SessionCtx<'_>,
        workspace_id: WorkspaceId,
        now: u64,
    ) -> bool {
        let Some(fanned) = &self.fanned else {
            return false;
        };
        self.cleanup.is_none()
            && self.live.load(std::sync::atomic::Ordering::SeqCst)
            && fanned.holds()
            && session.trust().actor(peer, workspace_id, now).is_ok()
            && self.source.as_ref().is_none_or(|source| {
                source.lease.is_none()
                    && source.alive()
                    && source
                        .trust()
                        .actor(&source.peer, workspace_id, now)
                        .is_ok()
                    && source.trust().actor(peer, workspace_id, now).is_ok()
            })
    }

    pub(super) fn current_subscription(
        &self,
        runtime: &crate::workspace::WorkspaceRuntime,
        peer: &AuthenticatedPeer,
        outbound: &mpsc::Sender<Envelope>,
    ) -> bool {
        self.live.load(std::sync::atomic::Ordering::SeqCst)
            && runtime
                .session(&peer.device_public_key, outbound)
                .and_then(|current| current.state.as_ref())
                .is_some_and(|current| Arc::ptr_eq(&current.live, &self.live))
    }
}

pub(super) struct StateCommit {
    pub(super) envelope: Arc<Envelope>,
    pub(super) pending: HashMap<[u8; 32], std::path::PathBuf>,
    pub(super) persisted_bytes: u64,
    pub(super) sessions: Vec<([u8; 32], mpsc::Sender<Envelope>)>,
    pub(super) logical_offline_bytes: u64,
    pub(super) publish: bool,
    pub(super) replay_targets: Vec<ReplayPendingTarget>,
    pub(super) accepted_deltas: u64,
}

pub(super) struct PreparedRecipients {
    pub(super) sessions: Vec<([u8; 32], mpsc::Sender<Envelope>)>,
    pub(super) replay_targets: Vec<ReplayPendingTarget>,
    pub(super) logical_offline_bytes: u64,
}

pub(super) async fn handle_delta(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
    env: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    let Envelope::Delta {
        workspace_id,
        author_device_public_key,
        ..
    } = &env
    else {
        return Err(PeerError::Protocol(
            "handle_delta called with a non-Delta envelope".into(),
        ));
    };
    let workspace_id = *workspace_id;
    if !subscribed.contains(&workspace_id) {
        return Err(PeerError::Protocol(
            "delta sent before subscribing to workspace".into(),
        ));
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            refusal: tilo_server_protocol::refusal::Refusal::NotReady,
        }));
    };
    // A host attached to the workspace carries its members' changes under
    // their keys; every other device writes only its own.
    let attached = ws.lock().await.attached(&peer.device_public_key);
    if author_device_public_key != &peer.device_public_key && !attached {
        return Err(PeerError::Protocol(
            "author does not match authenticated device".into(),
        ));
    }
    let refused = |refusal| {
        refused_change(
            peer,
            session.own_device_public_key,
            attached,
            *author_device_public_key,
            &env,
            refusal,
        )
    };
    if super::over_storage(peer, session, &ws).await {
        // Neither kept nor forwarded, and the session stays, as for a
        // refused change below.
        tracing::warn!(device = %hex::encode(peer.device_public_key), "refused a change to a full workspace");
        let refusal = tilo_server_protocol::refusal::Refusal::Full;
        file_refusal_of(&ws, &env, &refusal.to_string()).await;
        return Ok(refused(refusal));
    }
    // A page asks a record's channel for access on a protocol of its own, and is
    // refused there by name.
    let op = crate::peer::admission::protocol_of(&env);
    let channel = crate::peer::admission::resource_of(&env);
    let Some(source) = authorized_state_source(&ws, peer, session, op, channel).await? else {
        trace_native_publication("server_source_refused", &env, None);
        // A live revoke already pushed NotAuthorised and dropped this
        // subscriber; a follow-up write must close rather than refuse twice.
        let runtime = ws.lock().await;
        if !current_session(&runtime, peer, session.outbound) {
            return Err(PeerError::Protocol(
                "subscriber authorization changed".into(),
            ));
        }
        let asker = StateSource::new(peer, session);
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            refusal: refused_reason(&runtime, &asker.trust(), peer, op, channel),
        }));
    };
    trace_native_publication("server_authorized", &env, None);
    if writes_for_an_environment(peer, session, std::slice::from_ref(&env)) {
        // Neither kept nor forwarded, and the session stays, as for a
        // refused change below.
        return Ok(None);
    }
    if let Some(refusal) = change_refusal(
        &ws,
        peer,
        session.own_device_public_key,
        attached,
        *author_device_public_key,
        &env,
    )
    .await
    {
        // Neither kept nor forwarded. The session stays: one bad change is
        // not a membership decision. Its author's session is told which
        // changes of which document did not land, and why.
        return Ok(refused(refusal));
    }
    let observed = match (&env, session.compactor) {
        (Envelope::Delta { delta, .. }, Some(compactor)) => compactor.observe(delta),
        _ => None,
    };
    let admitting = session
        .compactor
        .zip(observed)
        .map(|(compactor, observed)| compactor.admitting(workspace_id, observed));
    // The tap hands the body to another task, so it needs the one owned copy;
    // it is taken after the workspace lock is released.
    let governor_tap = ws.lock().await.governor_tap.clone();
    let governor = governor_tap.and_then(|tap| Some((tap, governor_notice(&env)?)));
    enqueue_state(&ws, env, Some(source), session.metrics.clone()).await?;
    if let Some((tap, body)) = governor {
        drop(tell_governor(&ws, &tap, peer.device_public_key, body).await);
    }
    if let (Some(compactor), Some(observed)) = (session.compactor, observed) {
        compactor.note(&ws, workspace_id, observed);
    }
    drop(admitting);

    Ok(None)
}

/// Why the server refuses `env`, `peer`'s change in `ws`, once authorization
/// let it through: a change that names nothing it writes, one its author's
/// role grants nothing of (a Viewer writes none), one taking down what
/// someone else made without the workspace's leave, or one the workspace's
/// own copy of the record does not take in. None keeps and forwards the
/// change.
async fn change_refusal(
    ws: &SharedWorkspace,
    peer: &AuthenticatedPeer,
    own_device: &[u8; 32],
    attached: bool,
    claimed: [u8; 32],
    env: &Envelope,
) -> Option<tilo_server_protocol::refusal::Refusal> {
    let Envelope::Delta { delta, .. } = env else {
        return None;
    };
    // The change itself must say what it does and stay inside what its app
    // publishes, before the server keeps or forwards it.
    let payloads = ws.lock().await.store.payloads().clone();
    let named = crate::surfaces::governance_named(&*payloads, delta);
    let decision = Box::pin(crate::workspace::access_deciding(
        ws,
        claimed,
        &named,
        |device_access, payloads| {
            crate::surfaces::refusal(payloads, delta, device_access).is_some()
                || crate::surfaces::role_refusal(delta, device_access, claimed).is_some()
        },
    ))
    .await;
    let device_access = decision.device_access.as_deref();
    let store = ws.lock().await.store.clone();
    let refused = crate::surfaces::refusal(&*decision.payloads, delta, device_access)
        .or_else(|| crate::surfaces::role_refusal(delta, device_access, claimed))
        .map(|refusal| behind_governance(decision.governance_behind, refusal))
        .or_else(|| {
            (!attached)
                .then(|| crate::surfaces::own_refusal(&store, &peer.user_public_key, delta))
                .flatten()
        });
    if let Some(refusal) = refused {
        tracing::warn!(%refusal, device = %hex::encode(peer.device_public_key), "refused a document change");
        file_refusal_of(ws, env, &refusal.to_string()).await;
        return Some(refusal);
    }
    let author = deciding_author(peer, attached, own_device, &claimed);
    if let Some(reason) = desktop_refusal(ws, author, env).await {
        return Some(reason);
    }
    governor_refusal(ws, author, env).await
}

/// What `author`'s own session is told of `env`, a change the workspace
/// refuses for `refusal`: the document the change writes, the changes it
/// carries as their hashes, and why. `None` for a change that names no
/// document, and for one another connection carried: a server attached to the
/// workspace carries a member's changes under the member's key, and that
/// member reads the verdict the workspace filed ([`file_refusal_of`]), since
/// this connection is not its session.
fn refused_change(
    peer: &AuthenticatedPeer,
    own_device: &[u8; 32],
    attached: bool,
    claimed: [u8; 32],
    env: &Envelope,
    refusal: tilo_server_protocol::refusal::Refusal,
) -> Option<Envelope> {
    if deciding_author(peer, attached, own_device, &claimed) != peer.device_public_key {
        return None;
    }
    let body = governor_notice(env)?;
    let Ok(neighbour_wire::NeighbourMsg::AppDocument {
        app_id,
        cluster_id,
        document_id,
        ..
    }) = postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&body)
    else {
        return None;
    };
    let Envelope::Delta { workspace_id, .. } = env else {
        return None;
    };
    Some(Envelope::ChangeRefused {
        workspace_id: *workspace_id,
        app_id,
        cluster_id,
        document_id,
        changes: changes_of(&body),
        refusal,
    })
}

/// The changes `body`, a document write, carries, as their hashes: those its
/// envelope claims, else the changes of its payload itself (a writer that
/// claims none, and every snapshot).
fn changes_of(body: &[u8]) -> Vec<[u8; 32]> {
    let claimed = tilo_peer_state::document_frame::claimed_changes(body);
    if !claimed.is_empty() {
        return claimed;
    }
    let Ok(neighbour_wire::NeighbourMsg::AppDocument { envelope, .. }) =
        postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body)
    else {
        return Vec::new();
    };
    let Ok(mutation) = collab_types::DocumentMutation::decode(&envelope.payload) else {
        return Vec::new();
    };
    let bytes = match mutation {
        collab_types::DocumentMutation::Snapshot(bytes)
        | collab_types::DocumentMutation::Incremental(bytes) => bytes,
    };
    tilo_failure::ignored!(
        "a payload that is not automerge bytes names no change",
        collab_types::parse_changes(&bytes).unwrap_or_default()
    )
    .iter()
    .map(|change| change.hash().0)
    .collect()
}

/// Why the workspace refuses `env`, `author`'s change, as its own copy of the
/// record decides it ([`crate::workspaces::Workspaces::refuses_write`]);
/// `None` in a workspace this server does not govern, for a change that is no
/// record write, and for one the workspace takes in.
///
/// This is the governor's verdict, so it is the same one its copy gives the
/// change when it follows it: a change it refuses is neither kept nor
/// forwarded, and the record's stored history never holds it for a later
/// reader to meet, or for its writer to number its next change after.
pub(super) async fn governor_refusal(
    ws: &SharedWorkspace,
    author: [u8; 32],
    env: &Envelope,
) -> Option<tilo_server_protocol::refusal::Refusal> {
    let body = governor_notice(env)?;
    let authority = ws.lock().await.workspace_authority.clone()?;
    let workspaces = authority.workspaces()?;
    let reason = workspaces
        .refuses_write(authority.workspace(), author, &body)
        .await?;
    // Neither kept nor forwarded: the write's verdict is filed in this
    // device's own workspace slice, the one place its author reads it.
    file_refusal_of(ws, env, &reason).await;
    tracing::warn!(%reason, device = %hex::encode(author), "refused a change the workspace does not take in");
    Some(tilo_server_protocol::refusal::Refusal::NotAllowed(reason))
}

/// File `ws`'s verdict on `env`, a change this host refused for `reason`, in
/// its own slice of the workspace document, and publish it as the workspace's
/// next governance: the change's author reads why it is not here, live or on
/// catch-up, and every member of the workspace keeps the verdict.
pub(super) async fn file_refusal_of(ws: &SharedWorkspace, env: &Envelope, reason: &str) {
    let Some(body) = governor_notice(env) else {
        return;
    };
    let Some(authority) = ws.lock().await.workspace_authority.clone() else {
        return;
    };
    let Some(workspaces) = authority.workspaces() else {
        return;
    };
    if let Err(error) = workspaces
        .publish_refusal(authority.workspace(), &body, reason)
        .await
    {
        tracing::warn!(%error, %reason, "the workspace did not file its refusal of a change");
    }
}

/// Hand `body`, a change `author` made that the server kept, to the workspace's
/// governor through `tap`. Never dropped: the governor's copy is a replica
/// of the workspace's records, and a replica that skipped a change would hold
/// a different record than everyone else. A governor that has fallen
/// behind holds the author back until it takes the change in. Nothing is
/// sent once the governor stopped following (the workspace closed). Answers
/// once the governor's copy took the change in or refused it, or stopped
/// following.
pub(crate) async fn tell_governor(
    ws: &SharedWorkspace,
    tap: &tokio::sync::mpsc::Sender<crate::workspace::GovernorNotice>,
    author: [u8; 32],
    body: Vec<u8>,
) -> tokio::sync::oneshot::Receiver<()> {
    // The workspace's link carries the write as it is admitted, not on the
    // governor's own schedule: a linked device reads it live, and one that
    // joins reads the history as it stands then.
    crate::workspaces::carry_admitted(ws, author, &body).await;
    let (notice, answer) = crate::workspace::GovernorNotice::new(author, body);
    if tap.send(notice).await.is_err() {
        tracing::debug!("the workspace's governor no longer follows it");
    }
    answer
}

/// Whose access decides `claimed`'s change that came over `peer`'s
/// session. A device's session writes only its own (checked before). A
/// host attached to the workspace carries its members' changes under their
/// own keys, so each is decided as that member's, never as that of the host
/// that carried it; one claiming this host's own device is decided as the
/// carrying host's: what this host's device writes, the workspace's governor's
/// own, never reaches it from another host to be taken in.
pub(super) fn deciding_author(
    peer: &AuthenticatedPeer,
    attached: bool,
    own_device: &[u8; 32],
    claimed: &[u8; 32],
) -> [u8; 32] {
    if !attached || claimed == own_device {
        peer.device_public_key
    } else {
        *claimed
    }
}

/// Why the workspace refuses `env`, `author`'s change, when it writes the
/// workspace's shared desktop
/// ([`crate::workspaces::Workspaces::decide_desktop_write`]); `None` for any
/// other change, and in a workspace this server does not govern.
pub(super) async fn desktop_refusal(
    ws: &SharedWorkspace,
    author: [u8; 32],
    env: &Envelope,
) -> Option<tilo_server_protocol::refusal::Refusal> {
    let body = governor_notice(env)?;
    if !crate::workspaces::writes_desktop(&body) {
        return None;
    }
    let authority = ws.lock().await.workspace_authority.clone()?;
    let workspaces = authority.workspaces()?;
    let reason = workspaces
        .decide_desktop_write(authority.workspace(), author, &body)
        .await
        .err()?;
    // Neither kept nor forwarded: a document write's verdict is filed in this
    // device's own workspace slice, the one place its author reads it.
    file_refusal_of(ws, env, &reason).await;
    tracing::warn!(%reason, device = %hex::encode(author), "refused a shared desktop change");
    Some(tilo_server_protocol::refusal::Refusal::NotAllowed(reason))
}

/// What the workspace's governor follows of `env`, a member's state change: its
/// retained body. The workspace's governor takes part in its channels and creates
/// records from these, so every change the server keeps must reach it, one
/// sent alone and one in a checkpoint prefix alike.
pub(super) fn governor_notice(env: &Envelope) -> Option<Vec<u8>> {
    let Envelope::Delta { delta, .. } = env else {
        return None;
    };
    let (_, _, body) = crate::coedit::retention_view(delta)?;
    Some(body.to_vec())
}

pub(super) async fn handle_delta_prefix(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
    envelopes: Vec<Envelope>,
    inbound_budget: Vec<Option<Arc<tokio::sync::OwnedSemaphorePermit>>>,
) -> Result<Option<Envelope>, PeerError> {
    let Some(first) = envelopes.first() else {
        return Err(PeerError::Protocol(
            "checkpoint prefix started with a non-delta".into(),
        ));
    };
    let Envelope::Delta {
        workspace_id,
        author_device_public_key,
        ..
    } = first
    else {
        return Err(PeerError::Protocol(
            "checkpoint prefix started with a non-delta".into(),
        ));
    };
    if !subscribed.contains(workspace_id) {
        return Err(PeerError::Protocol(
            "delta sent before subscribing to workspace".into(),
        ));
    }
    // A prefix of changes rides on the protocol its first change does.
    let op = crate::peer::admission::protocol_of(first);
    let channel = crate::peer::admission::resource_of(first);
    let Some(workspace) = session.workspaces.lock().await.get(workspace_id).cloned() else {
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id: *workspace_id,
            refusal: tilo_server_protocol::refusal::Refusal::NotReady,
        }));
    };
    if author_device_public_key != &peer.device_public_key
        && !workspace.lock().await.attached(&peer.device_public_key)
    {
        return Err(PeerError::Protocol(
            "author does not match authenticated device".into(),
        ));
    }
    let Some(source) = authorized_state_source(&workspace, peer, session, op, channel).await?
    else {
        trace_all(&envelopes, "server_source_refused");
        let runtime = workspace.lock().await;
        if !current_session(&runtime, peer, session.outbound) {
            return Err(PeerError::Protocol(
                "subscriber authorization changed".into(),
            ));
        }
        let asker = StateSource::new(peer, session);
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id: *workspace_id,
            refusal: refused_reason(&runtime, &asker.trust(), peer, op, channel),
        }));
    };
    trace_all(&envelopes, "server_authorized");
    if writes_for_an_environment(peer, session, &envelopes) {
        return Ok(None);
    }
    if let Err(answer) = refused_prefix(&workspace, peer, session, &envelopes).await {
        return Ok(answer);
    }
    let governor_tap = workspace.lock().await.governor_tap.clone();
    // Every change of the prefix reaches the workspace's governor once it is
    // kept, as one sent alone does (`handle_delta`): an access request and
    // the next checkpoint arriving together must not hide the request from
    // the governor's own key holder.
    let notices: Vec<Vec<u8>> = if governor_tap.is_some() {
        envelopes.iter().filter_map(governor_notice).collect()
    } else {
        Vec::new()
    };
    for (envelope, budget) in envelopes.into_iter().zip(inbound_budget) {
        enqueue_state(
            &workspace,
            envelope,
            Some(source.clone()),
            session.metrics.clone(),
        )
        .await?;
        drop(budget);
    }
    if let Some(tap) = governor_tap {
        for body in notices {
            drop(tell_governor(&workspace, &tap, peer.device_public_key, body).await);
        }
    }
    Ok(None)
}

/// Whether the server keeps none of `envelopes`, a checkpoint prefix: a change
/// of it the workspace refuses decides the whole prefix, which is kept whole
/// or not at all. `Err` carries what the refused change's author's session is
/// told of it, when its own connection sent it.
async fn refused_prefix(
    workspace: &SharedWorkspace,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    envelopes: &[Envelope],
) -> Result<(), Option<Envelope>> {
    let attached = workspace.lock().await.attached(&peer.device_public_key);
    for envelope in envelopes {
        let Envelope::Delta {
            author_device_public_key,
            ..
        } = envelope
        else {
            continue;
        };
        let author = deciding_author(
            peer,
            attached,
            session.own_device_public_key,
            author_device_public_key,
        );
        if let Some(refusal) = desktop_refusal(workspace, author, envelope).await {
            return Err(refused_change(
                peer,
                session.own_device_public_key,
                attached,
                *author_device_public_key,
                envelope,
                refusal,
            ));
        }
        if let Some(refusal) = governor_refusal(workspace, author, envelope).await {
            return Err(refused_change(
                peer,
                session.own_device_public_key,
                attached,
                *author_device_public_key,
                envelope,
                refusal,
            ));
        }
    }
    Ok(())
}

pub(super) async fn authorized_state_source(
    workspace: &SharedWorkspace,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    op: WorkspaceOperation,
    channel: Option<[u8; 32]>,
) -> Result<Option<StateSource>, PeerError> {
    let source = StateSource::new(peer, session);
    let runtime = crate::workspace_io::fresh_membership(workspace).await?;
    Ok(source.publishes(&runtime, op, channel).then_some(source))
}

/// Whether `peer` is a browser environment's device and `envelopes` write the
/// workspace's own document — its shared desktop, its governance — rather
/// than its own record. Such a device writes none of that itself: what it
/// shares is asked of this host, which writes it after checking it concerns
/// the device's own windows. Its write is neither kept nor forwarded, and the
/// session stays, since a session that writes its shell as any session does is
/// not misbehaving.
fn writes_for_an_environment(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    envelopes: &[Envelope],
) -> bool {
    let Some(scope) = session.environment(&peer.device_public_key) else {
        return false;
    };
    let outside = envelopes.iter().any(|envelope| {
        matches!(envelope, Envelope::Delta { .. })
            && !crate::environment::Environment::Placed(scope)
                .writes(crate::peer::admission::resource_of(envelope))
    });
    if outside {
        tracing::debug!(
            device = %hex::encode(peer.device_public_key),
            "a browser environment's device wrote the workspace's document, which is the host's to write for it"
        );
    }
    outside
}

/// Why the server took none of `peer`'s frames into the workspace: the
/// admission decision's own reason where there is one, so a page that asked a
/// channel for access is told exactly why not.
pub(super) fn refused_reason(
    runtime: &crate::workspace::WorkspaceRuntime,
    trust: &workspace_access::ServerTrust<'_>,
    peer: &AuthenticatedPeer,
    op: WorkspaceOperation,
    channel: Option<[u8; 32]>,
) -> tilo_server_protocol::refusal::Refusal {
    let outside = crate::environment::environment_of(trust.environments, &peer.device_public_key)
        .filter(|environment| !environment.reaches(channel))
        .map(|_| workspace_access::DenyReason::OutOfEnvironmentScope.refusal());
    match workspace_access::decide(runtime, trust, peer, op) {
        workspace_access::Decision::Deny(reason) => reason.refusal(),
        workspace_access::Decision::Admit | workspace_access::Decision::AdmitAndEnrol => {
            outside.unwrap_or_else(unpublished)
        }
    }
}

/// Trace `step` for each of `envelopes`, the frames of one publication.
fn trace_all(envelopes: &[Envelope], step: &str) {
    for envelope in envelopes {
        trace_native_publication(step, envelope, None);
    }
}

/// `refusal` of a change, or, for a change that waited in vain for the
/// workspace's access to be read from its newer governance, that it waited
/// for that: what its writer is told rather than a grant it was given
/// meanwhile.
pub(super) fn behind_governance(
    behind: bool,
    refusal: tilo_server_protocol::refusal::Refusal,
) -> tilo_server_protocol::refusal::Refusal {
    if behind {
        tilo_server_protocol::refusal::Refusal::NotGranted(
            collab_types::GOVERNANCE_NEVER_ARRIVED.into(),
        )
    } else {
        refusal
    }
}

/// Why the server took none of `peer`'s frames into the workspace.
pub(super) fn unpublished() -> tilo_server_protocol::refusal::Refusal {
    tilo_server_protocol::refusal::Refusal::NotAMember("workspace membership changed".into())
}
