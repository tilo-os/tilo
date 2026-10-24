//! State admission: [`enqueue_state`] and its host and federated variants
//! hand a delta to the workspace's ordered persistence owner, which
//! retains it and fans it out once durable.

use std::ops::ControlFlow;

use super::{
    Arc, Envelope, FanoutStamp, HashMap, Invariant, LiveFanout, LiveFrame, PeerError,
    PreparedRecipients, PublicationFrames, PublicationScope, ReplayCatchup, ReplayPendingTarget,
    ReplayPendingWork, SelectedPublication, SessionLease, SharedWorkspace, StateCommit,
    StateSource, Step, StoreError, WorkLimit, captured_live_current, live_budget,
    retention_publication_kind, steward_audience, steward_only_delta, test_point,
    trace_native_publication,
};

pub(super) async fn enqueue_state(
    workspace: &SharedWorkspace,
    envelope: Envelope,
    source: Option<StateSource>,
    role: crate::auth::PeerRole,
    metrics: Arc<crate::metrics::Metrics>,
) -> Result<(), PeerError> {
    Box::pin(enqueue_state_inner(
        workspace, envelope, source, role, metrics, false,
    ))
    .await
    .map(drop)
}

/// A state commit admitted to its workspace's ordered persistence owner.
pub(super) struct AdmittedState {
    /// Resolves once the commit is durable and published.
    pub(super) completion: tokio::sync::oneshot::Receiver<Result<StateCommit, StoreError>>,
    /// The durable host order it took; 0 until then, or when unpublished.
    pub(super) sequence: Arc<std::sync::atomic::AtomicU64>,
}

/// Correlate admission and owner phases without retaining a signed payload.
#[derive(Clone)]
struct StateAdmissionTrace {
    id: blake3::Hash,
    kind: &'static str,
    workspace_id: u64,
    author: [u8; 4],
}

impl StateAdmissionTrace {
    fn new(envelope: &Envelope) -> Option<Self> {
        if !tracing::enabled!(target: "tilo_relay::native_publication", tracing::Level::DEBUG) {
            return None;
        }
        let Envelope::Delta {
            workspace_id,
            author_device_public_key,
            delta,
            ..
        } = envelope
        else {
            return None;
        };
        let payload = crate::coedit::CoeditPayload::decode(delta)?;
        let (kind, body) = match payload {
            crate::coedit::CoeditPayload::Checkpoint { body, .. } => ("checkpoint", body),
            crate::coedit::CoeditPayload::RetainedDelta { body, .. } => ("delta", body),
        };
        let mut author = [0; 4];
        author.copy_from_slice(&author_device_public_key[..4]);
        Some(Self {
            id: blake3::hash(&body),
            kind,
            workspace_id: *workspace_id,
            author,
        })
    }

    fn emit(&self, phase: &str) {
        tracing::debug!(target: "tilo_relay::native_publication",
            phase, id = %self.id, kind = self.kind,
            workspace_id = self.workspace_id,
            author = %hex::encode(self.author),
            "native relay publication hop");
    }
}

/// A dropped admission future must be distinguishable from a slow owner job.
struct AdmissionWaitTrace(Option<StateAdmissionTrace>);

impl Drop for AdmissionWaitTrace {
    fn drop(&mut self) {
        if let Some(trace) = &self.0 {
            trace.emit("persistence_admission_cancelled");
        }
    }
}

/// A host commit holding its place in its workspace's order, not yet
/// durable.
pub(crate) struct HostAdmission(AdmittedState);

impl HostAdmission {
    /// Wait until the commit is durable and published; returns its host
    /// order.
    pub(crate) async fn durable(self) -> Result<u64, String> {
        self.0
            .completion
            .await
            .map_err(|_| "the workspace owner stopped before the commit was durable".to_owned())?
            .map_err(|error| error.to_string())?;
        Ok(self.0.sequence.load(std::sync::atomic::Ordering::SeqCst))
    }
}

/// Checkpoint stages written at once, process-wide: each holds a blocking
/// thread through its barriers.
const FRESH_STAGES: usize = 16;

/// Stage `envelope` before its owner turn when it is a checkpoint that may
/// start its lineage, or replace the whole of it
/// ([`crate::store::WorkspaceStore::stage_fresh_checkpoint`]). Every
/// newcomer of a room retains such a checkpoint first, its identity record,
/// then its access requests and acknowledgements, each replacing its last;
/// the owner commits one in a single barrier instead of three rounds of
/// them. `None` when it is not one, or the stage failed: the turn then
/// retains it as any checkpoint.
async fn stage_fresh_checkpoint(
    store: &crate::store::WorkspaceStore,
    envelope: &Envelope,
) -> UntakenStage {
    UntakenStage(stage_fresh_checkpoint_inner(store, envelope).await)
}

async fn stage_fresh_checkpoint_inner(
    store: &crate::store::WorkspaceStore,
    envelope: &Envelope,
) -> Option<crate::store::retained::FreshCheckpoint> {
    static STAGES: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    let Envelope::Delta {
        author_device_public_key,
        delta,
        ..
    } = envelope
    else {
        return None;
    };
    let Some((channel, crate::coedit::RetentionKind::Checkpoint)) =
        crate::coedit::retention_metadata(delta)
    else {
        return None;
    };
    // Most checkpoints replace an earlier one of their lineage: one stat
    // tells, before anything is encoded or leaves this task. Of those, one
    // that stands alone (a device's next access request, acknowledgement or
    // certificate) replaces the whole of it, and is staged too.
    if store.lineage_has_generation(author_device_public_key, &channel)
        && !standalone_checkpoint(store, &channel, delta)
    {
        return None;
    }
    let raw = crate::transport::encode(envelope).ok()?;
    let permit = STAGES
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(FRESH_STAGES)))
        .clone()
        .acquire_owned()
        .await
        .ok()?;
    let store = store.clone();
    let staged = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        crate::timing::windowed("stage.checkpoint", || {
            store.stage_fresh_checkpoint(&channel, &raw)
        })
    })
    .await
    .ok()?;
    staged
        .inspect_err(|error| tracing::debug!(%error, "fresh checkpoint stage failed"))
        .ok()
        .flatten()
}

/// Whether `delta`, a checkpoint on `channel`, is the whole of its route
/// ([`crate::payloads::Checkpoint::standalone`]).
fn standalone_checkpoint(
    store: &crate::store::WorkspaceStore,
    channel: &[u8; 32],
    delta: &[u8],
) -> bool {
    let Some(crate::coedit::CoeditPayload::Checkpoint { body, .. }) =
        crate::coedit::CoeditPayload::decode(delta)
    else {
        return false;
    };
    store
        .payloads()
        .checkpoint(channel, &body)
        .is_some_and(|checkpoint| checkpoint.standalone)
}

/// A staged checkpoint until its owner turn takes it. A stage no turn
/// takes (the job failed or was refused before its turn) is discarded when
/// this drops: its lineage may never commit, and nothing else would.
struct UntakenStage(Option<crate::store::retained::FreshCheckpoint>);

impl UntakenStage {
    fn take(&mut self) -> Option<crate::store::retained::FreshCheckpoint> {
        self.0.take()
    }
}

impl Drop for UntakenStage {
    fn drop(&mut self) {
        let Some(fresh) = self.0.take() else {
            return;
        };
        // A few unlinks, off the async thread when there is one.
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn_blocking(move || fresh.discard())),
            Err(_) => fresh.discard(),
        }
    }
}

/// Begin retaining `raw` in the owner's turn: commit `fresh`, its stage,
/// when there is one and it still starts its lineage; otherwise retain it
/// as usual and leave the stage to the owner's disposal. A stage is only
/// ever of a checkpoint, which has no `seq`.
fn retain_or_commit_stage(
    state: &mut crate::persistence::State,
    channel: &[u8; 32],
    seq: Option<u64>,
    raw: Vec<u8>,
    fresh: Option<crate::store::retained::FreshCheckpoint>,
) -> Result<crate::store::retained::RetainBuild, crate::store::StoreError> {
    match (fresh, seq) {
        (Some(fresh), None) => {
            // Disposed of after the job, unless its lineage selected it.
            state.retention_candidates.push(fresh.discard_paths());
            match state.store.start_fresh_retain(channel, fresh, raw)? {
                Ok(build) => {
                    // Counted: the turns that only selected their stage.
                    crate::timing::window("owner.commit.staged", crate::timing::start());
                    Ok(build)
                }
                Err((_, raw)) => {
                    crate::timing::window("owner.commit.stage_moved", crate::timing::start());
                    state.store.start_retain(channel, None, raw)
                }
            }
        }
        (fresh, seq) => {
            if let Some(fresh) = fresh {
                state.retention_candidates.push(fresh.discard_paths());
            }
            state.store.start_retain(channel, seq, raw)
        }
    }
}

/// Admit `envelope`, authored by this host's own device, through the
/// workspace's ordered persistence owner, as a device's commit is admitted,
/// without waiting for it to be durable: commits queued one after another
/// keep that order, so a caller can queue under a lock and wait after
/// releasing it.
pub(crate) async fn enqueue_host_state(
    workspace: &SharedWorkspace,
    envelope: Envelope,
    metrics: Arc<crate::metrics::Metrics>,
) -> Result<HostAdmission, String> {
    Box::pin(enqueue_state_inner(
        workspace,
        envelope,
        None,
        crate::auth::PeerRole::Device,
        metrics,
        false,
    ))
    .await
    .map(HostAdmission)
    .map_err(|error| error.to_string())
}
pub async fn enqueue_federated(
    workspace: &SharedWorkspace,
    envelope: Envelope,
    source: StateSource,
    metrics: Arc<crate::metrics::Metrics>,
    sync: bool,
) -> Result<(), PeerError> {
    Box::pin(enqueue_state_inner(
        workspace,
        envelope,
        Some(source),
        crate::auth::PeerRole::PeerRelay,
        metrics,
        sync,
    ))
    .await
    .map(drop)
}

/// What the capacity timings call a change's owner turn and publication:
/// by author (this host or a member) and retention kind. Read only while
/// the timings are recorded.
fn state_timing_steps(host: bool, delta: &[u8]) -> (&'static str, &'static str) {
    if !crate::timing::enabled() {
        return ("", "");
    }
    match (
        host,
        crate::coedit::retention_metadata(delta).map(|(_, kind)| kind),
    ) {
        (true, Some(crate::coedit::RetentionKind::Checkpoint)) => (
            "state.host_checkpoint.persist",
            "state.host_checkpoint.publish",
        ),
        (true, Some(crate::coedit::RetentionKind::Delta)) => {
            ("state.host_delta.persist", "state.host_delta.publish")
        }
        (false, Some(crate::coedit::RetentionKind::Checkpoint)) => device_checkpoint_steps(delta),
        (false, Some(crate::coedit::RetentionKind::Delta)) => {
            ("state.delta.persist", "state.delta.publish")
        }
        (_, None) => ("state.plain.persist", "state.plain.publish"),
    }
}

/// [`state_timing_steps`] for a member's checkpoint: the governance it
/// carries, when it is a channel request, acknowledgement or certificate.
fn device_checkpoint_steps(delta: &[u8]) -> (&'static str, &'static str) {
    use neighbour_wire::NeighbourMsg;
    let plain = ("state.checkpoint.persist", "state.checkpoint.publish");
    let Some(crate::coedit::CoeditPayload::Checkpoint { body, .. }) =
        crate::coedit::CoeditPayload::decode(delta)
    else {
        return plain;
    };
    match postcard::from_bytes::<NeighbourMsg>(&body) {
        Ok(NeighbourMsg::MlsAccessRequest { .. }) => (
            "state.checkpoint.request.persist",
            "state.checkpoint.request.publish",
        ),
        Ok(NeighbourMsg::MlsRoutedAccessRequest { .. }) => (
            "state.checkpoint.routed_request.persist",
            "state.checkpoint.routed_request.publish",
        ),
        Ok(NeighbourMsg::MlsGovernanceAck { .. }) => (
            "state.checkpoint.ack.persist",
            "state.checkpoint.ack.publish",
        ),
        Ok(NeighbourMsg::IdentityHello { .. }) => (
            "state.checkpoint.hello.persist",
            "state.checkpoint.hello.publish",
        ),
        Ok(NeighbourMsg::AppDocument { .. }) => (
            "state.checkpoint.document.persist",
            "state.checkpoint.document.publish",
        ),
        _ => plain,
    }
}

/// Admit `envelope` through its workspace's ordered persistence owner:
/// authorize its source and stage a fresh checkpoint, then submit the owner
/// turn ([`StateTurn`]) that makes it durable and the completion
/// ([`StatePublisher`]) that publishes it.
pub(super) async fn enqueue_state_inner(
    workspace: &SharedWorkspace,
    envelope: Envelope,
    source: Option<StateSource>,
    role: crate::auth::PeerRole,
    metrics: Arc<crate::metrics::Metrics>,
    sync: bool,
) -> Result<AdmittedState, PeerError> {
    let (author, delta) = admitted_delta(&envelope)?;
    let delta_len = delta.len();
    let charge = delta_len.saturating_add(256).saturating_mul(3);
    let (persist_step, publish_step) = state_timing_steps(source.is_none(), delta);
    let persist_begun = Arc::new(std::sync::OnceLock::new());
    let persist_ended = persist_begun.clone();
    let timed_bytes = delta_len as u64;
    trace_native_publication("admission_started", &envelope, None);
    let (payloads, store) = authorize_admission(workspace, &envelope, source.as_ref()).await?;
    // What only a channel's steward reads goes to the room's governor alone
    // when this relay governs the room (`steward_audience`).
    let steward_only = steward_only_delta(&*payloads, delta);
    let owner = crate::persistence::handle(workspace).await;
    let continuity_evidence = source.as_ref().and_then(|source| {
        crate::store::continuity::Evidence::authenticated(&*payloads, &envelope, &source.peer)
    });
    // A checkpoint that may start its lineage is staged before its turn;
    // the turn commits the stage, or retains it as usual if it cannot.
    let fresh = if sync || continuity_evidence.is_some() {
        UntakenStage(None)
    } else {
        stage_fresh_checkpoint(&store, &envelope).await
    };
    drop(store);
    trace_native_publication("persistence_admission_wait", &envelope, None);
    let admission_trace = StateAdmissionTrace::new(&envelope);
    let mut wait_trace = AdmissionWaitTrace(admission_trace.clone());
    let durable = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let preparation = prepare_admission(
        workspace.clone(),
        source.clone(),
        author,
        role,
        delta_len,
        steward_only,
    );
    let mut turn = StateTurn {
        envelope: Some(envelope),
        sync,
        fresh,
        continuity_evidence,
        durable_sequence: durable.clone(),
        trace: admission_trace.clone(),
        metrics: metrics.clone(),
        persist_begun,
        sequence: None,
        commit: None,
        retained_build: None,
        replay_pending_work: None,
        pending_work: None,
    };
    // What this host authors itself (room and channel governance) has a
    // lane of its own, so it never waits behind a crowd's member traffic.
    let lane = if source.is_none() {
        crate::persistence::Lane::Host
    } else {
        crate::persistence::Lane::Member
    };
    let publisher = StatePublisher {
        workers: owner.workers(),
        workspace: workspace.clone(),
        source,
        metrics,
        trace: admission_trace.clone(),
        timing: PublicationTiming {
            persist_step,
            publish_step,
            persist_ended,
            timed_bytes,
        },
    };
    let completion = Box::pin(owner.submit_commit_wait(
        lane,
        charge,
        preparation,
        move |state, prepared| turn.step(state, prepared),
        move |result| publisher.complete(result),
    ))
    .await;
    wait_trace.0 = None;
    let completion = match completion {
        Ok(completion) => {
            if let Some(trace) = &admission_trace {
                trace.emit("persistence_admitted");
            }
            completion
        }
        Err(error) => {
            if let Some(trace) = &admission_trace {
                trace.emit("persistence_admission_failed");
            }
            return Err(error.into());
        }
    };
    Ok(AdmittedState {
        completion,
        sequence: durable,
    })
}

/// The author of the delta `envelope` carries, and the delta.
fn admitted_delta(envelope: &Envelope) -> Result<([u8; 32], &[u8]), PeerError> {
    let Envelope::Delta {
        author_device_public_key: author,
        delta,
        ..
    } = envelope
    else {
        return Err(PeerError::Protocol("expected delta".into()));
    };
    if crate::persistence::correlated_diagnostics()
        && let Envelope::Delta {
            workspace_id,
            authored_at_ns,
            ..
        } = envelope
    {
        tracing::info!(phase = "server_delta_admit", workspace_id, authored_at_ns);
    }
    Ok((*author, delta))
}

/// The room's payload codecs and store, read under a membership check that
/// the source may still write to it.
type AuthorizedAdmission = (
    Arc<dyn crate::payloads::ShellPayloads>,
    crate::store::WorkspaceStore,
);

/// Check the source may still write to the room before anything is staged.
async fn authorize_admission(
    workspace: &SharedWorkspace,
    envelope: &Envelope,
    source: Option<&StateSource>,
) -> Result<AuthorizedAdmission, PeerError> {
    let runtime = crate::workspace_io::fresh_membership(workspace).await?;
    if source.is_some_and(|source| !source.authorized(&runtime)) {
        trace_native_publication("admission_source_changed", envelope, None);
        return Err(PeerError::Protocol(
            "state source authorization changed".into(),
        ));
    }
    trace_native_publication("admission_authorized", envelope, None);
    let payloads = runtime.store.payloads().clone();
    let store = runtime.store.clone();
    drop(runtime);
    Ok((payloads, store))
}

/// Who an admitted delta goes to, captured in its owner turn's order.
struct AdmissionRecipients {
    /// Every device the delta is owed to, live or not.
    recipients: std::collections::HashSet<[u8; 32]>,
    /// The live sessions and replay targets among them.
    prepared: PreparedRecipients,
}

/// The owner turn's preparation: snapshot an admitted delta's recipients
/// under a fresh membership read.
async fn prepare_admission(
    workspace: SharedWorkspace,
    source: Option<StateSource>,
    author: [u8; 32],
    role: crate::auth::PeerRole,
    delta_len: usize,
    steward_only: bool,
) -> Result<AdmissionRecipients, crate::store::StoreError> {
    let runtime = crate::workspace_io::fresh_membership(&workspace).await?;
    if source
        .as_ref()
        .is_some_and(|source| !source.authorized(&runtime))
    {
        return Err(crate::persistence::error(
            "state source authorization changed",
        ));
    }
    let mut recipients = runtime
        .membership
        .devices
        .iter()
        .copied()
        .filter(|key| key != &author)
        .collect::<std::collections::HashSet<_>>();
    if role == crate::auth::PeerRole::Device {
        recipients.extend(
            runtime
                .known_peer_relays
                .iter()
                .copied()
                .filter(|key| key != &author),
        );
    }
    // Include admitted cross-user live members; explicit device membership is
    // not the only signed-room authority supported by subscriptions.
    // A change only the steward reads reaches no other session, live
    // or through a catch-up.
    let steward = steward_audience(&runtime, steward_only);
    // A device's own change is not sent back to it. What the host commits
    // itself (the governor's decisions, a room bot's writes) no session
    // sent, so it reaches the host device's own session too: the room
    // actions the host runs for members read through that session, and
    // would otherwise never see a bot complete what a member started.
    let host_commit = source.is_none();
    let sessions = runtime
        .subscribers
        .iter()
        .filter(|(key, sub)| {
            (**key != author || host_commit)
                && !(role == crate::auth::PeerRole::PeerRelay && runtime.federated(key, sub))
                && steward
                    .as_ref()
                    .is_none_or(|steward| steward.contains(*key))
        })
        .map(|(key, sub)| (*key, sub.tx.clone()))
        .collect::<Vec<_>>();
    let session_lives = sessions
        .iter()
        .filter_map(|(device, tx)| {
            runtime
                .state_sessions
                .get(device)
                .map(|session| SessionLease {
                    device: *device,
                    outbound: tx.clone(),
                    live: session.live.clone(),
                })
        })
        .collect();
    let replay_targets = sessions
        .iter()
        .filter_map(|(device, _)| {
            let session = runtime.state_sessions.get(device)?;
            let catchup = session.replay.as_ref()?.clone();
            (session.live.load(std::sync::atomic::Ordering::SeqCst)
                && catchup.phase.load(std::sync::atomic::Ordering::SeqCst) == ReplayCatchup::ACTIVE)
                .then(|| ReplayPendingTarget {
                    device: *device,
                    live: session.live.clone(),
                    catchup,
                })
        })
        .collect::<Vec<_>>();
    recipients.extend(sessions.iter().map(|(key, _)| *key));
    if role == crate::auth::PeerRole::PeerRelay {
        recipients.retain(|key| !runtime.known_peer_relays.contains(key));
        tell_withheld(&runtime, source.as_ref());
    }
    let logical_offline_bytes = (delta_len as u64).saturating_mul(
        recipients
            .iter()
            .filter(|key| !runtime.subscribers.contains_key(*key))
            .count() as u64,
    );

    drop(runtime);
    Ok(AdmissionRecipients {
        recipients,
        prepared: PreparedRecipients {
            session_lives,
            sessions,
            replay_targets,
            logical_offline_bytes,
        },
    })
}

/// Tell every other federated host subscribed to `runtime`'s workspace that
/// it is behind: a change one federated host sent is not forwarded to
/// another (that would loop a mesh), so each asks for what it lacks
/// instead. From a host, a `SyncRequest` says exactly that
/// ([`crate::federation`]); its heads are not this host's to send.
fn tell_withheld(runtime: &crate::workspace::WorkspaceRuntime, source: Option<&StateSource>) {
    let origin = source.map(|source| source.peer.device_public_key);
    let workspace_id = runtime.store.workspace_id();
    for (key, subscriber) in &runtime.subscribers {
        if runtime.federated(key, subscriber) && Some(*key) != origin {
            // A host too far behind to take the word reconnects, and asks
            // for everything it lacks then.
            if subscriber
                .tx
                .try_send(Envelope::SyncRequest {
                    workspace_id,
                    have_heads: Vec::new(),
                })
                .is_err()
            {
                let _ = subscriber.disconnect.try_send(());
            }
        }
    }
}

/// Queued offline copies of an unretained delta: its encoding and the
/// recipients not yet queued.
type OfflineQueue = (Vec<u8>, std::collections::hash_set::IntoIter<[u8; 32]>);

/// An admitted delta's owner turn, stepped one bounded quantum at a time:
/// take its sequence and begin retaining it, finish the retained write,
/// hold it for catching-up sessions, queue it for offline recipients, then
/// materialize it.
struct StateTurn {
    envelope: Option<Envelope>,
    sync: bool,
    fresh: UntakenStage,
    continuity_evidence: Option<crate::store::continuity::Evidence>,
    durable_sequence: Arc<std::sync::atomic::AtomicU64>,
    trace: Option<StateAdmissionTrace>,
    metrics: Arc<crate::metrics::Metrics>,
    /// When the owner began the turn, for the capacity timings.
    persist_begun: Arc<std::sync::OnceLock<std::time::Instant>>,
    sequence: Option<u64>,
    commit: Option<StateCommit>,
    retained_build: Option<crate::store::retained::RetainBuild>,
    replay_pending_work: Option<ReplayPendingWork>,
    pending_work: Option<OfflineQueue>,
}

impl StateTurn {
    fn step(
        &mut self,
        state: &mut crate::persistence::State,
        prepared: &mut Option<AdmissionRecipients>,
    ) -> Result<Step<StateCommit>, crate::store::StoreError> {
        if self.commit.is_none() {
            return self.begin(state, prepared);
        }
        if let ControlFlow::Break(step) = self.step_retention(state)? {
            return Ok(step);
        }
        if matches!(self.step_replay_pending(state)?, Step::More) {
            return Ok(Step::More);
        }
        if matches!(self.step_offline_queue(state)?, Step::More) {
            return Ok(Step::More);
        }
        self.sequence.invariant("sequence missing")?;
        let current = self.commit.as_ref().invariant("commit missing")?;
        let Envelope::Delta { delta, .. } = &*current.envelope else {
            return Err(crate::persistence::error(
                "state commit envelope is not a delta",
            ));
        };
        state.materialize(delta);
        Ok(Step::Done(self.commit.take().invariant("commit missing")?))
    }

    /// The turn's first quantum: take the delta's sequence and begin
    /// retaining it, or finish at once when a sync frame changes nothing.
    fn begin(
        &mut self,
        state: &mut crate::persistence::State,
        prepared: &mut Option<AdmissionRecipients>,
    ) -> Result<Step<StateCommit>, crate::store::StoreError> {
        // This job changes retained history only through retention.
        state.retaining_only();
        if let Some(begun) = crate::timing::start() {
            let _ = self.persist_begun.set(begun);
        }
        if let Some(trace) = &self.trace {
            trace.emit("persistence_owner_started");
        }
        let AdmissionRecipients {
            recipients,
            prepared: captured,
        } = prepared.take().invariant("ordered recipients")?;
        let envelope = self.envelope.take().invariant("admitted envelope")?;
        let Envelope::Delta { delta, .. } = &envelope else {
            return Err(crate::persistence::error("expected admitted delta"));
        };
        let publish = !self.sync || state.would_materialize(delta);
        if !publish {
            return Ok(Step::Done(admitted_commit(
                None, envelope, captured, publish,
            )));
        }
        let seq = crate::timing::windowed("owner.commit.sequence", || state.next_sequence())?;
        self.sequence = Some(seq);
        self.durable_sequence
            .store(seq, std::sync::atomic::Ordering::SeqCst);
        let continuity = self
            .continuity_evidence
            .clone()
            .map(|evidence| evidence.accept(seq));
        let raw = crate::transport::encode(&envelope)
            .map_err(|e| crate::persistence::error(e.to_string()))?;
        let retention =
            crate::coedit::retention_metadata(delta).map(|(channel, kind)| match kind {
                crate::coedit::RetentionKind::Checkpoint => (channel, None),
                crate::coedit::RetentionKind::Delta => (channel, Some(seq)),
            });
        if let Some((channel, seq)) = retention {
            test_point!(pause(state.store.root(), "state-persist"));
            // A checkpoint may replace its lineage's generation whole, as
            // listed: every record the owner's group holds is under its name
            // first. A delta joins the group (`retained::SharedGroup`).
            if seq.is_none() {
                state.group.flush()?;
            }
            let build = crate::timing::windowed("owner.commit.retain_start", || {
                retain_or_commit_stage(state, &channel, seq, raw, self.fresh.take())
            })?
            .with_continuity(continuity.clone());
            let build = if seq.is_some() {
                build.deferring(state.group.records())?
            } else {
                build
            };
            state.retention_scratch = Some(build.scratch().to_owned());
            self.retained_build = Some(build);
        } else {
            self.pending_work = Some((raw, recipients.into_iter()));
        }
        self.commit = Some(admitted_commit(continuity, envelope, captured, publish));
        Ok(Step::More)
    }

    /// Advance the retained write by one quantum. Breaks with the turn's
    /// step while the write runs, or once it failed past its cut.
    fn step_retention(
        &mut self,
        state: &mut crate::persistence::State,
    ) -> Result<ControlFlow<Step<StateCommit>>, crate::store::StoreError> {
        let current = self.commit.as_mut().invariant("commit missing")?;
        let Some(build) = self.retained_build.as_mut() else {
            return Ok(ControlFlow::Continue(()));
        };
        let result = match crate::timing::windowed("owner.commit.retain_step", || {
            build.step(WorkLimit::default())
        }) {
            Ok(result) => result,
            Err(error) => {
                let Some(activation) = build.publication_on_error() else {
                    return Err(error);
                };
                // Keep the captured sessions and uncertain/committed cut
                // across the worker error so completion fences them.
                let Some(selected) = SelectedPublication::from_publication(activation) else {
                    return Err(error);
                };
                current.selected = Some(selected);
                current.activation_error = Some(error);
                return Ok(ControlFlow::Break(Step::Done(
                    self.commit.take().invariant("commit missing")?,
                )));
            }
        };
        let Step::Done(outcome) = result else {
            return Ok(ControlFlow::Break(Step::More));
        };
        let written = outcome.written;
        let publication = retention_publication_kind(&outcome.publication);
        if let Some(work) = select_retained_publication(current, outcome.publication)? {
            self.replay_pending_work = Some(work);
        }
        if crate::persistence::diagnostics() {
            trace_retention_complete(build, publication);
        }
        if !build.holds_scratch() {
            // A plain delta made no scratch, so the owner has
            // nothing to dispose of after this job.
            state.retention_scratch = None;
        }
        self.metrics.add_bytes_queued(written);
        current.persisted_bytes = written;
        self.retained_build = None;
        Ok(ControlFlow::Continue(()))
    }

    /// Hold the commit for sessions catching up by replay, one output budget
    /// per quantum. A failure is the activation's, for completion to fence.
    fn step_replay_pending(
        &mut self,
        state: &mut crate::persistence::State,
    ) -> Result<Step<()>, crate::store::StoreError> {
        let current = self.commit.as_mut().invariant("commit missing")?;
        if let Some(work) = self.replay_pending_work.as_mut() {
            match crate::timing::windowed("owner.commit.held_step", || {
                work.step(state, crate::persistence::output_budget())
            }) {
                Ok(Step::More) => return Ok(Step::More),
                Ok(Step::Done(written)) => {
                    self.metrics.add_bytes_queued(written);
                    current.persisted_bytes = current.persisted_bytes.saturating_add(written);
                    self.replay_pending_work = None;
                }
                Err(error) => {
                    current.activation_error = Some(error);
                    self.replay_pending_work = None;
                }
            }
        }
        Ok(Step::Done(()))
    }

    /// Queue an unretained delta for its recipients, one bounded run of them
    /// per quantum.
    fn step_offline_queue(
        &mut self,
        state: &crate::persistence::State,
    ) -> Result<Step<()>, crate::store::StoreError> {
        let current = self.commit.as_mut().invariant("commit missing")?;
        if let Some((raw, recipients)) = &mut self.pending_work {
            for _ in 0..crate::store::bounded::RUN_ENTRIES {
                let Some(recipient) = recipients.next() else {
                    self.pending_work = None;
                    break;
                };
                let path = state.store.enqueue_pending(
                    &recipient,
                    self.sequence.invariant("sequence missing")?,
                    raw,
                )?;
                current.pending.insert(recipient, path);
                self.metrics.add_bytes_queued(raw.len() as u64);
                current.persisted_bytes = current.persisted_bytes.saturating_add(raw.len() as u64);
            }
            if self.pending_work.is_some() {
                return Ok(Step::More);
            }
        }
        Ok(Step::Done(()))
    }
}

/// The commit an admitted delta makes, before anything of it is written.
fn admitted_commit(
    continuity: Option<crate::store::continuity::Receipt>,
    envelope: Envelope,
    prepared: PreparedRecipients,
    publish: bool,
) -> StateCommit {
    StateCommit {
        continuity,
        envelope: Arc::new(envelope),
        pending: HashMap::new(),
        persisted_bytes: 0,
        sessions: prepared.sessions,
        session_lives: prepared.session_lives,
        logical_offline_bytes: prepared.logical_offline_bytes,
        publish,
        selected: None,
        recovery_scope: false,
        activation_error: None,
        replay_targets: prepared.replay_targets,
        accepted_deltas: 1,
    }
}

/// Record on `current` what its finished retained write publishes; returns
/// the replay hold its catching-up sessions need, if any.
fn select_retained_publication(
    current: &mut StateCommit,
    publication: crate::store::shell_selection::Publication,
) -> Result<Option<ReplayPendingWork>, crate::store::StoreError> {
    match publication {
        crate::store::shell_selection::Publication::Suppress => {
            current.publish = false;
            current.replay_targets.clear();
        }
        publication @ (crate::store::shell_selection::Publication::Activate(_)
        | crate::store::shell_selection::Publication::ReplayRequired(_)) => {
            let Some(selected) = SelectedPublication::from_publication(publication) else {
                return Err(crate::persistence::error(
                    "selected publication changed kind",
                ));
            };
            current.selected = Some(selected.clone());
            if !current.replay_targets.is_empty() {
                match ReplayPendingWork::selected(&selected, current.replay_targets.clone()) {
                    Ok(work) => return Ok(Some(work)),
                    Err(error) => current.activation_error = Some(error),
                }
            }
        }
        crate::store::shell_selection::Publication::Original => {
            if !current.replay_targets.is_empty() {
                return Ok(Some(ReplayPendingWork::original(
                    current.envelope.clone(),
                    current.replay_targets.clone(),
                    current.continuity.clone(),
                )));
            }
        }
    }
    Ok(None)
}

fn trace_retention_complete(build: &crate::store::retained::RetainBuild, publication: &str) {
    let selection = build.selection_stats();
    tracing::info!(
        phase = "retention_complete",
        verified_publication = publication,
        selected_obsolete = build.selected_obsolete(),
        retention_preparation_reads = build.retention_stats().preparation_reads,
        retention_peak_metadata_entries = build.retention_stats().peak_metadata_entries,
        selection_preparation_reads = selection.map_or(0, |stats| stats.preparation_reads),
        selection_peak_metadata_entries = selection.map_or(0, |stats| stats.peak_metadata_entries),
    );
}

/// An admitted delta's completion: once its owner turn made it durable,
/// publish it to the room's live sessions.
struct StatePublisher {
    workers: Arc<tokio::sync::Semaphore>,
    workspace: SharedWorkspace,
    source: Option<StateSource>,
    metrics: Arc<crate::metrics::Metrics>,
    trace: Option<StateAdmissionTrace>,
    timing: PublicationTiming,
}

/// What the capacity timings record of a change's owner turn and
/// publication ([`state_timing_steps`]).
struct PublicationTiming {
    persist_step: &'static str,
    publish_step: &'static str,
    persist_ended: Arc<std::sync::OnceLock<std::time::Instant>>,
    timed_bytes: u64,
}

impl StatePublisher {
    async fn complete(
        self,
        result: Result<StateCommit, crate::store::StoreError>,
    ) -> Result<StateCommit, crate::store::StoreError> {
        crate::timing::event(
            self.timing.persist_step,
            self.timing.persist_ended.get().copied(),
            self.timing.timed_bytes,
        );
        let publishing = crate::timing::start();
        let result = match result {
            Ok(commit) => self.publish(commit).await,
            Err(error) => Err(error),
        };
        if let Some(trace) = &self.trace {
            trace.emit(if result.is_ok() {
                "persistence_completed"
            } else {
                "persistence_failed"
            });
        }
        if crate::persistence::correlated_diagnostics()
            && let Err(error) = &result
        {
            tracing::info!(phase = "state_commit_error", error = %error);
        }
        if result.is_err()
            && let Some(source) = self.source
        {
            let _ = source.disconnect.try_send(());
        }
        crate::timing::event(
            self.timing.publish_step,
            publishing,
            self.timing.timed_bytes,
        );
        result
    }

    async fn publish(
        &self,
        mut commit: StateCommit,
    ) -> Result<StateCommit, crate::store::StoreError> {
        trace_native_publication(
            if commit.activation_error.is_some() {
                "activation_error"
            } else if commit.publish {
                "durable"
            } else {
                "suppressed"
            },
            &commit.envelope,
            None,
        );
        if !commit.publish {
            return Ok(commit);
        }
        for _ in 0..commit.accepted_deltas {
            self.metrics.inc_deltas_received();
        }
        if crate::persistence::correlated_diagnostics()
            && let Envelope::Delta {
                workspace_id,
                authored_at_ns,
                ..
            } = &*commit.envelope
        {
            tracing::info!(
                phase = "server_delta_committed",
                workspace_id,
                authored_at_ns,
                sessions = commit.sessions.len()
            );
        }
        if crate::persistence::diagnostics() {
            tracing::info!(
                phase = "state_commit",
                persisted_envelope_bytes = commit.persisted_bytes,
                logical_offline_bytes = commit.logical_offline_bytes
            );
        }
        let scope = PublicationScope::of(&commit);
        let replay_targets = commit.replay_targets.clone();
        let mut delivered = 0u64;
        let publication_result = self.deliver(&mut commit, scope, &mut delivered).await;
        self.metrics.add_deltas_fanned_out(delivered);
        if let Err(error) = publication_result {
            if scope.activating || !replay_targets.is_empty() {
                // A partial activation cannot leave a ready subscriber
                // waiting forever for its missing exact half. Fence only
                // the original session; reconnect uses durable full basis.
                self.fence(&commit, scope, &replay_targets).await;
            }
            return Err(error);
        }
        Ok(commit)
    }

    /// Publish each frame of `commit` to its captured sessions, counting
    /// every delivery in `delivered`.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room guard is used until the end of each frame, through LiveFanout::deliver; the lint counts only method calls on the guard"
    )]
    async fn deliver(
        &self,
        commit: &mut StateCommit,
        scope: PublicationScope,
        delivered: &mut u64,
    ) -> Result<(), crate::store::StoreError> {
        if let Some(error) = commit.activation_error.take() {
            return Err(error);
        }
        let mut frames = PublicationFrames::open(
            commit.selected.take(),
            &commit.envelope,
            self.workers.clone(),
        )
        .await?;
        while let Some(envelope) = frames.next().await? {
            let budget = live_budget(&envelope);
            test_point!(
                if matches!(envelope, super::LiveEnvelope::Original(_))
                    => check(room self.workspace, "live-delivery-budget")
                    else budget = Err(crate::persistence::error(
                        "injected live delivery budget failure",
                    ))
            );
            // Every hydration/worker wait precedes a fresh current-source,
            // recipient membership and exact subscriber-channel fence.
            let mut runtime = crate::workspace_io::fresh_membership(&self.workspace).await?;
            let fanned = FanoutStamp::of(&runtime);
            if self
                .source
                .as_ref()
                .is_some_and(|source| !source.authorized(&runtime))
            {
                if scope.activating {
                    return Err(crate::persistence::error(
                        "activation source authorization changed",
                    ));
                }
                break;
            }
            let frame = LiveFrame::new(&runtime, &envelope, &budget, fanned, self.source.as_ref());
            // Presence the author sent after this delta was held for
            // it and goes out with it; from here on nothing waits.
            if let Envelope::Delta {
                author_device_public_key,
                ..
            } = &*envelope
            {
                runtime.delta_published(author_device_public_key);
            }
            LiveFanout {
                frame: &frame,
                source: self.source.as_ref(),
                scope,
                session_lives: &commit.session_lives,
                trace_recipients: true,
            }
            .deliver(
                &mut runtime,
                &commit.sessions,
                &mut commit.pending,
                delivered,
            )?;
            test_point!(signal(runtime.store.root(), "live-publication-complete"));
        }
        Ok(())
    }

    /// Fence the sessions a failed publication may have left without the
    /// rest of their basis.
    async fn fence(
        &self,
        commit: &StateCommit,
        scope: PublicationScope,
        replay_targets: &[ReplayPendingTarget],
    ) {
        let mut runtime = self.workspace.lock().await;
        if scope.activating {
            for (recipient, original) in &commit.sessions {
                if scope.recovering
                    && !captured_live_current(&commit.session_lives, &runtime, recipient, original)
                {
                    continue;
                }
                if let Some(sub) = runtime.subscribers.get(recipient)
                    && sub.tx.same_channel(original)
                {
                    let _ = sub.disconnect.try_send(());
                    runtime.remove_subscriber_session(recipient, original);
                }
            }
        } else {
            for target in replay_targets {
                if target.current(&runtime)
                    && let Some(sub) = runtime.subscribers.get(&target.device)
                {
                    let outbound = sub.tx.clone();
                    let _ = sub.disconnect.try_send(());
                    runtime.remove_subscriber_session(&target.device, &outbound);
                }
            }
        }
    }
}
