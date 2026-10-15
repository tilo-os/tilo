//! Committing a checkpoint batch: the projected and serial commits and
//! their publication behind the commit fence.

use super::{
    Arc, CheckpointBatch, CheckpointBatchMode, Envelope, FanoutStamp, HashMap, Invariant,
    LiveEnvelope, ReplayCatchup, ReplayPendingTarget, ReplayPendingWork, SelectedPublication,
    SessionLives, SharedWorkspace, StateCommit, StateDelivery, StateSource, Step, WorkLimit,
    captured_live_current, commit_and_materialize, gated_checkpoint_channel, live_audience, mpsc,
    state_commit_for, step_commit_replay, trace_native_publication, union_session_lives,
    union_sessions,
};

pub(super) enum ProjectedCheckpointStep {
    More,
    Commit(Box<StateCommit>),
    Done,
}

pub(super) struct ProjectedCheckpointCommit {
    mode: CheckpointBatchMode,
    pub(super) selected_committed: bool,
    publication: Option<crate::store::shell_selection::Publication>,
    pub(super) gc_index: usize,
    commit: Option<StateCommit>,
    replay: Option<ReplayPendingWork>,
    publication_index: usize,
    emission_done: bool,
    pub(super) full_fence: Option<CheckpointCommitFence>,
    pub(super) recovery_fence: Option<CheckpointCommitFence>,
    supplemental_recovery: Option<std::path::PathBuf>,
}

impl ProjectedCheckpointCommit {
    pub(super) fn new(batch: &CheckpointBatch, mode: CheckpointBatchMode) -> Self {
        let full_fence = (mode == CheckpointBatchMode::AllOriginal).then(|| {
            let (sessions, replay_targets) = union_sessions(
                batch
                    .inputs
                    .iter()
                    .filter_map(|input| input.prepared.as_ref()),
            );
            CheckpointCommitFence {
                recovery_lives: None,
                activating: true,
                sessions,
                replay_targets,
            }
        });
        let supplemental_recovery = (mode == CheckpointBatchMode::AllOriginal)
            .then(|| {
                batch
                    .projected
                    .as_ref()
                    .and_then(|p| p.supplemental_recovery.clone())
            })
            .flatten();
        // Both exist only in AllOriginal mode, so the pairing never drops a fence.
        let recovery_fence = supplemental_recovery
            .as_ref()
            .and_then(|_| full_fence.clone())
            .map(|mut fence| {
                fence.recovery_lives = Some(union_session_lives(
                    batch
                        .inputs
                        .iter()
                        .filter_map(|input| input.prepared.as_ref()),
                ));
                fence
            });
        Self {
            recovery_fence,
            supplemental_recovery,
            mode,
            selected_committed: false,
            publication: None,
            gc_index: 0,
            commit: None,
            replay: None,
            publication_index: 0,
            emission_done: false,
            full_fence,
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
    )]
    pub(super) fn step(
        &mut self,
        state: &mut crate::persistence::State,
        batch: &mut CheckpointBatch,
    ) -> Result<ProjectedCheckpointStep, crate::store::StoreError> {
        if !self.selected_committed {
            match batch
                .selector
                .as_mut()
                .invariant("projected checkpoint selector")?
                .step(WorkLimit::default())?
            {
                Step::More => return Ok(ProjectedCheckpointStep::More),
                Step::Done(crate::store::shell_selection::SelectionOutcome::BatchPublished {
                    supplemental_recovery,
                    publication,
                    dispositions,
                }) => {
                    // CURRENT has already switched when Selection returns. Set
                    // this before validating the repeated outcome so every
                    // mismatch takes the immutable full-session fence.
                    self.selected_committed = true;
                    let expected = batch.projected.as_ref().ok_or_else(|| {
                        crate::persistence::error("projected checkpoint outcome disappeared")
                    })?;
                    if publication != expected.publication
                        || dispositions != expected.dispositions
                        || supplemental_recovery != expected.supplemental_recovery
                    {
                        return Err(crate::persistence::error(
                            "checkpoint projection changed while committing",
                        ));
                    }
                    self.publication = Some(publication);
                    return Ok(ProjectedCheckpointStep::More);
                }
                Step::Done(_) => {
                    return Err(crate::persistence::error(
                        "checkpoint batch returned an invalid selection outcome",
                    ));
                }
            }
        }
        if let Some(input) = batch.inputs.get_mut(self.gc_index) {
            match input
                .stage
                .as_mut()
                .invariant("stage missing")?
                .finish_gc(WorkLimit::default())?
            {
                Step::More => return Ok(ProjectedCheckpointStep::More),
                Step::Done(()) => {
                    commit_and_materialize(state, input)?;
                    self.gc_index = self.gc_index.saturating_add(1);
                    return Ok(ProjectedCheckpointStep::More);
                }
            }
        }
        // A previous serial prefix may still be held. Deliver its safe full
        // fallback before any current Original checkpoint can advance a floor.
        // This is publication only: every input keeps its original disposition.
        if let Some(path) = &self.supplemental_recovery {
            if self.commit.is_none() {
                let prepared = || {
                    batch
                        .inputs
                        .iter()
                        .filter_map(|input| input.prepared.as_ref())
                };
                let (sessions, replay_targets) = union_sessions(prepared());
                let selected = SelectedPublication::Recovery(path.clone());
                self.replay = (!replay_targets.is_empty())
                    .then(|| ReplayPendingWork::selected(&selected, replay_targets.clone()))
                    .transpose()?;
                self.commit = Some(StateCommit {
                    continuity: None,
                    envelope: batch
                        .inputs
                        .last()
                        .invariant("admitted prefix")?
                        .envelope
                        .clone(),
                    pending: HashMap::new(),
                    persisted_bytes: 0,
                    sessions,
                    session_lives: union_session_lives(prepared()),
                    logical_offline_bytes: 0,
                    publish: true,
                    selected: Some(selected),
                    recovery_scope: false,
                    activation_error: None,
                    replay_targets,
                    accepted_deltas: 0,
                });
                return Ok(ProjectedCheckpointStep::More);
            }
            if matches!(
                step_commit_replay(&mut self.replay, &mut self.commit, state)?,
                Step::More
            ) {
                return Ok(ProjectedCheckpointStep::More);
            }
            self.supplemental_recovery = None;
            return Ok(ProjectedCheckpointStep::Commit(Box::new(
                self.commit.take().invariant("commit missing")?,
            )));
        }
        if self.mode == CheckpointBatchMode::AllOriginal {
            #[cfg(test)]
            if self.publication_index == 1 {
                crate::store::test_barrier::check(
                    state.store.root(),
                    "checkpoint-prefix-after-first-original",
                )?;
            }
            if self.publication != Some(crate::store::shell_selection::Publication::Original) {
                return Err(crate::persistence::error(
                    "all-original checkpoint projection activated a selected basis",
                ));
            }
            if self.publication_index == batch.inputs.len() {
                return Ok(ProjectedCheckpointStep::Done);
            }
            if self.commit.is_none() {
                let mut commit = state_commit_for(
                    batch
                        .inputs
                        .get_mut(self.publication_index)
                        .invariant("checkpoint input missing")?,
                    crate::store::shell_selection::Publication::Original,
                )?;
                commit.recovery_scope = self.recovery_fence.is_some();
                self.replay = (!commit.replay_targets.is_empty()).then(|| {
                    ReplayPendingWork::original(
                        commit.envelope.clone(),
                        commit.replay_targets.clone(),
                        commit.continuity.clone(),
                    )
                });
                self.commit = Some(commit);
                return Ok(ProjectedCheckpointStep::More);
            }
            if matches!(
                step_commit_replay(&mut self.replay, &mut self.commit, state)?,
                Step::More
            ) {
                return Ok(ProjectedCheckpointStep::More);
            }
            self.publication_index = self.publication_index.saturating_add(1);
            return Ok(ProjectedCheckpointStep::Commit(Box::new(
                self.commit.take().invariant("commit missing")?,
            )));
        }
        if self.emission_done {
            return Ok(ProjectedCheckpointStep::Done);
        }
        if self.commit.is_none() {
            let prepared = batch
                .inputs
                .iter_mut()
                .map(|input| input.prepared.take().invariant("prepared checkpoint input"))
                .collect::<Result<Vec<_>, _>>()?;
            let (sessions, replay_targets) = union_sessions(&prepared);
            let (publish, selected) = match self
                .publication
                .take()
                .invariant("represented checkpoint publication")?
            {
                crate::store::shell_selection::Publication::Suppress => (false, None),
                crate::store::shell_selection::Publication::Activate(path) => {
                    (true, Some(SelectedPublication::Activation(path)))
                }
                crate::store::shell_selection::Publication::ReplayRequired(_)
                | crate::store::shell_selection::Publication::Original => {
                    return Err(crate::persistence::error(
                        "represented checkpoint batch returned original publication",
                    ));
                }
            };
            let commit = StateCommit {
                continuity: None,
                envelope: batch
                    .inputs
                    .last()
                    .invariant("inputs missing")?
                    .envelope
                    .clone(),
                pending: HashMap::new(),
                persisted_bytes: batch
                    .inputs
                    .iter()
                    .map(|input| {
                        input
                            .stage
                            .as_ref()
                            .map(crate::store::retained::CheckpointStage::written)
                            .invariant("checkpoint stage missing")
                    })
                    .sum::<Result<_, _>>()?,
                session_lives: Vec::new(),
                sessions,
                logical_offline_bytes: prepared.iter().map(|item| item.logical_offline_bytes).sum(),
                publish,
                selected,
                recovery_scope: false,
                activation_error: None,
                replay_targets,
                accepted_deltas: batch.inputs.len() as u64,
            };
            self.commit = Some(commit);
            let commit = self.commit.as_ref().invariant("commit missing")?;
            self.replay = if let Some(path) = &commit.selected {
                (!commit.replay_targets.is_empty())
                    .then(|| ReplayPendingWork::selected(path, commit.replay_targets.clone()))
                    .transpose()?
            } else {
                None
            };
            return Ok(ProjectedCheckpointStep::More);
        }
        if matches!(
            step_commit_replay(&mut self.replay, &mut self.commit, state)?,
            Step::More
        ) {
            return Ok(ProjectedCheckpointStep::More);
        }
        self.emission_done = true;
        Ok(ProjectedCheckpointStep::Commit(Box::new(
            self.commit.take().invariant("commit missing")?,
        )))
    }

    pub(super) fn error_fence(&self, batch: &CheckpointBatch) -> Option<CheckpointCommitFence> {
        if self.mode == CheckpointBatchMode::AllOriginal
            && (self.selected_committed
                || batch
                    .selector
                    .as_ref()
                    .is_some_and(|selector| selector.current_switch_attempted()))
        {
            return self
                .recovery_fence
                .clone()
                .or_else(|| self.full_fence.clone());
        }
        // The represented commit now owns these recipients. Reconstructing a
        // fence from consumed inputs would hide the publisher's captured fence.
        if self.emission_done {
            return None;
        }
        if let Some(commit) = &self.commit {
            return Some(CheckpointCommitFence::capture(commit));
        }
        let activating = batch
            .selector
            .as_ref()
            .is_some_and(|selector| selector.publication_on_error().is_some());
        if !activating {
            return None;
        }
        let mut sessions = Vec::new();
        let mut replay_targets = Vec::new();
        for prepared in batch
            .inputs
            .iter()
            .filter_map(|input| input.prepared.as_ref())
        {
            for (device, tx) in &prepared.sessions {
                if !sessions
                    .iter()
                    .any(|(known, current): &([u8; 32], mpsc::Sender<Envelope>)| {
                        known == device && current.same_channel(tx)
                    })
                {
                    sessions.push((*device, tx.clone()));
                }
            }
            for target in &prepared.replay_targets {
                if !replay_targets.iter().any(|known: &ReplayPendingTarget| {
                    known.device == target.device
                        && Arc::ptr_eq(&known.live, &target.live)
                        && Arc::ptr_eq(&known.catchup, &target.catchup)
                }) {
                    replay_targets.push(target.clone());
                }
            }
        }
        Some(CheckpointCommitFence {
            recovery_lives: None,
            activating: true,
            sessions,
            replay_targets,
        })
    }
}

pub(super) fn promote_checkpoint_input(
    _state: &mut crate::persistence::State,
    batch: &mut CheckpointBatch,
    index: usize,
) -> Result<(), crate::store::StoreError> {
    let input = batch
        .inputs
        .get_mut(index)
        .invariant("checkpoint input missing")?;
    input.stage.as_mut().invariant("stage missing")?.promote()
}

pub(super) struct SerialCheckpointCommit {
    index: usize,
    retain: Option<crate::store::retained::RetainBuild>,
    selection: Option<Box<dyn crate::store::shell_selection::ShellSelection>>,
    publication: Option<crate::store::shell_selection::Publication>,
    retained_written: u64,
    commit: Option<StateCommit>,
    replay: Option<ReplayPendingWork>,
    post_cut_fence: Option<CheckpointCommitFence>,
}

impl SerialCheckpointCommit {
    pub(super) fn new(
        state: &mut crate::persistence::State,
        batch: &mut CheckpointBatch,
        index: usize,
    ) -> Result<Self, crate::store::StoreError> {
        let (retain, selection) = if batch
            .inputs
            .get(index)
            .invariant("checkpoint input missing")?
            .stage
            .is_some()
        {
            promote_checkpoint_input(state, batch, index)?;
            let selector = state.store.bound_shell()?.selection(
                &state.store,
                batch.root.join(format!("serial-{index:02}")),
                Some(
                    batch
                        .inputs
                        .get(index)
                        .invariant("checkpoint input missing")?
                        .incoming,
                ),
            )?;
            state.retention_candidates.push(selector.cleanup_paths());
            (None, Some(selector))
        } else {
            let input = batch
                .inputs
                .get(index)
                .invariant("checkpoint input missing")?;
            (
                Some(
                    state
                        .store
                        .start_retain_in(
                            &input.channel,
                            None,
                            input.raw.clone(),
                            batch.root.join(format!("serial-retain-{index:02}")),
                        )?
                        .with_continuity(input.continuity.clone()),
                ),
                None,
            )
        };
        Ok(Self {
            index,
            retain,
            selection,
            publication: None,
            retained_written: 0,
            commit: None,
            replay: None,
            post_cut_fence: None,
        })
    }

    pub(super) fn step(
        &mut self,
        state: &mut crate::persistence::State,
        batch: &mut CheckpointBatch,
    ) -> Result<Step<StateCommit>, crate::store::StoreError> {
        if let Some(retain) = &mut self.retain {
            match retain.step(WorkLimit::default())? {
                Step::More => return Ok(Step::More),
                Step::Done(outcome) => {
                    self.retained_written = outcome.written;
                    self.publication = Some(outcome.publication);
                    self.retain = None;
                    return Ok(Step::More);
                }
            }
        }
        if let Some(selection) = &mut self.selection {
            match selection.step(WorkLimit::default())? {
                Step::More => return Ok(Step::More),
                Step::Done(crate::store::shell_selection::SelectionOutcome::Published(
                    publication,
                )) => {
                    if matches!(
                        publication,
                        crate::store::shell_selection::Publication::Activate(_)
                            | crate::store::shell_selection::Publication::ReplayRequired(_)
                    ) {
                        let prepared = batch
                            .inputs
                            .get(self.index)
                            .invariant("checkpoint input missing")?
                            .prepared
                            .as_ref()
                            .invariant("prepared checkpoint input")?;
                        self.post_cut_fence = Some(CheckpointCommitFence {
                            recovery_lives: matches!(
                                publication,
                                crate::store::shell_selection::Publication::ReplayRequired(_)
                            )
                            .then(|| prepared.session_lives.clone()),
                            activating: true,
                            sessions: prepared.sessions.clone(),
                            replay_targets: prepared.replay_targets.clone(),
                        });
                    }
                    self.publication = Some(publication);
                    self.selection = None;
                    return Ok(Step::More);
                }
                Step::Done(_) => {
                    return Err(crate::persistence::error(
                        "serial checkpoint fallback returned invalid selection outcome",
                    ));
                }
            }
        }
        if self.commit.is_none() {
            let input = batch
                .inputs
                .get_mut(self.index)
                .invariant("checkpoint input missing")?;
            if let Some(stage) = input.stage.as_mut()
                && matches!(stage.finish_gc(WorkLimit::default())?, Step::More)
            {
                return Ok(Step::More);
            }
            commit_and_materialize(state, input)?;
            let mut commit = state_commit_for(
                input,
                self.publication
                    .take()
                    .invariant("serial checkpoint publication")?,
            )?;
            commit.persisted_bytes = commit.persisted_bytes.max(self.retained_written);
            self.commit = Some(commit);
            let commit = self.commit.as_ref().invariant("commit missing")?;
            self.replay = if let Some(path) = &commit.selected {
                (!commit.replay_targets.is_empty())
                    .then(|| ReplayPendingWork::selected(path, commit.replay_targets.clone()))
                    .transpose()?
            } else if commit.publish && !commit.replay_targets.is_empty() {
                Some(ReplayPendingWork::original(
                    commit.envelope.clone(),
                    commit.replay_targets.clone(),
                    commit.continuity.clone(),
                ))
            } else {
                None
            };
            return Ok(Step::More);
        }
        if matches!(
            step_commit_replay(&mut self.replay, &mut self.commit, state)?,
            Step::More
        ) {
            return Ok(Step::More);
        }
        Ok(Step::Done(self.commit.take().invariant("commit missing")?))
    }

    pub(super) fn error_fence(&self, batch: &CheckpointBatch) -> Option<CheckpointCommitFence> {
        if let Some(commit) = &self.commit {
            return Some(CheckpointCommitFence::capture(commit));
        }
        if let Some(fence) = &self.post_cut_fence {
            return Some(fence.clone());
        }
        let publication = self
            .selection
            .as_ref()
            .and_then(|selection| selection.publication_on_error())
            .or_else(|| {
                self.retain
                    .as_ref()
                    .and_then(super::store::retained::RetainBuild::publication_on_error)
            })?;
        let prepared = batch.inputs.get(self.index)?.prepared.as_ref()?;
        Some(CheckpointCommitFence {
            recovery_lives: matches!(
                publication,
                crate::store::shell_selection::Publication::ReplayRequired(_)
            )
            .then(|| prepared.session_lives.clone()),
            activating: true,
            sessions: prepared.sessions.clone(),
            replay_targets: prepared.replay_targets.clone(),
        })
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
)]
pub(super) async fn publish_checkpoint_commit(
    workspace: &SharedWorkspace,
    source: &StateSource,
    workers: Arc<tokio::sync::Semaphore>,
    metrics: &crate::metrics::Metrics,
    mut commit: StateCommit,
) -> Result<(), crate::store::StoreError> {
    trace_native_publication(
        if commit.activation_error.is_some() {
            "prefix_activation_error"
        } else if commit.publish {
            "prefix_durable"
        } else {
            "prefix_suppressed"
        },
        &commit.envelope,
        None,
    );
    if !commit.publish {
        return Ok(());
    }
    for _ in 0..commit.accepted_deltas {
        metrics.inc_deltas_received();
    }
    let activating = commit.selected.is_some() || commit.recovery_scope;
    let recovering =
        commit.recovery_scope || matches!(commit.selected, Some(SelectedPublication::Recovery(_)));
    let mut cursor = if let Some(path) = commit.selected.take() {
        Some(crate::persistence::blocking(workers.clone(), move || path.open()).await??)
    } else {
        None
    };
    let mut original = cursor.is_none();
    let mut delivered = 0u64;
    loop {
        let (envelope, budget) = if let Some(mut selected) = cursor.take() {
            let (next, result) = crate::persistence::blocking(workers.clone(), move || {
                let result = selected.step(crate::persistence::output_budget());
                (selected, result)
            })
            .await?;
            cursor = Some(next);
            match result? {
                Step::More => continue,
                Step::Done(None) => break,
                Step::Done(Some(batch)) => (LiveEnvelope::Selected(Arc::new(batch)), Ok(None)),
            }
        } else if original {
            original = false;
            let bytes = match &*commit.envelope {
                Envelope::Delta { delta, .. } => delta.len().saturating_add(256),
                _ => 1,
            };
            (
                LiveEnvelope::Original(commit.envelope.clone()),
                crate::persistence::delivery_budget(bytes).map(Some),
            )
        } else {
            break;
        };
        let mut runtime = crate::workspace_io::fresh_membership(workspace).await?;
        let fanned = FanoutStamp::of(&runtime);
        if !source.authorized(&runtime) {
            if activating {
                return Err(crate::persistence::error(
                    "activation source authorization changed",
                ));
            }
            break;
        }
        let payloads = &**runtime.store.payloads();
        let checkpoint_channel = gated_checkpoint_channel(payloads, &envelope, Some(source));
        let audience = live_audience(payloads, &envelope);
        for (recipient, original) in &commit.sessions {
            if recovering
                && !captured_live_current(&commit.session_lives, &runtime, recipient, original)
            {
                continue;
            }
            let Some(sub) = runtime.subscribers.get(recipient) else {
                continue;
            };
            if !sub.tx.same_channel(original) {
                continue;
            }
            let Some(target) = runtime.state_sessions.get(recipient) else {
                continue;
            };
            if !source.admits(&runtime, &target.peer) {
                continue;
            }
            if audience
                .as_ref()
                .is_some_and(|audience| !audience.contains(&target.peer.device_public_key))
            {
                continue;
            }
            let replay_phase = target
                .replay
                .as_ref()
                .map(|replay| replay.phase.load(std::sync::atomic::Ordering::SeqCst));
            if replay_phase == Some(ReplayCatchup::ACTIVE) {
                target
                    .replay
                    .as_ref()
                    .invariant("replay missing")?
                    .dirty
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                continue;
            }
            if target.peer.role == crate::auth::PeerRole::Device
                && checkpoint_channel
                    .is_some_and(|channel| !runtime.needs_checkpoint(recipient, &channel))
            {
                continue;
            }
            let Ok(budget) = &budget else {
                if activating {
                    return Err(crate::persistence::error(
                        "activation delivery byte backpressure",
                    ));
                }
                if replay_phase == Some(ReplayCatchup::BOUNDARY) {
                    target
                        .live
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                    let _ = sub.disconnect.try_send(());
                } else if let Some(lag) = &target.lag {
                    lag.mark(runtime.store.workspace_id());
                }
                continue;
            };
            let delivery = StateDelivery {
                envelope: envelope.clone(),
                cleanup: commit.pending.remove(recipient),
                source: Some(source.clone()),
                _budget: budget.clone(),
                live: target.live.clone(),
                fanned: fanned.clone(),
            };
            match target.tx.try_send(delivery) {
                Ok(()) => {
                    delivered = delivered.saturating_add(1);
                    if target.peer.role == crate::auth::PeerRole::Device
                        && let Some(channel) = checkpoint_channel
                    {
                        runtime.mark_checkpoint_served(recipient, channel);
                    }
                }
                Err(mpsc::error::TrySendError::Full(delivery)) => {
                    if activating || matches!(&delivery.envelope, LiveEnvelope::Selected(_)) {
                        let _ = sub.disconnect.try_send(());
                        runtime.remove_subscriber_session(recipient, original);
                    } else if replay_phase == Some(ReplayCatchup::BOUNDARY) {
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
    }
    metrics.add_deltas_fanned_out(delivered);
    metrics.add_bytes_queued(commit.persisted_bytes);
    Ok(())
}

#[derive(Clone)]
pub(super) struct CheckpointCommitFence {
    recovery_lives: Option<SessionLives>,
    activating: bool,
    sessions: Vec<([u8; 32], mpsc::Sender<Envelope>)>,
    replay_targets: Vec<ReplayPendingTarget>,
}

impl CheckpointCommitFence {
    pub(super) fn capture(commit: &StateCommit) -> Self {
        Self {
            recovery_lives: (commit.recovery_scope
                || matches!(commit.selected, Some(SelectedPublication::Recovery(_))))
            .then(|| commit.session_lives.clone()),
            activating: commit.selected.is_some() || commit.recovery_scope,
            sessions: commit.sessions.clone(),
            replay_targets: commit.replay_targets.clone(),
        }
    }

    pub(super) async fn apply(self, workspace: &SharedWorkspace) {
        let mut runtime = workspace.lock().await;
        if self.activating {
            for (recipient, original) in &self.sessions {
                if self.recovery_lives.as_ref().is_some_and(|lives| {
                    !captured_live_current(lives, &runtime, recipient, original)
                }) {
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
            for target in &self.replay_targets {
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

pub(super) async fn publish_checkpoint_commit_fenced(
    workspace: &SharedWorkspace,
    source: &StateSource,
    workers: Arc<tokio::sync::Semaphore>,
    metrics: &crate::metrics::Metrics,
    commit: StateCommit,
) -> Result<(), crate::store::StoreError> {
    let fence = CheckpointCommitFence::capture(&commit);
    let result = publish_checkpoint_commit(workspace, source, workers, metrics, commit).await;
    if result.is_err() {
        fence.apply(workspace).await;
    }
    result
}
