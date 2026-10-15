//! State admission: [`enqueue_state`] and its host and federated variants
//! hand a delta to the workspace's ordered persistence owner, which
//! retains it and fans it out once durable.

use super::{
    Arc, Envelope, FanoutStamp, HashMap, Invariant, LiveEnvelope, PeerError, ReplayCatchup,
    ReplayPendingTarget, ReplayPendingWork, RoomOperation, SelectedPublication, SessionLease,
    SharedWorkspace, StateCommit, StateDelivery, StateSource, Step, StoreError, WorkLimit,
    captured_live_current, gated_checkpoint_channel, live_audience, mpsc,
    retention_publication_kind, room_access, trace_native_publication,
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

/// Fresh-lineage stages written at once, process-wide: each holds a
/// blocking thread through its barriers.
const FRESH_STAGES: usize = 16;

/// Stage `envelope` before its owner turn when it is a checkpoint that may
/// start its lineage ([`crate::store::WorkspaceStore::stage_fresh_checkpoint`]).
/// Every newcomer of a room retains such a checkpoint first, its identity
/// record, and the owner commits one in a single barrier instead of three
/// rounds of them. `None` when it is not one, or the stage failed: the turn
/// then retains it as any checkpoint.
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
    // tells, before anything is encoded or leaves this task.
    if store.lineage_has_generation(author_device_public_key, &channel) {
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
        store.stage_fresh_checkpoint(&channel, &raw)
    })
    .await
    .ok()?;
    staged
        .inspect_err(|error| tracing::debug!(%error, "fresh checkpoint stage failed"))
        .ok()
        .flatten()
}

/// A staged first checkpoint until its owner turn takes it. A stage no turn
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
                Ok(build) => Ok(build),
                Err((_, raw)) => state.store.start_retain(channel, None, raw),
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
pub(crate) async fn enqueue_federated(
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

#[expect(
    clippy::too_many_lines,
    reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the workspace handle and delivery budget move into the persistence closures; the lint does not see the move"
)]
pub(super) async fn enqueue_state_inner(
    workspace: &SharedWorkspace,
    envelope: Envelope,
    source: Option<StateSource>,
    role: crate::auth::PeerRole,
    metrics: Arc<crate::metrics::Metrics>,
    sync: bool,
) -> Result<AdmittedState, PeerError> {
    let Envelope::Delta {
        author_device_public_key: author,
        delta,
        ..
    } = &envelope
    else {
        return Err(PeerError::Protocol("expected delta".into()));
    };
    let author = *author;
    if crate::persistence::correlated_diagnostics()
        && let Envelope::Delta {
            workspace_id,
            authored_at_ns,
            ..
        } = &envelope
    {
        tracing::info!(phase = "server_delta_admit", workspace_id, authored_at_ns);
    }
    let charge = delta.len().saturating_add(256).saturating_mul(3);
    let (persist_step, publish_step) = state_timing_steps(source.is_none(), delta);
    let persist_begun = Arc::new(std::sync::OnceLock::new());
    let persist_ended = persist_begun.clone();
    let timed_bytes = delta.len() as u64;
    trace_native_publication("admission_started", &envelope, None);
    let runtime = crate::workspace_io::fresh_membership(workspace).await?;
    if source
        .as_ref()
        .is_some_and(|source| !source.authorized(&runtime))
    {
        trace_native_publication("admission_source_changed", &envelope, None);
        return Err(PeerError::Protocol(
            "state source authorization changed".into(),
        ));
    }
    trace_native_publication("admission_authorized", &envelope, None);
    let payloads = runtime.store.payloads().clone();
    let store = runtime.store.clone();
    drop(runtime);
    let owner = crate::persistence::handle(workspace).await;
    let continuity_evidence = source.as_ref().and_then(|source| {
        crate::store::continuity::Evidence::authenticated(&*payloads, &envelope, &source.peer)
    });
    // A checkpoint that may start its lineage is staged before its turn;
    // the turn commits the stage, or retains it as usual if it cannot.
    let mut fresh = if sync || continuity_evidence.is_some() {
        UntakenStage(None)
    } else {
        stage_fresh_checkpoint(&store, &envelope).await
    };
    drop(store);
    trace_native_publication("persistence_admission_wait", &envelope, None);
    let admission_trace = StateAdmissionTrace::new(&envelope);
    let mut wait_trace = AdmissionWaitTrace(admission_trace.clone());
    let workers = owner.workers();
    let workspace = workspace.clone();
    let failure = source.clone();
    let persist_metrics = metrics.clone();
    let prepare_workspace = workspace.clone();
    let prepare_source = source.clone();
    let delta_len = delta.len();
    let mut envelope = Some(envelope);
    let mut commit: Option<StateCommit> = None;
    let mut sequence = None;
    let mut retained_build: Option<crate::store::retained::RetainBuild> = None;
    let mut pending_work = None;
    let mut replay_pending_work: Option<ReplayPendingWork> = None;
    let durable = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let durable_sequence = durable.clone();
    let operation_trace = admission_trace.clone();
    let completion_trace = admission_trace.clone();
    let completion = Box::pin(owner.submit_resumable_wait(
        charge,
        async move {
            let runtime = crate::workspace_io::fresh_membership(&prepare_workspace).await?;
            if prepare_source.as_ref().is_some_and(|source| !source.authorized(&runtime)) {
                return Err(crate::persistence::error("state source authorization changed"));
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
            let sessions = runtime
                .subscribers
                .iter()
                .filter(|(key, sub)| {
                    **key != author
                        && !(role == crate::auth::PeerRole::PeerRelay
                            && sub.role == crate::auth::PeerRole::PeerRelay)
                })
                .map(|(key, sub)| (*key, sub.tx.clone()))
                .collect::<Vec<_>>();
            let session_lives = sessions.iter().filter_map(|(device,tx)| runtime.state_sessions.get(device).map(|session| SessionLease {device:*device,outbound:tx.clone(),live:session.live.clone()})).collect();
            let replay_targets = sessions
                .iter()
                .filter_map(|(device, _)| {
                    let session = runtime.state_sessions.get(device)?;
                    let catchup = session.replay.as_ref()?.clone();
                    (session.live.load(std::sync::atomic::Ordering::SeqCst)
                        && catchup.phase.load(std::sync::atomic::Ordering::SeqCst)
                            == ReplayCatchup::ACTIVE)
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
            }
            let logical_offline_bytes = (delta_len as u64).saturating_mul(
                recipients
                    .iter()
                    .filter(|key| !runtime.subscribers.contains_key(*key))
                    .count() as u64,
            );

            drop(runtime);
            Ok((
                recipients,
                sessions,
                session_lives,
                replay_targets,
                logical_offline_bytes,
            ))
        },
        move |state, prepared| {
            if commit.is_none() {
                if let Some(begun) = crate::timing::start() {
                    let _ = persist_begun.set(begun);
                }
                if let Some(trace) = &operation_trace {
                    trace.emit("persistence_owner_started");
                }
                let (recipients, sessions, session_lives, replay_targets, logical_offline_bytes) =
                    prepared.take().invariant("ordered recipients")?;
                let envelope = envelope.take().invariant("admitted envelope")?;
                let Envelope::Delta { delta, .. } = &envelope else {
                    return Err(crate::persistence::error("expected admitted delta"));
                };
                let publish = !sync || state.would_materialize(delta);
                if !publish {
                    return Ok(Step::Done(StateCommit {
                        continuity: None,
                        envelope: Arc::new(envelope),
                        pending: HashMap::new(),
                        persisted_bytes: 0,
                        sessions,
                        session_lives,
                        logical_offline_bytes,
                        publish,
                        selected: None,
                        recovery_scope: false,
        activation_error: None,
                        replay_targets,
                        accepted_deltas: 1,
                    }));
                }
                let seq = state.next_sequence()?;
                sequence = Some(seq);
                durable_sequence.store(seq, std::sync::atomic::Ordering::SeqCst);
                let continuity = continuity_evidence.clone().map(|evidence| evidence.accept(seq));
                let raw = crate::transport::encode(&envelope)
                    .map_err(|e| crate::persistence::error(e.to_string()))?;
                let retention =
                    crate::coedit::retention_metadata(delta).map(|(channel, kind)| match kind {
                        crate::coedit::RetentionKind::Checkpoint => (channel, None),
                        crate::coedit::RetentionKind::Delta => (channel, Some(seq)),
                    });
                if let Some((channel, seq)) = retention {
                    #[cfg(test)]
                    crate::store::test_barrier::pause(state.store.root(), "state-persist");
                    let build = retain_or_commit_stage(state, &channel, seq, raw, fresh.take())?.with_continuity(continuity.clone());
                    state.retention_scratch = Some(build.scratch().to_owned());
                    retained_build = Some(build);
                } else {
                    pending_work = Some((raw, recipients.into_iter()));
                }
                commit = Some(StateCommit {
                    continuity,
                    envelope: Arc::new(envelope),
                    pending: HashMap::new(),
                    persisted_bytes: 0,
                    sessions,
                    session_lives,
                    logical_offline_bytes,
                    publish,
                    selected: None,
                    recovery_scope: false,
        activation_error: None,
                    replay_targets,
                    accepted_deltas: 1,
                });
                return Ok(Step::More);
            }
            let current = commit.as_mut().invariant("commit missing")?;
            if let Some(build) = retained_build.as_mut() {
                let result = match build.step(WorkLimit::default()) {
                    Ok(result) => result,
                    Err(error) => {
                        let Some(activation) = build.publication_on_error() else { return Err(error); };
                        // Keep the captured sessions and uncertain/committed cut
                        // across the worker error so completion fences them.
                        let Some(selected) = SelectedPublication::from_publication(activation) else {
                            return Err(error);
                        };
                        current.selected = Some(selected);
                        current.activation_error = Some(error);
                        return Ok(Step::Done(commit.take().invariant("commit missing")?));
                    }
                };
                match result {
                    Step::More => return Ok(Step::More),
                    Step::Done(outcome) => {
                        let written = outcome.written;
                        let publication = retention_publication_kind(&outcome.publication);
                        match outcome.publication {
                            crate::store::shell_selection::Publication::Suppress => {
                                current.publish = false;
                                current.replay_targets.clear();
                            }
                            publication @ (crate::store::shell_selection::Publication::Activate(_)
                                | crate::store::shell_selection::Publication::ReplayRequired(_)) => {
                                let Some(selected) = SelectedPublication::from_publication(publication) else {
                                    return Err(crate::persistence::error("selected publication changed kind"));
                                };
                                current.selected = Some(selected.clone());
                                if !current.replay_targets.is_empty() {
                                    match ReplayPendingWork::selected(
                                        &selected,
                                        current.replay_targets.clone(),
                                    ) {
                                        Ok(work) => replay_pending_work = Some(work),
                                        Err(error) => current.activation_error = Some(error),
                                    }
                                }
                            }
                            crate::store::shell_selection::Publication::Original => {
                                if !current.replay_targets.is_empty() {
                                    replay_pending_work = Some(ReplayPendingWork::original(
                                        current.envelope.clone(),
                                        current.replay_targets.clone(),
                                        current.continuity.clone(),
                                    ));
                                }
                            }
                        }
                        if crate::persistence::diagnostics() {
                            let selection = build.selection_stats();
                            tracing::info!(
                                phase = "retention_complete",
                                verified_publication = publication,
                                selected_obsolete = build.selected_obsolete(),
                                retention_preparation_reads = build
                                    .retention_stats()
                                    .preparation_reads,
                                retention_peak_metadata_entries = build
                                    .retention_stats()
                                    .peak_metadata_entries,
                                selection_preparation_reads = selection
                                    .map_or(0, |stats| stats.preparation_reads),
                                selection_peak_metadata_entries = selection
                                    .map_or(0, |stats| stats.peak_metadata_entries),
                            );
                        }
                        if !build.holds_scratch() {
                            // A plain delta made no scratch, so the owner has
                            // nothing to dispose of after this job.
                            state.retention_scratch = None;
                        }
                        persist_metrics.add_bytes_queued(written);
                        current.persisted_bytes = written;
                        retained_build = None;
                    }
                }
            }
            if let Some(work) = replay_pending_work.as_mut() {
                match work.step(state, crate::persistence::output_budget()) {
                    Ok(Step::More) => return Ok(Step::More),
                    Ok(Step::Done(written)) => {
                        persist_metrics.add_bytes_queued(written);
                        current.persisted_bytes = current.persisted_bytes.saturating_add(written);
                        replay_pending_work = None;
                    }
                    Err(error) => {
                        current.activation_error = Some(error);
                        replay_pending_work = None;
                    }
                }
            }
            if let Some((raw, recipients)) = &mut pending_work {
                for _ in 0..crate::store::bounded::RUN_ENTRIES {
                    let Some(recipient) = recipients.next() else {
                        pending_work = None;
                        break;
                    };
                    let path = state
                        .store
                        .enqueue_pending(&recipient, sequence.invariant("sequence missing")?, raw)?;
                    current.pending.insert(recipient, path);
                    persist_metrics.add_bytes_queued(raw.len() as u64);
                    current.persisted_bytes =
                        current.persisted_bytes.saturating_add(raw.len() as u64);
                }
                if pending_work.is_some() {
                    return Ok(Step::More);
                }
            }
            sequence.invariant("sequence missing")?;
            let Envelope::Delta { delta, .. } = &*current.envelope else {
                return Err(crate::persistence::error("state commit envelope is not a delta"));
            };
            state.materialize(delta);
            Ok(Step::Done(commit.take().invariant("commit missing")?))
        },
        move |result| async move {
            crate::timing::event(persist_step, persist_ended.get().copied(), timed_bytes);
            let publishing = crate::timing::start();
            let result = async {
                let mut commit = result?;
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
                    metrics.inc_deltas_received();
                }
                if crate::persistence::correlated_diagnostics() && let Envelope::Delta { workspace_id, authored_at_ns, .. } = &*commit.envelope { tracing::info!(phase="server_delta_committed", workspace_id, authored_at_ns, sessions=commit.sessions.len()); }
                if crate::persistence::diagnostics() {
                    tracing::info!(phase="state_commit", persisted_envelope_bytes=commit.persisted_bytes, logical_offline_bytes=commit.logical_offline_bytes);
                }
                let activating = commit.selected.is_some() || commit.recovery_scope;
                let recovering = commit.recovery_scope || matches!(commit.selected, Some(SelectedPublication::Recovery(_)));
                let replay_targets = commit.replay_targets.clone();
                let mut delivered = 0u64;
                let publication_result: Result<(), crate::store::StoreError> = async {
                if let Some(error) = commit.activation_error.take() { return Err(error); }
                let mut cursor = if let Some(path) = commit.selected.take() {
                    Some(crate::persistence::blocking(workers.clone(), move || path.open()).await??)
                } else { None };
                let mut original = cursor.is_none();
                loop {
                    let (envelope, budget) = if let Some(mut selected) = cursor.take() {
                        let (next, result) = crate::persistence::blocking(workers.clone(), move || {
                            let result = selected.step(crate::persistence::output_budget());
                            (selected, result)
                        }).await?;
                        cursor = Some(next);
                        match result? {
                            Step::More => continue,
                            Step::Done(None) => break,
                            Step::Done(Some(batch)) => (LiveEnvelope::Selected(Arc::new(batch)), Ok(None)),
                        }
                    } else if original {
                        original = false;
                        let bytes = match &*commit.envelope { Envelope::Delta {delta, ..} => delta.len().saturating_add(256), _ => 1 };
                        let budget = crate::persistence::delivery_budget(bytes).map(Some);
                        #[cfg(test)]
                        let budget = {
                            let mut budget = budget;
                            let root = workspace.lock().await.store.root().to_path_buf();
                            if crate::store::test_barrier::check(&root, "live-delivery-budget").is_err() {
                                budget = Err(crate::persistence::error("injected live delivery budget failure"));
                            }
                            budget
                        };
                        (LiveEnvelope::Original(commit.envelope.clone()), budget)
                    } else { break };
                    // Every hydration/worker wait precedes a fresh current-source,
                    // recipient membership and exact subscriber-channel fence.
                    let mut runtime = crate::workspace_io::fresh_membership(&workspace).await?;
                    let fanned = FanoutStamp::of(&runtime);
                    if source.as_ref().is_some_and(|source| !source.authorized(&runtime)) {
                        if activating { return Err(crate::persistence::error("activation source authorization changed")); }
                        break;
                    }
                    let payloads = &**runtime.store.payloads();
                    let checkpoint_channel =
                        gated_checkpoint_channel(payloads, &envelope, source.as_ref());
                    let audience = live_audience(payloads, &envelope);
                    // Presence the author sent after this delta was held for
                    // it and goes out with it; from here on nothing waits.
                    if let Envelope::Delta { author_device_public_key, .. } = &*envelope {
                        runtime.delta_published(author_device_public_key);
                    }
                    for (recipient, original) in &commit.sessions {
                        if recovering && !captured_live_current(&commit.session_lives, &runtime, recipient, original) { continue; }
                        let Some(sub) = runtime.subscribers.get(recipient) else { continue };
                        if !sub.tx.same_channel(original) { continue; }
                        let Some(target) = runtime.state_sessions.get(recipient) else { continue };
                        // A host-authored frame has no source trust to ask, and
                        // checks the recipient's room listing only.
                        let admitted = match source.as_ref() {
                            Some(source) => source.admits(&runtime, &target.peer),
                            None => room_access::listed(room_access::Room::current(&runtime), &target.peer, RoomOperation::State),
                        };
                        if !admitted { continue; }
                        if audience.as_ref().is_some_and(|audience| !audience.contains(&target.peer.device_public_key)) { continue; }
                        let replay_phase = target.replay.as_ref().map(|replay| {
                            replay.phase.load(std::sync::atomic::Ordering::SeqCst)
                        });
                        if replay_phase == Some(ReplayCatchup::ACTIVE) {
                            // This commit is already durable. The replay owner
                            // will take one final coherent cut before opening
                            // this exact session to post-Ready live delivery.
                            target.replay.as_ref().invariant("replay missing")?
                                .dirty
                                .store(true, std::sync::atomic::Ordering::SeqCst);
                            continue;
                        }
                        if target.peer.role == crate::auth::PeerRole::Device
                            && checkpoint_channel.is_some_and(|channel| !runtime.needs_checkpoint(recipient, &channel)) { continue; }
                        // Durable coverage was committed before publication. If
                        // the shared live-byte budget is full, leave that coverage
                        // for replay/anti-entropy without fencing a healthy wire.
                        let Ok(budget) = &budget else {
                            if activating {
                                return Err(crate::persistence::error(
                                    "activation delivery byte backpressure",
                                ));
                            }
                            if replay_phase == Some(ReplayCatchup::BOUNDARY) {
                                target.live.store(false, std::sync::atomic::Ordering::SeqCst);
                                let _ = sub.disconnect.try_send(());
                            } else if let Some(lag) = &target.lag {
                                lag.mark(runtime.store.workspace_id());
                            }
                            continue
                        };
                        if crate::persistence::correlated_diagnostics() && let Envelope::Delta { workspace_id, authored_at_ns, .. } = &*envelope {
                            tracing::info!(phase="server_delta_publication", workspace_id, authored_at_ns, recipient=%hex::encode(&recipient[..4]));
                        }
                        let delivery = StateDelivery {
                            envelope: envelope.clone(), cleanup: commit.pending.remove(recipient), source: source.clone(),
                            _budget: budget.clone(), live: target.live.clone(), fanned: fanned.clone(),
                        };
                        match target.tx.try_send(delivery) {
                            Ok(()) => {
                                delivered = delivered.saturating_add(1);
                                if target.peer.role == crate::auth::PeerRole::Device && let Some(channel) = checkpoint_channel {
                                    runtime.mark_checkpoint_served(recipient, channel);
                                }
                            }
                            // Ordinary live traffic retains replay coverage under
                            // backpressure. A selected activation is a multi-frame
                            // basis whose exact retry is suppressed after CURRENT;
                            // a partial recipient must reconnect to recover it.
                            Err(mpsc::error::TrySendError::Full(delivery)) => {
                                if activating
                                    || matches!(&delivery.envelope, LiveEnvelope::Selected(_))
                                {
                                    let _ = sub.disconnect.try_send(());
                                    runtime.remove_subscriber_session(recipient, original);
                                    continue;
                                }
                                if replay_phase == Some(ReplayCatchup::BOUNDARY) {
                                    target
                                        .live
                                        .store(false, std::sync::atomic::Ordering::SeqCst);
                                    let _ = sub.disconnect.try_send(());
                                } else if let Some(lag) = &target.lag {
                                    lag.mark(runtime.store.workspace_id());
                                }
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                let _ = sub.disconnect.try_send(());
                                runtime.remove_subscriber_session(recipient, original);
                            }
                        }
                    }
                    #[cfg(test)]
                    crate::store::test_barrier::signal(runtime.store.root(), "live-publication-complete");
                }
                Ok(())
                }.await;
                metrics.add_deltas_fanned_out(delivered);
                if let Err(error) = publication_result {
                    if activating || !replay_targets.is_empty() {
                        // A partial activation cannot leave a ready subscriber
                        // waiting forever for its missing exact half. Fence only
                        // the original session; reconnect uses durable full basis.
                        let mut runtime = workspace.lock().await;
                        if activating {
                            for (recipient, original) in &commit.sessions {
                                if recovering && !captured_live_current(&commit.session_lives, &runtime, recipient, original) { continue; }
                                if let Some(sub) = runtime.subscribers.get(recipient)
                                    && sub.tx.same_channel(original) {
                                    let _ = sub.disconnect.try_send(());
                                    runtime.remove_subscriber_session(recipient, original);
                                }
                            }
                        } else {
                            for target in &replay_targets {
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
                    return Err(error);
                }
                Ok(commit)
            }
            .await;
            if let Some(trace) = &completion_trace {
                trace.emit(if result.is_ok() {
                    "persistence_completed"
                } else {
                    "persistence_failed"
                });
            }
            if crate::persistence::correlated_diagnostics() && let Err(error) = &result {
                tracing::info!(phase = "state_commit_error", error = %error);
            }
            if result.is_err()
                && let Some(source) = failure
            {
                let _ = source.disconnect.try_send(());
            }
            crate::timing::event(publish_step, publishing, timed_bytes);
            result
        },
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
