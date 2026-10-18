//! Owner-private report prose and links derived from durable native action receipts.
//! Neither store is part of a room publication.
use crate::{NativeWorkspaceOwner, RoomId};
use std::collections::BTreeMap;
use tilo_actions::task_messaging::{TaskOutputRef, TaskRunAddress};
use tilo_peer_state::workspace_task::WorkspaceTaskAuthority;

/// Exact native report body, retained only after the canonical reducer accepts it.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePrivateTaskReport {
    /// Exact source message identity: the owner-command invocation UUID.
    pub invocation: uuid::Uuid,
    /// Authenticated device that submitted this exact invocation.
    pub source_device: [u8; 32],
    pub role: NativePrivateTaskReportRole,
    /// Owner wall-clock acceptance time, nanoseconds since the UNIX epoch.
    pub accepted_at_unix_ns: u64,
    /// Owner acceptance order, independent of untrusted model report counters.
    pub sequence: u64,
    pub task_run: TaskRunAddress,
    /// Authenticated reporting principal; host failures retain the run actor separately.
    pub actor: String,
    pub kind: NativePrivateTaskReportKind,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<tilo_actions::task_messaging::TaskReportPresentation>,
}

impl std::fmt::Debug for NativePrivateTaskReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativePrivateTaskReport { private: redacted }")
    }
}

/// Automatic run reports are assistant messages, including host-reported failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativePrivateTaskReportRole {
    Assistant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativePrivateTaskReportKind {
    Presentation,
    Progress,
    Finish,
    Failure,
}

/// Archival events have no authenticated invocation identity and must never be
/// promoted to `NativePrivateTaskReport` or returned by the report transcript.
pub(crate) fn validate_historical_reports(
    reports: &BTreeMap<String, Vec<serde_json::Value>>,
) -> Result<(), String> {
    if reports.len() > 4096 || reports.values().map(Vec::len).sum::<usize>() > 4096 {
        return Err("private historical task report capacity exhausted".into());
    }
    for events in reports.values() {
        for event in events {
            if !event.is_object()
                || event
                    .get("note")
                    .and_then(serde_json::Value::as_str)
                    .is_none()
            {
                return Err("invalid private historical task report".into());
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_reports(
    reports: &BTreeMap<String, NativePrivateTaskReport>,
) -> Result<(), String> {
    if reports.len() > 4096 {
        return Err("private task report capacity exhausted".into());
    }
    let mut sequences = std::collections::BTreeSet::new();
    for (id, report) in reports {
        report.task_run.validate()?;
        validate_presentation_body(&report.body, report.kind, report.presentation.as_ref())?;
        if report.invocation.is_nil()
            || report.accepted_at_unix_ns == 0
            || report.sequence == 0
            || report.sequence > reports.len() as u64
            || !sequences.insert(report.sequence)
            || *id != report.invocation.to_string()
            || (report.actor != "host"
                && report.actor != format!("agent:{}", report.task_run.agent_id))
        {
            return Err("invalid private task report".into());
        }
    }
    Ok(())
}

fn validate_presentation_body(
    body: &str,
    kind: NativePrivateTaskReportKind,
    presentation: Option<&tilo_actions::task_messaging::TaskReportPresentation>,
) -> Result<(), String> {
    if let Some(presentation) = presentation {
        presentation.validate()?;
        if kind != NativePrivateTaskReportKind::Presentation || body != presentation.text {
            return Err("private task presentation body mismatch".into());
        }
    } else if kind == NativePrivateTaskReportKind::Presentation
        || body.trim().is_empty()
        || body.len() > tilo_desktop_model::MAX_WORKSPACE_TASK_SUMMARY_BYTES
    {
        return Err("invalid private task report body".into());
    }
    Ok(())
}

struct PrivateReportInput<'a> {
    task_id: &'a String,
    epoch: u64,
    body: &'a String,
    kind: NativePrivateTaskReportKind,
    neutral: &'static str,
}

fn private_report_input(
    command: &tilo_peer_state::workspace_task::WorkspaceTaskCommand,
) -> Option<PrivateReportInput<'_>> {
    use tilo_peer_state::workspace_task::WorkspaceTaskCommand as C;
    let (task_id, epoch, body, kind, neutral) = match command {
        C::Finish {
            task_id,
            run_epoch,
            summary,
            outcome,
        } => (
            task_id,
            *run_epoch,
            summary,
            NativePrivateTaskReportKind::Finish,
            match outcome {
                tilo_peer_state::workspace_task::WorkspaceTaskFinish::Done => "done",
                tilo_peer_state::workspace_task::WorkspaceTaskFinish::Waiting => "waiting",
                tilo_peer_state::workspace_task::WorkspaceTaskFinish::Cancelled => "cancelled",
            },
        ),
        C::Abandon {
            task_id,
            run_epoch,
            reason,
        } => (
            task_id,
            *run_epoch,
            reason,
            NativePrivateTaskReportKind::Failure,
            "abandoned",
        ),
        C::PublicProgress {
            task_id,
            run_epoch,
            message,
            ..
        } => (
            task_id,
            *run_epoch,
            message,
            NativePrivateTaskReportKind::Progress,
            "progress",
        ),
        C::Presentation {
            task_id,
            run_epoch,
            presentation,
            ..
        } => (
            task_id,
            *run_epoch,
            &presentation.text,
            NativePrivateTaskReportKind::Presentation,
            "progress",
        ),
        // Explicitly reviewed non-report commands. Keep this exhaustive so a
        // new prose-bearing command cannot silently bypass private custody.
        C::Queue { .. }
        | C::QueueWithMetadata { .. }
        | C::SetMetadata { .. }
        | C::StartRun { .. }
        | C::Direction { .. }
        | C::AppliedAck { .. }
        | C::DirectionForRun { .. }
        | C::Retry { .. }
        | C::ExecutionPhase { .. }
        | C::CancelForRun { .. }
        | C::CreateManual { .. }
        | C::AttachResource { .. }
        | C::SetExecutionBudget { .. }
        | C::ApproveRun { .. }
        | C::QueueInstruction { .. }
        | C::QueueInstructionAbout { .. }
        | C::RequestConfirmation { .. }
        | C::AnswerConfirmation { .. }
        | C::SettleConfirmation { .. }
        | C::CreateSubtask { .. }
        | C::MoveTask { .. }
        | C::SetLabels { .. }
        | C::SetAssignees { .. }
        | C::QueuePrivateSubtask { .. }
        | C::BindExecutor { .. } => return None,
    };
    Some(PrivateReportInput {
        task_id,
        epoch,
        body,
        kind,
        neutral,
    })
}

/// Strip automatic prose before it reaches Automerge, including historical changes.
/// Manual coordination remains public. The original request digest still binds the
/// invocation, so a retry cannot replace a private body behind a neutral summary.
pub(crate) fn prepare_report(
    command: &tilo_peer_state::workspace_task::WorkspaceTaskCommand,
    peer: &tilo_peer_state::StatePeer,
    authority: &WorkspaceTaskAuthority,
    invocation: uuid::Uuid,
) -> Result<
    (
        tilo_peer_state::workspace_task::WorkspaceTaskCommand,
        Option<NativePrivateTaskReport>,
    ),
    String,
> {
    use tilo_peer_state::workspace_task::WorkspaceTaskCommand as C;
    let Some(PrivateReportInput {
        task_id,
        epoch,
        body,
        kind,
        neutral,
    }) = private_report_input(command)
    else {
        return Ok((command.clone(), None));
    };
    let snapshot = tilo_peer_state::workspace_task::snapshot(
        peer.admitted_room().ok_or("private report room absent")?,
    )?;
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == *task_id)
        .ok_or("private report task absent")?;
    if task.route == tilo_desktop_model::WorkspaceTaskRoute::Manual {
        return Ok((command.clone(), None));
    }
    // Do not let the neutral replacement bypass the original input bounds.
    let presentation = match command {
        C::Presentation { presentation, .. } => Some(
            serde_json::from_value::<tilo_actions::task_messaging::TaskReportPresentation>(
                serde_json::to_value(presentation).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?,
        ),
        _ => None,
    };
    validate_presentation_body(body, kind, presentation.as_ref())?;
    let mut public = command.clone();
    match &mut public {
        C::Finish { summary, .. } => *summary = neutral.into(),
        C::Abandon { reason, .. } => *reason = neutral.into(),
        C::PublicProgress { message, .. } => *message = neutral.into(),
        C::Presentation { presentation, .. } => {
            presentation.text.clear();
            presentation.reasoning.clear();
            presentation.detail.clear();
        }
        _ => return Err("private report changed variant".into()),
    }
    // Let the canonical reducer verify the original request digest on retries,
    // even after this run ended or a new actor was bound. Never mint a new message.
    if tilo_peer_state::workspace_task::result_of(
        peer.admitted_room().ok_or("private report room absent")?,
        tilo_peer_state::workspace_task::WorkspaceTaskCommandId {
            device: authority.certificate().device_public_key,
            invocation,
        },
    )?
    .is_some()
    {
        return Ok((public, None));
    }
    let run = snapshot
        .controls
        .get(task_id)
        .and_then(|control| control.active_run.as_ref())
        .filter(|run| run.epoch == epoch)
        .ok_or("private report does not address the current run")?;
    let actor = match authority {
        WorkspaceTaskAuthority::Agent { agent_id, .. } => format!("agent:{agent_id}"),
        WorkspaceTaskAuthority::Host { .. } => "host".into(),
        WorkspaceTaskAuthority::Person { .. } => {
            return Err("person cannot report an automatic run".into());
        }
    };
    // The canonical reducer below checks liveness and reporting authority.
    // The transcript binds the run actor, not mutable queue-assignment metadata.
    let task_run = TaskRunAddress {
        task_id: task_id.clone(),
        run_epoch: epoch,
        agent_id: run.agent_id.clone(),
    };
    let report = NativePrivateTaskReport {
        invocation,
        source_device: authority.certificate().device_public_key,
        role: NativePrivateTaskReportRole::Assistant,
        accepted_at_unix_ns: crate::enrollment::ns(std::time::SystemTime::now())?,
        sequence: 0,
        task_run,
        actor,
        kind,
        body: body.clone(),
        presentation,
    };
    Ok((public, Some(report)))
}

#[cfg(test)]
mod presentation_tests {
    use super::*;
    use collab_types::task_presentation::{TaskPresentation, TaskPresentationFinish};

    #[test]
    fn private_presentation_type_preserves_current_wire_fields_and_exact_bounds() {
        let wire = TaskPresentation {
            run: "r".repeat(TaskPresentation::MAX_RUN_BYTES),
            sequence: 9,
            received_at: Some(321),
            text: "é".repeat(2048),
            reasoning: "r".repeat(2048),
            finish: Some(TaskPresentationFinish::Refused),
            detail: "d".repeat(TaskPresentation::MAX_DETAIL_BYTES),
        };
        let json = serde_json::to_value(&wire).unwrap();
        let private: tilo_actions::task_messaging::TaskReportPresentation =
            serde_json::from_value(json.clone()).unwrap();
        private.validate().unwrap();
        assert_eq!(serde_json::to_value(&private).unwrap(), json);
        validate_presentation_body(
            &wire.text,
            NativePrivateTaskReportKind::Presentation,
            Some(&private),
        )
        .unwrap();
        for field in ["run", "text", "reasoning", "detail"] {
            let mut oversize = json.clone();
            let current = oversize[field].as_str().unwrap().to_owned();
            oversize[field] = format!("{current}x").into();
            let private: tilo_actions::task_messaging::TaskReportPresentation =
                serde_json::from_value(oversize).unwrap();
            assert!(private.validate().is_err(), "{field}");
        }
    }
}

fn key(output: &TaskOutputRef) -> Result<String, String> {
    serde_json::to_string(&(
        &output.task_run,
        &output.invocation,
        &output.provider,
        &output.resource,
    ))
    .map_err(|e| e.to_string())
}
pub(crate) fn validate(outputs: &BTreeMap<String, TaskOutputRef>) -> Result<(), String> {
    if outputs.len() > 4096 {
        return Err("private task output receipt capacity exhausted".into());
    }
    for (id, output) in outputs {
        output.validate()?;
        if *id != key(output)? {
            return Err("private task output receipt identity mismatch".into());
        }
    }
    Ok(())
}
impl NativeWorkspaceOwner {
    /// Read exact original automatic report bodies under current private-task custody.
    /// Historical reads are available to the authorized person, never a host-only caller.
    ///
    /// # Errors
    /// Refuses unrelated people, stale agent runs, revoked authority, and invalid storage.
    pub fn task_report_transcript(
        &self,
        room: RoomId,
        task_id: &str,
        epoch: u64,
        authority: &WorkspaceTaskAuthority,
    ) -> Result<Vec<NativePrivateTaskReport>, String> {
        let _turn = self.0.mutation.lock();
        self.workspace_task_private_instructions(
            room,
            task_id,
            epoch,
            authority,
            std::time::SystemTime::now(),
        )?;
        self.with_current(room, |view| {
            let reports = &view.record.private_task_text.reports;
            validate_reports(reports)?;
            let mut transcript: Vec<_> = reports
                .values()
                .filter(|report| {
                    report.task_run.task_id == task_id && report.task_run.run_epoch == epoch
                })
                .cloned()
                .collect();
            transcript.sort_by_key(|report| report.sequence);
            Ok(transcript)
        })
    }

    /// The native broker supplies only verified durable invocation result links.
    /// Application JSON and public progress do not call this API.
    ///
    /// # Errors
    /// Refuses stale run authority, invalid references, or conflicting retained receipts.
    pub fn record_task_outputs(
        &self,
        room: RoomId,
        address: &TaskRunAddress,
        outputs: Vec<TaskOutputRef>,
        authorize: impl FnOnce(&crate::NativeCommittedView) -> Result<(), String>,
    ) -> Result<(), String> {
        if outputs.is_empty() {
            return Ok(());
        }
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| {
            authorize(view)?;
            let snapshot = tilo_peer_state::workspace_task::snapshot(
                view.peer()
                    .admitted_room()
                    .ok_or("task output room absent")?,
            )?;
            crate::private_task_mailboxes::live(&snapshot, address)?;
            let run = snapshot
                .controls
                .get(&address.task_id)
                .and_then(|control| control.active_run.as_ref())
                .ok_or("task output run ended")?;
            view.peer().workspace().check_task_execution_actor(
                &address.agent_id,
                run.approval.as_ref(),
                std::time::SystemTime::now(),
            )?;
            Ok(view.clone())
        })?;
        let store = &mut candidate.record.private_task_mailboxes.outputs;
        let before = store.clone();
        for output in outputs {
            output.validate()?;
            if output.task_run != *address {
                return Err("task output belongs to another run".into());
            }
            let key = key(&output)?;
            if let Some(old) = store.get(&key) {
                if old != &output {
                    return Err("task output receipt was replaced".into());
                }
            } else {
                store.insert(key, output);
            }
        }
        validate(store)?;
        if *store == before {
            return Ok(());
        }
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("owner generation exhausted")?;
        self.publish_room(room, candidate, None)
    }

    ///
    /// # Errors
    /// Refuses callers without current authority over the exact task and epoch.
    pub fn task_outputs(
        &self,
        room: RoomId,
        task_id: &str,
        epoch: u64,
        authority: &WorkspaceTaskAuthority,
    ) -> Result<Vec<TaskOutputRef>, String> {
        let _turn = self.0.mutation.lock();
        self.workspace_task_private_instructions(
            room,
            task_id,
            epoch,
            authority,
            std::time::SystemTime::now(),
        )?;
        self.with_current(room, |view| {
            let store = &view.record.private_task_mailboxes.outputs;
            validate(store)?;
            Ok(store
                .values()
                .filter(|output| {
                    output.task_run.task_id == task_id && output.task_run.run_epoch == epoch
                })
                .cloned()
                .collect())
        })
    }
}
