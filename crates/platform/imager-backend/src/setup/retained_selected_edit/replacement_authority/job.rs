//! Root-owned continuation of one real source reservation, on the existing
//! job surface. Unknown delivery retains ownership and never yields Finished.
use super::*;
use crate::setup::retained_source_relay::selected_parent::SourceSelectedParent;
use runtime_fs::{
    EntryName,
    directory_backend::{JobPoll, JobUpdate, WorkspaceJobBackend},
    external_history::replacement_control::ReplacementReply,
};
use std::sync::Mutex;

pub(super) struct OwnedReplacementJob {
    source: std::sync::Weak<SourceSelectedParent>,
    operation: [u8; 16],
    state: Mutex<State>,
}
struct State {
    source: Option<Arc<SourceSelectedParent>>,
    pending: Option<Arc<PendingSelectedReplacement>>,
    attempted: bool,
    finished: bool,
    handoff: Option<crate::setup::retained_source_worker::PendingHandoff>,
    completed: Option<Arc<dyn WorkspaceJobBackend>>,
    failed: Option<String>,
    terminal: Option<JobPoll>,
    acknowledged: Option<Arc<AcknowledgedSelectedReplacement>>,
    history_read: Option<
        Result<Option<Arc<crate::setup::retained_history_read::RootHistoryReadContinuity>>, String>,
    >,
}
/// Evidence minted only from this job's actual native terminal poll.
pub struct ObservedReplacementFinished {
    poll: JobPoll,
    native_job: Arc<dyn WorkspaceJobBackend>,
}
impl ObservedReplacementFinished {
    pub fn retire_controller_cache(&self) -> Result<(), String> {
        self.native_job.retire_controller_cache()
    }

    pub fn check_operation(&self, operation: [u8; 16]) -> Result<(), String> {
        if self.poll.operation_id != operation
            || !self.poll.updates.iter().any(|u| {
                matches!(
                    u,
                    JobUpdate::Finished {
                        completed: 1,
                        failed: 0,
                        cancelled: 0
                    }
                )
            })
        {
            return Err("Observed native terminal changed".into());
        }
        Ok(())
    }
}
impl RootSelectedReplacementSeed {
    pub(in crate::setup) fn check_completed_registry_transfer(
        self: &Arc<Self>,
        proof: &AcknowledgedSelectedReplacement,
    ) -> Result<(), String> {
        self.check().map_err(|error| error.to_string())?;
        if !Arc::ptr_eq(self, proof.pending().seed()) {
            return Err("Completed replacement seed changed".into());
        }
        let owners = self
            .replacements
            .lock()
            .map_err(|_| "Replacement registry poisoned".to_string())?;
        if owners.contains_key(&proof.pending().observation().operation) {
            return Err("Native completion has not transferred registry ownership".into());
        }
        drop(owners);
        self.check().map_err(|error| error.to_string())
    }

    pub(in crate::setup) fn has_unresolved_replacement(&self) -> bool {
        self.transfer_failed
            .load(std::sync::atomic::Ordering::Acquire)
            || self.replacements.lock().map_or(true, |v| !v.is_empty())
    }
    pub(in crate::setup) fn start_owned_replacement(
        self: &Arc<Self>,
        source: &Arc<SourceSelectedParent>,
        temporary: &EntryName,
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        // Cleanup is independent of creating this new native reservation. A
        // transient cleanup failure keeps every old owner and cannot erase it.
        let _ = source.prune_completed_handoffs();
        let pending = self.reserve(source, temporary).map_err(|e| e.to_string())?;
        let operation = pending.observation().operation;
        let job = Arc::new(OwnedReplacementJob {
            source: Arc::downgrade(source),
            operation,
            state: Mutex::new(State {
                source: Some(source.clone()),
                pending: Some(pending),
                attempted: false,
                finished: false,
                handoff: None,
                completed: None,
                failed: None,
                terminal: None,
                acknowledged: None,
                history_read: None,
            }),
        });
        {
            let mut owners = self
                .replacements
                .lock()
                .map_err(|_| "Replacement ownership unavailable".to_string())?;
            if owners.len() >= 64 || owners.contains_key(&operation) {
                return Err("Replacement ownership limit".into());
            }
            // Strong ownership precedes the first possible native effect.
            owners.insert(operation, job.clone());
        }
        Ok(job)
    }
}
impl OwnedReplacementJob {
    fn drive(&self, state: &mut State) -> Result<(), String> {
        let source = state
            .source
            .as_ref()
            .ok_or_else(|| "Source custody missing".to_string())?
            .clone();
        let pending = state
            .pending
            .as_ref()
            .ok_or_else(|| "Pending ownership missing".to_string())?
            .clone();
        pending.check_custody(&source)?;
        source.begin_pending(pending.clone())?;
        let request = uuid::Uuid::from_bytes(pending.observation().operation).to_string();
        let daemon = &pending.seed().daemon;
        let reply = if state.attempted {
            daemon.replacement_command(&request, None)
        } else {
            let handoff = source
                .connect_pending_handoff(&pending)
                .map_err(|e| e.to_string())?;
            state.handoff = Some(handoff);
            // A failed write/read cannot prove Start was unobserved. Ownership
            // remains in the actual seed registry even if this client drops.
            state.attempted = true;
            daemon.replacement_command(&request, state.handoff.as_ref())
        }
        .map_err(|e| e.to_string())?;
        match reply {
            ReplacementReply::Pending { operation_id, .. } => {
                if operation_id != request {
                    return Err("Replacement operation changed".into());
                }
            }
            ReplacementReply::Retired { .. } => {
                return Err("Replacement retirement is not a Save outcome".into());
            }
            ReplacementReply::Refused { .. } => {
                return Err("Certified replacement refused; custody retained".into());
            }
            ReplacementReply::Committed {
                prepared, receipt, ..
            } => {
                let proof = Arc::new(
                    pending
                        .acknowledged(*prepared, *receipt)
                        .map_err(|e| e.to_string())?,
                );
                // SAME source submitted job, whose terminal updates were actually
                // observed and cached; no DTO or signed receipt fabricates JobPoll.
                let completed = pending.backend().acknowledged_committed_job()?;
                match completed.live_scope() {
                    Some(Ok(())) => {}
                    _ => return Err("Acknowledged native job scope unavailable".into()),
                }
                source.commit_lineage(&proof)?;
                proof.check_custody(&source)?;
                // Eagerly adopt the new native leaf into the independently live
                // independently admitted Reader before releasing this same ACK. Reader
                // closure does not undo a committed save or revive its proof.
                state.history_read = Some(
                    pending
                        .seed()
                        .adopt_history_acknowledged(&proof, &source)
                        .map_err(|error| error.to_string()),
                );
                state.acknowledged = Some(proof);
                state.completed = Some(completed);
            }
        }
        Ok(())
    }
}
impl WorkspaceJobBackend for OwnedReplacementJob {
    fn live_scope(&self) -> Option<Result<(), String>> {
        Some((|| {
            let state = self
                .state
                .lock()
                .map_err(|_| "Replacement job poisoned".to_string())?;
            if let (Some(pending), Some(source)) = (&state.pending, &state.source) {
                return pending.check_custody(source);
            }
            if !state.finished {
                return Err("Replacement custody missing".into());
            }
            self.source
                .upgrade()
                .ok_or_else(|| "Selected source retired".to_string())?
                .check_authority()
        })())
    }
    fn poll(&self) -> Result<JobPoll, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Replacement job poisoned".to_string())?;
        if state.finished {
            return Ok(JobPoll {
                operation_id: self.operation,
                updates: Vec::new(),
            });
        }
        let source = state
            .source
            .as_ref()
            .ok_or_else(|| "Source custody missing".to_string())?
            .clone();
        let pending = state
            .pending
            .as_ref()
            .ok_or_else(|| "Pending ownership missing".to_string())?
            .clone();
        if let Some(error) = &state.failed {
            return Err(error.clone());
        }
        if state.completed.is_none() {
            if let Err(error) = self.drive(&mut state) {
                // A drain-in-progress is retryable with this SAME pending Arc.
                if !error.starts_with("Selected replacement pending;") {
                    state.failed = Some(error.clone());
                }
                return Err(error);
            }
        }
        let Some(completed) = &state.completed else {
            return Ok(JobPoll {
                operation_id: pending.observation().operation,
                updates: Vec::new(),
            });
        };
        if state.terminal.is_none() {
            let poll = completed.poll()?;
            if poll.operation_id != pending.observation().operation {
                return Err("Acknowledged native job operation changed".into());
            }
            if !poll.updates.iter().any(|v| {
                matches!(
                    v,
                    JobUpdate::Finished {
                        completed: 1,
                        failed: 0,
                        cancelled: 0
                    }
                )
            }) {
                return Ok(poll);
            }
            // Cache the ACTUALLY observed terminal updates before any fallible
            // ownership transfer. A retry must not reconsume a draining job.
            state.terminal = Some(poll);
        }
        pending.check_custody(&source)?;
        let proof = state
            .acknowledged
            .as_ref()
            .ok_or_else(|| "Actual acknowledged replacement missing".to_string())?
            .clone();
        let continuity = state
            .history_read
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .and_then(|value| value.clone());
        let finished = ObservedReplacementFinished {
            poll: state
                .terminal
                .as_ref()
                .ok_or_else(|| "Actual native terminal missing".to_string())?
                .clone(),
            native_job: state
                .completed
                .as_ref()
                .ok_or_else(|| "Actual native job missing".to_string())?
                .clone(),
        };
        if let Some(handoff) = state.handoff.take() {
            if let Err(handoff) =
                source.retain_completed_handoff(handoff, proof.clone(), continuity, finished)
            {
                state.handoff = Some(*handoff);
                return Err(
                    "Completed source handoff ownership not transferred; custody retained".into(),
                );
            }
        }
        // The latest signed ACK stays owned. Only independently verified old
        // continuations can leave the completed queue; transient refusals do not
        // undo the actually observed successful native Save.
        let _ = source.prune_completed_handoffs();
        let transferred = {
            let mut owners = pending
                .seed()
                .replacements
                .lock()
                .map_err(|_| "Replacement registry poisoned".to_string())?;
            let actual = owners
                .get(&self.operation)
                .ok_or_else(|| "Actual completed registry owner missing".to_string())?;
            if !std::ptr::eq(Arc::as_ptr(actual), self) {
                return Err("Completed registry owner changed".into());
            }
            owners
                .remove(&self.operation)
                .ok_or_else(|| "Actual completed registry owner missing".to_string())?
        };
        // Keep the actual removed registry owner until the fallible source gate
        // transition succeeds. A client disappearing cannot abandon this phase.
        if let Err(error) = source.finish_completed_transfer(&proof) {
            if let Ok(mut owners) = pending.seed().replacements.lock() {
                if owners.contains_key(&self.operation) {
                    // An impossible conflicting owner must not drop either
                    // actual continuation. Its source gate stays closed.
                    pending
                        .seed()
                        .transfer_failed
                        .store(true, std::sync::atomic::Ordering::Release);
                    std::mem::forget(transferred);
                    return Err("Completed transfer registry conflict; custody retained".into());
                }
                owners.insert(self.operation, transferred);
            } else {
                // Poison makes has_unresolved_replacement refuse retirement.
                // Preserve this genuine owner for the root helper lifetime.
                pending
                    .seed()
                    .transfer_failed
                    .store(true, std::sync::atomic::Ordering::Release);
                std::mem::forget(transferred);
            }
            return Err(error);
        }
        let poll = state
            .terminal
            .take()
            .ok_or_else(|| "Observed native terminal missing".to_string())?;
        // The completed queue owns the actual ACK, Read continuity and handler.
        // A caller retaining the public drained job must not retain duplicate
        // native pin/job/Read handles forever or trigger a second submission.
        state.source = None;
        state.pending = None;
        state.completed = None;
        state.acknowledged = None;
        state.history_read = None;
        state.finished = true;
        Ok(poll)
    }
    fn cancel(&self) -> Result<(), String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "Replacement job poisoned".to_string())?;
        if state.finished {
            return Ok(());
        }
        state
            .pending
            .as_ref()
            .ok_or_else(|| "Pending ownership missing".to_string())?
            .backend()
            .cancel()
    }
}
