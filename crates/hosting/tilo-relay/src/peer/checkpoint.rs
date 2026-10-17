//! Checkpoint prefixes: which buffered checkpoints may be retained
//! together, the checkpoint batch and its mode, the pending-replay work
//! a batch leaves for catching-up sessions, and [`enqueue_checkpoint_prefix`].

use super::{
    Arc, AuthenticatedPeer, CheckpointCommitFence, Envelope, EnvelopeV2, HashMap, Invariant,
    PeerError, PreparedRecipients, ProjectedCheckpointCommit, ProjectedCheckpointStep,
    ReplayCatchup, SelectedPublication, SerialCheckpointCommit, SharedWorkspace, StateCommit,
    StateSource, Step, WorkLimit, checkpoint_prefix_post_first_promotion_fault, inbound_bytes,
    mpsc, prepare_recipients, promote_checkpoint_input, publish_checkpoint_commit,
    publish_checkpoint_commit_fenced, trace_native_publication,
};

pub(super) const RETENTION_PREFIX_FRAME_CAP: usize = 32;
pub(super) const RETENTION_PREFIX_BYTE_CAP: usize = 64 * 1024 * 1024;

pub(super) fn eligible_checkpoint_prefix<'a>(
    current: &Envelope,
    backlog: impl Iterator<Item = &'a EnvelopeV2>,
) -> usize {
    let Envelope::Delta {
        workspace_id,
        author_device_public_key,
        delta,
        ..
    } = current
    else {
        return 1;
    };
    let Some((current_channel, crate::coedit::RetentionKind::Checkpoint)) =
        crate::coedit::retention_metadata(delta)
    else {
        return 1;
    };
    let current_bytes = delta.len().saturating_add(256);
    if current_bytes > RETENTION_PREFIX_BYTE_CAP {
        return 1;
    }
    let mut channels = std::collections::HashSet::new();
    channels.insert(current_channel);
    let mut total_bytes = current_bytes;
    let mut total = 1;
    for frame in backlog {
        if total == RETENTION_PREFIX_FRAME_CAP {
            break;
        }
        let EnvelopeV2::State(Envelope::Delta {
            workspace_id: frame_workspace,
            author_device_public_key: frame_author,
            delta: frame_delta,
            ..
        }) = frame
        else {
            break;
        };
        if frame_workspace != workspace_id || frame_author != author_device_public_key {
            break;
        }
        let Some((channel, crate::coedit::RetentionKind::Checkpoint)) =
            crate::coedit::retention_metadata(frame_delta)
        else {
            break;
        };
        if !channels.insert(channel) {
            break;
        }
        let frame_bytes = frame_delta.len().saturating_add(256);
        if total_bytes.saturating_add(frame_bytes) > RETENTION_PREFIX_BYTE_CAP {
            break;
        }
        total_bytes = total_bytes.saturating_add(frame_bytes);
        total = total.saturating_add(1);
    }
    total
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct BufferedRetentionPrefix {
    pub(super) frames: usize,
    pub(super) input_bytes: usize,
    pub(super) checkpoints: usize,
    pub(super) deltas: usize,
    pub(super) distinct_channels: usize,
    pub(super) frame_cap_reached: bool,
    pub(super) byte_cap_reached: bool,
    pub(super) stop_reason: &'static str,
}

pub(super) fn buffered_retention_prefix<'a>(
    frames: impl Iterator<Item = &'a EnvelopeV2>,
) -> Option<BufferedRetentionPrefix> {
    buffered_retention_prefix_with_limits(
        frames,
        RETENTION_PREFIX_FRAME_CAP,
        RETENTION_PREFIX_BYTE_CAP,
    )
}

pub(super) fn buffered_retention_prefix_with_limits<'a>(
    frames: impl Iterator<Item = &'a EnvelopeV2>,
    frame_cap: usize,
    byte_cap: usize,
) -> Option<BufferedRetentionPrefix> {
    let mut group: Option<(u64, [u8; 32])> = None;
    let mut channels = std::collections::HashSet::new();
    let mut inspected = false;
    let mut shape = BufferedRetentionPrefix {
        frames: 0,
        input_bytes: 0,
        checkpoints: 0,
        deltas: 0,
        distinct_channels: 0,
        frame_cap_reached: false,
        byte_cap_reached: false,
        stop_reason: "end",
    };
    for frame in frames {
        inspected = true;
        if shape.frames == frame_cap {
            shape.frame_cap_reached = true;
            shape.stop_reason = "frame_cap";
            break;
        }
        let EnvelopeV2::State(Envelope::Delta {
            workspace_id,
            author_device_public_key,
            delta,
            ..
        }) = frame
        else {
            shape.stop_reason = "non_delta";
            break;
        };
        let Some((channel, kind)) = crate::coedit::retention_metadata(delta) else {
            shape.stop_reason = "non_retained";
            break;
        };
        let current = (*workspace_id, *author_device_public_key);
        match group {
            Some(expected) if expected != current => {
                shape.stop_reason = if expected.0 == current.0 {
                    "author_change"
                } else {
                    "workspace_change"
                };
                break;
            }
            None => group = Some(current),
            _ => {}
        }
        let bytes = inbound_bytes(frame);
        if shape.input_bytes.saturating_add(bytes) > byte_cap {
            shape.byte_cap_reached = true;
            shape.stop_reason = "byte_cap";
            break;
        }
        shape.frames = shape.frames.saturating_add(1);
        shape.input_bytes = shape.input_bytes.saturating_add(bytes);
        channels.insert(channel);
        match kind {
            crate::coedit::RetentionKind::Checkpoint => {
                shape.checkpoints = shape.checkpoints.saturating_add(1);
            }
            crate::coedit::RetentionKind::Delta => shape.deltas = shape.deltas.saturating_add(1),
        }
    }
    if !inspected {
        return None;
    }
    if shape.frames == frame_cap && shape.stop_reason == "end" {
        shape.frame_cap_reached = true;
        shape.stop_reason = "frame_cap";
    }
    shape.distinct_channels = channels.len();
    Some(shape)
}

pub(super) fn retention_publication_kind(
    publication: &crate::store::shell_selection::Publication,
) -> &'static str {
    match publication {
        crate::store::shell_selection::Publication::Suppress => "suppress",
        crate::store::shell_selection::Publication::Activate(_) => "activate",
        crate::store::shell_selection::Publication::ReplayRequired(_) => "recovery",
        crate::store::shell_selection::Publication::Original => "original",
    }
}

#[derive(Clone)]
pub(super) struct ReplayPendingTarget {
    pub(super) device: [u8; 32],
    pub(super) live: Arc<std::sync::atomic::AtomicBool>,
    pub(super) catchup: Arc<ReplayCatchup>,
}
impl ReplayPendingTarget {
    fn active(&self) -> bool {
        self.live.load(std::sync::atomic::Ordering::SeqCst)
            && self.catchup.phase.load(std::sync::atomic::Ordering::SeqCst) == ReplayCatchup::ACTIVE
    }
    pub(super) fn current(&self, runtime: &crate::workspace::WorkspaceRuntime) -> bool {
        runtime
            .state_sessions
            .get(&self.device)
            .is_some_and(|session| {
                Arc::ptr_eq(&session.live, &self.live)
                    && session
                        .replay
                        .as_ref()
                        .is_some_and(|catchup| Arc::ptr_eq(catchup, &self.catchup))
            })
    }
}

enum ReplayPendingPublication {
    Original(Option<Arc<Envelope>>),
    Selected(crate::store::replay::PublicationCursor),
}
struct ReplayPendingFrame {
    bytes: Vec<u8>,
    _selected: Option<crate::store::replay::ReplayBatch>,
    sequence: Option<u64>,
    next_target: usize,
}
pub(super) struct ReplayPendingWork {
    continuity: Option<crate::store::continuity::Receipt>,
    publication: ReplayPendingPublication,
    targets: Vec<ReplayPendingTarget>,
    frame: Option<ReplayPendingFrame>,
    written: u64,
}
/// Advance a commit's pending replay by one output budget; once it finishes,
/// the bytes it wrote count toward the commit.
pub(super) fn step_commit_replay(
    replay: &mut Option<ReplayPendingWork>,
    commit: &mut Option<StateCommit>,
    state: &mut crate::persistence::State,
) -> Result<Step<()>, crate::store::StoreError> {
    if let Some(work) = replay {
        let Step::Done(written) = work.step(state, crate::persistence::output_budget())? else {
            return Ok(Step::More);
        };
        let commit = commit.as_mut().invariant("commit missing")?;
        commit.persisted_bytes = commit.persisted_bytes.saturating_add(written);
        *replay = None;
    }
    Ok(Step::Done(()))
}

impl ReplayPendingWork {
    pub(super) fn original(
        envelope: Arc<Envelope>,
        targets: Vec<ReplayPendingTarget>,
        continuity: Option<crate::store::continuity::Receipt>,
    ) -> Self {
        Self {
            continuity,
            publication: ReplayPendingPublication::Original(Some(envelope)),
            targets,
            frame: None,
            written: 0,
        }
    }
    pub(super) fn selected(
        publication: &SelectedPublication,
        targets: Vec<ReplayPendingTarget>,
    ) -> Result<Self, crate::store::StoreError> {
        Ok(Self {
            continuity: None,
            publication: ReplayPendingPublication::Selected(publication.open()?),
            targets,
            frame: None,
            written: 0,
        })
    }
    pub(super) fn step(
        &mut self,
        state: &mut crate::persistence::State,
        budget: &crate::store::replay::ReplayBudget,
    ) -> Result<Step<u64>, crate::store::StoreError> {
        for _ in 0..crate::store::bounded::RUN_ENTRIES {
            if self.frame.is_none() {
                let (bytes, selected) = match &mut self.publication {
                    ReplayPendingPublication::Original(envelope) => envelope
                        .take()
                        .map(|envelope| crate::transport::encode(&envelope))
                        .transpose()
                        .map(|bytes| (bytes, None))
                        .map_err(|error| crate::persistence::error(error.to_string()))?,
                    ReplayPendingPublication::Selected(cursor) => match cursor.step(budget)? {
                        Step::More => continue,
                        Step::Done(Some(batch)) => {
                            let bytes = crate::transport::encode(
                                batch.front().invariant("one activation frame")?,
                            )
                            .map_err(|error| crate::persistence::error(error.to_string()))?;
                            (Some(bytes), Some(batch))
                        }
                        Step::Done(None) => (None, None),
                    },
                };
                let Some(bytes) = bytes else {
                    return Ok(Step::Done(self.written));
                };
                self.frame = Some(ReplayPendingFrame {
                    bytes,
                    _selected: selected,
                    sequence: None,
                    next_target: 0,
                });
            }
            let frame = self.frame.as_mut().invariant("frame missing")?;
            let Some(target) = self.targets.get(frame.next_target) else {
                self.frame = None;
                continue;
            };
            frame.next_target = frame.next_target.saturating_add(1);
            if !target.active() {
                continue;
            }
            let sequence = if let Some(sequence) = frame.sequence {
                sequence
            } else {
                let sequence = match &self.continuity {
                    Some(receipt) => receipt.sequence,
                    None => state.next_sequence()?,
                };
                frame.sequence = Some(sequence);
                sequence
            };
            if let Some(receipt) = &self.continuity {
                state
                    .store
                    .enqueue_continuity_pending(&target.device, &frame.bytes, receipt)?;
            } else {
                // Not the device's durable queue: one fsynced file per
                // catching-up session for each commit made a crowd joining
                // at once write the room's every change once per newcomer.
                let holding = crate::timing::start();
                target.catchup.hold(sequence, &frame.bytes)?;
                crate::timing::window("state.held_write", holding);
            }
            self.written = self.written.saturating_add(frame.bytes.len() as u64);
        }
        #[cfg(test)]
        if self.frame.is_some() {
            crate::store::test_barrier::pause(state.store.root(), "replay-pending-frame-held");
        }
        Ok(Step::More)
    }
}

pub(super) struct CheckpointBatchInput {
    pub(super) continuity: Option<crate::store::continuity::Receipt>,
    pub(super) envelope: Arc<Envelope>,
    pub(super) raw: Vec<u8>,
    pub(super) channel: [u8; 32],
    pub(super) incoming: [u8; 32],
    pub(super) stage: Option<crate::store::retained::CheckpointStage>,
    pub(super) prepared: Option<PreparedRecipients>,
    sequence: Option<u64>,
    cleanup_registered: bool,
}

pub(super) struct CheckpointBatch {
    pub(super) root: std::path::PathBuf,
    pub(super) inputs: Vec<CheckpointBatchInput>,
    pub(super) selector: Option<Box<dyn crate::store::shell_selection::ShellSelection>>,
    pub(super) projected: Option<crate::store::shell_selection::BatchSelectionOutcome>,
    stage_index: usize,
    all_projectable: bool,
}

pub(super) fn new_checkpoint_batch(
    state: &mut crate::persistence::State,
    envelopes: Vec<Envelope>,
    peer: Option<&AuthenticatedPeer>,
) -> Result<CheckpointBatch, crate::store::StoreError> {
    let root = state.store.root().join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    std::fs::create_dir(&root)?;
    state.retention_scratch = Some(root.clone());
    let mut inputs = Vec::with_capacity(envelopes.len());
    for (index, envelope) in envelopes.into_iter().enumerate() {
        let Envelope::Delta { delta, .. } = &envelope else {
            return Err(crate::persistence::error(
                "checkpoint batch contained non-delta",
            ));
        };
        let Some((channel, crate::coedit::RetentionKind::Checkpoint)) =
            crate::coedit::retention_metadata(delta)
        else {
            return Err(crate::persistence::error(
                "checkpoint batch contained non-checkpoint",
            ));
        };
        let raw = crate::transport::encode(&envelope)
            .map_err(|error| crate::persistence::error(error.to_string()))?;
        let incoming =
            tilo_relay_store::frames::incoming_identity(&raw, state.store.workspace_id())
                .ok_or_else(|| crate::persistence::error("invalid checkpoint identity"))?;
        // Reservation is speculative until fresh authorization permits promotion.
        // Keep the same ordered receipt through projection and serial fallback.
        let sequence = state.next_sequence()?;
        let continuity = peer
            .and_then(|peer| {
                crate::store::continuity::Evidence::authenticated(
                    &**state.store.payloads(),
                    &envelope,
                    peer,
                )
            })
            .map(|evidence| evidence.accept(sequence));
        let checkpoint_stage = if state.store.checkpoint_lineage_exists(&channel, &raw)? {
            Some(
                state
                    .store
                    .start_staged_checkpoint(
                        &channel,
                        raw.clone(),
                        root.join(format!("input-{index:02}")),
                    )?
                    .with_continuity(continuity.clone()),
            )
        } else {
            None
        };
        inputs.push(CheckpointBatchInput {
            continuity,
            envelope: Arc::new(envelope),
            raw,
            channel,
            incoming,
            stage: checkpoint_stage,
            prepared: None,
            sequence: Some(sequence),
            cleanup_registered: false,
        });
    }
    Ok(CheckpointBatch {
        root,
        inputs,
        selector: None,
        projected: None,
        stage_index: 0,
        all_projectable: true,
    })
}

pub(super) fn step_checkpoint_batch(
    state: &mut crate::persistence::State,
    batch: &mut CheckpointBatch,
) -> Result<Step<()>, crate::store::StoreError> {
    #[cfg(test)]
    let _progress = checkpoint_progress_step(batch);
    if let Some(input) = batch.inputs.get_mut(batch.stage_index) {
        let Some(checkpoint_stage) = input.stage.as_mut() else {
            batch.all_projectable = false;
            batch.stage_index = batch.stage_index.saturating_add(1);
            return Ok(Step::More);
        };
        let result = checkpoint_stage.step(WorkLimit::default());
        if !input.cleanup_registered
            && let Some(paths) = checkpoint_stage.cleanup_paths()
        {
            state.retention_candidates.push(paths);
            input.cleanup_registered = true;
        }
        match result? {
            Step::More => return Ok(Step::More),
            Step::Done(eligible) => {
                batch.all_projectable &= eligible;
                batch.stage_index = batch.stage_index.saturating_add(1);
                return Ok(Step::More);
            }
        }
    }
    if batch.selector.is_none() && batch.all_projectable {
        let overrides = batch
            .inputs
            .iter()
            .map(|input| {
                let checkpoint_stage = input.stage.as_ref().invariant("stage missing")?;
                Ok((
                    checkpoint_stage.lineage_name()?.to_os_string(),
                    checkpoint_stage.projected_generation().to_path_buf(),
                ))
            })
            .collect::<Result<_, crate::store::StoreError>>()?;
        let incoming = batch.inputs.iter().map(|input| input.incoming).collect();
        let selector = state.store.bound_shell()?.projected_selection(
            &state.store,
            batch.root.join("selection"),
            incoming,
            overrides,
        )?;
        state.retention_candidates.push(selector.cleanup_paths());
        batch.selector = Some(selector);
    }
    let Some(selector) = batch.selector.as_mut() else {
        return Ok(Step::Done(()));
    };
    match selector.step_projection(WorkLimit::default())? {
        Step::More => Ok(Step::More),
        Step::Done(projected) => {
            batch.projected = Some(projected);
            Ok(Step::Done(()))
        }
    }
}

/// Live recipient sessions and replay targets gathered from prepared recipients.
type UnionSessions = (
    Vec<([u8; 32], mpsc::Sender<Envelope>)>,
    Vec<ReplayPendingTarget>,
);

pub(super) fn union_sessions<'a>(
    prepared: impl IntoIterator<Item = &'a PreparedRecipients>,
) -> UnionSessions {
    let mut sessions: Vec<([u8; 32], mpsc::Sender<Envelope>)> = Vec::new();
    let mut replay_targets: Vec<ReplayPendingTarget> = Vec::new();
    for item in prepared {
        for (device, tx) in &item.sessions {
            if !sessions
                .iter()
                .any(|(known, current)| known == device && current.same_channel(tx))
            {
                sessions.push((*device, tx.clone()));
            }
        }
        for target in &item.replay_targets {
            if !replay_targets.iter().any(|known| {
                known.device == target.device
                    && Arc::ptr_eq(&known.live, &target.live)
                    && Arc::ptr_eq(&known.catchup, &target.catchup)
            }) {
                replay_targets.push(target.clone());
            }
        }
    }
    (sessions, replay_targets)
}

pub(super) fn commit_and_materialize(
    state: &mut crate::persistence::State,
    input: &CheckpointBatchInput,
) -> Result<u64, crate::store::StoreError> {
    if input.sequence.is_none() {
        return Err(crate::persistence::error(
            "checkpoint sequence was not reserved",
        ));
    }
    let Envelope::Delta { delta, .. } = &*input.envelope else {
        return Err(crate::persistence::error(
            "checkpoint input was not a delta",
        ));
    };
    state.materialize(delta);
    Ok(input
        .stage
        .as_ref()
        .map_or(0, super::store::retained::CheckpointStage::written))
}

pub(super) fn state_commit_for(
    input: &mut CheckpointBatchInput,
    publication: crate::store::shell_selection::Publication,
) -> Result<StateCommit, crate::store::StoreError> {
    let prepared = input
        .prepared
        .take()
        .ok_or_else(|| crate::persistence::error("checkpoint recipients were not prepared"))?;
    let (publish, selected) = match publication {
        crate::store::shell_selection::Publication::Suppress => (false, None),
        crate::store::shell_selection::Publication::Activate(path) => {
            (true, Some(SelectedPublication::Activation(path)))
        }
        crate::store::shell_selection::Publication::ReplayRequired(path) => {
            (true, Some(SelectedPublication::Recovery(path)))
        }
        crate::store::shell_selection::Publication::Original => (true, None),
    };
    Ok(StateCommit {
        continuity: input.continuity.clone(),
        envelope: input.envelope.clone(),
        pending: HashMap::new(),
        persisted_bytes: input
            .stage
            .as_ref()
            .map_or(0, super::store::retained::CheckpointStage::written),
        session_lives: prepared.session_lives,
        sessions: prepared.sessions,
        logical_offline_bytes: prepared.logical_offline_bytes,
        publish,
        selected,
        recovery_scope: false,
        activation_error: None,
        replay_targets: prepared.replay_targets,
        accepted_deltas: 1,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CheckpointBatchMode {
    Represented,
    AllOriginal,
    Serial,
}

#[cfg(test)]
type CheckpointBatchModeObservers =
    std::sync::Mutex<HashMap<u64, Arc<std::sync::Mutex<Vec<CheckpointBatchMode>>>>>;
#[cfg(test)]
static CHECKPOINT_BATCH_MODE_OBSERVERS: std::sync::OnceLock<CheckpointBatchModeObservers> =
    std::sync::OnceLock::new();

#[cfg(test)]
#[derive(Debug, Default)]
pub(super) struct CheckpointProgress {
    active: Option<String>,
    completed: std::collections::BTreeMap<String, (usize, u128)>,
}

#[cfg(test)]
pub(super) static CHECKPOINT_PROGRESS: std::sync::OnceLock<
    std::sync::Mutex<HashMap<u64, Arc<std::sync::Mutex<CheckpointProgress>>>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
struct CheckpointProgressStep {
    observed: Arc<std::sync::Mutex<CheckpointProgress>>,
    phase: String,
    started: std::time::Instant,
}

#[cfg(test)]
impl Drop for CheckpointProgressStep {
    fn drop(&mut self) {
        let mut observed = self
            .observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = observed.completed.entry(self.phase.clone()).or_default();
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry.1.saturating_add(self.started.elapsed().as_micros());
        observed.active = None;
    }
}

#[cfg(test)]
fn checkpoint_progress_step(batch: &CheckpointBatch) -> Option<CheckpointProgressStep> {
    let phase = if batch.stage_index < batch.inputs.len() {
        "retention-stage".into()
    } else {
        batch.selector.as_ref().map_or_else(
            || "selection-create".into(),
            |selector| selector.diagnostic_phase(),
        )
    };
    checkpoint_progress_phase(batch, phase)
}

#[cfg(test)]
fn checkpoint_progress_phase(
    batch: &CheckpointBatch,
    phase: String,
) -> Option<CheckpointProgressStep> {
    let Envelope::Delta { workspace_id, .. } = &*batch.inputs.first()?.envelope else {
        return None;
    };
    let observed = CHECKPOINT_PROGRESS
        .get()?
        .lock()
        .ok()?
        .get(workspace_id)?
        .clone();
    observed.lock().ok()?.active = Some(phase.clone());
    Some(CheckpointProgressStep {
        observed,
        phase,
        started: std::time::Instant::now(),
    })
}

#[cfg(test)]
pub(super) fn track_checkpoint_batch_modes(
    workspace_id: u64,
) -> Arc<std::sync::Mutex<Vec<CheckpointBatchMode>>> {
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    CHECKPOINT_BATCH_MODE_OBSERVERS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(workspace_id, observed.clone());
    observed
}

#[cfg(test)]
fn observe_checkpoint_batch_mode(batch: &CheckpointBatch, mode: CheckpointBatchMode) {
    let Some(Envelope::Delta { workspace_id, .. }) =
        batch.inputs.first().map(|input| &*input.envelope)
    else {
        return;
    };
    if let Some(observed) = CHECKPOINT_BATCH_MODE_OBSERVERS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .get(workspace_id)
    {
        observed.lock().unwrap().push(mode);
    }
}

fn checkpoint_batch_mode(batch: &CheckpointBatch) -> CheckpointBatchMode {
    checkpoint_mode_for(batch.projected.as_ref(), batch.inputs.len())
}

pub(super) fn checkpoint_mode_for(
    outcome: Option<&crate::store::shell_selection::BatchSelectionOutcome>,
    input_count: usize,
) -> CheckpointBatchMode {
    use crate::store::shell_selection::{BatchProjectionReason, IncomingDisposition, Publication};
    let Some(outcome) = outcome else {
        return CheckpointBatchMode::Serial;
    };
    if outcome.dispositions.len() != input_count {
        return CheckpointBatchMode::Serial;
    }
    if outcome.reason == BatchProjectionReason::FinalBasisOmission
        && outcome.publication == Publication::Original
        && outcome
            .dispositions
            .iter()
            .all(|value| *value == IncomingDisposition::Original)
    {
        return CheckpointBatchMode::AllOriginal;
    }
    if outcome.reason == BatchProjectionReason::Represented
        && outcome.publication != Publication::Original
        && outcome
            .dispositions
            .iter()
            .all(|value| *value != IncomingDisposition::Original)
    {
        return CheckpointBatchMode::Represented;
    }
    CheckpointBatchMode::Serial
}

#[expect(
    clippy::too_many_lines,
    reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "each owner-state guard lives exactly for one blocking checkpoint quantum and ends with its closure"
)]
pub(super) async fn enqueue_checkpoint_prefix(
    workspace: &SharedWorkspace,
    envelopes: Vec<Envelope>,
    source: StateSource,
    role: crate::auth::PeerRole,
    metrics: Arc<crate::metrics::Metrics>,
    inbound_budget: Vec<Option<Arc<tokio::sync::OwnedSemaphorePermit>>>,
) -> Result<(), PeerError> {
    for envelope in &envelopes {
        trace_native_publication("prefix_received", envelope, None);
    }
    let diagnostics = crate::persistence::diagnostics();
    let admitted_at = diagnostics.then(std::time::Instant::now);
    let message_units = u32::try_from(envelopes.len())
        .map_err(|_| PeerError::Protocol("checkpoint prefix is too long".into()))?;
    let charged_size = envelopes.iter().try_fold(0usize, |total, envelope| {
        let Envelope::Delta { delta, .. } = envelope else {
            return Err(PeerError::Protocol(
                "checkpoint prefix contained non-delta".into(),
            ));
        };
        Ok(total.saturating_add(delta.len().saturating_add(256).saturating_mul(3)))
    })?;
    if diagnostics {
        tracing::info!(
            phase = "checkpoint_prefix_admitted",
            inputs = message_units,
            charged_bytes = charged_size,
        );
    }
    let owner = crate::persistence::handle(workspace).await;
    let work_workspace = workspace.clone();
    let work_source = source.clone();
    let work_metrics = metrics.clone();
    let rx = owner
        .submit_sequence_wait(
            message_units,
            charged_size,
            move |state, workers| async move {
                let queue_wait_ms = admitted_at.map_or(0, |started| started.elapsed().as_millis());
                let projection_started = diagnostics.then(std::time::Instant::now);
                let runtime = crate::workspace_io::fresh_membership(&work_workspace).await?;
                if !work_source.authorized(&runtime) {
                    return Err(crate::persistence::error(
                        "state source authorization changed",
                    ));
                }
                drop(runtime);
                let staging_peer = work_source.peer.clone();
                let staging_state = state.clone();
                let mut batch = crate::persistence::blocking(workers.clone(), move || {
                    let mut state = staging_state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    new_checkpoint_batch(&mut state, envelopes, Some(&staging_peer))
                })
                .await??;
                let started = diagnostics.then(std::time::Instant::now);
                loop {
                    let staging_state = state.clone();
                    let (next, result) =
                        Box::pin(crate::persistence::blocking(workers.clone(), move || {
                            let mut state = staging_state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let result = step_checkpoint_batch(&mut state, &mut batch);
                            (batch, result)
                        }))
                        .await?;
                    batch = next;
                    if matches!(result?, Step::Done(())) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                let projection_elapsed_ms =
                    projection_started.map_or(0, |started| started.elapsed().as_millis());
                if tracing::enabled!(target: "tilo_relay::native_publication", tracing::Level::DEBUG) {
                    for (index, input) in batch.inputs.iter().enumerate() {
                        use crate::store::shell_selection::IncomingDisposition;
                        let disposition = batch.projected.as_ref().and_then(|outcome| outcome.dispositions.get(index));
                        let phase = match disposition {
                            Some(IncomingDisposition::Covered) => "prefix_covered",
                            Some(IncomingDisposition::Staged) => "prefix_staged",
                            Some(IncomingDisposition::SelectedLive) => "prefix_selected_live",
                            Some(IncomingDisposition::Original) => "prefix_original",
                            None => "prefix_unclassified",
                        };
                        trace_native_publication(phase, &input.envelope, None);
                    }
                }
                if diagnostics {
                    use crate::store::shell_selection::{
                        BatchProjectionReason, IncomingDisposition, Publication,
                    };
                    let (publication, reason, dispositions) = batch.projected.as_ref().map_or(
                        ("none", "unprojectable_stage", None),
                        |outcome| {
                            let publication = match outcome.publication {
                                Publication::Original => "original",
                                Publication::Suppress => "suppress",
                                Publication::Activate(_) => "activate",
                                Publication::ReplayRequired(_) => "recovery",
                            };
                            let reason = match outcome.reason {
                                BatchProjectionReason::Represented => "represented",
                                BatchProjectionReason::FinalBasisOmission => "final_basis_omission",
                                BatchProjectionReason::UnresolvedTailDependencies => {
                                    "unresolved_tail_dependencies"
                                }
                            };
                            (publication, reason, Some(outcome.dispositions.as_slice()))
                        },
                    );
                    let dispositions = dispositions.unwrap_or_default();
                    let unclassified = batch.inputs.len().saturating_sub(dispositions.len());
                    let covered = dispositions
                        .iter()
                        .filter(|value| **value == IncomingDisposition::Covered)
                        .count();
                    let staged = dispositions
                        .iter()
                        .filter(|value| **value == IncomingDisposition::Staged)
                        .count();
                    let selected_live = dispositions
                        .iter()
                        .filter(|value| **value == IncomingDisposition::SelectedLive)
                        .count();
                    let originals = dispositions
                        .iter()
                        .filter(|value| **value == IncomingDisposition::Original)
                        .count();
                    let first_original = dispositions
                        .iter()
                        .position(|value| *value == IncomingDisposition::Original);
                    let last_original = dispositions
                        .iter()
                        .rposition(|value| *value == IncomingDisposition::Original);
                    tracing::info!(
                        phase = "checkpoint_prefix_projection",
                        inputs = batch.inputs.len(),
                        queue_wait_ms,
                        projection_elapsed_ms,
                        publication,
                        reason,
                        unclassified,
                        covered,
                        staged,
                        selected_live,
                        originals,
                        ?first_original,
                        ?last_original,
                    );
                }
                let mode = checkpoint_batch_mode(&batch);
                #[cfg(test)]
                observe_checkpoint_batch_mode(&batch, mode);
                for index in 0..batch.inputs.len() {
                    let input = batch
                        .inputs
                        .get(index)
                        .invariant("checkpoint input missing")?;
                    let Envelope::Delta {
                        author_device_public_key,
                        delta,
                        ..
                    } = &*input.envelope
                    else {
                        return Err(crate::persistence::error(
                            "checkpoint batch input is not a delta",
                        ));
                    };
                    let prepared = {
                        #[cfg(test)]
                        let _progress =
                            checkpoint_progress_phase(&batch, "fresh-recipients".into());
                        prepare_recipients(
                            &work_workspace,
                            &work_source,
                            *author_device_public_key,
                            role,
                            delta.len(),
                        )
                        .await?
                    };
                    batch
                        .inputs
                        .get_mut(index)
                        .invariant("checkpoint input missing")?
                        .prepared = Some(prepared);
                    if mode == CheckpointBatchMode::Serial {
                        let operation_state = state.clone();
                        let (next, result) =
                            Box::pin(crate::persistence::blocking(workers.clone(), move || {
                                let mut state = operation_state
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                let result =
                                    SerialCheckpointCommit::new(&mut state, &mut batch, index);
                                (batch, result)
                            }))
                            .await?;
                        batch = next;
                        let mut work = result?;
                        loop {
                            let operation_state = state.clone();
                            let (next_batch, next_work, result) = Box::pin(
                                crate::persistence::blocking(workers.clone(), move || {
                                    let mut state = operation_state
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                                    let result = work.step(&mut state, &mut batch);
                                    (batch, work, result)
                                }),
                            )
                            .await?;
                            batch = next_batch;
                            work = next_work;
                            let result = match result {
                                Ok(result) => result,
                                Err(error) => {
                                    if let Some(fence) = work.error_fence(&batch) {
                                        fence.apply(&work_workspace).await;
                                    }
                                    return Err(error);
                                }
                            };
                            match result {
                                Step::More => tokio::task::yield_now().await,
                                Step::Done(commit) => {
                                    publish_checkpoint_commit_fenced(
                                        &work_workspace,
                                        &work_source,
                                        workers.clone(),
                                        &work_metrics,
                                        commit,
                                    )
                                    .await?;
                                    break;
                                }
                            }
                        }
                    } else {
                        let operation_state = state.clone();
                        let (next, result) =
                            Box::pin(crate::persistence::blocking(workers.clone(), move || {
                                let mut state = operation_state
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                #[cfg(test)]
                                let _progress = checkpoint_progress_phase(&batch, "promote".into());
                                let result =
                                    promote_checkpoint_input(&mut state, &mut batch, index);
                                let result = if index == 0 {
                                    result.and_then(|()| {
                                        checkpoint_prefix_post_first_promotion_fault(
                                            state.store.root(),
                                        )
                                    })
                                } else {
                                    result
                                };
                                (batch, result)
                            }))
                            .await?;
                        batch = next;
                        result?;
                    }
                }
                if mode != CheckpointBatchMode::Serial {
                    let mut work = ProjectedCheckpointCommit::new(&batch, mode);
                    loop {
                        // Every AllOriginal worker quantum runs after all N
                        // promotions. A worker acquisition/join error cannot
                        // prove whether the selected CURRENT switched, so the
                        // immutable exact-session fence is conservative here.
                        let worker_fence = work
                            .recovery_fence
                            .clone()
                            .or_else(|| work.full_fence.clone());
                        let operation_state = state.clone();
                        let blocking =
                            Box::pin(crate::persistence::blocking(workers.clone(), move || {
                                let mut state = operation_state
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                #[cfg(test)]
                                let _progress = checkpoint_progress_phase(
                                    &batch,
                                    if !work.selected_committed {
                                        format!(
                                            "selected-{}",
                                            batch.selector.as_ref().unwrap().diagnostic_phase()
                                        )
                                    } else if work.gc_index < batch.inputs.len() {
                                        "retained-gc".into()
                                    } else {
                                        "delivery".into()
                                    },
                                );
                                let result = work.step(&mut state, &mut batch);
                                (batch, work, result)
                            }))
                            .await;
                        let (next_batch, next_work, result) = match blocking {
                            Ok(result) => result,
                            Err(error) => {
                                if let Some(fence) = worker_fence {
                                    fence.apply(&work_workspace).await;
                                }
                                return Err(error);
                            }
                        };
                        batch = next_batch;
                        work = next_work;
                        let result = match result {
                            Ok(result) => result,
                            Err(error) => {
                                if let Some(fence) = work.error_fence(&batch) {
                                    fence.apply(&work_workspace).await;
                                }
                                return Err(error);
                            }
                        };
                        match result {
                            ProjectedCheckpointStep::More => tokio::task::yield_now().await,
                            ProjectedCheckpointStep::Commit(commit) => {
                                let commit = *commit;
                                #[cfg(test)]
                                let _progress = checkpoint_progress_phase(&batch, "publish".into());
                                let commit_fence = CheckpointCommitFence::capture(&commit);
                                if let Err(error) = publish_checkpoint_commit(
                                    &work_workspace,
                                    &work_source,
                                    workers.clone(),
                                    &work_metrics,
                                    commit,
                                )
                                .await
                                {
                                    work.error_fence(&batch)
                                        .unwrap_or(commit_fence)
                                        .apply(&work_workspace)
                                        .await;
                                    return Err(error);
                                }
                            }
                            ProjectedCheckpointStep::Done => break,
                        }
                    }
                }
                if diagnostics {
                    tracing::info!(
                        phase = "checkpoint_prefix_complete",
                        inputs = batch.inputs.len(),
                        disposition = match mode {
                            CheckpointBatchMode::Represented => "represented",
                            CheckpointBatchMode::AllOriginal => "all_original",
                            CheckpointBatchMode::Serial => "serial",
                        },
                        success = true,
                        elapsed_ms = started.map_or(0, |started| started.elapsed().as_millis()),
                        selection_preparation_reads = batch
                            .selector
                            .as_ref()
                            .map_or(0, |selector| selector.stats().preparation_reads),
                    );
                }
                drop(inbound_budget);
                Ok(())
            },
        )
        .await?;
    let failure = source.clone();
    tokio::spawn(async move {
        if crate::persistence::receipt(rx).await.is_err() {
            failure.failed();
        }
    });
    Ok(())
}

pub(super) fn checkpoint_prefix_recovery_allowed(
    runtime: &crate::workspace::WorkspaceRuntime,
    peer: &AuthenticatedPeer,
    author_device_public_key: [u8; 32],
    source: &StateSource,
) -> bool {
    runtime.checkpoint_prefix_recovery
        == crate::config::CheckpointPrefixRecovery::NativeOwnerDurableBaseline
        && peer.role == crate::auth::PeerRole::Device
        && author_device_public_key == peer.device_public_key
        && runtime
            .store
            .shell_binding()
            .is_some_and(|binding| binding.governor() == &peer.cert)
        && source.authorized(runtime)
}
