//! The live state pipeline's types: the sessions a delta reaches
//! ([`StateSession`], [`StateSource`], [`Lag`]), what is delivered to them
//! ([`StateDelivery`]), recipient preparation, and the inbound `Delta`
//! handlers.

use super::{
    AgentRegistry, Arc, AuthenticatedPeer, Envelope, HashMap, PeerError, RelayTrust, ReplayCatchup,
    ReplayPendingTarget, RoomOperation, SessionCtx, SharedWorkspace, current_session,
    enqueue_state, lost_session, mpsc, over_change_limit, room_access, trace_native_publication,
};
use tilo_lock::MutexRecover as _;

/// Handle an inbound `Delta`: validate the author, fan out to every
/// other subscriber via the same path a federated peer relay uses,
/// and account it in the metrics.
#[derive(Clone)]
pub(crate) struct StateSession {
    pub(crate) peer: AuthenticatedPeer,
    pub(crate) tx: mpsc::Sender<StateDelivery>,
    pub(crate) live: Arc<std::sync::atomic::AtomicBool>,
    pub(super) replay: Option<Arc<ReplayCatchup>>,
    pub(super) lag: Option<Arc<Lag>>,
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
/// The rooms whose live deliveries a connection could not take and left on
/// disk (its pending queue and the retained log). Once the connection's
/// queues drain it resumes each of them from its position, so the replay
/// carries exactly what it missed; a lagging reader costs disk and latency,
/// never its session. A subscription without a resume log catches up through
/// its ordinary full retained replay, with current authorization checked again.
#[derive(Default)]
pub struct Lag {
    rooms: std::sync::Mutex<std::collections::BTreeSet<u64>>,
    pub(super) wake: tokio::sync::Notify,
}
impl Lag {
    pub(super) fn mark(&self, workspace_id: u64) {
        // Counted, not timed: how often a live delivery fell back to replay.
        crate::timing::window("state.lag_mark", crate::timing::start());
        let first = self.rooms.lock_recover().insert(workspace_id);
        if first {
            self.wake.notify_one();
        }
    }

    pub(super) fn next(&self) -> Option<u64> {
        self.rooms.lock_recover().pop_first()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.rooms.lock_recover().is_empty()
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
    pub(super) federation: Option<Arc<std::sync::atomic::AtomicBool>>,
    pub(super) session_live: Option<Arc<std::sync::atomic::AtomicBool>>,
}
impl StateSource {
    /// The source of an outbound federation lease. Its trust is the
    /// relay's own user only — not the live handshake policy, which may admit
    /// collaborators' roots — and it carries no agent delegations: a peer
    /// relay is never an agent.
    pub fn federated(
        peer: AuthenticatedPeer,
        tx: mpsc::Sender<Envelope>,
        disconnect: mpsc::Sender<()>,
        revocations: Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
        own_user: [u8; 32],
        lease: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            peer,
            tx,
            disconnect,
            revocations,
            policy: Arc::new(std::sync::RwLock::new(
                crate::policy::TrustPolicy::AllowMatchingUser,
            )),
            own_user,
            agents: AgentRegistry::default(),
            federation: Some(lease),
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
            federation: None,
            session_live: session.connection_live.cloned(),
        }
    }
    pub(super) fn trust(&self) -> RelayTrust<'_> {
        RelayTrust {
            revocations: &self.revocations,
            policy: &self.policy,
            own_user: &self.own_user,
            agents: &self.agents,
        }
    }
    /// Whether `peer` may hold state in `runtime`'s room under this source's
    /// trust: the source itself, or a recipient of its frames.
    pub(super) fn admits(
        &self,
        runtime: &crate::workspace::WorkspaceRuntime,
        peer: &AuthenticatedPeer,
    ) -> bool {
        room_access::decide(runtime, &self.trust(), peer, RoomOperation::State).admitted()
    }
    /// Whether this source may still hold the room's state over its current
    /// session: what keeps its subscription and its frames flowing.
    pub fn authorized(&self, runtime: &crate::workspace::WorkspaceRuntime) -> bool {
        self.authorized_for(runtime, RoomOperation::State)
    }
    /// Whether this source may take a frame of its own into the room now: a
    /// spectator holds the room's state, and never publishes into it.
    fn publishes(&self, runtime: &crate::workspace::WorkspaceRuntime) -> bool {
        self.authorized_for(runtime, RoomOperation::Publish)
    }
    fn authorized_for(
        &self,
        runtime: &crate::workspace::WorkspaceRuntime,
        op: RoomOperation,
    ) -> bool {
        self.alive()
            && room_access::decide(runtime, &self.trust(), &self.peer, op).admitted()
            && match &self.federation {
                Some(lease) => {
                    lease.load(std::sync::atomic::Ordering::SeqCst)
                        && runtime
                            .outbound_federation
                            .get(&self.peer.device_public_key)
                            .is_some_and(|current| Arc::ptr_eq(current, lease))
                }
                None => current_session(runtime, &self.peer, &self.tx),
            }
    }
}

#[derive(Clone)]
pub(super) enum LiveEnvelope {
    Original(Arc<Envelope>),
    Selected(Arc<crate::store::replay::ReplayBatch>),
}
// Return only channels subject to the ordinary once-per-request live gate.
// This is delivery policy, not authority: callers must first revalidate the
// current authenticated source and retain all recipient/session fences.
/// The devices a retained message goes to live when only they can use it
/// ([`crate::payloads::ShellPayloads::audience`]); `None` for everyone. In a
/// room this relay governs, what only a channel's steward reads goes to the
/// governor alone ([`steward_audience`]).
pub(super) fn live_audience(
    runtime: &crate::workspace::WorkspaceRuntime,
    envelope: &LiveEnvelope,
) -> Option<std::collections::BTreeSet<[u8; 32]>> {
    let LiveEnvelope::Original(original) = envelope else {
        return None;
    };
    let Envelope::Delta { delta, .. } = &**original else {
        return None;
    };
    let (crate::coedit::CoeditPayload::Checkpoint { body, .. }, _) =
        postcard::take_from_bytes::<crate::coedit::CoeditPayload>(delta).ok()?
    else {
        return None;
    };
    let payloads = &**runtime.store.payloads();
    payloads
        .audience(&body)
        .or_else(|| steward_audience(runtime, payloads.steward_only(&body)))
}

/// Who a change goes to, live or into a catch-up, when it is what only a
/// channel's steward reads (`steward_only`) and this relay governs the room:
/// the governor, which stewards the room's channels and takes such changes
/// in itself. A member device ignores them, so a crowd's requests and
/// acknowledgements cost no member a delivery. `None` otherwise: everyone.
pub(super) fn steward_audience(
    runtime: &crate::workspace::WorkspaceRuntime,
    steward_only: bool,
) -> Option<std::collections::BTreeSet<[u8; 32]>> {
    runtime
        .room_authority
        .as_ref()
        .filter(|_| steward_only)
        .map(|authority| std::collections::BTreeSet::from([*authority.governor()]))
}

/// Whether `delta` is a checkpoint of what only a channel's steward reads
/// ([`crate::payloads::ShellPayloads::steward_only`]).
pub(super) fn steward_only_delta(
    payloads: &dyn crate::payloads::ShellPayloads,
    delta: &[u8],
) -> bool {
    matches!(
        crate::coedit::CoeditPayload::decode(delta),
        Some(crate::coedit::CoeditPayload::Checkpoint { body, .. }) if payloads.steward_only(&body)
    )
}

pub(super) fn gated_checkpoint_channel(
    payloads: &dyn crate::payloads::ShellPayloads,
    envelope: &LiveEnvelope,
    source: Option<&StateSource>,
) -> Option<[u8; 32]> {
    let LiveEnvelope::Original(original) = envelope else {
        return None;
    };
    let Envelope::Delta { delta, .. } = &**original else {
        return None;
    };
    let (payload, outer_trailing) =
        postcard::take_from_bytes::<crate::coedit::CoeditPayload>(delta).ok()?;
    let crate::coedit::CoeditPayload::Checkpoint { channel, body } = payload else {
        return None;
    };
    // Unknown wrapper suffixes retain the generic checkpoint gate.
    if outer_trailing.is_empty()
        && let Some(checkpoint) = payloads.checkpoint(&channel, &body)
    {
        // Channel governance is a sequence of steps that each replace the
        // last on one route, not a baseline of content a live device already
        // holds: every one must reach it.
        let governance = checkpoint.kind == crate::payloads::CheckpointKind::Governance;
        let continuity = source.is_some_and(|source| {
            matches!(
                checkpoint.kind,
                crate::payloads::CheckpointKind::Continuity { user }
                    if user == source.peer.user_public_key
            )
        });
        if governance || continuity {
            return None;
        }
    }
    Some(channel)
}

impl std::ops::Deref for LiveEnvelope {
    type Target = Envelope;
    #[expect(
        clippy::expect_used,
        reason = "a Selected envelope is only built from a non-empty activation batch"
    )]
    fn deref(&self) -> &Envelope {
        match self {
            Self::Original(envelope) => envelope,
            Self::Selected(batch) => batch.front().expect("one activation frame"),
        }
    }
}
pub(crate) struct StateDelivery {
    pub(super) envelope: LiveEnvelope,
    pub(super) cleanup: Option<std::path::PathBuf>,
    pub(super) source: Option<StateSource>,
    pub(super) _budget: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    pub(super) live: Arc<std::sync::atomic::AtomicBool>,
    /// What the fan-out that authorised this delivery under the room's lock
    /// read; `None` when the writer must authorise it under the lock again.
    pub(super) fanned: Option<FanoutStamp>,
}

/// The state a fan-out authorised its deliveries in: the authority epoch
/// ([`crate::authority_epoch`]) and the room's membership generation, read
/// under the room's lock before any recipient was checked.
///
/// Everything that authorisation read for a device recipient and a device
/// author either narrows only with an epoch bump (membership, the room's
/// admission, subscriptions, revocations, the trust policy, agent
/// delegations), changes the delivery's own lease flag, or runs out by the
/// clock, which the writer checks without the lock. So
/// while the epoch and generation read the same, the writer need not take
/// the room's lock to authorise the delivery again: a crowd's writers took
/// it once per member for every commit, and every turn of the room queued
/// behind them.
#[derive(Clone)]
pub(crate) struct FanoutStamp {
    epoch: u64,
    generation: crate::membership_watch::DiskGeneration,
    loaded: u64,
}

impl FanoutStamp {
    /// The stamp of a fan-out holding `runtime`'s lock, unless the room's
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
    /// authorised it, decided without the room's lock: its lease is live,
    /// nothing the fan-out read has changed ([`FanoutStamp`]), and neither
    /// the recipient's nor the author's certificate or agent delegation has
    /// run out since. `false` means only that the writer must ask under the
    /// lock.
    pub(super) fn still_fanned(
        &self,
        peer: &AuthenticatedPeer,
        session: &SessionCtx<'_>,
        workspace_id: u64,
        now: u64,
    ) -> bool {
        let Some(fanned) = &self.fanned else {
            return false;
        };
        self.cleanup.is_none()
            && peer.role == crate::auth::PeerRole::Device
            && self.live.load(std::sync::atomic::Ordering::SeqCst)
            && fanned.holds()
            && session.trust().principal(peer, workspace_id, now).is_ok()
            && self.source.as_ref().is_none_or(|source| {
                source.federation.is_none()
                    && source.peer.role == crate::auth::PeerRole::Device
                    && source.alive()
                    && source
                        .trust()
                        .principal(&source.peer, workspace_id, now)
                        .is_ok()
                    && source.trust().principal(peer, workspace_id, now).is_ok()
            })
    }

    pub(super) fn current_subscription(
        &self,
        runtime: &crate::workspace::WorkspaceRuntime,
        peer: &AuthenticatedPeer,
        outbound: &mpsc::Sender<Envelope>,
    ) -> bool {
        self.live.load(std::sync::atomic::Ordering::SeqCst)
            && current_session(runtime, peer, outbound)
            && runtime
                .state_sessions
                .get(&peer.device_public_key)
                .is_some_and(|current| Arc::ptr_eq(&current.live, &self.live))
    }
}

#[derive(Clone)]
pub(super) enum SelectedPublication {
    Activation(std::path::PathBuf),
    Recovery(std::path::PathBuf),
}
impl SelectedPublication {
    /// The selected publication a store outcome names, if it names one.
    pub(super) fn from_publication(
        publication: crate::store::shell_selection::Publication,
    ) -> Option<Self> {
        match publication {
            crate::store::shell_selection::Publication::Activate(path) => {
                Some(Self::Activation(path))
            }
            crate::store::shell_selection::Publication::ReplayRequired(path) => {
                Some(Self::Recovery(path))
            }
            crate::store::shell_selection::Publication::Original
            | crate::store::shell_selection::Publication::Suppress => None,
        }
    }
    pub(super) fn open(
        &self,
    ) -> Result<crate::store::replay::PublicationCursor, crate::store::StoreError> {
        match self {
            Self::Activation(path) => crate::store::replay::PublicationCursor::open(path),
            Self::Recovery(path) => crate::store::replay::PublicationCursor::open_recovery(path),
        }
    }
}
#[derive(Clone)]
pub(super) struct SessionLease {
    pub(super) device: [u8; 32],
    pub(super) outbound: mpsc::Sender<Envelope>,
    pub(super) live: Arc<std::sync::atomic::AtomicBool>,
}
pub(super) type SessionLives = Vec<SessionLease>;
pub(super) fn captured_live_current(
    lives: &SessionLives,
    runtime: &crate::workspace::WorkspaceRuntime,
    device: &[u8; 32],
    outbound: &mpsc::Sender<Envelope>,
) -> bool {
    lives.iter().any(|lease| {
        lease.device == *device
            && lease.outbound.same_channel(outbound)
            && lease.live.load(std::sync::atomic::Ordering::SeqCst)
            && runtime
                .state_sessions
                .get(device)
                .is_some_and(|session| Arc::ptr_eq(&lease.live, &session.live))
    })
}
pub(super) struct StateCommit {
    pub(super) continuity: Option<crate::store::continuity::Receipt>,
    pub(super) envelope: Arc<Envelope>,
    pub(super) pending: HashMap<[u8; 32], std::path::PathBuf>,
    pub(super) persisted_bytes: u64,
    pub(super) sessions: Vec<([u8; 32], mpsc::Sender<Envelope>)>,
    pub(super) session_lives: SessionLives,
    pub(super) logical_offline_bytes: u64,
    pub(super) publish: bool,
    pub(super) selected: Option<SelectedPublication>,
    pub(super) recovery_scope: bool,
    pub(super) activation_error: Option<crate::store::StoreError>,
    pub(super) replay_targets: Vec<ReplayPendingTarget>,
    pub(super) accepted_deltas: u64,
}

pub(super) struct PreparedRecipients {
    pub(super) session_lives: SessionLives,
    pub(super) sessions: Vec<([u8; 32], mpsc::Sender<Envelope>)>,
    pub(super) replay_targets: Vec<ReplayPendingTarget>,
    pub(super) logical_offline_bytes: u64,
}

pub(super) async fn handle_delta(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
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
            reason: "workspace not bound on this relay".into(),
        }));
    };
    if peer.role == crate::auth::PeerRole::Device
        && author_device_public_key != &peer.device_public_key
    {
        return Err(PeerError::Protocol(
            "author does not match authenticated device".into(),
        ));
    }
    if let Some(limited) = over_change_limit(peer, session) {
        // Neither kept nor forwarded, and the session stays, as for a
        // refused change below.
        tracing::warn!(%limited, device = %hex::encode(peer.device_public_key), "refused a change over the member limit");
        return Ok(None);
    }
    let Some(source) = authorized_state_source(&ws, peer, session).await? else {
        trace_native_publication("server_source_refused", &env, None);
        // A live revoke already pushed NotAuthorised and dropped this
        // subscriber; a follow-up write must close rather than refuse twice.
        let runtime = ws.lock().await;
        if !current_session(&runtime, peer, session.outbound) {
            return Err(lost_session(&runtime, peer, session, RoomOperation::State));
        }
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: unpublished(&runtime, peer).into(),
        }));
    };
    trace_native_publication("server_authorized", &env, None);
    // The change itself must say what it does and stay inside what its app
    // publishes, before the relay keeps or forwards it.
    let (standings, payloads) = {
        let runtime = ws.lock().await;
        (runtime.standings.clone(), runtime.store.payloads().clone())
    };
    if let Envelope::Delta { delta, .. } = &env
        && let Some(reason) = crate::surfaces::refusal(&*payloads, delta, standings.as_deref())
    {
        // Neither kept nor forwarded. The session stays: one bad change
        // is not a membership decision.
        tracing::warn!(%reason, device = %hex::encode(peer.device_public_key), "refused a document change");
        return Ok(None);
    }
    if desktop_refusal(&ws, peer.device_public_key, &env)
        .await
        .is_some()
    {
        // Neither kept nor forwarded, as above.
        return Ok(None);
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
    let governor_tap = {
        let mut runtime = ws.lock().await;
        if peer.role == crate::auth::PeerRole::Device {
            // The caret and typing this device sends next wait for this delta.
            runtime.note_delta(peer.device_public_key, tokio::time::Instant::now());
        }
        runtime.governor_tap.clone()
    };
    let governor = governor_tap.and_then(|tap| Some((tap, governor_notice(&env)?)));
    enqueue_state(&ws, env, Some(source), peer.role, session.metrics.clone()).await?;
    if let Some((tap, body)) = governor {
        tell_governor(&tap, peer.device_public_key, body).await;
    }
    if let (Some(compactor), Some(observed)) = (session.compactor, observed) {
        compactor.note(&ws, workspace_id, observed);
    }
    drop(admitting);

    Ok(None)
}

/// Hand `body`, a change `author` made that the relay kept, to the room's
/// governor through `tap`. Never dropped: the governor's copy is a replica
/// of the room's records, and a replica that skipped a change would hold
/// a different record than everyone else. A governor that has fallen
/// behind holds the author back until it takes the change in. Nothing is
/// sent once the governor stopped following (the room closed).
pub(crate) async fn tell_governor(
    tap: &tokio::sync::mpsc::Sender<([u8; 32], Vec<u8>)>,
    author: [u8; 32],
    body: Vec<u8>,
) {
    if tap.send((author, body)).await.is_err() {
        tracing::debug!("the room's governor no longer follows it");
    }
}

/// Why the room refuses `env`, `author`'s change, when it writes the room's
/// shared layout: its topology or its shared desktop
/// ([`crate::rooms::Rooms::decide_layout_write`]); `None` for any other
/// change, and in a room this relay does not govern.
pub(super) async fn desktop_refusal(
    ws: &SharedWorkspace,
    author: [u8; 32],
    env: &Envelope,
) -> Option<String> {
    let body = governor_notice(env)?;
    if !crate::rooms::writes_desktop(&body) && !writes_topology(&body) {
        return None;
    }
    let authority = ws.lock().await.room_authority.clone()?;
    let reason = authority
        .rooms()?
        .decide_layout_write(authority.room(), author, &body)
        .await
        .err()?;
    if reason == crate::rooms::LAYOUT_REFUSAL {
        tracing::debug!(%reason, device = %hex::encode(author), "refused a shared layout change");
    } else {
        tracing::warn!(%reason, device = %hex::encode(author), "refused a shared layout change");
    }
    Some(reason)
}

/// Whether `body` is a change to some room's document (`SyncResp`), told
/// apart without decoding the changes it carries.
fn writes_topology(body: &[u8]) -> bool {
    matches!(
        postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body),
        Ok(neighbour_wire::NeighbourMsg::SyncResp { .. })
    )
}

/// What the room's governor follows of `env`, a member's state change: its
/// retained body. The room's governor stewards its channels and creates
/// records from these, so every change the relay keeps must reach it, one
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
    subscribed: &[u64],
    envelopes: Vec<Envelope>,
    inbound_budget: Vec<Option<Arc<tokio::sync::OwnedSemaphorePermit>>>,
) -> Result<Option<Envelope>, PeerError> {
    let Some(Envelope::Delta {
        workspace_id,
        author_device_public_key,
        ..
    }) = envelopes.first()
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
    let Some(workspace) = session.workspaces.lock().await.get(workspace_id).cloned() else {
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id: *workspace_id,
            reason: "workspace not bound on this relay".into(),
        }));
    };
    if peer.role == crate::auth::PeerRole::Device
        && author_device_public_key != &peer.device_public_key
    {
        return Err(PeerError::Protocol(
            "author does not match authenticated device".into(),
        ));
    }
    let Some(source) = authorized_state_source(&workspace, peer, session).await? else {
        for envelope in &envelopes {
            trace_native_publication("server_source_refused", envelope, None);
        }
        let runtime = workspace.lock().await;
        if !current_session(&runtime, peer, session.outbound) {
            return Err(lost_session(&runtime, peer, session, RoomOperation::State));
        }
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id: *workspace_id,
            reason: unpublished(&runtime, peer).into(),
        }));
    };
    for envelope in &envelopes {
        trace_native_publication("server_authorized", envelope, None);
    }
    for envelope in &envelopes {
        if desktop_refusal(&workspace, peer.device_public_key, envelope)
            .await
            .is_some()
        {
            // The prefix is kept whole or not at all.
            return Ok(None);
        }
    }
    let governor_tap = workspace.lock().await.governor_tap.clone();
    // Every change of the prefix reaches the room's governor once it is
    // kept, as one sent alone does (`handle_delta`): an access request and
    // the next checkpoint arriving together must not hide the request from
    // the channel's steward.
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
            peer.role,
            session.metrics.clone(),
        )
        .await?;
        drop(budget);
    }
    if let Some(tap) = governor_tap {
        for body in notices {
            tell_governor(&tap, peer.device_public_key, body).await;
        }
    }
    Ok(None)
}

pub(super) async fn authorized_state_source(
    workspace: &SharedWorkspace,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<StateSource>, PeerError> {
    let source = StateSource::new(peer, session);
    let runtime = crate::workspace_io::fresh_membership(workspace).await?;
    Ok(source.publishes(&runtime).then_some(source))
}

/// Why the relay took none of `peer`'s frames into the room.
pub(super) fn unpublished(
    runtime: &crate::workspace::WorkspaceRuntime,
    peer: &AuthenticatedPeer,
) -> &'static str {
    if room_access::watches_only(runtime, peer) {
        room_access::DenyReason::WatchOnly.reason()
    } else {
        "workspace membership changed"
    }
}
